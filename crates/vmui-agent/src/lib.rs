use std::{net::SocketAddr, pin::Pin, sync::Arc};

use anyhow::{Context, Result};
use chrono::Utc;
use futures_core::Stream;
use futures_util::StreamExt;
use tokio::sync::{mpsc, RwLock};
use tokio_stream::{iter, wrappers::ReceiverStream};
use tonic::{transport::Server, Request, Response, Status};
use tracing::{info, warn};
use vmui_core::{AgentConfig, AgentRuntimeState, SessionRuntime};
use vmui_platform::{BackendArtifact, BackendEvent, BackendSessionParams, UiBackend};
use vmui_protocol as domain;
use vmui_transport_grpc::{pb, ConvertError};

type SharedState = Arc<RwLock<AgentRuntimeState>>;
type SessionStream = ReceiverStream<Result<pb::ServerMsg, Status>>;
type ArtifactStream =
    Pin<Box<dyn Stream<Item = Result<pb::ArtifactChunk, Status>> + Send + 'static>>;

pub struct UiAgentService<B> {
    backend: Arc<B>,
    server_version: String,
    state: SharedState,
}

impl<B> UiAgentService<B>
where
    B: UiBackend + 'static,
{
    pub fn new(config: AgentConfig, backend: B) -> Result<Self> {
        let state = AgentRuntimeState::new(&config)?;
        Ok(Self {
            backend: Arc::new(backend),
            server_version: env!("CARGO_PKG_VERSION").to_owned(),
            state: Arc::new(RwLock::new(state)),
        })
    }

    pub fn state_handle(&self) -> SharedState {
        Arc::clone(&self.state)
    }

    async fn process_session(
        &self,
        mut inbound: tonic::Streaming<pb::ClientMsg>,
        tx: mpsc::Sender<Result<pb::ServerMsg, Status>>,
    ) -> Result<(), Status> {
        let mut context: Option<SessionContext> = None;

        while let Some(message) = inbound.next().await {
            let message = message?;
            let client_message =
                domain::ClientMessage::try_from(message).map_err(invalid_argument_status)?;

            match client_message {
                domain::ClientMessage::Hello(hello) => {
                    if context.is_some() {
                        return Err(Status::failed_precondition(
                            "hello has already been received for this session",
                        ));
                    }

                    let session_id = domain::SessionId::new("sess");
                    let runtime = SessionRuntime::new(
                        session_id.clone(),
                        hello.requested_mode.clone(),
                        self.backend.backend_id(),
                    );

                    {
                        let mut state = self.state.write().await;
                        state.sessions.open_session(runtime);
                    }

                    let ack = domain::HelloAck {
                        session_id: session_id.clone(),
                        server_version: self.server_version.clone(),
                        backend_id: self.backend.backend_id().to_owned(),
                        capabilities: backend_capabilities(&*self.backend),
                        negotiated_mode: hello.requested_mode.clone(),
                    };
                    send_server_message(&tx, domain::ServerMessage::HelloAck(ack)).await?;

                    context = Some(SessionContext {
                        session_id,
                        mode: hello.requested_mode,
                        subscribed: false,
                        event_task: None,
                    });
                }
                domain::ClientMessage::Subscribe(subscribe) => {
                    let context = context
                        .as_mut()
                        .ok_or_else(|| Status::failed_precondition("hello must be sent first"))?;

                    if context.subscribed {
                        return Err(Status::failed_precondition(
                            "subscribe has already been received for this session",
                        ));
                    }

                    let params = BackendSessionParams {
                        session_id: context.session_id.clone(),
                        mode: context.mode.clone(),
                        shallow: subscribe.shallow,
                    };

                    {
                        let mut state = self.state.write().await;
                        state
                            .sessions
                            .mark_subscribed(&context.session_id, subscribe.shallow)
                            .map_err(internal_status)?;
                    }

                    let (snapshot, event_stream) = if subscribe.include_diff_stream {
                        let backend_session = self
                            .backend
                            .open_session(params.clone())
                            .await
                            .map_err(internal_status)?;
                        (
                            backend_session.initial_snapshot,
                            Some(backend_session.events),
                        )
                    } else {
                        (
                            self.backend
                                .capture_snapshot(params.clone())
                                .await
                                .map_err(internal_status)?,
                            None,
                        )
                    };

                    {
                        let mut state = self.state.write().await;
                        state
                            .sessions
                            .apply_snapshot(&context.session_id, snapshot.clone())
                            .map_err(internal_status)?;
                    }

                    send_server_message(&tx, domain::ServerMessage::InitialSnapshot(snapshot))
                        .await?;

                    if let Some(stream) = event_stream {
                        let state = Arc::clone(&self.state);
                        let tx = tx.clone();
                        let backend = Arc::clone(&self.backend);
                        let session_id = context.session_id.clone();
                        let params = params.clone();
                        context.event_task = Some(tokio::spawn(async move {
                            forward_backend_events(backend, params, stream, state, session_id, tx)
                                .await;
                        }));
                    }

                    context.subscribed = true;
                }
                domain::ClientMessage::ActionRequest(action_request) => {
                    let context = context.as_ref().ok_or_else(|| {
                        Status::failed_precondition("hello must be sent before actions")
                    })?;

                    if !context.subscribed {
                        return Err(Status::failed_precondition(
                            "subscribe must be sent before actions",
                        ));
                    }

                    let action_result = self
                        .backend
                        .perform_action(action_request)
                        .await
                        .map_err(internal_status)?;
                    let vmui_platform::BackendActionResult {
                        action_id,
                        ok,
                        status,
                        message,
                        artifacts: backend_artifacts,
                    } = action_result;
                    let action_result = {
                        let mut state = self.state.write().await;
                        let artifacts = persist_action_artifacts(
                            &mut state,
                            Some(context.session_id.clone()),
                            backend_artifacts,
                        )
                        .map_err(internal_status)?;
                        domain::ActionResult {
                            action_id,
                            ok,
                            status,
                            message,
                            artifacts,
                        }
                    };
                    send_server_message(&tx, domain::ServerMessage::ActionResult(action_result))
                        .await?;
                }
                domain::ClientMessage::ReadArtifact(_) => {
                    send_server_message(
                        &tx,
                        domain::ServerMessage::Warning(domain::WarningEvent {
                            code: "use_read_artifact_rpc".to_owned(),
                            message:
                                "artifact payloads are served via the dedicated ReadArtifact RPC"
                                    .to_owned(),
                        }),
                    )
                    .await?;
                }
                domain::ClientMessage::Ping => {
                    send_server_message(&tx, domain::ServerMessage::Pong).await?;
                }
            }
        }

        if let Some(context) = context {
            if let Some(task) = context.event_task {
                task.abort();
            }

            let mut state = self.state.write().await;
            state
                .sessions
                .close_session(&context.session_id)
                .map_err(internal_status)?;
        }

        Ok(())
    }
}

#[tonic::async_trait]
impl<B> pb::ui_agent_server::UiAgent for UiAgentService<B>
where
    B: UiBackend + 'static,
{
    type SessionStream = SessionStream;
    type ReadArtifactStream = ArtifactStream;

    async fn session(
        &self,
        request: Request<tonic::Streaming<pb::ClientMsg>>,
    ) -> Result<Response<Self::SessionStream>, Status> {
        let (tx, rx) = mpsc::channel(32);
        let service = self.clone();

        tokio::spawn(async move {
            let result = service
                .process_session(request.into_inner(), tx.clone())
                .await;
            if let Err(status) = result {
                let _ = tx.send(Err(status)).await;
            }
        });

        Ok(Response::new(ReceiverStream::new(rx)))
    }

    async fn read_artifact(
        &self,
        request: Request<pb::ReadArtifactRequest>,
    ) -> Result<Response<Self::ReadArtifactStream>, Status> {
        let request = domain::ReadArtifactRequest::try_from(request.into_inner())
            .map_err(invalid_argument_status)?;
        let bytes = {
            let state = self.state.read().await;
            state
                .artifacts
                .read_bytes(&request.artifact_id)
                .map_err(internal_status)?
        };

        let artifact_id = request.artifact_id.to_string();
        let chunks = bytes
            .chunks(64 * 1024)
            .map(|chunk| {
                Ok(pb::ArtifactChunk {
                    artifact_id: artifact_id.clone(),
                    data: chunk.to_vec(),
                })
            })
            .collect::<Vec<_>>();

        Ok(Response::new(Box::pin(iter(chunks))))
    }
}

impl<B> Clone for UiAgentService<B> {
    fn clone(&self) -> Self {
        Self {
            backend: Arc::clone(&self.backend),
            server_version: self.server_version.clone(),
            state: Arc::clone(&self.state),
        }
    }
}

pub async fn run_daemon<B>(config: AgentConfig, backend: B) -> Result<()>
where
    B: UiBackend + 'static,
{
    let addr: SocketAddr = config
        .bind_addr
        .parse()
        .with_context(|| format!("invalid bind address `{}`", config.bind_addr))?;
    let service = UiAgentService::new(config.clone(), backend)?;

    info!(
        bind_addr = %addr,
        artifact_dir = %config.artifact_dir.display(),
        backend = service.backend.backend_id(),
        "starting vmui-agent daemon"
    );

    Server::builder()
        .add_service(pb::ui_agent_server::UiAgentServer::new(service))
        .serve_with_shutdown(addr, shutdown_signal())
        .await
        .context("gRPC server failed")?;

    Ok(())
}

fn persist_action_artifacts(
    state: &mut AgentRuntimeState,
    session_id: Option<domain::SessionId>,
    artifacts: Vec<BackendArtifact>,
) -> Result<Vec<domain::ArtifactDescriptor>> {
    artifacts
        .into_iter()
        .map(|artifact| {
            state
                .artifacts
                .write_bytes(
                    session_id.clone(),
                    artifact.kind,
                    artifact.mime_type,
                    &artifact.bytes,
                )
                .map_err(anyhow::Error::from)
        })
        .collect()
}

async fn forward_backend_events<B>(
    backend: Arc<B>,
    params: BackendSessionParams,
    mut stream: vmui_platform::BackendEventStream,
    state: SharedState,
    session_id: domain::SessionId,
    tx: mpsc::Sender<Result<pb::ServerMsg, Status>>,
) where
    B: UiBackend + 'static,
{
    while let Some(event) = stream.next().await {
        match event {
            BackendEvent::Diff(diff) => {
                let update_result = {
                    let mut state = state.write().await;
                    state.sessions.apply_diff_metadata(&session_id, &diff)
                };

                match update_result {
                    Ok(()) => {
                        if send_server_message(&tx, domain::ServerMessage::DiffBatch(diff))
                            .await
                            .is_err()
                        {
                            break;
                        }
                    }
                    Err(error) => {
                        let current_rev = {
                            let state = state.read().await;
                            state
                                .sessions
                                .state(&session_id)
                                .map(|record| record.revision())
                                .unwrap_or(diff.base_rev)
                        };
                        let reason = error.to_string();
                        let resync_signal = domain::UiDiffBatch {
                            base_rev: current_rev,
                            new_rev: current_rev.saturating_add(1),
                            emitted_at: Utc::now(),
                            ops: vec![domain::DiffOp::SnapshotResync {
                                reason: reason.clone(),
                            }],
                        };

                        if send_server_message(&tx, domain::ServerMessage::DiffBatch(resync_signal))
                            .await
                            .is_err()
                        {
                            break;
                        }

                        match backend.capture_snapshot(params.clone()).await {
                            Ok(snapshot) => {
                                let apply_result = {
                                    let mut state = state.write().await;
                                    state.sessions.apply_snapshot(&session_id, snapshot.clone())
                                };

                                match apply_result {
                                    Ok(()) => {
                                        if send_server_message(
                                            &tx,
                                            domain::ServerMessage::InitialSnapshot(snapshot),
                                        )
                                        .await
                                        .is_err()
                                        {
                                            break;
                                        }
                                    }
                                    Err(snapshot_error) => {
                                        if send_server_message(
                                            &tx,
                                            domain::ServerMessage::Warning(domain::WarningEvent {
                                                code: "session_resync_apply_failed".to_owned(),
                                                message: snapshot_error.to_string(),
                                            }),
                                        )
                                        .await
                                        .is_err()
                                        {
                                            break;
                                        }
                                    }
                                }
                            }
                            Err(refresh_error) => {
                                if send_server_message(
                                    &tx,
                                    domain::ServerMessage::Warning(domain::WarningEvent {
                                        code: "session_resync_refresh_failed".to_owned(),
                                        message: refresh_error.to_string(),
                                    }),
                                )
                                .await
                                .is_err()
                                {
                                    break;
                                }
                            }
                        }
                    }
                }
            }
            BackendEvent::Warning { code, message } => {
                if send_server_message(
                    &tx,
                    domain::ServerMessage::Warning(domain::WarningEvent { code, message }),
                )
                .await
                .is_err()
                {
                    break;
                }
            }
        }
    }
}

async fn send_server_message(
    tx: &mpsc::Sender<Result<pb::ServerMsg, Status>>,
    message: domain::ServerMessage,
) -> Result<(), Status> {
    tx.send(Ok(message.into()))
        .await
        .map_err(|_| Status::cancelled("session stream closed"))
}

fn backend_capabilities<B: UiBackend>(backend: &B) -> Vec<String> {
    let capabilities = backend.capabilities();
    let mut labels = vec!["grpc-session".to_owned(), "artifact-read".to_owned()];
    if capabilities.supports_uia {
        labels.push("uia".to_owned());
    }
    if capabilities.supports_msaa {
        labels.push("msaa".to_owned());
    }
    if capabilities.supports_ocr_fallback {
        labels.push("ocr-fallback".to_owned());
    }
    if capabilities.supports_artifacts {
        labels.push("artifacts".to_owned());
    }
    labels
}

fn invalid_argument_status(error: ConvertError) -> Status {
    Status::invalid_argument(error.to_string())
}

fn internal_status(error: impl std::fmt::Display) -> Status {
    Status::internal(error.to_string())
}

async fn shutdown_signal() {
    if let Err(error) = tokio::signal::ctrl_c().await {
        warn!(%error, "failed to listen for shutdown signal");
    }
}

struct SessionContext {
    session_id: domain::SessionId,
    mode: domain::SessionMode,
    subscribed: bool,
    event_task: Option<tokio::task::JoinHandle<()>>,
}

#[cfg(test)]
mod tests {
    use std::{
        collections::{BTreeMap, VecDeque},
        sync::{Arc, Mutex},
    };

    use chrono::{TimeZone, Utc};
    use futures_util::stream;
    use tempfile::tempdir;
    use tokio::{
        net::TcpListener,
        sync::{mpsc, oneshot},
    };
    use tokio_stream::wrappers::{ReceiverStream, TcpListenerStream};
    use tonic::transport::{Channel, Endpoint, Server};
    use vmui_platform::{
        BackendActionResult, BackendArtifact, BackendCapabilities, BackendEvent, BackendSession,
        BackendSessionParams, UiBackend,
    };
    use vmui_protocol::{
        ActionId, ActionRequest, ActionStatus, ActionTarget, BackendKind, CapturePolicy, DiffOp,
        ElementId, ElementNode, ElementStates, Locator, Rect, SessionMode, UiDiffBatch, UiSnapshot,
        WindowId, WindowState,
    };
    use vmui_transport_grpc::encode_action_request;
    use vmui_transport_grpc::pb::{self, client_msg, server_msg, ui_agent_client::UiAgentClient};

    use super::*;

    #[derive(Clone)]
    struct StubBackend {
        snapshots: Arc<Mutex<VecDeque<u64>>>,
        events: Vec<BackendEvent>,
        action_artifacts: Vec<BackendArtifact>,
    }

    impl Default for StubBackend {
        fn default() -> Self {
            Self {
                snapshots: Arc::new(Mutex::new(VecDeque::from([1]))),
                events: Vec::new(),
                action_artifacts: vec![BackendArtifact {
                    kind: "log-text".to_owned(),
                    mime_type: "text/plain".to_owned(),
                    bytes: b"hello".to_vec(),
                }],
            }
        }
    }

    impl StubBackend {
        fn with_events_and_snapshots(
            events: Vec<BackendEvent>,
            snapshot_revs: impl IntoIterator<Item = u64>,
        ) -> Self {
            Self {
                snapshots: Arc::new(Mutex::new(snapshot_revs.into_iter().collect())),
                events,
                ..Self::default()
            }
        }

        fn next_snapshot_rev(&self) -> u64 {
            let mut snapshots = self.snapshots.lock().expect("snapshot mutex poisoned");
            match snapshots.len() {
                0 => 1,
                1 => *snapshots.front().expect("single snapshot rev"),
                _ => snapshots.pop_front().expect("next snapshot rev"),
            }
        }
    }

    #[tonic::async_trait]
    impl UiBackend for StubBackend {
        fn backend_id(&self) -> &'static str {
            "stub-backend"
        }

        fn capabilities(&self) -> BackendCapabilities {
            BackendCapabilities {
                supports_uia: false,
                supports_msaa: false,
                supports_ocr_fallback: false,
                supports_artifacts: true,
            }
        }

        async fn open_session(
            &self,
            params: BackendSessionParams,
        ) -> anyhow::Result<BackendSession> {
            Ok(BackendSession {
                initial_snapshot: sample_snapshot(
                    params.session_id.clone(),
                    params.mode.clone(),
                    self.next_snapshot_rev(),
                ),
                events: Box::pin(stream::iter(self.events.clone())),
            })
        }

        async fn capture_snapshot(
            &self,
            params: BackendSessionParams,
        ) -> anyhow::Result<UiSnapshot> {
            Ok(sample_snapshot(
                params.session_id,
                params.mode,
                self.next_snapshot_rev(),
            ))
        }

        async fn perform_action(
            &self,
            action: ActionRequest,
        ) -> anyhow::Result<BackendActionResult> {
            Ok(BackendActionResult {
                action_id: action.action_id,
                ok: true,
                status: ActionStatus::Completed,
                message: "ok".to_owned(),
                artifacts: self.action_artifacts.clone(),
            })
        }
    }

    #[tokio::test]
    async fn session_returns_ack_then_initial_snapshot() {
        let dir = tempdir().expect("tempdir");
        let (mut client, shutdown) = spawn_test_server(dir.path().to_path_buf())
            .await
            .expect("spawn server");

        let outbound = tokio_stream::iter(vec![
            pb::ClientMsg {
                payload: Some(client_msg::Payload::Hello(pb::Hello {
                    client_name: "test".to_owned(),
                    client_version: "0.1.0".to_owned(),
                    requested_mode: pb::SessionMode::Configurator as i32,
                })),
            },
            pb::ClientMsg {
                payload: Some(client_msg::Payload::Subscribe(pb::Subscribe {
                    include_initial_snapshot: true,
                    include_diff_stream: true,
                    shallow: true,
                })),
            },
        ]);

        let response = client.session(outbound).await.expect("session RPC");
        let mut inbound = response.into_inner();

        let first = inbound
            .message()
            .await
            .expect("stream read")
            .expect("hello ack");
        let second = inbound
            .message()
            .await
            .expect("stream read")
            .expect("snapshot");

        match first.payload.expect("payload") {
            server_msg::Payload::HelloAck(ack) => {
                assert_eq!(ack.backend_id, "stub-backend");
                assert_eq!(ack.negotiated_mode, pb::SessionMode::Configurator as i32);
            }
            other => panic!("unexpected first payload: {other:?}"),
        }

        match second.payload.expect("payload") {
            server_msg::Payload::InitialSnapshot(snapshot) => {
                assert_eq!(snapshot.rev, 1);
                assert_eq!(snapshot.mode, pb::SessionMode::Configurator as i32);
                assert_eq!(snapshot.windows.len(), 1);
            }
            other => panic!("unexpected second payload: {other:?}"),
        }

        let _ = shutdown.send(());
    }

    #[tokio::test]
    async fn read_artifact_streams_written_bytes() {
        let dir = tempdir().expect("tempdir");
        let (mut client, shutdown, state) = spawn_test_server_with_state(dir.path().to_path_buf())
            .await
            .expect("spawn server");
        let artifact_id = {
            let mut state = state.write().await;
            state
                .artifacts
                .write_bytes(
                    None,
                    "snapshot-json",
                    "application/json",
                    br#"{"hello":"world"}"#,
                )
                .expect("write artifact")
                .artifact_id
                .to_string()
        };

        let response = client
            .read_artifact(pb::ReadArtifactRequest { artifact_id })
            .await
            .expect("read_artifact");
        let mut stream = response.into_inner();
        let mut bytes = Vec::new();

        while let Some(chunk) = stream.message().await.expect("chunk read") {
            bytes.extend_from_slice(&chunk.data);
        }

        assert_eq!(bytes, br#"{"hello":"world"}"#);
        let _ = shutdown.send(());
    }

    #[tokio::test]
    async fn action_request_roundtrips_over_grpc() {
        let dir = tempdir().expect("tempdir");
        let (mut client, shutdown) = spawn_test_server(dir.path().to_path_buf())
            .await
            .expect("spawn server");
        let action = encode_action_request(&ActionRequest {
            action_id: ActionId::from("act-1"),
            timeout_ms: 5_000,
            target: ActionTarget::Desktop,
            kind: domain::ActionKind::ListWindows,
            capture_policy: CapturePolicy::OnFailure,
        })
        .expect("encode action");

        let outbound = tokio_stream::iter(vec![
            pb::ClientMsg {
                payload: Some(client_msg::Payload::Hello(pb::Hello {
                    client_name: "test".to_owned(),
                    client_version: "0.1.0".to_owned(),
                    requested_mode: pb::SessionMode::EnterpriseUi as i32,
                })),
            },
            pb::ClientMsg {
                payload: Some(client_msg::Payload::Subscribe(pb::Subscribe {
                    include_initial_snapshot: false,
                    include_diff_stream: false,
                    shallow: false,
                })),
            },
            pb::ClientMsg {
                payload: Some(client_msg::Payload::ActionRequest(action)),
            },
        ]);

        let response = client.session(outbound).await.expect("session RPC");
        let mut inbound = response.into_inner();
        let _ = inbound.message().await.expect("ack read").expect("ack");
        let snapshot = inbound
            .message()
            .await
            .expect("snapshot read")
            .expect("snapshot payload");
        match snapshot.payload.expect("payload") {
            server_msg::Payload::InitialSnapshot(snapshot) => {
                assert_eq!(snapshot.rev, 1);
            }
            other => panic!("unexpected payload: {other:?}"),
        }
        let action_result = inbound
            .message()
            .await
            .expect("action result read")
            .expect("action result payload");

        let artifact_id = match action_result.payload.expect("payload") {
            server_msg::Payload::ActionResult(result) => {
                assert!(result.ok);
                assert_eq!(result.artifacts.len(), 1);
                assert_eq!(result.artifacts[0].kind, "log-text");
                result.artifacts[0].artifact_id.clone()
            }
            other => panic!("unexpected payload: {other:?}"),
        };

        let read = client
            .read_artifact(pb::ReadArtifactRequest { artifact_id })
            .await
            .expect("read action artifact");
        let mut stream = read.into_inner();
        let mut bytes = Vec::new();
        while let Some(chunk) = stream.message().await.expect("artifact chunk") {
            bytes.extend_from_slice(&chunk.data);
        }

        assert_eq!(bytes, b"hello");
        let _ = shutdown.send(());
    }

    #[tokio::test]
    async fn stale_diff_triggers_resync_event_and_snapshot_refresh() {
        let dir = tempdir().expect("tempdir");
        let backend = StubBackend::with_events_and_snapshots(
            vec![BackendEvent::Diff(UiDiffBatch {
                base_rev: 9,
                new_rev: 10,
                emitted_at: Utc
                    .timestamp_millis_opt(1_700_000_001_000)
                    .single()
                    .unwrap(),
                ops: vec![DiffOp::FocusChanged {
                    window_id: WindowId::from("wnd-1"),
                    element_id: Some(ElementId::from("elt-1")),
                }],
            })],
            [10, 11],
        );
        let (mut client, shutdown, state) =
            spawn_test_server_with_backend(dir.path().to_path_buf(), backend)
                .await
                .expect("spawn server");

        let (outbound_tx, outbound_rx) = mpsc::channel(4);
        outbound_tx
            .send(pb::ClientMsg {
                payload: Some(client_msg::Payload::Hello(pb::Hello {
                    client_name: "test".to_owned(),
                    client_version: "0.1.0".to_owned(),
                    requested_mode: pb::SessionMode::EnterpriseUi as i32,
                })),
            })
            .await
            .expect("send hello");
        outbound_tx
            .send(pb::ClientMsg {
                payload: Some(client_msg::Payload::Subscribe(pb::Subscribe {
                    include_initial_snapshot: true,
                    include_diff_stream: true,
                    shallow: false,
                })),
            })
            .await
            .expect("send subscribe");

        let response = client
            .session(ReceiverStream::new(outbound_rx))
            .await
            .expect("session RPC");
        let mut inbound = response.into_inner();
        let _ = inbound.message().await.expect("ack read").expect("ack");
        let initial = inbound
            .message()
            .await
            .expect("initial snapshot read")
            .expect("initial snapshot");
        let resync = inbound
            .message()
            .await
            .expect("resync read")
            .expect("resync payload");
        let refreshed = inbound
            .message()
            .await
            .expect("refreshed snapshot read")
            .expect("refreshed snapshot payload");

        match initial.payload.expect("payload") {
            server_msg::Payload::InitialSnapshot(snapshot) => assert_eq!(snapshot.rev, 10),
            other => panic!("unexpected payload: {other:?}"),
        }

        match resync.payload.expect("payload") {
            server_msg::Payload::DiffBatch(diff) => {
                assert_eq!(diff.ops.len(), 1);
                assert_eq!(diff.ops[0].op, "snapshot_resync");
            }
            other => panic!("unexpected payload: {other:?}"),
        }

        match refreshed.payload.expect("payload") {
            server_msg::Payload::InitialSnapshot(snapshot) => assert_eq!(snapshot.rev, 11),
            other => panic!("unexpected payload: {other:?}"),
        }

        let current_rev = {
            let state = state.read().await;
            let session_id = state
                .sessions
                .session_ids()
                .into_iter()
                .next()
                .expect("session id");
            state
                .sessions
                .runtime(&session_id)
                .and_then(|runtime| runtime.last_revision)
                .expect("last revision")
        };

        assert_eq!(current_rev, 11);
        drop(outbound_tx);
        let _ = shutdown.send(());
    }

    async fn spawn_test_server(
        artifact_dir: std::path::PathBuf,
    ) -> Result<(UiAgentClient<Channel>, oneshot::Sender<()>), Box<dyn std::error::Error>> {
        let (client, shutdown, _) =
            spawn_test_server_with_backend(artifact_dir, StubBackend::default()).await?;
        Ok((client, shutdown))
    }

    async fn spawn_test_server_with_state(
        artifact_dir: std::path::PathBuf,
    ) -> Result<
        (UiAgentClient<Channel>, oneshot::Sender<()>, SharedState),
        Box<dyn std::error::Error>,
    > {
        spawn_test_server_with_backend(artifact_dir, StubBackend::default()).await
    }

    async fn spawn_test_server_with_backend(
        artifact_dir: std::path::PathBuf,
        backend: StubBackend,
    ) -> Result<
        (UiAgentClient<Channel>, oneshot::Sender<()>, SharedState),
        Box<dyn std::error::Error>,
    > {
        let listener = TcpListener::bind("127.0.0.1:0").await?;
        let addr = listener.local_addr()?;
        let incoming = TcpListenerStream::new(listener);
        let (shutdown_tx, shutdown_rx) = oneshot::channel();

        let service = UiAgentService::new(
            AgentConfig {
                bind_addr: addr.to_string(),
                artifact_dir,
                default_mode: SessionMode::EnterpriseUi,
            },
            backend,
        )?;
        let state = service.state_handle();

        tokio::spawn(async move {
            Server::builder()
                .add_service(pb::ui_agent_server::UiAgentServer::new(service))
                .serve_with_incoming_shutdown(incoming, async move {
                    let _ = shutdown_rx.await;
                })
                .await
                .expect("server must run");
        });

        let client = UiAgentClient::connect(
            Endpoint::from_shared(format!("http://{addr}"))?
                .connect_timeout(std::time::Duration::from_secs(2)),
        )
        .await?;

        Ok((client, shutdown_tx, state))
    }

    fn sample_snapshot(session_id: domain::SessionId, mode: SessionMode, rev: u64) -> UiSnapshot {
        UiSnapshot {
            session_id,
            rev,
            mode,
            captured_at: Utc
                .timestamp_millis_opt(1_700_000_000_123)
                .single()
                .unwrap(),
            windows: vec![WindowState {
                window_id: WindowId::from("wnd-1"),
                pid: 10,
                process_name: Some("1cv8.exe".to_owned()),
                title: "1C".to_owned(),
                bounds: Rect {
                    left: 0,
                    top: 0,
                    width: 800,
                    height: 600,
                },
                root: ElementNode {
                    element_id: ElementId::from("elt-1"),
                    parent_id: None,
                    backend: BackendKind::Mixed,
                    control_type: "Window".to_owned(),
                    class_name: Some("V8Wnd".to_owned()),
                    name: Some("1C".to_owned()),
                    automation_id: Some("root".to_owned()),
                    native_window_handle: Some(10),
                    bounds: Rect {
                        left: 0,
                        top: 0,
                        width: 800,
                        height: 600,
                    },
                    locator: Locator {
                        window_fingerprint: "1cv8.exe:1C".to_owned(),
                        path: vec![],
                    },
                    properties: BTreeMap::new(),
                    states: ElementStates {
                        enabled: true,
                        visible: true,
                        focused: true,
                        selected: false,
                        expanded: false,
                        toggled: false,
                    },
                    children: vec![],
                    confidence: 1.0,
                },
            }],
        }
    }
}

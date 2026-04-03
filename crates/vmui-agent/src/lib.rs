use std::{net::SocketAddr, pin::Pin, sync::Arc, time::Duration};

use anyhow::{Context, Result};
use chrono::Utc;
use futures_core::Stream;
use futures_util::StreamExt;
use serde::Serialize;
use serde_json::json;
use tokio::{
    sync::{mpsc, watch, RwLock},
    time::{timeout, Instant},
};
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

struct ResolvedElement<'a> {
    window: &'a domain::WindowState,
    node: &'a domain::ElementNode,
}

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

    async fn execute_action(
        &self,
        context: &SessionContext,
        action: domain::ActionRequest,
    ) -> Result<domain::ActionResult, Status> {
        let action = self.enrich_action_target(context, action).await;

        match action.kind.clone() {
            domain::ActionKind::ListWindows => self.execute_list_windows(context, action).await,
            domain::ActionKind::GetTree(request) => {
                self.execute_get_tree(context, action, request).await
            }
            domain::ActionKind::WaitFor(options) => {
                self.execute_wait_for(context, action, options).await
            }
            domain::ActionKind::WriteArtifact(options) => {
                self.execute_write_artifact(context, action, options).await
            }
            domain::ActionKind::OcrRegion(options)
                if !self.backend.capabilities().supports_ocr_fallback =>
            {
                self.execute_unsupported_ocr(context, action, options).await
            }
            _ => self.execute_backend_action(context, action).await,
        }
    }

    async fn enrich_action_target(
        &self,
        context: &SessionContext,
        mut action: domain::ActionRequest,
    ) -> domain::ActionRequest {
        let Ok(snapshot) = self.best_effort_snapshot(context).await else {
            return action;
        };

        match &mut action.target {
            domain::ActionTarget::Window(locator) => {
                if let Some(window) = resolve_window(&snapshot, locator) {
                    locator
                        .window_id
                        .get_or_insert_with(|| window.window_id.clone());
                    locator.title.get_or_insert_with(|| window.title.clone());
                    locator.pid.get_or_insert(window.pid);
                }
            }
            domain::ActionTarget::Element(locator) => {
                if let Some(resolved) = resolve_element(&snapshot, locator) {
                    locator
                        .element_id
                        .get_or_insert_with(|| resolved.node.element_id.clone());
                    locator
                        .locator
                        .get_or_insert_with(|| resolved.node.locator.clone());
                }
            }
            domain::ActionTarget::Region(region) => {
                if region.window_id.is_none() && snapshot.windows.len() == 1 {
                    region.window_id = Some(snapshot.windows[0].window_id.clone());
                }
            }
            domain::ActionTarget::Desktop => {}
        }

        action
    }

    async fn best_effort_snapshot(
        &self,
        context: &SessionContext,
    ) -> Result<domain::UiSnapshot, Status> {
        let (snapshot, resync_reason, current_rev) = {
            let state = self.state.read().await;
            let session_state = state
                .sessions
                .state(&context.session_id)
                .ok_or_else(|| Status::failed_precondition("session state is unavailable"))?;
            (
                session_state.snapshot().cloned(),
                session_state.resync_reason().map(ToOwned::to_owned),
                session_state.revision(),
            )
        };

        if let Some(snapshot) = snapshot {
            if resync_reason.is_none() {
                return Ok(snapshot);
            }
        }

        let mut snapshot = self
            .backend
            .capture_snapshot(BackendSessionParams {
                session_id: context.session_id.clone(),
                mode: context.mode.clone(),
                shallow: false,
            })
            .await
            .map_err(internal_status)?;
        normalize_snapshot_revision(&mut snapshot, current_rev);
        Ok(snapshot)
    }

    async fn execute_list_windows(
        &self,
        context: &SessionContext,
        action: domain::ActionRequest,
    ) -> Result<domain::ActionResult, Status> {
        let snapshot = self.best_effort_snapshot(context).await?;
        let artifacts = {
            let mut state = self.state.write().await;
            vec![write_json_artifact(
                &mut state,
                Some(context.session_id.clone()),
                "snapshot-json",
                &snapshot,
            )
            .map_err(internal_status)?]
        };

        Ok(domain::ActionResult {
            action_id: action.action_id,
            ok: true,
            status: domain::ActionStatus::Completed,
            message: format!(
                "stored {} windows from revision {}",
                snapshot.windows.len(),
                snapshot.rev
            ),
            artifacts,
        })
    }

    async fn execute_get_tree(
        &self,
        context: &SessionContext,
        action: domain::ActionRequest,
        request: domain::TreeRequest,
    ) -> Result<domain::ActionResult, Status> {
        let snapshot = self.best_effort_snapshot(context).await?;
        let payload = build_tree_payload(&snapshot, &action.target, request.raw, request.max_depth)
            .ok_or_else(|| Status::not_found("requested tree target was not found"))?;
        let artifacts = {
            let mut state = self.state.write().await;
            vec![write_json_artifact(
                &mut state,
                Some(context.session_id.clone()),
                "snapshot-json",
                &payload,
            )
            .map_err(internal_status)?]
        };

        Ok(domain::ActionResult {
            action_id: action.action_id,
            ok: true,
            status: domain::ActionStatus::Completed,
            message: "stored requested tree".to_owned(),
            artifacts,
        })
    }

    async fn execute_write_artifact(
        &self,
        context: &SessionContext,
        action: domain::ActionRequest,
        options: domain::WriteArtifactOptions,
    ) -> Result<domain::ActionResult, Status> {
        let snapshot = self.best_effort_snapshot(context).await?;
        let payload = build_write_artifact_payload(&snapshot, &action.target, options.note.clone())
            .ok_or_else(|| Status::not_found("requested artifact target was not found"))?;
        let kind = if options.kind.trim().is_empty() {
            "snapshot-json"
        } else {
            options.kind.as_str()
        };
        let artifacts = {
            let mut state = self.state.write().await;
            vec![
                write_json_artifact(&mut state, Some(context.session_id.clone()), kind, &payload)
                    .map_err(internal_status)?,
            ]
        };

        Ok(domain::ActionResult {
            action_id: action.action_id,
            ok: true,
            status: domain::ActionStatus::Completed,
            message: format!("stored artifact `{kind}`"),
            artifacts,
        })
    }

    async fn execute_wait_for(
        &self,
        context: &SessionContext,
        action: domain::ActionRequest,
        options: domain::WaitForOptions,
    ) -> Result<domain::ActionResult, Status> {
        let deadline = Instant::now() + Duration::from_millis(action.timeout_ms.max(1));
        let required_stability = Duration::from_millis(options.stable_for_ms);
        let mut revision_rx = context.revision_tx.subscribe();
        let mut satisfied_since: Option<Instant> = None;

        loop {
            let snapshot = self.best_effort_snapshot(context).await?;
            if wait_condition_matches(&snapshot, &action.target, &options.condition) {
                let now = Instant::now();
                let since = satisfied_since.get_or_insert(now);
                if now.duration_since(*since) >= required_stability {
                    let mut result = domain::ActionResult {
                        action_id: action.action_id.clone(),
                        ok: true,
                        status: domain::ActionStatus::Completed,
                        message: format!("condition `{:?}` satisfied", options.condition),
                        artifacts: Vec::new(),
                    };
                    self.maybe_attach_snapshot_artifact(
                        context,
                        &action,
                        true,
                        &mut result.artifacts,
                    )
                    .await?;
                    return Ok(result);
                }
            } else {
                satisfied_since = None;
            }

            let now = Instant::now();
            if now >= deadline {
                let mut result = domain::ActionResult {
                    action_id: action.action_id.clone(),
                    ok: false,
                    status: domain::ActionStatus::TimedOut,
                    message: format!(
                        "condition `{:?}` was not satisfied before timeout",
                        options.condition
                    ),
                    artifacts: Vec::new(),
                };
                self.maybe_attach_snapshot_artifact(context, &action, false, &mut result.artifacts)
                    .await?;
                return Ok(result);
            }

            let wait_for = deadline.saturating_duration_since(now);
            if timeout(wait_for, revision_rx.changed()).await.is_err() {
                let mut result = domain::ActionResult {
                    action_id: action.action_id.clone(),
                    ok: false,
                    status: domain::ActionStatus::TimedOut,
                    message: format!(
                        "condition `{:?}` was not satisfied before timeout",
                        options.condition
                    ),
                    artifacts: Vec::new(),
                };
                self.maybe_attach_snapshot_artifact(context, &action, false, &mut result.artifacts)
                    .await?;
                return Ok(result);
            }
        }
    }

    async fn execute_unsupported_ocr(
        &self,
        context: &SessionContext,
        action: domain::ActionRequest,
        options: domain::OcrOptions,
    ) -> Result<domain::ActionResult, Status> {
        let screenshot_request = domain::ActionRequest {
            action_id: domain::ActionId::from(format!("{}-capture", action.action_id)),
            timeout_ms: action.timeout_ms,
            target: action.target.clone(),
            kind: domain::ActionKind::CaptureRegion(domain::CaptureOptions {
                format: domain::CaptureFormat::Png,
            }),
            capture_policy: domain::CapturePolicy::Never,
        };
        let screenshot_artifacts = match self.backend.perform_action(screenshot_request).await {
            Ok(result) => {
                let mut state = self.state.write().await;
                persist_action_artifacts(
                    &mut state,
                    Some(context.session_id.clone()),
                    result.artifacts,
                )
                .map_err(internal_status)?
            }
            Err(_) => Vec::new(),
        };

        let ocr_artifact = {
            let payload = json!({
                "supported": false,
                "language_hint": options.language_hint,
                "message": "ocr fallback is not available on the current backend"
            });
            let mut state = self.state.write().await;
            write_json_artifact(
                &mut state,
                Some(context.session_id.clone()),
                "ocr-json",
                &payload,
            )
            .map_err(internal_status)?
        };

        let mut artifacts = screenshot_artifacts;
        artifacts.push(ocr_artifact);
        Ok(domain::ActionResult {
            action_id: action.action_id,
            ok: false,
            status: domain::ActionStatus::Unsupported,
            message: "ocr fallback is not available on the current backend".to_owned(),
            artifacts,
        })
    }

    async fn execute_backend_action(
        &self,
        context: &SessionContext,
        action: domain::ActionRequest,
    ) -> Result<domain::ActionResult, Status> {
        let action_timeout = Duration::from_millis(action.timeout_ms.max(1));
        let action_result =
            match timeout(action_timeout, self.backend.perform_action(action.clone())).await {
                Ok(result) => result.map_err(internal_status)?,
                Err(_) => {
                    let mut action_result = domain::ActionResult {
                        action_id: action.action_id.clone(),
                        ok: false,
                        status: domain::ActionStatus::TimedOut,
                        message: format!(
                            "action `{}` did not complete before timeout",
                            action_kind_label(&action.kind)
                        ),
                        artifacts: Vec::new(),
                    };
                    self.maybe_attach_snapshot_artifact(
                        context,
                        &action,
                        false,
                        &mut action_result.artifacts,
                    )
                    .await?;
                    return Ok(action_result);
                }
            };
        let vmui_platform::BackendActionResult {
            action_id,
            ok,
            status,
            message,
            artifacts: backend_artifacts,
        } = action_result;
        let mut action_result = {
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
        self.maybe_attach_snapshot_artifact(
            context,
            &action,
            action_result.ok && action_result.status == domain::ActionStatus::Completed,
            &mut action_result.artifacts,
        )
        .await?;
        Ok(action_result)
    }

    async fn maybe_attach_snapshot_artifact(
        &self,
        context: &SessionContext,
        action: &domain::ActionRequest,
        success: bool,
        artifacts: &mut Vec<domain::ArtifactDescriptor>,
    ) -> Result<(), Status> {
        if !should_attach_snapshot_artifact(&action.kind, &action.capture_policy, success)
            || artifacts
                .iter()
                .any(|artifact| artifact.kind == "snapshot-json")
        {
            return Ok(());
        }

        let Ok(snapshot) = self.best_effort_snapshot(context).await else {
            return Ok(());
        };
        let artifact = {
            let mut state = self.state.write().await;
            write_json_artifact(
                &mut state,
                Some(context.session_id.clone()),
                "snapshot-json",
                &snapshot,
            )
            .map_err(internal_status)?
        };
        artifacts.push(artifact);
        Ok(())
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
                    let (revision_tx, _) = watch::channel(0);

                    context = Some(SessionContext {
                        session_id,
                        mode: hello.requested_mode,
                        subscribed: false,
                        event_task: None,
                        revision_tx,
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

                    let backend_session = self
                        .backend
                        .open_session(params.clone())
                        .await
                        .map_err(internal_status)?;
                    let snapshot = backend_session.initial_snapshot;
                    let event_stream = Some(backend_session.events);

                    {
                        let mut state = self.state.write().await;
                        state
                            .sessions
                            .apply_snapshot(&context.session_id, snapshot.clone())
                            .map_err(internal_status)?;
                    }
                    let _ = context.revision_tx.send(snapshot.rev);

                    send_server_message(&tx, domain::ServerMessage::InitialSnapshot(snapshot))
                        .await?;

                    if let Some(stream) = event_stream {
                        let state = Arc::clone(&self.state);
                        let tx = tx.clone();
                        let backend = Arc::clone(&self.backend);
                        let session_id = context.session_id.clone();
                        let params = params.clone();
                        let revision_tx = context.revision_tx.clone();
                        context.event_task = Some(tokio::spawn(async move {
                            forward_backend_events(
                                backend,
                                params,
                                stream,
                                state,
                                session_id,
                                tx,
                                revision_tx,
                                subscribe.include_diff_stream,
                            )
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

                    let action_result = self.execute_action(context, action_request).await?;
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

fn write_json_artifact<T: Serialize>(
    state: &mut AgentRuntimeState,
    session_id: Option<domain::SessionId>,
    kind: &str,
    value: &T,
) -> Result<domain::ArtifactDescriptor> {
    let bytes = serde_json::to_vec_pretty(value)?;
    state
        .artifacts
        .write_bytes(session_id, kind, "application/json", &bytes)
        .map_err(anyhow::Error::from)
}

fn normalize_snapshot_revision(snapshot: &mut domain::UiSnapshot, current_rev: domain::Revision) {
    if snapshot.rev <= current_rev {
        snapshot.rev = current_rev.saturating_add(1);
    }
}

fn build_tree_payload(
    snapshot: &domain::UiSnapshot,
    target: &domain::ActionTarget,
    raw: bool,
    max_depth: Option<u32>,
) -> Option<serde_json::Value> {
    match target {
        domain::ActionTarget::Desktop => {
            let mut snapshot = snapshot.clone();
            if let Some(max_depth) = max_depth {
                for window in &mut snapshot.windows {
                    truncate_node_depth(&mut window.root, max_depth);
                }
            }
            if raw {
                Some(json!(snapshot))
            } else {
                Some(json!({ "snapshot": snapshot }))
            }
        }
        domain::ActionTarget::Window(locator) => {
            let mut window = resolve_window(snapshot, locator)?.clone();
            if let Some(max_depth) = max_depth {
                truncate_node_depth(&mut window.root, max_depth);
            }
            if raw {
                Some(json!(window))
            } else {
                Some(json!({ "window": window }))
            }
        }
        domain::ActionTarget::Element(locator) => {
            let resolved = resolve_element(snapshot, locator)?;
            let mut node = resolved.node.clone();
            if let Some(max_depth) = max_depth {
                truncate_node_depth(&mut node, max_depth);
            }
            if raw {
                Some(json!(node))
            } else {
                Some(json!({
                    "window_id": resolved.window.window_id.clone(),
                    "element": node
                }))
            }
        }
        domain::ActionTarget::Region(_) => None,
    }
}

fn build_write_artifact_payload(
    snapshot: &domain::UiSnapshot,
    target: &domain::ActionTarget,
    note: Option<String>,
) -> Option<serde_json::Value> {
    match target {
        domain::ActionTarget::Desktop => Some(json!({
            "note": note,
            "snapshot": snapshot
        })),
        domain::ActionTarget::Window(locator) => {
            let window = resolve_window(snapshot, locator)?;
            Some(json!({
                "note": note,
                "window": window
            }))
        }
        domain::ActionTarget::Element(locator) => {
            let resolved = resolve_element(snapshot, locator)?;
            Some(json!({
                "note": note,
                "window_id": resolved.window.window_id.clone(),
                "element": resolved.node
            }))
        }
        domain::ActionTarget::Region(region) => Some(json!({
            "note": note,
            "region": region,
            "window": region.window_id.as_ref().and_then(|window_id| {
                snapshot.windows.iter().find(|window| &window.window_id == window_id)
            })
        })),
    }
}

fn wait_condition_matches(
    snapshot: &domain::UiSnapshot,
    target: &domain::ActionTarget,
    condition: &domain::WaitCondition,
) -> bool {
    match target {
        domain::ActionTarget::Desktop => match condition {
            domain::WaitCondition::Exists
            | domain::WaitCondition::Visible
            | domain::WaitCondition::Enabled => true,
            domain::WaitCondition::Focused => snapshot
                .windows
                .iter()
                .any(|window| subtree_has_focus(&window.root)),
            domain::WaitCondition::Gone => false,
        },
        domain::ActionTarget::Window(locator) => {
            let window = resolve_window(snapshot, locator);
            match condition {
                domain::WaitCondition::Exists => window.is_some(),
                domain::WaitCondition::Visible => window
                    .map(|window| {
                        window.root.states.visible
                            && window.bounds.width > 0
                            && window.bounds.height > 0
                    })
                    .unwrap_or(false),
                domain::WaitCondition::Enabled => window
                    .map(|window| window.root.states.enabled)
                    .unwrap_or(false),
                domain::WaitCondition::Focused => window
                    .map(|window| subtree_has_focus(&window.root))
                    .unwrap_or(false),
                domain::WaitCondition::Gone => window.is_none(),
            }
        }
        domain::ActionTarget::Element(locator) => {
            let element = resolve_element(snapshot, locator);
            match condition {
                domain::WaitCondition::Exists => element.is_some(),
                domain::WaitCondition::Visible => element
                    .map(|element| element.node.states.visible)
                    .unwrap_or(false),
                domain::WaitCondition::Enabled => element
                    .map(|element| element.node.states.enabled)
                    .unwrap_or(false),
                domain::WaitCondition::Focused => element
                    .map(|element| element.node.states.focused)
                    .unwrap_or(false),
                domain::WaitCondition::Gone => element.is_none(),
            }
        }
        domain::ActionTarget::Region(region) => {
            let window_present = region
                .window_id
                .as_ref()
                .map(|window_id| {
                    snapshot
                        .windows
                        .iter()
                        .any(|window| &window.window_id == window_id)
                })
                .unwrap_or(true);
            match condition {
                domain::WaitCondition::Exists
                | domain::WaitCondition::Visible
                | domain::WaitCondition::Enabled => window_present,
                domain::WaitCondition::Focused => false,
                domain::WaitCondition::Gone => !window_present,
            }
        }
    }
}

fn should_attach_snapshot_artifact(
    kind: &domain::ActionKind,
    policy: &domain::CapturePolicy,
    success: bool,
) -> bool {
    let explicit_artifact_action = matches!(
        kind,
        domain::ActionKind::ListWindows
            | domain::ActionKind::GetTree(_)
            | domain::ActionKind::WriteArtifact(_)
            | domain::ActionKind::CaptureRegion(_)
            | domain::ActionKind::OcrRegion(_)
    );
    if explicit_artifact_action {
        return false;
    }

    match policy {
        domain::CapturePolicy::Never => false,
        domain::CapturePolicy::OnFailure => !success,
        domain::CapturePolicy::Always => true,
    }
}

fn resolve_window<'a>(
    snapshot: &'a domain::UiSnapshot,
    locator: &domain::WindowLocator,
) -> Option<&'a domain::WindowState> {
    if let Some(window_id) = &locator.window_id {
        return snapshot
            .windows
            .iter()
            .find(|window| &window.window_id == window_id);
    }

    let mut candidates = snapshot.windows.iter().filter(|window| {
        locator
            .title
            .as_ref()
            .map(|title| &window.title == title)
            .unwrap_or(true)
            && locator.pid.map(|pid| window.pid == pid).unwrap_or(true)
    });

    match (
        locator.title.is_some() || locator.pid.is_some(),
        snapshot.windows.len(),
    ) {
        (true, _) => candidates.next(),
        (false, 1) => snapshot.windows.first(),
        _ => None,
    }
}

fn resolve_element<'a>(
    snapshot: &'a domain::UiSnapshot,
    locator: &domain::ElementLocator,
) -> Option<ResolvedElement<'a>> {
    if let Some(element_id) = &locator.element_id {
        if let Some(found) = find_element_by_id(snapshot, element_id) {
            return Some(found);
        }
    }

    locator
        .locator
        .as_ref()
        .and_then(|locator| find_element_by_locator(snapshot, locator))
}

fn find_element_by_id<'a>(
    snapshot: &'a domain::UiSnapshot,
    element_id: &domain::ElementId,
) -> Option<ResolvedElement<'a>> {
    snapshot.windows.iter().find_map(|window| {
        find_node_by_id(&window.root, element_id).map(|node| ResolvedElement { window, node })
    })
}

fn find_element_by_locator<'a>(
    snapshot: &'a domain::UiSnapshot,
    locator: &domain::Locator,
) -> Option<ResolvedElement<'a>> {
    snapshot.windows.iter().find_map(|window| {
        if window.root.locator.window_fingerprint != locator.window_fingerprint {
            return None;
        }
        find_node_by_locator(&window.root, locator).map(|node| ResolvedElement { window, node })
    })
}

fn find_node_by_id<'a>(
    node: &'a domain::ElementNode,
    element_id: &domain::ElementId,
) -> Option<&'a domain::ElementNode> {
    if &node.element_id == element_id {
        return Some(node);
    }

    node.children
        .iter()
        .find_map(|child| find_node_by_id(child, element_id))
}

fn find_node_by_locator<'a>(
    node: &'a domain::ElementNode,
    locator: &domain::Locator,
) -> Option<&'a domain::ElementNode> {
    if &node.locator == locator {
        return Some(node);
    }

    node.children
        .iter()
        .find_map(|child| find_node_by_locator(child, locator))
}

fn subtree_has_focus(node: &domain::ElementNode) -> bool {
    node.states.focused || node.children.iter().any(subtree_has_focus)
}

fn truncate_node_depth(node: &mut domain::ElementNode, max_depth: u32) {
    if max_depth == 0 {
        node.children.clear();
        return;
    }

    for child in &mut node.children {
        truncate_node_depth(child, max_depth - 1);
    }
}

fn action_kind_label(kind: &domain::ActionKind) -> &'static str {
    match kind {
        domain::ActionKind::ListWindows => "list_windows",
        domain::ActionKind::GetTree(_) => "get_tree",
        domain::ActionKind::FocusWindow => "focus_window",
        domain::ActionKind::ClickElement(_) => "click_element",
        domain::ActionKind::SetValue(_) => "set_value",
        domain::ActionKind::Invoke => "invoke",
        domain::ActionKind::SendKeys(_) => "send_keys",
        domain::ActionKind::WaitFor(_) => "wait_for",
        domain::ActionKind::CaptureRegion(_) => "capture_region",
        domain::ActionKind::OcrRegion(_) => "ocr_region",
        domain::ActionKind::WriteArtifact(_) => "write_artifact",
    }
}

async fn forward_backend_events<B>(
    backend: Arc<B>,
    params: BackendSessionParams,
    mut stream: vmui_platform::BackendEventStream,
    state: SharedState,
    session_id: domain::SessionId,
    tx: mpsc::Sender<Result<pb::ServerMsg, Status>>,
    revision_tx: watch::Sender<domain::Revision>,
    emit_diff_stream: bool,
) where
    B: UiBackend + 'static,
{
    while let Some(event) = stream.next().await {
        match event {
            BackendEvent::Diff(diff) => {
                let update_result = {
                    let mut state = state.write().await;
                    state.sessions.apply_diff(&session_id, &diff)
                };

                match update_result {
                    Ok(()) => {
                        let _ = revision_tx.send(diff.new_rev);
                        if emit_diff_stream {
                            if send_server_message(&tx, domain::ServerMessage::DiffBatch(diff))
                                .await
                                .is_err()
                            {
                                break;
                            }
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

                        if emit_diff_stream {
                            if send_server_message(
                                &tx,
                                domain::ServerMessage::DiffBatch(resync_signal),
                            )
                            .await
                            .is_err()
                            {
                                break;
                            }
                        }

                        match backend.capture_snapshot(params.clone()).await {
                            Ok(mut snapshot) => {
                                let apply_result = {
                                    let mut state = state.write().await;
                                    let current_rev = state
                                        .sessions
                                        .state(&session_id)
                                        .map(|record| record.revision())
                                        .unwrap_or(0);
                                    normalize_snapshot_revision(&mut snapshot, current_rev);
                                    state.sessions.apply_snapshot(&session_id, snapshot.clone())
                                };

                                match apply_result {
                                    Ok(()) => {
                                        let _ = revision_tx.send(snapshot.rev);
                                        if emit_diff_stream {
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
    if capabilities.supports_live_observer {
        labels.push("observer-active".to_owned());
    } else {
        labels.push("observer-unavailable".to_owned());
    }
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
    revision_tx: watch::Sender<domain::Revision>,
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
        time::{sleep, Duration},
    };
    use tokio_stream::wrappers::{ReceiverStream, TcpListenerStream};
    use tonic::transport::{Channel, Endpoint, Server};
    use vmui_platform::{
        BackendActionResult, BackendArtifact, BackendCapabilities, BackendEvent, BackendSession,
        BackendSessionParams, UiBackend,
    };
    use vmui_protocol::{
        ActionId, ActionRequest, ActionStatus, ActionTarget, BackendKind, CapturePolicy, DiffOp,
        ElementId, ElementLocator, ElementNode, ElementStates, Locator, Rect, SessionMode,
        TreeRequest, UiDiffBatch, UiSnapshot, WaitCondition, WaitForOptions, WindowId,
        WindowLocator, WindowState,
    };
    use vmui_transport_grpc::encode_action_request;
    use vmui_transport_grpc::pb::{self, client_msg, server_msg, ui_agent_client::UiAgentClient};

    use super::*;

    #[derive(Clone)]
    struct StubBackend {
        snapshots: Arc<Mutex<VecDeque<u64>>>,
        events: Vec<BackendEvent>,
        action_ok: bool,
        action_status: ActionStatus,
        action_message: String,
        action_artifacts: Vec<BackendArtifact>,
    }

    impl Default for StubBackend {
        fn default() -> Self {
            Self {
                snapshots: Arc::new(Mutex::new(VecDeque::from([1]))),
                events: Vec::new(),
                action_ok: true,
                action_status: ActionStatus::Completed,
                action_message: "ok".to_owned(),
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

        fn with_action_result(
            status: ActionStatus,
            ok: bool,
            message: impl Into<String>,
            artifacts: Vec<BackendArtifact>,
        ) -> Self {
            Self {
                action_ok: ok,
                action_status: status,
                action_message: message.into(),
                action_artifacts: artifacts,
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
                supports_live_observer: true,
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
                ok: self.action_ok,
                status: self.action_status.clone(),
                message: self.action_message.clone(),
                artifacts: self.action_artifacts.clone(),
            })
        }
    }

    #[derive(Clone, Default)]
    struct WaitBackend;

    #[tonic::async_trait]
    impl UiBackend for WaitBackend {
        fn backend_id(&self) -> &'static str {
            "wait-backend"
        }

        fn capabilities(&self) -> BackendCapabilities {
            BackendCapabilities {
                supports_live_observer: true,
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
            let (tx, rx) = mpsc::unbounded_channel();
            tokio::spawn(async move {
                sleep(Duration::from_millis(50)).await;
                let _ = tx.send(BackendEvent::Diff(UiDiffBatch {
                    base_rev: 1,
                    new_rev: 2,
                    emitted_at: Utc::now(),
                    ops: vec![DiffOp::WindowRemoved {
                        window_id: WindowId::from("wnd-1"),
                    }],
                }));
            });

            Ok(BackendSession {
                initial_snapshot: sample_snapshot(params.session_id, params.mode, 1),
                events: Box::pin(ReceiverStream::new({
                    let (event_tx, event_rx) = tokio::sync::mpsc::channel(4);
                    tokio::spawn(async move {
                        let mut rx = rx;
                        while let Some(event) = rx.recv().await {
                            let _ = event_tx.send(event).await;
                        }
                    });
                    event_rx
                })),
            })
        }

        async fn capture_snapshot(
            &self,
            params: BackendSessionParams,
        ) -> anyhow::Result<UiSnapshot> {
            Ok(sample_snapshot(params.session_id, params.mode, 1))
        }

        async fn perform_action(
            &self,
            action: ActionRequest,
        ) -> anyhow::Result<BackendActionResult> {
            Ok(BackendActionResult {
                action_id: action.action_id,
                ok: false,
                status: ActionStatus::Unsupported,
                message: "wait backend does not execute write actions".to_owned(),
                artifacts: Vec::new(),
            })
        }
    }

    #[derive(Clone, Default)]
    struct SlowActionBackend;

    #[tonic::async_trait]
    impl UiBackend for SlowActionBackend {
        fn backend_id(&self) -> &'static str {
            "slow-action-backend"
        }

        fn capabilities(&self) -> BackendCapabilities {
            BackendCapabilities {
                supports_live_observer: true,
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
                initial_snapshot: sample_snapshot(params.session_id, params.mode, 1),
                events: Box::pin(stream::empty::<BackendEvent>()),
            })
        }

        async fn capture_snapshot(
            &self,
            params: BackendSessionParams,
        ) -> anyhow::Result<UiSnapshot> {
            Ok(sample_snapshot(params.session_id, params.mode, 1))
        }

        async fn perform_action(
            &self,
            action: ActionRequest,
        ) -> anyhow::Result<BackendActionResult> {
            sleep(Duration::from_millis(50)).await;
            Ok(BackendActionResult {
                action_id: action.action_id,
                ok: true,
                status: ActionStatus::Completed,
                message: "slow action completed".to_owned(),
                artifacts: Vec::new(),
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
    async fn list_windows_returns_snapshot_artifact_over_grpc() {
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
                assert_eq!(result.artifacts[0].kind, "snapshot-json");
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

        let payload = String::from_utf8(bytes).expect("utf8 payload");
        assert!(payload.contains("\"windows\""));
        assert!(payload.contains("\"wnd-1\""));
        let _ = shutdown.send(());
    }

    #[tokio::test]
    async fn failed_backend_action_with_on_failure_capture_attaches_snapshot_artifact() {
        let dir = tempdir().expect("tempdir");
        let backend = StubBackend::with_action_result(
            ActionStatus::Failed,
            false,
            "fallback=coordinate-click",
            Vec::new(),
        );
        let (mut client, shutdown, _) =
            spawn_test_server_with_backend(dir.path().to_path_buf(), backend)
                .await
                .expect("spawn server");
        let action = encode_action_request(&ActionRequest {
            action_id: ActionId::from("act-focus"),
            timeout_ms: 2_000,
            target: ActionTarget::Window(WindowLocator {
                window_id: Some(WindowId::from("wnd-1")),
                title: None,
                pid: None,
            }),
            kind: domain::ActionKind::FocusWindow,
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
        let _ = inbound
            .message()
            .await
            .expect("snapshot read")
            .expect("snapshot payload");
        let action_result = inbound
            .message()
            .await
            .expect("action result read")
            .expect("action result payload");

        let artifact_id = match action_result.payload.expect("payload") {
            server_msg::Payload::ActionResult(result) => {
                assert!(!result.ok);
                assert_eq!(result.status, "failed");
                assert_eq!(result.message, "fallback=coordinate-click");
                assert_eq!(result.artifacts.len(), 1);
                assert_eq!(result.artifacts[0].kind, "snapshot-json");
                result.artifacts[0].artifact_id.clone()
            }
            other => panic!("unexpected payload: {other:?}"),
        };

        let read = client
            .read_artifact(pb::ReadArtifactRequest { artifact_id })
            .await
            .expect("read failure artifact");
        let mut stream = read.into_inner();
        let mut bytes = Vec::new();
        while let Some(chunk) = stream.message().await.expect("artifact chunk") {
            bytes.extend_from_slice(&chunk.data);
        }
        let payload = String::from_utf8(bytes).expect("utf8 payload");
        assert!(payload.contains("\"wnd-1\""));
        let _ = shutdown.send(());
    }

    #[tokio::test]
    async fn wait_for_completes_from_diff_updates() {
        let dir = tempdir().expect("tempdir");
        let (mut client, shutdown, _) =
            spawn_test_server_with_backend(dir.path().to_path_buf(), WaitBackend)
                .await
                .expect("spawn server");
        let action = encode_action_request(&ActionRequest {
            action_id: ActionId::from("act-wait"),
            timeout_ms: 1_000,
            target: ActionTarget::Window(WindowLocator {
                window_id: Some(WindowId::from("wnd-1")),
                title: None,
                pid: None,
            }),
            kind: domain::ActionKind::WaitFor(WaitForOptions {
                condition: WaitCondition::Gone,
                stable_for_ms: 0,
            }),
            capture_policy: CapturePolicy::Never,
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
                    include_initial_snapshot: true,
                    include_diff_stream: true,
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
        let _ = inbound
            .message()
            .await
            .expect("snapshot read")
            .expect("snapshot payload");
        loop {
            let message = inbound
                .message()
                .await
                .expect("stream read")
                .expect("stream payload");

            match message.payload.expect("payload") {
                server_msg::Payload::DiffBatch(_) => continue,
                server_msg::Payload::ActionResult(result) => {
                    assert!(result.ok);
                    assert_eq!(result.status, "completed");
                    assert!(result.message.contains("Gone"));
                    break;
                }
                other => panic!("unexpected payload: {other:?}"),
            }
        }
        let _ = shutdown.send(());
    }

    #[tokio::test]
    async fn wait_for_completes_from_backend_events_without_diff_stream() {
        let dir = tempdir().expect("tempdir");
        let (mut client, shutdown, _) =
            spawn_test_server_with_backend(dir.path().to_path_buf(), WaitBackend)
                .await
                .expect("spawn server");
        let action = encode_action_request(&ActionRequest {
            action_id: ActionId::from("act-wait-no-diff"),
            timeout_ms: 1_000,
            target: ActionTarget::Window(WindowLocator {
                window_id: Some(WindowId::from("wnd-1")),
                title: None,
                pid: None,
            }),
            kind: domain::ActionKind::WaitFor(WaitForOptions {
                condition: WaitCondition::Gone,
                stable_for_ms: 0,
            }),
            capture_policy: CapturePolicy::Never,
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
                    include_initial_snapshot: true,
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
        let _ = inbound
            .message()
            .await
            .expect("snapshot read")
            .expect("snapshot payload");

        loop {
            let message = inbound
                .message()
                .await
                .expect("stream read")
                .expect("stream payload");

            match message.payload.expect("payload") {
                server_msg::Payload::DiffBatch(diff) => {
                    panic!("diff stream must stay disabled, got {diff:?}")
                }
                server_msg::Payload::ActionResult(result) => {
                    assert!(result.ok);
                    assert_eq!(result.status, "completed");
                    assert!(result.message.contains("Gone"));
                    break;
                }
                other => panic!("unexpected payload: {other:?}"),
            }
        }
        let _ = shutdown.send(());
    }

    #[tokio::test]
    async fn backend_action_timeout_returns_timed_out_result() {
        let dir = tempdir().expect("tempdir");
        let (mut client, shutdown, _) =
            spawn_test_server_with_backend(dir.path().to_path_buf(), SlowActionBackend)
                .await
                .expect("spawn server");
        let action = encode_action_request(&ActionRequest {
            action_id: ActionId::from("act-timeout"),
            timeout_ms: 5,
            target: ActionTarget::Window(WindowLocator {
                window_id: Some(WindowId::from("wnd-1")),
                title: None,
                pid: None,
            }),
            kind: domain::ActionKind::FocusWindow,
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
        let _ = inbound
            .message()
            .await
            .expect("snapshot read")
            .expect("snapshot payload");
        let action_result = inbound
            .message()
            .await
            .expect("action result read")
            .expect("action result payload");

        match action_result.payload.expect("payload") {
            server_msg::Payload::ActionResult(result) => {
                assert!(!result.ok);
                assert_eq!(result.status, "timed_out");
                assert!(result.message.contains("focus_window"));
                assert_eq!(result.artifacts.len(), 1);
                assert_eq!(result.artifacts[0].kind, "snapshot-json");
            }
            other => panic!("unexpected payload: {other:?}"),
        }

        let _ = shutdown.send(());
    }

    #[tokio::test]
    async fn get_tree_raw_flag_changes_artifact_shape() {
        let dir = tempdir().expect("tempdir");
        let (mut client, shutdown) = spawn_test_server(dir.path().to_path_buf())
            .await
            .expect("spawn server");
        let wrapped = encode_action_request(&ActionRequest {
            action_id: ActionId::from("act-tree-wrapped"),
            timeout_ms: 1_000,
            target: ActionTarget::Element(ElementLocator {
                element_id: Some(ElementId::from("elt-1")),
                locator: None,
            }),
            kind: domain::ActionKind::GetTree(TreeRequest {
                raw: false,
                max_depth: Some(0),
            }),
            capture_policy: CapturePolicy::Never,
        })
        .expect("encode action");
        let raw = encode_action_request(&ActionRequest {
            action_id: ActionId::from("act-tree-raw"),
            timeout_ms: 1_000,
            target: ActionTarget::Element(ElementLocator {
                element_id: Some(ElementId::from("elt-1")),
                locator: None,
            }),
            kind: domain::ActionKind::GetTree(TreeRequest {
                raw: true,
                max_depth: Some(0),
            }),
            capture_policy: CapturePolicy::Never,
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
                payload: Some(client_msg::Payload::ActionRequest(wrapped)),
            },
            pb::ClientMsg {
                payload: Some(client_msg::Payload::ActionRequest(raw)),
            },
        ]);

        let response = client.session(outbound).await.expect("session RPC");
        let mut inbound = response.into_inner();
        let _ = inbound.message().await.expect("ack read").expect("ack");
        let _ = inbound
            .message()
            .await
            .expect("snapshot read")
            .expect("snapshot payload");

        let wrapped_artifact = match inbound
            .message()
            .await
            .expect("wrapped result read")
            .expect("wrapped result payload")
            .payload
            .expect("payload")
        {
            server_msg::Payload::ActionResult(result) => {
                assert!(result.ok);
                assert_eq!(result.artifacts.len(), 1);
                result.artifacts[0].artifact_id.clone()
            }
            other => panic!("unexpected payload: {other:?}"),
        };
        let raw_artifact = match inbound
            .message()
            .await
            .expect("raw result read")
            .expect("raw result payload")
            .payload
            .expect("payload")
        {
            server_msg::Payload::ActionResult(result) => {
                assert!(result.ok);
                assert_eq!(result.artifacts.len(), 1);
                result.artifacts[0].artifact_id.clone()
            }
            other => panic!("unexpected payload: {other:?}"),
        };

        let wrapped_json = read_artifact_json(&mut client, wrapped_artifact).await;
        let raw_json = read_artifact_json(&mut client, raw_artifact).await;

        assert_eq!(
            wrapped_json
                .get("window_id")
                .and_then(serde_json::Value::as_str),
            Some("wnd-1")
        );
        assert!(wrapped_json.get("element").is_some());
        assert_eq!(
            raw_json
                .get("element_id")
                .and_then(serde_json::Value::as_str),
            Some("elt-1")
        );
        assert!(raw_json.get("element").is_none());

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

    async fn spawn_test_server_with_backend<B>(
        artifact_dir: std::path::PathBuf,
        backend: B,
    ) -> Result<
        (UiAgentClient<Channel>, oneshot::Sender<()>, SharedState),
        Box<dyn std::error::Error>,
    >
    where
        B: UiBackend + 'static,
    {
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

    async fn read_artifact_json(
        client: &mut UiAgentClient<Channel>,
        artifact_id: String,
    ) -> serde_json::Value {
        let response = client
            .read_artifact(pb::ReadArtifactRequest { artifact_id })
            .await
            .expect("read_artifact");
        let mut stream = response.into_inner();
        let mut bytes = Vec::new();
        while let Some(chunk) = stream.message().await.expect("artifact chunk") {
            bytes.extend_from_slice(&chunk.data);
        }
        serde_json::from_slice(&bytes).expect("json artifact")
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
                backend: BackendKind::Mixed,
                confidence: 0.9,
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

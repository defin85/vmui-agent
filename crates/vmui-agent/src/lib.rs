use std::{
    collections::{BTreeMap, BTreeSet},
    net::SocketAddr,
    pin::Pin,
    sync::Arc,
    time::Duration,
};

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
            domain::ActionKind::CollectDiagnosticBundle(options) => {
                self.execute_collect_diagnostic_bundle(context, action, options)
                    .await
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

    async fn execute_collect_diagnostic_bundle(
        &self,
        context: &SessionContext,
        action: domain::ActionRequest,
        options: domain::DiagnosticBundleOptions,
    ) -> Result<domain::ActionResult, Status> {
        let snapshot = self.best_effort_snapshot(context).await?;
        let recent_diffs = {
            let state = self.state.read().await;
            state
                .sessions
                .recent_diffs(&context.session_id)
                .unwrap_or_default()
        };
        let target_tree =
            build_tree_payload(&snapshot, &action.target, false, options.max_tree_depth);
        let baseline_comparison = self
            .build_baseline_comparison(&snapshot, options.baseline_artifact_id.as_ref())
            .await?;
        let fallback_surfaces = collect_fallback_surfaces(&snapshot, &action.target);
        let bundle_payload = json!({
            "diagnostic_context": {
                "session_id": context.session_id.clone(),
                "mode": context.mode.clone(),
                "step_id": options.step_id.clone(),
                "step_label": options.step_label.clone(),
                "test_verdict": options.test_verdict.clone(),
                "test_verdict_source": "external_runner",
                "daemon_diagnostic_status": "completed",
                "note": options.note.clone(),
                "baseline_artifact_id": options.baseline_artifact_id.clone(),
            },
            "target": action.target.clone(),
            "active_windows": snapshot.windows.clone(),
            "target_tree": target_tree.clone(),
            "recent_diffs": recent_diffs.clone(),
            "baseline_comparison": baseline_comparison.clone(),
            "fallback_surfaces": fallback_surfaces,
        });

        let mut artifacts = {
            let mut state = self.state.write().await;
            let mut artifacts = vec![
                write_json_artifact(
                    &mut state,
                    Some(context.session_id.clone()),
                    "diagnostic-json",
                    &bundle_payload,
                )
                .map_err(internal_status)?,
                write_json_artifact(
                    &mut state,
                    Some(context.session_id.clone()),
                    "snapshot-json",
                    &snapshot,
                )
                .map_err(internal_status)?,
                write_json_artifact(
                    &mut state,
                    Some(context.session_id.clone()),
                    "diff-json",
                    &recent_diffs,
                )
                .map_err(internal_status)?,
            ];

            if let Some(target_tree) = &target_tree {
                artifacts.push(
                    write_json_artifact(
                        &mut state,
                        Some(context.session_id.clone()),
                        "snapshot-json",
                        target_tree,
                    )
                    .map_err(internal_status)?,
                );
            }

            if let Some(comparison) = &baseline_comparison {
                artifacts.push(
                    write_json_artifact(
                        &mut state,
                        Some(context.session_id.clone()),
                        "baseline-comparison-json",
                        comparison,
                    )
                    .map_err(internal_status)?,
                );
            }

            artifacts
        };

        artifacts.extend(
            self.capture_diagnostic_target_artifacts(context, &action.target, &snapshot)
                .await?,
        );

        Ok(domain::ActionResult {
            action_id: action.action_id,
            ok: true,
            status: domain::ActionStatus::Completed,
            message: format!(
                "collected diagnostic bundle for step `{}` with {} artifacts",
                options.step_label,
                artifacts.len()
            ),
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

    async fn build_baseline_comparison(
        &self,
        snapshot: &domain::UiSnapshot,
        baseline_artifact_id: Option<&domain::ArtifactId>,
    ) -> Result<Option<serde_json::Value>, Status> {
        let Some(baseline_artifact_id) = baseline_artifact_id else {
            return Ok(None);
        };

        let bytes = {
            let state = self.state.read().await;
            match state.artifacts.read_bytes(baseline_artifact_id) {
                Ok(bytes) => bytes,
                Err(error) => {
                    return Ok(Some(json!({
                        "status": "missing_baseline_artifact",
                        "baseline_artifact_id": baseline_artifact_id,
                        "message": error.to_string(),
                    })));
                }
            }
        };

        let baseline_snapshot = match decode_baseline_snapshot(&bytes) {
            Ok(snapshot) => snapshot,
            Err(message) => {
                return Ok(Some(json!({
                    "status": "invalid_baseline_artifact",
                    "baseline_artifact_id": baseline_artifact_id,
                    "message": message,
                })));
            }
        };

        Ok(Some(compare_snapshots(
            baseline_artifact_id,
            &baseline_snapshot,
            snapshot,
        )))
    }

    async fn capture_diagnostic_target_artifacts(
        &self,
        context: &SessionContext,
        target: &domain::ActionTarget,
        snapshot: &domain::UiSnapshot,
    ) -> Result<Vec<domain::ArtifactDescriptor>, Status> {
        let Some(target) = build_diagnostic_capture_target(snapshot, target) else {
            return Ok(Vec::new());
        };

        let capture_request = domain::ActionRequest {
            action_id: domain::ActionId::new("diag-capture"),
            timeout_ms: 2_000,
            target,
            kind: domain::ActionKind::CaptureRegion(domain::CaptureOptions {
                format: domain::CaptureFormat::Png,
            }),
            capture_policy: domain::CapturePolicy::Never,
        };

        let action_result = self
            .backend
            .perform_action(capture_request)
            .await
            .map_err(internal_status)?;
        if !action_result.ok || action_result.status != domain::ActionStatus::Completed {
            return Ok(Vec::new());
        }

        let mut state = self.state.write().await;
        persist_action_artifacts(
            &mut state,
            Some(context.session_id.clone()),
            action_result.artifacts,
        )
        .map_err(internal_status)
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

fn decode_baseline_snapshot(bytes: &[u8]) -> Result<domain::UiSnapshot, String> {
    if let Ok(snapshot) = serde_json::from_slice::<domain::UiSnapshot>(bytes) {
        return Ok(snapshot);
    }

    let payload: serde_json::Value = serde_json::from_slice(bytes)
        .map_err(|error| format!("baseline json decode failed: {error}"))?;
    if let Some(snapshot) = payload.get("snapshot") {
        return serde_json::from_value(snapshot.clone())
            .map_err(|error| format!("baseline snapshot decode failed: {error}"));
    }

    Err("baseline artifact does not contain a ui snapshot".to_owned())
}

fn compare_snapshots(
    baseline_artifact_id: &domain::ArtifactId,
    expected: &domain::UiSnapshot,
    actual: &domain::UiSnapshot,
) -> serde_json::Value {
    let expected_windows = expected
        .windows
        .iter()
        .map(compared_window)
        .collect::<Vec<_>>();
    let actual_windows = actual
        .windows
        .iter()
        .map(compared_window)
        .collect::<Vec<_>>();
    let matched_windows = match_windows(&expected_windows, &actual_windows);
    let matched_expected = matched_windows
        .iter()
        .map(|pair| pair.expected_idx)
        .collect::<BTreeSet<_>>();
    let matched_actual = matched_windows
        .iter()
        .map(|pair| pair.actual_idx)
        .collect::<BTreeSet<_>>();

    let added_windows = actual_windows
        .iter()
        .enumerate()
        .filter(|(index, _)| !matched_actual.contains(index))
        .map(|(_, window)| diagnostic_window_inventory_json(window))
        .collect::<Vec<_>>();
    let removed_windows = expected_windows
        .iter()
        .enumerate()
        .filter(|(index, _)| !matched_expected.contains(index))
        .map(|(_, window)| diagnostic_window_inventory_json(window))
        .collect::<Vec<_>>();
    let matched_windows_json = matched_windows
        .iter()
        .map(|pair| {
            let expected_window = &expected_windows[pair.expected_idx];
            let actual_window = &actual_windows[pair.actual_idx];
            json!({
                "expected_window_id": expected_window.window.window_id,
                "actual_window_id": actual_window.window.window_id,
                "match_score": pair.score,
                "expected_title": expected_window.window.title,
                "actual_title": actual_window.window.title,
                "process_name": actual_window
                    .summary
                    .process_name
                    .clone()
                    .or_else(|| expected_window.summary.process_name.clone()),
                "onec_window_profile": actual_window
                    .summary
                    .onec_window_profile
                    .clone()
                    .or_else(|| expected_window.summary.onec_window_profile.clone()),
                "root_class_name": actual_window
                    .summary
                    .root_class_name
                    .clone()
                    .or_else(|| expected_window.summary.root_class_name.clone()),
            })
        })
        .collect::<Vec<_>>();
    let changed_windows = matched_windows
        .iter()
        .filter_map(|pair| {
            let expected_window = &expected_windows[pair.expected_idx];
            let actual_window = &actual_windows[pair.actual_idx];
            let changed_fields = expected_window
                .summary
                .changed_fields(&actual_window.summary);

            (!changed_fields.is_empty()).then(|| {
                json!({
                    "expected_window_id": expected_window.window.window_id,
                    "actual_window_id": actual_window.window.window_id,
                    "match_score": pair.score,
                    "changed_fields": changed_fields,
                    "expected": expected_window.summary.json_view(),
                    "actual": actual_window.summary.json_view(),
                })
            })
        })
        .collect::<Vec<_>>();
    let matches = expected.mode == actual.mode
        && added_windows.is_empty()
        && removed_windows.is_empty()
        && changed_windows.is_empty();

    json!({
        "status": "compared",
        "baseline_artifact_id": baseline_artifact_id,
        "matches": matches,
        "expected_mode": expected.mode,
        "actual_mode": actual.mode,
        "expected_window_count": expected.windows.len(),
        "actual_window_count": actual.windows.len(),
        "matched_windows": matched_windows_json,
        "added_windows": added_windows,
        "removed_windows": removed_windows,
        "changed_windows": changed_windows,
    })
}

struct ComparedWindow<'a> {
    window: &'a domain::WindowState,
    summary: DiagnosticWindowSummary,
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct DiagnosticWindowSummary {
    title: String,
    process_name: Option<String>,
    backend: domain::BackendKind,
    confidence_bits: u32,
    width: i32,
    height: i32,
    onec_window_profile: Option<String>,
    onec_fallback_reason: Option<String>,
    root_control_type: String,
    root_class_name: Option<String>,
    root_name: Option<String>,
    root_automation_id: Option<String>,
    tree_signature: String,
    tree_digest: String,
    node_count: usize,
    fallback_surface_count: usize,
}

impl DiagnosticWindowSummary {
    fn changed_fields(&self, other: &Self) -> Vec<&'static str> {
        let mut fields = Vec::new();

        if self.title != other.title {
            fields.push("title");
        }
        if self.process_name != other.process_name {
            fields.push("process_name");
        }
        if self.backend != other.backend {
            fields.push("backend");
        }
        if self.confidence_bits != other.confidence_bits {
            fields.push("confidence");
        }
        if self.width != other.width || self.height != other.height {
            fields.push("window_size");
        }
        if self.onec_window_profile != other.onec_window_profile {
            fields.push("onec_window_profile");
        }
        if self.onec_fallback_reason != other.onec_fallback_reason {
            fields.push("onec_fallback_reason");
        }
        if self.root_control_type != other.root_control_type {
            fields.push("root_control_type");
        }
        if self.root_class_name != other.root_class_name {
            fields.push("root_class_name");
        }
        if self.root_name != other.root_name {
            fields.push("root_name");
        }
        if self.root_automation_id != other.root_automation_id {
            fields.push("root_automation_id");
        }
        if self.tree_signature != other.tree_signature {
            fields.push("tree_state");
        }

        fields
    }

    fn json_view(&self) -> serde_json::Value {
        json!({
            "title": self.title,
            "process_name": self.process_name,
            "backend": self.backend,
            "confidence": f32::from_bits(self.confidence_bits),
            "window_size": {
                "width": self.width,
                "height": self.height,
            },
            "onec_window_profile": self.onec_window_profile,
            "onec_fallback_reason": self.onec_fallback_reason,
            "root_control_type": self.root_control_type,
            "root_class_name": self.root_class_name,
            "root_name": self.root_name,
            "root_automation_id": self.root_automation_id,
            "tree_digest": self.tree_digest,
            "node_count": self.node_count,
            "fallback_surface_count": self.fallback_surface_count,
        })
    }
}

#[derive(Clone, Debug)]
struct MatchedWindowPair {
    expected_idx: usize,
    actual_idx: usize,
    score: usize,
}

#[derive(Clone, Debug)]
struct NodeSemanticState {
    signature: String,
    node_count: usize,
    fallback_surface_count: usize,
}

fn compared_window(window: &domain::WindowState) -> ComparedWindow<'_> {
    ComparedWindow {
        window,
        summary: summarize_window(window),
    }
}

fn summarize_window(window: &domain::WindowState) -> DiagnosticWindowSummary {
    let tree_state = summarize_node(&window.root);

    DiagnosticWindowSummary {
        title: window.title.clone(),
        process_name: window.process_name.clone(),
        backend: window.backend.clone(),
        confidence_bits: window.confidence.to_bits(),
        width: window.bounds.width,
        height: window.bounds.height,
        onec_window_profile: property_string(&window.root.properties, "onec_window_profile"),
        onec_fallback_reason: property_string(&window.root.properties, "onec_fallback_reason"),
        root_control_type: window.root.control_type.clone(),
        root_class_name: window.root.class_name.clone(),
        root_name: window.root.name.clone(),
        root_automation_id: window.root.automation_id.clone(),
        tree_digest: stable_digest(&tree_state.signature),
        tree_signature: tree_state.signature,
        node_count: tree_state.node_count,
        fallback_surface_count: tree_state.fallback_surface_count,
    }
}

fn summarize_node(node: &domain::ElementNode) -> NodeSemanticState {
    let mut children = node.children.iter().map(summarize_node).collect::<Vec<_>>();
    children.sort_by(|left, right| left.signature.cmp(&right.signature));

    let child_signatures = children
        .iter()
        .map(|child| child.signature.as_str())
        .collect::<Vec<_>>()
        .join(",");
    let signature = format!(
        "backend={};control={};class={};name={};automation={};confidence={:08x};states={};properties={};children=[{}]",
        backend_label(&node.backend),
        normalize_text(&node.control_type),
        normalize_optional_text(node.class_name.as_deref()),
        normalize_optional_text(node.name.as_deref()),
        normalize_optional_text(node.automation_id.as_deref()),
        node.confidence.to_bits(),
        node_states_signature(&node.states),
        property_map_signature(&node.properties),
        child_signatures,
    );

    NodeSemanticState {
        signature,
        node_count: 1 + children.iter().map(|child| child.node_count).sum::<usize>(),
        fallback_surface_count: usize::from(
            property_string(&node.properties, "onec_fallback_reason").is_some(),
        ) + children
            .iter()
            .map(|child| child.fallback_surface_count)
            .sum::<usize>(),
    }
}

fn match_windows(
    expected: &[ComparedWindow<'_>],
    actual: &[ComparedWindow<'_>],
) -> Vec<MatchedWindowPair> {
    let mut candidates = Vec::new();

    for (expected_idx, expected_window) in expected.iter().enumerate() {
        for (actual_idx, actual_window) in actual.iter().enumerate() {
            let score = window_match_score(&expected_window.summary, &actual_window.summary);
            if score > 0 {
                candidates.push(MatchedWindowPair {
                    expected_idx,
                    actual_idx,
                    score,
                });
            }
        }
    }

    candidates.sort_by(|left, right| {
        right
            .score
            .cmp(&left.score)
            .then_with(|| left.expected_idx.cmp(&right.expected_idx))
            .then_with(|| left.actual_idx.cmp(&right.actual_idx))
    });

    let mut matched_expected = BTreeSet::new();
    let mut matched_actual = BTreeSet::new();
    let mut matched = Vec::new();

    for candidate in candidates {
        if candidate.score < 7
            || matched_expected.contains(&candidate.expected_idx)
            || matched_actual.contains(&candidate.actual_idx)
        {
            continue;
        }

        matched_expected.insert(candidate.expected_idx);
        matched_actual.insert(candidate.actual_idx);
        matched.push(candidate);
    }

    matched.sort_by(|left, right| left.expected_idx.cmp(&right.expected_idx));
    matched
}

fn window_match_score(
    expected: &DiagnosticWindowSummary,
    actual: &DiagnosticWindowSummary,
) -> usize {
    let same_process = normalized_optional_eq(
        expected.process_name.as_deref(),
        actual.process_name.as_deref(),
    );
    let same_profile = normalized_optional_eq(
        expected.onec_window_profile.as_deref(),
        actual.onec_window_profile.as_deref(),
    );
    let same_root_class = normalized_optional_eq(
        expected.root_class_name.as_deref(),
        actual.root_class_name.as_deref(),
    );

    if !same_process && !same_profile && !same_root_class {
        return 0;
    }

    let mut score = 0;
    if same_process {
        score += 4;
    }
    if same_profile {
        score += 5;
    }
    if same_root_class {
        score += 4;
    }
    if normalize_text(&expected.root_control_type) == normalize_text(&actual.root_control_type) {
        score += 2;
    }
    if normalized_optional_eq(expected.root_name.as_deref(), actual.root_name.as_deref()) {
        score += 2;
    }
    if normalize_text(&expected.title) == normalize_text(&actual.title) {
        score += 1;
    }
    if expected.width == actual.width && expected.height == actual.height {
        score += 1;
    }
    if expected.backend == actual.backend {
        score += 1;
    }
    if expected.tree_digest == actual.tree_digest {
        score += 1;
    }

    score
}

fn diagnostic_window_inventory_json(window: &ComparedWindow<'_>) -> serde_json::Value {
    json!({
        "window_id": window.window.window_id,
        "title": window.window.title,
        "process_name": window.window.process_name,
        "onec_window_profile": window.summary.onec_window_profile,
        "root_control_type": window.summary.root_control_type,
        "root_class_name": window.summary.root_class_name,
        "tree_digest": window.summary.tree_digest,
    })
}

fn backend_label(backend: &domain::BackendKind) -> &'static str {
    match backend {
        domain::BackendKind::Uia => "uia",
        domain::BackendKind::Msaa => "msaa",
        domain::BackendKind::Ocr => "ocr",
        domain::BackendKind::Mixed => "mixed",
    }
}

fn node_states_signature(states: &domain::ElementStates) -> String {
    format!(
        "enabled={};visible={};focused={};selected={};expanded={};toggled={}",
        states.enabled,
        states.visible,
        states.focused,
        states.selected,
        states.expanded,
        states.toggled,
    )
}

fn property_map_signature(properties: &BTreeMap<String, domain::PropertyValue>) -> String {
    properties
        .iter()
        .map(|(key, value)| {
            format!(
                "{}={}",
                normalize_text(key),
                property_value_signature(value)
            )
        })
        .collect::<Vec<_>>()
        .join("|")
}

fn property_value_signature(value: &domain::PropertyValue) -> String {
    match value {
        domain::PropertyValue::Bool(value) => format!("bool:{value}"),
        domain::PropertyValue::I64(value) => format!("i64:{value}"),
        domain::PropertyValue::F64(value) => format!("f64:{:016x}", value.to_bits()),
        domain::PropertyValue::String(value) => format!("string:{}", normalize_text(value)),
        domain::PropertyValue::StringList(values) => format!(
            "strings:{}",
            values
                .iter()
                .map(|value| normalize_text(value))
                .collect::<Vec<_>>()
                .join(",")
        ),
        domain::PropertyValue::Rect(rect) => format!(
            "rect:{}:{}:{}:{}",
            rect.left, rect.top, rect.width, rect.height
        ),
        domain::PropertyValue::Null => "null".to_owned(),
    }
}

fn normalized_optional_eq(left: Option<&str>, right: Option<&str>) -> bool {
    match (left, right) {
        (Some(left), Some(right)) => {
            let left = normalize_text(left);
            let right = normalize_text(right);
            !left.is_empty() && left == right
        }
        _ => false,
    }
}

fn normalize_optional_text(value: Option<&str>) -> String {
    value.map(normalize_text).unwrap_or_default()
}

fn normalize_text(value: &str) -> String {
    value
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .to_lowercase()
}

fn stable_digest(value: &str) -> String {
    let mut hash = 0xcbf29ce484222325u64;
    for byte in value.as_bytes() {
        hash ^= u64::from(*byte);
        hash = hash.wrapping_mul(0x100000001b3);
    }
    format!("{hash:016x}")
}

fn property_string(
    properties: &BTreeMap<String, domain::PropertyValue>,
    key: &str,
) -> Option<String> {
    match properties.get(key) {
        Some(domain::PropertyValue::String(value)) => Some(value.clone()),
        _ => None,
    }
}

fn build_diagnostic_capture_target(
    snapshot: &domain::UiSnapshot,
    target: &domain::ActionTarget,
) -> Option<domain::ActionTarget> {
    match target {
        domain::ActionTarget::Desktop => None,
        domain::ActionTarget::Window(locator) => {
            let window = resolve_window(snapshot, locator)?;
            Some(domain::ActionTarget::Region(domain::RegionTarget {
                window_id: Some(window.window_id.clone()),
                bounds: domain::Rect {
                    left: 0,
                    top: 0,
                    width: window.bounds.width,
                    height: window.bounds.height,
                },
            }))
        }
        domain::ActionTarget::Element(locator) => {
            let resolved = resolve_element(snapshot, locator)?;
            Some(domain::ActionTarget::Region(domain::RegionTarget {
                window_id: Some(resolved.window.window_id.clone()),
                bounds: domain::Rect {
                    left: resolved.node.bounds.left - resolved.window.bounds.left,
                    top: resolved.node.bounds.top - resolved.window.bounds.top,
                    width: resolved.node.bounds.width,
                    height: resolved.node.bounds.height,
                },
            }))
        }
        domain::ActionTarget::Region(region) => Some(domain::ActionTarget::Region(region.clone())),
    }
}

fn collect_fallback_surfaces(
    snapshot: &domain::UiSnapshot,
    target: &domain::ActionTarget,
) -> Vec<serde_json::Value> {
    let mut surfaces = Vec::new();

    match target {
        domain::ActionTarget::Desktop => {
            for window in &snapshot.windows {
                collect_fallback_surfaces_from_node(&mut surfaces, &window.window_id, &window.root);
            }
        }
        domain::ActionTarget::Window(locator) => {
            if let Some(window) = resolve_window(snapshot, locator) {
                collect_fallback_surfaces_from_node(&mut surfaces, &window.window_id, &window.root);
            }
        }
        domain::ActionTarget::Element(locator) => {
            if let Some(resolved) = resolve_element(snapshot, locator) {
                collect_fallback_surfaces_from_node(
                    &mut surfaces,
                    &resolved.window.window_id,
                    resolved.node,
                );
            }
        }
        domain::ActionTarget::Region(region) => {
            if let Some(window_id) = &region.window_id {
                if let Some(window) = snapshot
                    .windows
                    .iter()
                    .find(|window| &window.window_id == window_id)
                {
                    collect_fallback_surfaces_from_node(
                        &mut surfaces,
                        &window.window_id,
                        &window.root,
                    );
                }
            }
        }
    }

    surfaces
}

fn collect_fallback_surfaces_from_node(
    surfaces: &mut Vec<serde_json::Value>,
    window_id: &domain::WindowId,
    node: &domain::ElementNode,
) {
    let fallback_reason = property_string(&node.properties, "onec_fallback_reason");
    let profile = property_string(&node.properties, "onec_profile");
    if fallback_reason.is_some()
        || node.backend != domain::BackendKind::Uia
        || node.confidence < 0.6
    {
        surfaces.push(json!({
            "window_id": window_id,
            "element_id": node.element_id,
            "backend": node.backend,
            "confidence": node.confidence,
            "onec_profile": profile,
            "onec_fallback_reason": fallback_reason,
        }));
    }

    for child in &node.children {
        collect_fallback_surfaces_from_node(surfaces, window_id, child);
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
            | domain::ActionKind::CollectDiagnosticBundle(_)
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
        domain::ActionKind::CollectDiagnosticBundle(_) => "collect_diagnostic_bundle",
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
mod tests;

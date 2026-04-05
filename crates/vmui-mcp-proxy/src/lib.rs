use std::{collections::BTreeMap, sync::Arc, time::Duration};

use anyhow::{Context, Result};
use rmcp::{
    handler::server::{router::tool::ToolRouter, wrapper::Parameters},
    model::{Content, Implementation, ProtocolVersion, ServerCapabilities, ServerInfo},
    tool, tool_handler, tool_router,
    transport::stdio,
    ServerHandler, ServiceExt,
};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use tokio::{
    sync::{mpsc, oneshot, Mutex, RwLock},
    task::JoinHandle,
    time::timeout,
};
use tokio_stream::wrappers::ReceiverStream;
use tonic::transport::Endpoint;
use tracing::info;
use vmui_core::UiStateStore;
use vmui_protocol as domain;
use vmui_transport_grpc::{encode_action_request, pb};

#[derive(Clone, Debug)]
pub struct ProxyConfig {
    pub daemon_addr: String,
}

impl ProxyConfig {
    pub fn from_env() -> Self {
        let daemon_addr = std::env::var("VMUI_DAEMON_ADDR")
            .ok()
            .filter(|value| !value.trim().is_empty())
            .unwrap_or_else(|| "127.0.0.1:50051".to_owned());
        Self { daemon_addr }
    }

    pub fn normalized_daemon_addr(&self) -> String {
        normalize_daemon_addr(&self.daemon_addr)
    }
}

#[derive(Clone)]
pub struct VmuiMcpProxy {
    config: ProxyConfig,
    sessions: Arc<RwLock<BTreeMap<String, Arc<DaemonSessionWorker>>>>,
    tool_router: ToolRouter<Self>,
}

#[tool_handler(router = self.tool_router)]
impl ServerHandler for VmuiMcpProxy {
    fn get_info(&self) -> ServerInfo {
        let mut info = ServerInfo::default();
        info.protocol_version = ProtocolVersion::V_2024_11_05;
        info.capabilities = ServerCapabilities::builder().enable_tools().build();
        info.server_info = Implementation::from_build_env();
        info.instructions =
            Some("vmui-mcp-proxy: stdio MCP adapter over vmui-agent daemon sessions".to_owned());
        info
    }
}

impl Default for VmuiMcpProxy {
    fn default() -> Self {
        Self::new(ProxyConfig::from_env())
    }
}

impl VmuiMcpProxy {
    pub fn new(config: ProxyConfig) -> Self {
        Self {
            config,
            sessions: Arc::new(RwLock::new(BTreeMap::new())),
            tool_router: Self::tool_router(),
        }
    }

    async fn open_session_internal(
        &self,
        profile: domain::SessionProfile,
    ) -> Result<SessionOpenResult, rmcp::ErrorData> {
        let logical_session_id = domain::SessionId::new("mcp").to_string();
        let worker = Arc::new(
            DaemonSessionWorker::open(
                logical_session_id.clone(),
                self.config.normalized_daemon_addr(),
                profile.clone(),
            )
            .await?,
        );
        let status = worker.status().await;

        self.sessions
            .write()
            .await
            .insert(logical_session_id.clone(), Arc::clone(&worker));

        Ok(SessionOpenResult {
            session_id: logical_session_id,
            profile: status.profile.clone(),
            daemon_session_id: status.daemon_session_id,
            backend_id: status.backend_id,
            capabilities: status.capabilities,
            connected: status.connected,
            continuity_invalidated: status.continuity_invalidated,
            continuity_reason: status.continuity_reason,
            snapshot_revision: status.snapshot_revision,
            window_count: status.window_count,
        })
    }

    async fn resolve_session(
        &self,
        session_id: Option<&str>,
    ) -> Result<(String, Arc<DaemonSessionWorker>), rmcp::ErrorData> {
        let sessions = self.sessions.read().await;
        if let Some(session_id) = session_id {
            let worker = sessions.get(session_id).ok_or_else(|| {
                rmcp::ErrorData::resource_not_found(
                    format!("unknown session_id `{session_id}`"),
                    None,
                )
            })?;
            return Ok((session_id.to_owned(), Arc::clone(worker)));
        }

        match sessions.len() {
            0 => Err(rmcp::ErrorData::invalid_params(
                "no active MCP sessions; call session_open first",
                None,
            )),
            1 => {
                let (session_id, worker) = sessions.iter().next().expect("single session");
                Ok((session_id.clone(), Arc::clone(worker)))
            }
            _ => Err(rmcp::ErrorData::invalid_params(
                "session_id is required because multiple logical sessions are active",
                None,
            )),
        }
    }

    async fn close_session_internal(
        &self,
        session_id: Option<&str>,
    ) -> Result<SessionCloseResult, rmcp::ErrorData> {
        let (resolved_session_id, worker) = self.resolve_session(session_id).await?;
        worker.close().await;
        self.sessions.write().await.remove(&resolved_session_id);

        Ok(SessionCloseResult {
            ok: true,
            session_id: resolved_session_id,
        })
    }

    async fn session_status_internal(
        &self,
        session_id: Option<&str>,
    ) -> Result<WorkerStatus, rmcp::ErrorData> {
        let (_, worker) = self.resolve_session(session_id).await?;
        Ok(worker.status().await)
    }
}

#[tool_router]
impl VmuiMcpProxy {
    #[tool(
        description = "Open a logical MCP session backed by one reusable daemon stream.",
        annotations(
            read_only_hint = false,
            destructive_hint = false,
            idempotent_hint = false
        )
    )]
    async fn session_open(
        &self,
        Parameters(params): Parameters<SessionOpenParams>,
    ) -> Result<Content, rmcp::ErrorData> {
        Content::json(self.open_session_internal(params.profile.into()).await?)
    }

    #[tool(
        description = "Inspect logical session health, continuity, and cached snapshot state.",
        annotations(read_only_hint = true, destructive_hint = false)
    )]
    async fn session_status(
        &self,
        Parameters(params): Parameters<SessionRefParams>,
    ) -> Result<Content, rmcp::ErrorData> {
        Content::json(
            self.session_status_internal(params.session_id.as_deref())
                .await?,
        )
    }

    #[tool(
        description = "Close a logical MCP session and release the owned daemon stream.",
        annotations(
            read_only_hint = false,
            destructive_hint = false,
            idempotent_hint = true
        )
    )]
    async fn session_close(
        &self,
        Parameters(params): Parameters<SessionRefParams>,
    ) -> Result<Content, rmcp::ErrorData> {
        Content::json(
            self.close_session_internal(params.session_id.as_deref())
                .await?,
        )
    }

    #[tool(
        description = "Return the current window inventory via daemon session state.",
        annotations(read_only_hint = true, destructive_hint = false)
    )]
    async fn list_windows(
        &self,
        Parameters(params): Parameters<ListWindowsParams>,
    ) -> Result<Content, rmcp::ErrorData> {
        let (session_id, worker) = self.resolve_session(params.session_id.as_deref()).await?;
        let outcome = worker
            .execute_json_read_action(
                domain::ActionRequest {
                    action_id: domain::ActionId::new("mcp-list-windows"),
                    timeout_ms: params.timeout_ms.unwrap_or(2_000),
                    target: domain::ActionTarget::Desktop,
                    kind: domain::ActionKind::ListWindows,
                    capture_policy: domain::CapturePolicy::Never,
                },
                true,
            )
            .await?;
        Content::json(ReadEnvelope::from_outcome(session_id, outcome))
    }

    #[tool(
        description = "Read a desktop, window, or element tree through the daemon contract.",
        annotations(read_only_hint = true, destructive_hint = false)
    )]
    async fn get_tree(
        &self,
        Parameters(params): Parameters<GetTreeParams>,
    ) -> Result<Content, rmcp::ErrorData> {
        let (session_id, worker) = self.resolve_session(params.session_id.as_deref()).await?;
        let outcome = worker
            .execute_json_read_action(
                domain::ActionRequest {
                    action_id: domain::ActionId::new("mcp-get-tree"),
                    timeout_ms: params.timeout_ms.unwrap_or(2_000),
                    target: params.target.try_into()?,
                    kind: domain::ActionKind::GetTree(domain::TreeRequest {
                        raw: params.raw,
                        max_depth: params.max_depth,
                    }),
                    capture_policy: domain::CapturePolicy::Never,
                },
                true,
            )
            .await?;
        Content::json(ReadEnvelope::from_outcome(session_id, outcome))
    }

    #[tool(
        description = "Read structured daemon runtime health, recovery, and artifact retention status.",
        annotations(read_only_hint = true, destructive_hint = false)
    )]
    async fn runtime_status(
        &self,
        Parameters(params): Parameters<RuntimeStatusParams>,
    ) -> Result<Content, rmcp::ErrorData> {
        let (session_id, worker) = self.resolve_session(params.session_id.as_deref()).await?;
        let outcome = worker
            .execute_json_read_action(
                domain::ActionRequest {
                    action_id: domain::ActionId::new("mcp-runtime-status"),
                    timeout_ms: params.timeout_ms.unwrap_or(2_000),
                    target: domain::ActionTarget::Desktop,
                    kind: domain::ActionKind::GetRuntimeStatus(
                        domain::RuntimeStatusRequest::default(),
                    ),
                    capture_policy: domain::CapturePolicy::Never,
                },
                true,
            )
            .await?;
        Content::json(ReadEnvelope::from_outcome(session_id, outcome))
    }

    #[tool(
        description = "Wait server-side for a state condition without screenshot polling.",
        annotations(read_only_hint = true, destructive_hint = false)
    )]
    async fn wait_for(
        &self,
        Parameters(params): Parameters<WaitForParams>,
    ) -> Result<Content, rmcp::ErrorData> {
        let (session_id, worker) = self.resolve_session(params.session_id.as_deref()).await?;
        let outcome = worker
            .execute_action(
                domain::ActionRequest {
                    action_id: domain::ActionId::new("mcp-wait-for"),
                    timeout_ms: params.timeout_ms.unwrap_or(5_000),
                    target: params.target.try_into()?,
                    kind: domain::ActionKind::WaitFor(domain::WaitForOptions {
                        condition: params.condition.into(),
                        stable_for_ms: params.stable_for_ms.unwrap_or(0),
                    }),
                    capture_policy: params
                        .capture_policy
                        .unwrap_or(CapturePolicyParam::Never)
                        .into(),
                },
                true,
            )
            .await?;
        Content::json(ActionEnvelope::from_outcome(session_id, outcome))
    }

    #[tool(
        description = "Focus a window through the daemon action contract.",
        annotations(
            read_only_hint = false,
            destructive_hint = true,
            idempotent_hint = false
        )
    )]
    async fn focus_window(
        &self,
        Parameters(params): Parameters<FocusWindowParams>,
    ) -> Result<Content, rmcp::ErrorData> {
        let (session_id, worker) = self.resolve_session(params.session_id.as_deref()).await?;
        let outcome = worker
            .execute_action(
                domain::ActionRequest {
                    action_id: domain::ActionId::new("mcp-focus-window"),
                    timeout_ms: params.timeout_ms.unwrap_or(2_000),
                    target: params.target.try_into()?,
                    kind: domain::ActionKind::FocusWindow,
                    capture_policy: params
                        .capture_policy
                        .unwrap_or(CapturePolicyParam::OnFailure)
                        .into(),
                },
                false,
            )
            .await?;
        Content::json(ActionEnvelope::from_outcome(session_id, outcome))
    }

    #[tool(
        description = "Invoke an element using daemon-side semantic actions.",
        annotations(
            read_only_hint = false,
            destructive_hint = true,
            idempotent_hint = false
        )
    )]
    async fn invoke(
        &self,
        Parameters(params): Parameters<InvokeParams>,
    ) -> Result<Content, rmcp::ErrorData> {
        let (session_id, worker) = self.resolve_session(params.session_id.as_deref()).await?;
        let outcome = worker
            .execute_action(
                domain::ActionRequest {
                    action_id: domain::ActionId::new("mcp-invoke"),
                    timeout_ms: params.timeout_ms.unwrap_or(2_000),
                    target: params.target.try_into()?,
                    kind: domain::ActionKind::Invoke,
                    capture_policy: params
                        .capture_policy
                        .unwrap_or(CapturePolicyParam::OnFailure)
                        .into(),
                },
                false,
            )
            .await?;
        Content::json(ActionEnvelope::from_outcome(session_id, outcome))
    }

    #[tool(
        description = "Click an element using daemon-side targeting and action execution.",
        annotations(
            read_only_hint = false,
            destructive_hint = true,
            idempotent_hint = false
        )
    )]
    async fn click_element(
        &self,
        Parameters(params): Parameters<ClickElementParams>,
    ) -> Result<Content, rmcp::ErrorData> {
        let (session_id, worker) = self.resolve_session(params.session_id.as_deref()).await?;
        let outcome = worker
            .execute_action(
                domain::ActionRequest {
                    action_id: domain::ActionId::new("mcp-click-element"),
                    timeout_ms: params.timeout_ms.unwrap_or(2_000),
                    target: params.target.try_into()?,
                    kind: domain::ActionKind::ClickElement(domain::ClickOptions {
                        button: params.button.into(),
                        clicks: params.clicks.unwrap_or(1),
                    }),
                    capture_policy: params
                        .capture_policy
                        .unwrap_or(CapturePolicyParam::OnFailure)
                        .into(),
                },
                false,
            )
            .await?;
        Content::json(ActionEnvelope::from_outcome(session_id, outcome))
    }

    #[tool(
        description = "Set an element value using daemon-side semantic action execution.",
        annotations(
            read_only_hint = false,
            destructive_hint = true,
            idempotent_hint = false
        )
    )]
    async fn set_value(
        &self,
        Parameters(params): Parameters<SetValueParams>,
    ) -> Result<Content, rmcp::ErrorData> {
        let (session_id, worker) = self.resolve_session(params.session_id.as_deref()).await?;
        let outcome = worker
            .execute_action(
                domain::ActionRequest {
                    action_id: domain::ActionId::new("mcp-set-value"),
                    timeout_ms: params.timeout_ms.unwrap_or(2_000),
                    target: params.target.try_into()?,
                    kind: domain::ActionKind::SetValue(domain::SetValueOptions {
                        value: params.value,
                        clear_first: params.clear_first.unwrap_or(false),
                    }),
                    capture_policy: params
                        .capture_policy
                        .unwrap_or(CapturePolicyParam::OnFailure)
                        .into(),
                },
                false,
            )
            .await?;
        Content::json(ActionEnvelope::from_outcome(session_id, outcome))
    }

    #[tool(
        description = "Send keys through the daemon session without silent retry after reconnect.",
        annotations(
            read_only_hint = false,
            destructive_hint = true,
            idempotent_hint = false
        )
    )]
    async fn send_keys(
        &self,
        Parameters(params): Parameters<SendKeysParams>,
    ) -> Result<Content, rmcp::ErrorData> {
        let (session_id, worker) = self.resolve_session(params.session_id.as_deref()).await?;
        let outcome = worker
            .execute_action(
                domain::ActionRequest {
                    action_id: domain::ActionId::new("mcp-send-keys"),
                    timeout_ms: params.timeout_ms.unwrap_or(2_000),
                    target: params.target.try_into()?,
                    kind: domain::ActionKind::SendKeys(domain::SendKeysOptions {
                        keys: params.keys,
                    }),
                    capture_policy: params
                        .capture_policy
                        .unwrap_or(CapturePolicyParam::OnFailure)
                        .into(),
                },
                false,
            )
            .await?;
        Content::json(ActionEnvelope::from_outcome(session_id, outcome))
    }

    #[tool(
        description = "Collect a daemon-side diagnostic bundle while preserving the external test verdict.",
        annotations(read_only_hint = true, destructive_hint = false)
    )]
    async fn collect_diagnostic_bundle(
        &self,
        Parameters(params): Parameters<CollectDiagnosticBundleParams>,
    ) -> Result<Content, rmcp::ErrorData> {
        let (session_id, worker) = self.resolve_session(params.session_id.as_deref()).await?;
        let outcome = worker
            .execute_action(
                domain::ActionRequest {
                    action_id: domain::ActionId::new("mcp-diagnostic-bundle"),
                    timeout_ms: params.timeout_ms.unwrap_or(5_000),
                    target: params.target.try_into()?,
                    kind: domain::ActionKind::CollectDiagnosticBundle(
                        domain::DiagnosticBundleOptions {
                            step_id: params.step_id,
                            step_label: params.step_label,
                            test_verdict: params.test_verdict.into(),
                            note: params.note,
                            baseline_artifact_id: params
                                .baseline_artifact_id
                                .map(domain::ArtifactId::from),
                            max_tree_depth: params.max_tree_depth,
                        },
                    ),
                    capture_policy: domain::CapturePolicy::Never,
                },
                true,
            )
            .await?;
        Content::json(ActionEnvelope::from_outcome(session_id, outcome))
    }

    #[tool(
        description = "Read a daemon artifact and decode it as JSON.",
        annotations(read_only_hint = true, destructive_hint = false)
    )]
    async fn artifact_read_json(
        &self,
        Parameters(params): Parameters<ArtifactReadParams>,
    ) -> Result<Content, rmcp::ErrorData> {
        let (_, worker) = self.resolve_session(params.session_id.as_deref()).await?;
        let artifact_id = domain::ArtifactId::from(params.artifact_id);
        let payload = worker.read_json_artifact(&artifact_id).await?;
        Content::json(payload)
    }

    #[tool(
        description = "Read a daemon artifact and decode it as UTF-8 text.",
        annotations(read_only_hint = true, destructive_hint = false)
    )]
    async fn artifact_read_text(
        &self,
        Parameters(params): Parameters<ArtifactReadParams>,
    ) -> Result<Content, rmcp::ErrorData> {
        let (_, worker) = self.resolve_session(params.session_id.as_deref()).await?;
        let artifact_id = domain::ArtifactId::from(params.artifact_id);
        let text = worker.read_text_artifact(&artifact_id).await?;
        Content::json(serde_json::json!({ "artifact_id": artifact_id, "text": text }))
    }
}

#[derive(Clone)]
struct DaemonSessionWorker {
    logical_session_id: String,
    daemon_addr: String,
    profile: domain::SessionProfile,
    state: Arc<RwLock<WorkerState>>,
    action_lock: Arc<Mutex<()>>,
}

struct WorkerState {
    connected: bool,
    daemon_session_id: Option<String>,
    backend_id: String,
    capabilities: Vec<String>,
    outbound_tx: Option<mpsc::Sender<pb::ClientMsg>>,
    pending_action: Option<oneshot::Sender<Result<domain::ActionResult, String>>>,
    reader_task: Option<JoinHandle<()>>,
    snapshot: UiStateStore,
    reconnect_count: u64,
    disconnect_reason: Option<String>,
    continuity_invalidated: bool,
    continuity_reason: Option<String>,
    warning_counts: BTreeMap<String, u64>,
    last_warning: Option<domain::WarningEvent>,
}

impl Default for WorkerState {
    fn default() -> Self {
        Self {
            connected: false,
            daemon_session_id: None,
            backend_id: String::new(),
            capabilities: Vec::new(),
            outbound_tx: None,
            pending_action: None,
            reader_task: None,
            snapshot: UiStateStore::default(),
            reconnect_count: 0,
            disconnect_reason: None,
            continuity_invalidated: false,
            continuity_reason: None,
            warning_counts: BTreeMap::new(),
            last_warning: None,
        }
    }
}

impl DaemonSessionWorker {
    async fn open(
        logical_session_id: String,
        daemon_addr: String,
        profile: domain::SessionProfile,
    ) -> Result<Self, rmcp::ErrorData> {
        let worker = Self {
            logical_session_id,
            daemon_addr,
            profile,
            state: Arc::new(RwLock::new(WorkerState::default())),
            action_lock: Arc::new(Mutex::new(())),
        };
        worker.connect(false, None).await?;
        Ok(worker)
    }

    async fn close(&self) {
        let mut state = self.state.write().await;
        if let Some(task) = state.reader_task.take() {
            task.abort();
        }
        state.connected = false;
        state.outbound_tx = None;
        state.pending_action = None;
        state.disconnect_reason = Some("session closed".to_owned());
    }

    async fn status(&self) -> WorkerStatus {
        let state = self.state.read().await;
        let snapshot = state.snapshot.snapshot().cloned();
        WorkerStatus {
            session_id: self.logical_session_id.clone(),
            profile: self.profile.clone().into(),
            daemon_session_id: state.daemon_session_id.clone(),
            backend_id: state.backend_id.clone(),
            capabilities: state.capabilities.clone(),
            connected: state.connected,
            snapshot_revision: snapshot.as_ref().map(|snapshot| snapshot.rev),
            window_count: snapshot.as_ref().map(|snapshot| snapshot.windows.len()),
            resync_required: state.snapshot.resync_reason().is_some(),
            resync_reason: state.snapshot.resync_reason().map(ToOwned::to_owned),
            reconnect_count: state.reconnect_count,
            disconnect_reason: state.disconnect_reason.clone(),
            continuity_invalidated: state.continuity_invalidated,
            continuity_reason: state.continuity_reason.clone(),
            warning_counts: state.warning_counts.clone(),
            last_warning: state.last_warning.clone(),
        }
    }

    async fn execute_json_read_action(
        &self,
        request: domain::ActionRequest,
        read_only: bool,
    ) -> Result<ReadActionOutcome, rmcp::ErrorData> {
        let outcome = self.execute_action(request, read_only).await?;
        let artifact = outcome
            .action_result
            .artifacts
            .first()
            .ok_or_else(|| rmcp::ErrorData::internal_error("expected action artifact", None))?;
        let payload = self.read_json_artifact(&artifact.artifact_id).await?;

        Ok(ReadActionOutcome {
            session: outcome.session,
            payload,
        })
    }

    async fn execute_action(
        &self,
        request: domain::ActionRequest,
        read_only: bool,
    ) -> Result<ActionOutcome, rmcp::ErrorData> {
        let _guard = self.action_lock.lock().await;
        let mut attempt = 0usize;
        let mut reconnected = false;

        loop {
            if !self.is_connected().await {
                if !read_only {
                    return Err(rmcp::ErrorData::internal_error(
                        "daemon session continuity was lost; retry the mutating action explicitly",
                        None,
                    ));
                }
                self.connect(true, Some("daemon session stream lost".to_owned()))
                    .await?;
                reconnected = true;
            }

            let result = self.execute_action_once(request.clone()).await;
            match result {
                Ok(action_result) => {
                    let session = self.session_outcome(reconnected).await;
                    return Ok(ActionOutcome {
                        session,
                        action_result,
                    });
                }
                Err(error) if read_only && attempt == 0 => {
                    self.connect(true, Some(error)).await?;
                    attempt += 1;
                    reconnected = true;
                }
                Err(error) => {
                    return Err(rmcp::ErrorData::internal_error(
                        format!(
                            "daemon action interrupted; retry is required because continuity was lost: {error}"
                        ),
                        None,
                    ));
                }
            }
        }
    }

    async fn read_json_artifact(
        &self,
        artifact_id: &domain::ArtifactId,
    ) -> Result<serde_json::Value, rmcp::ErrorData> {
        let bytes = self.read_artifact_bytes(artifact_id).await?;
        serde_json::from_slice(&bytes).map_err(|error| {
            rmcp::ErrorData::internal_error(
                format!("failed to decode artifact `{artifact_id}` as JSON: {error}"),
                None,
            )
        })
    }

    async fn read_text_artifact(
        &self,
        artifact_id: &domain::ArtifactId,
    ) -> Result<String, rmcp::ErrorData> {
        let bytes = self.read_artifact_bytes(artifact_id).await?;
        String::from_utf8(bytes).map_err(|error| {
            rmcp::ErrorData::internal_error(
                format!("failed to decode artifact `{artifact_id}` as UTF-8 text: {error}"),
                None,
            )
        })
    }

    async fn read_artifact_bytes(
        &self,
        artifact_id: &domain::ArtifactId,
    ) -> Result<Vec<u8>, rmcp::ErrorData> {
        let endpoint = Endpoint::from_shared(self.daemon_addr.clone()).map_err(|error| {
            rmcp::ErrorData::internal_error(
                format!("invalid daemon endpoint `{}`: {error}", self.daemon_addr),
                None,
            )
        })?;
        let channel = endpoint.connect().await.map_err(|error| {
            rmcp::ErrorData::internal_error(
                format!(
                    "failed to connect to daemon `{}`: {error}",
                    self.daemon_addr
                ),
                None,
            )
        })?;
        let mut client = pb::ui_agent_client::UiAgentClient::new(channel);
        let response = client
            .read_artifact(pb::ReadArtifactRequest {
                artifact_id: artifact_id.to_string(),
            })
            .await
            .map_err(|error| {
                rmcp::ErrorData::internal_error(
                    format!("failed to read artifact `{artifact_id}`: {error}"),
                    None,
                )
            })?;

        let mut stream = response.into_inner();
        let mut bytes = Vec::new();
        while let Some(chunk) = stream.message().await.map_err(|error| {
            rmcp::ErrorData::internal_error(
                format!("failed to stream artifact `{artifact_id}`: {error}"),
                None,
            )
        })? {
            bytes.extend_from_slice(&chunk.data);
        }

        Ok(bytes)
    }

    async fn connect(
        &self,
        is_reconnect: bool,
        reconnect_reason: Option<String>,
    ) -> Result<(), rmcp::ErrorData> {
        let endpoint = Endpoint::from_shared(self.daemon_addr.clone()).map_err(|error| {
            rmcp::ErrorData::internal_error(
                format!("invalid daemon endpoint `{}`: {error}", self.daemon_addr),
                None,
            )
        })?;
        let channel = endpoint.connect().await.map_err(|error| {
            rmcp::ErrorData::internal_error(
                format!(
                    "failed to connect to daemon `{}`: {error}",
                    self.daemon_addr
                ),
                None,
            )
        })?;
        let mut client = pb::ui_agent_client::UiAgentClient::new(channel);
        let (outbound_tx, outbound_rx) = mpsc::channel(32);
        let response = client
            .session(ReceiverStream::new(outbound_rx))
            .await
            .map_err(|error| {
                rmcp::ErrorData::internal_error(
                    format!("failed to open daemon session: {error}"),
                    None,
                )
            })?;
        let mut inbound = response.into_inner();

        send_client_message(
            &outbound_tx,
            pb::ClientMsg {
                payload: Some(pb::client_msg::Payload::Hello(pb::Hello {
                    client_name: "vmui-mcp-proxy".to_owned(),
                    client_version: env!("CARGO_PKG_VERSION").to_owned(),
                    requested_profile: Some(pb::SessionProfile::from(self.profile.clone())),
                })),
            },
        )
        .await?;
        send_client_message(
            &outbound_tx,
            pb::ClientMsg {
                payload: Some(pb::client_msg::Payload::Subscribe(pb::Subscribe {
                    include_initial_snapshot: true,
                    include_diff_stream: true,
                    shallow: false,
                })),
            },
        )
        .await?;

        let mut hello_ack: Option<domain::HelloAck> = None;
        let mut initial_snapshot: Option<domain::UiSnapshot> = None;
        let mut startup_warnings = Vec::new();
        while hello_ack.is_none() || initial_snapshot.is_none() {
            let message = inbound.message().await.map_err(|error| {
                rmcp::ErrorData::internal_error(
                    format!("failed to read daemon session startup message: {error}"),
                    None,
                )
            })?;
            let Some(message) = message else {
                return Err(rmcp::ErrorData::internal_error(
                    "daemon session ended before startup completed",
                    None,
                ));
            };
            match domain::ServerMessage::try_from(message).map_err(|error| {
                rmcp::ErrorData::internal_error(
                    format!("failed to decode daemon startup message: {error}"),
                    None,
                )
            })? {
                domain::ServerMessage::HelloAck(ack) => hello_ack = Some(ack),
                domain::ServerMessage::InitialSnapshot(snapshot) => {
                    initial_snapshot = Some(snapshot)
                }
                domain::ServerMessage::Warning(warning) => startup_warnings.push(warning),
                domain::ServerMessage::DiffBatch(_)
                | domain::ServerMessage::ActionResult(_)
                | domain::ServerMessage::ArtifactReady(_)
                | domain::ServerMessage::Pong => {}
            }
        }

        let hello_ack = hello_ack.expect("hello ack");
        let initial_snapshot = initial_snapshot.expect("initial snapshot");
        let mut worker_state = self.state.write().await;
        if let Some(task) = worker_state.reader_task.take() {
            task.abort();
        }
        worker_state.connected = true;
        worker_state.outbound_tx = Some(outbound_tx);
        worker_state.daemon_session_id = Some(hello_ack.session_id.to_string());
        worker_state.backend_id = hello_ack.backend_id;
        worker_state.capabilities = hello_ack.capabilities;
        worker_state.snapshot.replace_snapshot(initial_snapshot);
        worker_state.disconnect_reason = None;
        if is_reconnect {
            worker_state.reconnect_count += 1;
            worker_state.continuity_invalidated = true;
            worker_state.continuity_reason = reconnect_reason.or_else(|| {
                Some(
                    "daemon session was re-established and previous continuity is invalidated"
                        .to_owned(),
                )
            });
        }
        for warning in startup_warnings {
            record_worker_warning(&mut worker_state, warning);
        }

        let reader_state = Arc::clone(&self.state);
        worker_state.reader_task = Some(tokio::spawn(async move {
            reader_loop(inbound, reader_state).await;
        }));

        Ok(())
    }

    async fn is_connected(&self) -> bool {
        self.state.read().await.connected
    }

    async fn execute_action_once(
        &self,
        request: domain::ActionRequest,
    ) -> Result<domain::ActionResult, String> {
        let (outbound_tx, result_rx) = {
            let mut state = self.state.write().await;
            let outbound_tx = state.outbound_tx.clone().ok_or_else(|| {
                state
                    .disconnect_reason
                    .clone()
                    .unwrap_or_else(|| "daemon session is not connected".to_owned())
            })?;
            let (result_tx, result_rx) = oneshot::channel();
            if state.pending_action.replace(result_tx).is_some() {
                return Err("another daemon action is already pending".to_owned());
            }
            (outbound_tx, result_rx)
        };

        let encoded = encode_action_request(&request).map_err(|error| error.to_string())?;
        if outbound_tx
            .send(pb::ClientMsg {
                payload: Some(pb::client_msg::Payload::ActionRequest(encoded)),
            })
            .await
            .is_err()
        {
            self.fail_pending_action("daemon session stream closed".to_owned())
                .await;
            return Err("daemon session stream closed".to_owned());
        }

        match timeout(
            Duration::from_millis(request.timeout_ms.max(1) + 2_000),
            result_rx,
        )
        .await
        {
            Ok(Ok(Ok(result))) => Ok(result),
            Ok(Ok(Err(error))) => Err(error),
            Ok(Err(_)) => Err("daemon action response channel closed".to_owned()),
            Err(_) => Err("timed out while waiting for daemon action result".to_owned()),
        }
    }

    async fn fail_pending_action(&self, reason: String) {
        let pending = {
            let mut state = self.state.write().await;
            state.pending_action.take()
        };
        if let Some(sender) = pending {
            let _ = sender.send(Err(reason));
        }
    }

    async fn session_outcome(&self, reconnected: bool) -> SessionOutcome {
        let state = self.state.read().await;
        SessionOutcome {
            daemon_session_id: state.daemon_session_id.clone(),
            connected: state.connected,
            reconnected,
            continuity_invalidated: state.continuity_invalidated,
            continuity_reason: state.continuity_reason.clone(),
        }
    }
}

async fn reader_loop(
    mut inbound: tonic::Streaming<pb::ServerMsg>,
    state: Arc<RwLock<WorkerState>>,
) {
    let disconnect_reason = loop {
        let next = match inbound.message().await {
            Ok(next) => next,
            Err(error) => break format!("daemon stream error: {error}"),
        };
        let Some(message) = next else {
            break "daemon stream closed".to_owned();
        };
        let message = match domain::ServerMessage::try_from(message) {
            Ok(message) => message,
            Err(error) => break format!("failed to decode daemon message: {error}"),
        };

        match message {
            domain::ServerMessage::InitialSnapshot(snapshot) => {
                let mut state = state.write().await;
                state.snapshot.replace_snapshot(snapshot);
            }
            domain::ServerMessage::DiffBatch(diff) => {
                let mut state = state.write().await;
                let _ = state.snapshot.apply_diff(&diff);
            }
            domain::ServerMessage::ActionResult(result) => {
                let sender = {
                    let mut state = state.write().await;
                    state.pending_action.take()
                };
                if let Some(sender) = sender {
                    let _ = sender.send(Ok(result));
                }
            }
            domain::ServerMessage::Warning(warning) => {
                let mut state = state.write().await;
                if warning.code == "session_state_recovered" {
                    state.continuity_invalidated = true;
                    state.continuity_reason = Some(warning.message.clone());
                }
                record_worker_warning(&mut state, warning);
            }
            domain::ServerMessage::HelloAck(_)
            | domain::ServerMessage::ArtifactReady(_)
            | domain::ServerMessage::Pong => {}
        }
    };

    let pending = {
        let mut state = state.write().await;
        state.connected = false;
        state.outbound_tx = None;
        state.disconnect_reason = Some(disconnect_reason.clone());
        state.reader_task = None;
        state.pending_action.take()
    };
    if let Some(sender) = pending {
        let _ = sender.send(Err(disconnect_reason));
    }
}

fn record_worker_warning(state: &mut WorkerState, warning: domain::WarningEvent) {
    *state
        .warning_counts
        .entry(warning.code.clone())
        .or_default() += 1;
    state.last_warning = Some(warning);
}

async fn send_client_message(
    tx: &mpsc::Sender<pb::ClientMsg>,
    message: pb::ClientMsg,
) -> Result<(), rmcp::ErrorData> {
    tx.send(message).await.map_err(|_| {
        rmcp::ErrorData::internal_error("daemon client stream closed before startup finished", None)
    })
}

fn normalize_daemon_addr(raw: &str) -> String {
    let raw = raw.trim();
    if raw.starts_with("http://") || raw.starts_with("https://") {
        raw.to_owned()
    } else {
        format!("http://{raw}")
    }
}

pub async fn run_stdio_server() -> Result<()> {
    let handler = VmuiMcpProxy::default();
    info!(daemon_addr = %handler.config.normalized_daemon_addr(), "starting vmui-mcp-proxy");
    let service = handler
        .serve(stdio())
        .await
        .context("failed to start stdio MCP service")?;
    service
        .waiting()
        .await
        .context("stdio MCP service terminated with error")?;
    Ok(())
}

#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum ProxyObservationScope {
    Desktop,
    AttachedWindows,
}

impl From<ProxyObservationScope> for domain::ObservationScope {
    fn from(value: ProxyObservationScope) -> Self {
        match value {
            ProxyObservationScope::Desktop => Self::Desktop,
            ProxyObservationScope::AttachedWindows => Self::AttachedWindows,
        }
    }
}

impl From<domain::ObservationScope> for ProxyObservationScope {
    fn from(value: domain::ObservationScope) -> Self {
        match value {
            domain::ObservationScope::Desktop => Self::Desktop,
            domain::ObservationScope::AttachedWindows => Self::AttachedWindows,
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum ProxyDomainProfile {
    Generic,
    OnecEnterpriseUi,
    OnecConfigurator,
}

impl From<ProxyDomainProfile> for domain::DomainProfile {
    fn from(value: ProxyDomainProfile) -> Self {
        match value {
            ProxyDomainProfile::Generic => Self::Generic,
            ProxyDomainProfile::OnecEnterpriseUi => Self::OnecEnterpriseUi,
            ProxyDomainProfile::OnecConfigurator => Self::OnecConfigurator,
        }
    }
}

impl From<domain::DomainProfile> for ProxyDomainProfile {
    fn from(value: domain::DomainProfile) -> Self {
        match value {
            domain::DomainProfile::Generic => Self::Generic,
            domain::DomainProfile::OnecEnterpriseUi => Self::OnecEnterpriseUi,
            domain::DomainProfile::OnecConfigurator => Self::OnecConfigurator,
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct ProxyWindowFilter {
    pub window_id: Option<String>,
    pub title: Option<String>,
    pub pid: Option<u32>,
    pub process_name: Option<String>,
    pub class_name: Option<String>,
}

impl From<ProxyWindowFilter> for domain::WindowLocator {
    fn from(value: ProxyWindowFilter) -> Self {
        Self {
            window_id: value.window_id.map(domain::WindowId::from),
            title: value.title,
            pid: value.pid,
            process_name: value.process_name,
            class_name: value.class_name,
        }
    }
}

impl From<domain::WindowLocator> for ProxyWindowFilter {
    fn from(value: domain::WindowLocator) -> Self {
        Self {
            window_id: value.window_id.map(|id| id.to_string()),
            title: value.title,
            pid: value.pid,
            process_name: value.process_name,
            class_name: value.class_name,
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct ProxySessionProfile {
    pub observation_scope: ProxyObservationScope,
    pub domain_profile: ProxyDomainProfile,
    pub target_filter: Option<ProxyWindowFilter>,
}

impl From<ProxySessionProfile> for domain::SessionProfile {
    fn from(value: ProxySessionProfile) -> Self {
        domain::SessionProfile {
            observation_scope: value.observation_scope.into(),
            domain_profile: value.domain_profile.into(),
            target_filter: value.target_filter.map(Into::into),
        }
        .normalized()
    }
}

impl From<domain::SessionProfile> for ProxySessionProfile {
    fn from(value: domain::SessionProfile) -> Self {
        Self {
            observation_scope: value.observation_scope.into(),
            domain_profile: value.domain_profile.into(),
            target_filter: value.target_filter.map(Into::into),
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum CapturePolicyParam {
    Never,
    OnFailure,
    Always,
}

impl From<CapturePolicyParam> for domain::CapturePolicy {
    fn from(value: CapturePolicyParam) -> Self {
        match value {
            CapturePolicyParam::Never => Self::Never,
            CapturePolicyParam::OnFailure => Self::OnFailure,
            CapturePolicyParam::Always => Self::Always,
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum WaitConditionParam {
    Exists,
    Visible,
    Enabled,
    Focused,
    Gone,
}

impl From<WaitConditionParam> for domain::WaitCondition {
    fn from(value: WaitConditionParam) -> Self {
        match value {
            WaitConditionParam::Exists => Self::Exists,
            WaitConditionParam::Visible => Self::Visible,
            WaitConditionParam::Enabled => Self::Enabled,
            WaitConditionParam::Focused => Self::Focused,
            WaitConditionParam::Gone => Self::Gone,
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum MouseButtonParam {
    Left,
    Right,
    Middle,
}

impl From<MouseButtonParam> for domain::MouseButton {
    fn from(value: MouseButtonParam) -> Self {
        match value {
            MouseButtonParam::Left => Self::Left,
            MouseButtonParam::Right => Self::Right,
            MouseButtonParam::Middle => Self::Middle,
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum DiagnosticVerdictParam {
    Failed,
    TimedOut,
    Blocked,
    Passed,
}

impl From<DiagnosticVerdictParam> for domain::DiagnosticStepVerdict {
    fn from(value: DiagnosticVerdictParam) -> Self {
        match value {
            DiagnosticVerdictParam::Failed => Self::Failed,
            DiagnosticVerdictParam::TimedOut => Self::TimedOut,
            DiagnosticVerdictParam::Blocked => Self::Blocked,
            DiagnosticVerdictParam::Passed => Self::Passed,
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct SessionOpenParams {
    pub profile: ProxySessionProfile,
}

#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct SessionRefParams {
    pub session_id: Option<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct ListWindowsParams {
    pub session_id: Option<String>,
    pub timeout_ms: Option<u64>,
}

#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct RuntimeStatusParams {
    pub session_id: Option<String>,
    pub timeout_ms: Option<u64>,
}

#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct GetTreeParams {
    pub session_id: Option<String>,
    pub target: TargetParams,
    pub raw: bool,
    pub max_depth: Option<u32>,
    pub timeout_ms: Option<u64>,
}

#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct WaitForParams {
    pub session_id: Option<String>,
    pub target: TargetParams,
    pub condition: WaitConditionParam,
    pub stable_for_ms: Option<u64>,
    pub timeout_ms: Option<u64>,
    pub capture_policy: Option<CapturePolicyParam>,
}

#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct FocusWindowParams {
    pub session_id: Option<String>,
    pub target: TargetParams,
    pub timeout_ms: Option<u64>,
    pub capture_policy: Option<CapturePolicyParam>,
}

#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct InvokeParams {
    pub session_id: Option<String>,
    pub target: TargetParams,
    pub timeout_ms: Option<u64>,
    pub capture_policy: Option<CapturePolicyParam>,
}

#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct ClickElementParams {
    pub session_id: Option<String>,
    pub target: TargetParams,
    pub button: MouseButtonParam,
    pub clicks: Option<u8>,
    pub timeout_ms: Option<u64>,
    pub capture_policy: Option<CapturePolicyParam>,
}

#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct SetValueParams {
    pub session_id: Option<String>,
    pub target: TargetParams,
    pub value: String,
    pub clear_first: Option<bool>,
    pub timeout_ms: Option<u64>,
    pub capture_policy: Option<CapturePolicyParam>,
}

#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct SendKeysParams {
    pub session_id: Option<String>,
    pub target: TargetParams,
    pub keys: String,
    pub timeout_ms: Option<u64>,
    pub capture_policy: Option<CapturePolicyParam>,
}

#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct CollectDiagnosticBundleParams {
    pub session_id: Option<String>,
    pub target: TargetParams,
    pub step_id: Option<String>,
    pub step_label: String,
    pub test_verdict: DiagnosticVerdictParam,
    pub note: Option<String>,
    pub baseline_artifact_id: Option<String>,
    pub max_tree_depth: Option<u32>,
    pub timeout_ms: Option<u64>,
}

#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct ArtifactReadParams {
    pub session_id: Option<String>,
    pub artifact_id: String,
}

#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct RectParams {
    pub left: i32,
    pub top: i32,
    pub width: i32,
    pub height: i32,
}

impl From<RectParams> for domain::Rect {
    fn from(value: RectParams) -> Self {
        Self {
            left: value.left,
            top: value.top,
            width: value.width,
            height: value.height,
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct LocatorSegmentParams {
    pub control_type: String,
    pub class_name: Option<String>,
    pub automation_id: Option<String>,
    pub name: Option<String>,
    pub sibling_ordinal: Option<u32>,
}

impl From<LocatorSegmentParams> for domain::LocatorSegment {
    fn from(value: LocatorSegmentParams) -> Self {
        Self {
            control_type: value.control_type,
            class_name: value.class_name,
            automation_id: value.automation_id,
            name: value.name,
            sibling_ordinal: value.sibling_ordinal,
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct LocatorParams {
    pub window_fingerprint: String,
    pub path: Vec<LocatorSegmentParams>,
}

impl From<LocatorParams> for domain::Locator {
    fn from(value: LocatorParams) -> Self {
        Self {
            window_fingerprint: value.window_fingerprint,
            path: value.path.into_iter().map(Into::into).collect(),
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum TargetParams {
    Desktop,
    Window {
        window_id: Option<String>,
        title: Option<String>,
        pid: Option<u32>,
        process_name: Option<String>,
        class_name: Option<String>,
    },
    Element {
        element_id: Option<String>,
        locator: Option<LocatorParams>,
    },
    Region {
        window_id: Option<String>,
        bounds: RectParams,
    },
}

impl TryFrom<TargetParams> for domain::ActionTarget {
    type Error = rmcp::ErrorData;

    fn try_from(value: TargetParams) -> Result<Self, Self::Error> {
        Ok(match value {
            TargetParams::Desktop => Self::Desktop,
            TargetParams::Window {
                window_id,
                title,
                pid,
                process_name,
                class_name,
            } => Self::Window(domain::WindowLocator {
                window_id: window_id.map(domain::WindowId::from),
                title,
                pid,
                process_name,
                class_name,
            }),
            TargetParams::Element {
                element_id,
                locator,
            } => Self::Element(domain::ElementLocator {
                element_id: element_id.map(domain::ElementId::from),
                locator: locator.map(Into::into),
            }),
            TargetParams::Region { window_id, bounds } => Self::Region(domain::RegionTarget {
                window_id: window_id.map(domain::WindowId::from),
                bounds: bounds.into(),
            }),
        })
    }
}

#[derive(Clone, Debug, Serialize)]
pub struct SessionOpenResult {
    pub session_id: String,
    pub profile: ProxySessionProfile,
    pub daemon_session_id: Option<String>,
    pub backend_id: String,
    pub capabilities: Vec<String>,
    pub connected: bool,
    pub continuity_invalidated: bool,
    pub continuity_reason: Option<String>,
    pub snapshot_revision: Option<u64>,
    pub window_count: Option<usize>,
}

#[derive(Clone, Debug, Serialize)]
pub struct SessionCloseResult {
    pub ok: bool,
    pub session_id: String,
}

#[derive(Clone, Debug, Serialize)]
pub struct WorkerStatus {
    pub session_id: String,
    pub profile: ProxySessionProfile,
    pub daemon_session_id: Option<String>,
    pub backend_id: String,
    pub capabilities: Vec<String>,
    pub connected: bool,
    pub snapshot_revision: Option<u64>,
    pub window_count: Option<usize>,
    pub resync_required: bool,
    pub resync_reason: Option<String>,
    pub reconnect_count: u64,
    pub disconnect_reason: Option<String>,
    pub continuity_invalidated: bool,
    pub continuity_reason: Option<String>,
    pub warning_counts: BTreeMap<String, u64>,
    pub last_warning: Option<domain::WarningEvent>,
}

#[derive(Clone, Debug, Serialize)]
struct SessionOutcome {
    daemon_session_id: Option<String>,
    connected: bool,
    reconnected: bool,
    continuity_invalidated: bool,
    continuity_reason: Option<String>,
}

#[derive(Clone, Debug, Serialize)]
pub struct ReadEnvelope {
    pub session_id: String,
    pub daemon_session_id: Option<String>,
    pub connected: bool,
    pub reconnected: bool,
    pub continuity_invalidated: bool,
    pub continuity_reason: Option<String>,
    pub payload: serde_json::Value,
}

impl ReadEnvelope {
    fn from_outcome(session_id: String, outcome: ReadActionOutcome) -> Self {
        Self {
            session_id,
            daemon_session_id: outcome.session.daemon_session_id,
            connected: outcome.session.connected,
            reconnected: outcome.session.reconnected,
            continuity_invalidated: outcome.session.continuity_invalidated,
            continuity_reason: outcome.session.continuity_reason,
            payload: outcome.payload,
        }
    }
}

#[derive(Clone, Debug, Serialize)]
pub struct ActionEnvelope {
    pub session_id: String,
    pub daemon_session_id: Option<String>,
    pub connected: bool,
    pub reconnected: bool,
    pub continuity_invalidated: bool,
    pub continuity_reason: Option<String>,
    pub action_result: domain::ActionResult,
}

impl ActionEnvelope {
    fn from_outcome(session_id: String, outcome: ActionOutcome) -> Self {
        Self {
            session_id,
            daemon_session_id: outcome.session.daemon_session_id,
            connected: outcome.session.connected,
            reconnected: outcome.session.reconnected,
            continuity_invalidated: outcome.session.continuity_invalidated,
            continuity_reason: outcome.session.continuity_reason,
            action_result: outcome.action_result,
        }
    }
}

#[derive(Debug)]
struct ReadActionOutcome {
    session: SessionOutcome,
    payload: serde_json::Value,
}

#[derive(Debug)]
struct ActionOutcome {
    session: SessionOutcome,
    action_result: domain::ActionResult,
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::{TimeZone, Utc};
    use std::net::SocketAddr;
    use tempfile::tempdir;
    use tokio::{net::TcpListener, sync::oneshot};
    use tokio_stream::wrappers::TcpListenerStream;
    use tonic::transport::Server;
    use vmui_agent::UiAgentService;
    use vmui_core::{AgentConfig, ArtifactRetentionPolicy};
    use vmui_platform::{
        BackendActionResult, BackendArtifact, BackendCapabilities, BackendEvent, BackendSession,
        BackendSessionParams, UiBackend,
    };
    use vmui_protocol::{
        ActionStatus, BackendKind, ElementId, ElementNode, ElementStates, Locator, PropertyValue,
        Rect, SessionId, SessionProfile, UiSnapshot, WindowId, WindowState,
    };

    #[derive(Clone, Default)]
    struct CountingBackend;

    #[tonic::async_trait]
    impl UiBackend for CountingBackend {
        fn backend_id(&self) -> &'static str {
            "counting-backend"
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
                initial_snapshot: sample_snapshot(params.session_id, params.profile, 1),
                events: Box::pin(tokio_stream::empty::<BackendEvent>()),
            })
        }

        async fn capture_snapshot(
            &self,
            params: BackendSessionParams,
        ) -> anyhow::Result<UiSnapshot> {
            Ok(sample_snapshot(params.session_id, params.profile, 1))
        }

        async fn perform_action(
            &self,
            action: domain::ActionRequest,
        ) -> anyhow::Result<BackendActionResult> {
            Ok(BackendActionResult {
                action_id: action.action_id,
                ok: true,
                status: ActionStatus::Completed,
                message: "ok".to_owned(),
                artifacts: vec![BackendArtifact {
                    kind: "log-text".to_owned(),
                    mime_type: "text/plain".to_owned(),
                    bytes: b"ok".to_vec(),
                }],
            })
        }
    }

    struct DaemonHandle {
        addr: SocketAddr,
        shutdown: oneshot::Sender<()>,
    }

    async fn spawn_daemon(artifact_dir: std::path::PathBuf) -> DaemonHandle {
        let listener = TcpListener::bind("127.0.0.1:0").await.expect("bind daemon");
        let addr = listener.local_addr().expect("daemon addr");
        let incoming = TcpListenerStream::new(listener);
        let (shutdown_tx, shutdown_rx) = oneshot::channel();
        let service = UiAgentService::new(
            AgentConfig {
                bind_addr: addr.to_string(),
                artifact_dir,
                default_profile: enterprise_profile(),
                artifact_retention: ArtifactRetentionPolicy {
                    max_age_seconds: 24 * 60 * 60,
                    max_bytes: 128 * 1024 * 1024,
                    max_count: 128,
                    cleanup_interval_seconds: 1,
                },
            },
            CountingBackend,
        )
        .expect("create daemon service");

        tokio::spawn(async move {
            Server::builder()
                .add_service(pb::ui_agent_server::UiAgentServer::new(service))
                .serve_with_incoming_shutdown(incoming, async move {
                    let _ = shutdown_rx.await;
                })
                .await
                .expect("daemon server must run");
        });

        DaemonHandle {
            addr,
            shutdown: shutdown_tx,
        }
    }

    #[tokio::test]
    async fn read_actions_reconnect_when_worker_is_disconnected() {
        let dir = tempdir().expect("tempdir");
        let daemon = spawn_daemon(dir.path().to_path_buf()).await;
        let worker = DaemonSessionWorker::open(
            "mcp-sess-1".to_owned(),
            format!("http://{}", daemon.addr),
            enterprise_profile(),
        )
        .await
        .expect("open worker");

        {
            let mut state = worker.state.write().await;
            state.connected = false;
            state.outbound_tx = None;
            state.disconnect_reason = Some("forced_disconnect".to_owned());
        }

        let outcome = worker
            .execute_json_read_action(
                domain::ActionRequest {
                    action_id: domain::ActionId::new("test-list"),
                    timeout_ms: 2_000,
                    target: domain::ActionTarget::Desktop,
                    kind: domain::ActionKind::ListWindows,
                    capture_policy: domain::CapturePolicy::Never,
                },
                true,
            )
            .await
            .expect("read action reconnect");

        assert!(outcome.session.reconnected);
        assert!(outcome.session.continuity_invalidated);
        assert_eq!(outcome.payload["windows"].as_array().map(Vec::len), Some(1));

        let status = worker.status().await;
        assert!(status.connected);
        assert_eq!(status.reconnect_count, 1);
        assert!(status.continuity_invalidated);

        let _ = daemon.shutdown.send(());
    }

    #[tokio::test]
    async fn mutating_actions_do_not_reconnect_silently() {
        let dir = tempdir().expect("tempdir");
        let daemon = spawn_daemon(dir.path().to_path_buf()).await;
        let worker = DaemonSessionWorker::open(
            "mcp-sess-2".to_owned(),
            format!("http://{}", daemon.addr),
            enterprise_profile(),
        )
        .await
        .expect("open worker");

        {
            let mut state = worker.state.write().await;
            state.connected = false;
            state.outbound_tx = None;
            state.disconnect_reason = Some("forced_disconnect".to_owned());
        }

        let error = worker
            .execute_action(
                domain::ActionRequest {
                    action_id: domain::ActionId::new("test-focus"),
                    timeout_ms: 2_000,
                    target: domain::ActionTarget::Window(domain::WindowLocator {
                        window_id: Some(domain::WindowId::from("wnd-1")),
                        title: None,
                        pid: None,
                        process_name: None,
                        class_name: None,
                    }),
                    kind: domain::ActionKind::FocusWindow,
                    capture_policy: domain::CapturePolicy::OnFailure,
                },
                false,
            )
            .await
            .expect_err("mutating action must not auto-reconnect");

        assert_eq!(error.code.0, rmcp::model::ErrorCode::INTERNAL_ERROR.0);
        assert!(error.message.contains("retry"));

        let status = worker.status().await;
        assert!(!status.connected);
        assert_eq!(status.reconnect_count, 0);

        let _ = daemon.shutdown.send(());
    }

    fn enterprise_profile() -> SessionProfile {
        SessionProfile::onec_enterprise_ui()
    }

    fn sample_snapshot(session_id: SessionId, profile: SessionProfile, rev: u64) -> UiSnapshot {
        UiSnapshot {
            session_id,
            rev,
            profile,
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
                    properties: BTreeMap::from([(
                        "onec_window_profile".to_owned(),
                        PropertyValue::String("enterprise_ui_main_window".to_owned()),
                    )]),
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

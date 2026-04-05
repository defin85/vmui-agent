use std::{
    net::SocketAddr,
    sync::{
        atomic::{AtomicUsize, Ordering},
        Arc,
    },
    time::Duration,
};

use anyhow::Result;
use chrono::{TimeZone, Utc};
use rmcp::{
    service::RunningService,
    transport::{ConfigureCommandExt, TokioChildProcess},
    RoleClient, ServiceError, ServiceExt,
};
use serde::de::DeserializeOwned;
use serde_json::json;
use tempfile::tempdir;
use tokio::{net::TcpListener, process::Command, sync::oneshot};
use tokio_stream::wrappers::TcpListenerStream;
use tonic::transport::Server;
use vmui_agent::UiAgentService;
use vmui_core::{AgentConfig, ArtifactRetentionPolicy};
use vmui_platform::{
    BackendActionResult, BackendArtifact, BackendCapabilities, BackendEvent, BackendSession,
    BackendSessionParams, UiBackend,
};
use vmui_protocol::{
    ActionRequest, ActionStatus, BackendKind, ElementId, ElementNode, ElementStates, Locator,
    PropertyValue, Rect, SessionId, SessionProfile, UiSnapshot, WindowId, WindowState,
};
use vmui_transport_grpc::pb;

fn proxy_bin() -> &'static str {
    env!("CARGO_BIN_EXE_vmui-mcp-proxy")
}

#[derive(Clone, Default)]
struct CountingBackend {
    open_session_calls: Arc<AtomicUsize>,
}

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

    async fn open_session(&self, params: BackendSessionParams) -> anyhow::Result<BackendSession> {
        self.open_session_calls.fetch_add(1, Ordering::SeqCst);
        Ok(BackendSession {
            initial_snapshot: sample_snapshot(params.session_id, params.profile, 1),
            events: Box::pin(tokio_stream::empty::<BackendEvent>()),
        })
    }

    async fn capture_snapshot(&self, params: BackendSessionParams) -> anyhow::Result<UiSnapshot> {
        Ok(sample_snapshot(params.session_id, params.profile, 1))
    }

    async fn perform_action(&self, action: ActionRequest) -> anyhow::Result<BackendActionResult> {
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
    task: tokio::task::JoinHandle<()>,
}

async fn spawn_daemon(
    addr: SocketAddr,
    artifact_dir: std::path::PathBuf,
    backend: CountingBackend,
) -> Result<DaemonHandle> {
    let listener = TcpListener::bind(addr).await?;
    let actual_addr = listener.local_addr()?;
    let incoming = TcpListenerStream::new(listener);
    let (shutdown_tx, shutdown_rx) = oneshot::channel();
    let service = UiAgentService::new(
        AgentConfig {
            bind_addr: actual_addr.to_string(),
            artifact_dir,
            default_profile: enterprise_profile(),
            artifact_retention: ArtifactRetentionPolicy {
                max_age_seconds: 24 * 60 * 60,
                max_bytes: 128 * 1024 * 1024,
                max_count: 128,
                cleanup_interval_seconds: 1,
            },
        },
        backend.clone(),
    )?;

    let task = tokio::spawn(async move {
        Server::builder()
            .add_service(pb::ui_agent_server::UiAgentServer::new(service))
            .serve_with_incoming_shutdown(incoming, async move {
                let _ = shutdown_rx.await;
            })
            .await
            .expect("daemon server must run");
    });

    Ok(DaemonHandle {
        addr: actual_addr,
        shutdown: shutdown_tx,
        task,
    })
}

async fn stop_daemon(handle: DaemonHandle) {
    let _ = handle.shutdown.send(());
    handle.task.abort();
    tokio::time::sleep(Duration::from_millis(100)).await;
}

async fn spawn_proxy(daemon_addr: SocketAddr) -> RunningService<RoleClient, ()> {
    let cmd = Command::new(proxy_bin()).configure(|cmd| {
        cmd.env("RUST_LOG", "error");
        cmd.env("VMUI_DAEMON_ADDR", format!("http://{daemon_addr}"));
    });
    let transport = TokioChildProcess::new(cmd).expect("spawn vmui-mcp-proxy");
    ().serve(transport).await.expect("connect to mcp proxy")
}

fn json_object(value: serde_json::Value) -> rmcp::model::JsonObject {
    value.as_object().expect("json object").clone()
}

fn extract_json_text(result: rmcp::model::CallToolResult) -> serde_json::Value {
    let content = result.content.into_iter().next().expect("tool content");
    let text = content.as_text().expect("text content").text.clone();
    serde_json::from_str(&text).expect("json decode")
}

async fn call_tool<T: DeserializeOwned>(
    service: &RunningService<RoleClient, ()>,
    name: &'static str,
    args: serde_json::Value,
) -> T {
    let result = service
        .call_tool(
            serde_json::from_value(json!({
                "name": name,
                "arguments": json_object(args),
            }))
            .expect("call tool request"),
        )
        .await
        .expect("call_tool");
    let value = extract_json_text(result);
    serde_json::from_value(value).expect("decode tool result")
}

async fn call_tool_expect_error(
    service: &RunningService<RoleClient, ()>,
    name: &'static str,
    args: serde_json::Value,
) -> rmcp::model::ErrorData {
    let error = service
        .call_tool(
            serde_json::from_value(json!({
                "name": name,
                "arguments": json_object(args),
            }))
            .expect("call tool request"),
        )
        .await
        .expect_err("tool call must fail");
    match error {
        ServiceError::McpError(error) => error,
        other => panic!("unexpected service error: {other:?}"),
    }
}

#[derive(serde::Deserialize)]
struct SessionOpenResult {
    session_id: String,
}

#[derive(serde::Deserialize)]
struct ReadEnvelope {
    session_id: String,
    payload: serde_json::Value,
}

#[tokio::test]
async fn logical_session_reuses_one_daemon_stream_for_related_reads() {
    let dir = tempdir().expect("tempdir");
    let backend = CountingBackend::default();
    let daemon = spawn_daemon(
        "127.0.0.1:0".parse().expect("socket addr"),
        dir.path().to_path_buf(),
        backend.clone(),
    )
    .await
    .expect("spawn daemon");
    let proxy = spawn_proxy(daemon.addr).await;

    let opened: SessionOpenResult = call_tool(
        &proxy,
        "session_open",
        json!({
            "profile": {
                "observationScope": "desktop",
                "domainProfile": "onec_enterprise_ui"
            }
        }),
    )
    .await;
    let windows: ReadEnvelope = call_tool(
        &proxy,
        "list_windows",
        json!({ "sessionId": opened.session_id }),
    )
    .await;
    let tree: ReadEnvelope = call_tool(
        &proxy,
        "get_tree",
        json!({
            "sessionId": windows.session_id,
            "target": { "kind": "window", "windowId": "wnd-1" },
            "raw": true,
            "maxDepth": 0
        }),
    )
    .await;

    assert_eq!(windows.payload["windows"].as_array().map(Vec::len), Some(1));
    assert_eq!(tree.payload["root"]["element_id"], "elt-1");
    assert_eq!(backend.open_session_calls.load(Ordering::SeqCst), 1);
    proxy.cancel().await.expect("cancel proxy");
    stop_daemon(daemon).await;
}

#[tokio::test]
async fn omitting_session_id_is_rejected_when_multiple_sessions_exist() {
    let dir = tempdir().expect("tempdir");
    let daemon = spawn_daemon(
        "127.0.0.1:0".parse().expect("socket addr"),
        dir.path().to_path_buf(),
        CountingBackend::default(),
    )
    .await
    .expect("spawn daemon");
    let proxy = spawn_proxy(daemon.addr).await;

    let _: SessionOpenResult = call_tool(
        &proxy,
        "session_open",
        json!({
            "profile": {
                "observationScope": "desktop",
                "domainProfile": "onec_enterprise_ui"
            }
        }),
    )
    .await;
    let _: SessionOpenResult = call_tool(
        &proxy,
        "session_open",
        json!({
            "profile": {
                "observationScope": "desktop",
                "domainProfile": "onec_configurator"
            }
        }),
    )
    .await;

    let error = call_tool_expect_error(&proxy, "session_status", json!({})).await;
    assert_eq!(error.code.0, rmcp::model::ErrorCode::INVALID_PARAMS.0);
    assert!(error.message.contains("session_id"));
    proxy.cancel().await.expect("cancel proxy");
    stop_daemon(daemon).await;
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
                properties: std::collections::BTreeMap::from([(
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

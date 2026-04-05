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
use vmui_core::ArtifactRetentionPolicy;
use vmui_platform::{
    BackendActionResult, BackendArtifact, BackendCapabilities, BackendEvent, BackendSession,
    BackendSessionParams, UiBackend,
};
use vmui_protocol::{
    ActionId, ActionRequest, ActionStatus, ActionTarget, ArtifactId, BackendKind, CapturePolicy,
    DiagnosticBundleOptions, DiagnosticStepVerdict, DiffOp, ElementId, ElementLocator, ElementNode,
    ElementStates, Locator, PropertyValue, Rect, RuntimeHealthState, RuntimeStatusRequest,
    SessionId, SessionMode, TreeRequest, UiDiffBatch, UiSnapshot, WaitCondition, WaitForOptions,
    WindowId, WindowLocator, WindowState,
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

    async fn open_session(&self, params: BackendSessionParams) -> anyhow::Result<BackendSession> {
        Ok(BackendSession {
            initial_snapshot: sample_snapshot(
                params.session_id.clone(),
                params.mode.clone(),
                self.next_snapshot_rev(),
            ),
            events: Box::pin(stream::iter(self.events.clone())),
        })
    }

    async fn capture_snapshot(&self, params: BackendSessionParams) -> anyhow::Result<UiSnapshot> {
        Ok(sample_snapshot(
            params.session_id,
            params.mode,
            self.next_snapshot_rev(),
        ))
    }

    async fn perform_action(&self, action: ActionRequest) -> anyhow::Result<BackendActionResult> {
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

    async fn open_session(&self, params: BackendSessionParams) -> anyhow::Result<BackendSession> {
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

    async fn capture_snapshot(&self, params: BackendSessionParams) -> anyhow::Result<UiSnapshot> {
        Ok(sample_snapshot(params.session_id, params.mode, 1))
    }

    async fn perform_action(&self, action: ActionRequest) -> anyhow::Result<BackendActionResult> {
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

    async fn open_session(&self, params: BackendSessionParams) -> anyhow::Result<BackendSession> {
        Ok(BackendSession {
            initial_snapshot: sample_snapshot(params.session_id, params.mode, 1),
            events: Box::pin(stream::empty::<BackendEvent>()),
        })
    }

    async fn capture_snapshot(&self, params: BackendSessionParams) -> anyhow::Result<UiSnapshot> {
        Ok(sample_snapshot(params.session_id, params.mode, 1))
    }

    async fn perform_action(&self, action: ActionRequest) -> anyhow::Result<BackendActionResult> {
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
async fn collect_diagnostic_bundle_persists_bundle_diff_and_baseline_comparison() {
    let dir = tempdir().expect("tempdir");
    let backend = StubBackend::with_events_and_snapshots(
        vec![BackendEvent::Diff(UiDiffBatch {
            base_rev: 1,
            new_rev: 2,
            emitted_at: Utc
                .timestamp_millis_opt(1_700_000_002_000)
                .single()
                .unwrap(),
            ops: vec![DiffOp::PropertyChanged {
                element_id: ElementId::from("elt-1"),
                field: "onec_fallback_reason".to_owned(),
                value: PropertyValue::String("weak_semantic_metadata".to_owned()),
            }],
        })],
        [1],
    );
    let (mut client, shutdown, state) =
        spawn_test_server_with_backend(dir.path().to_path_buf(), backend)
            .await
            .expect("spawn server");

    let baseline_artifact_id = {
        let mut baseline = sample_snapshot(
            SessionId::from("sess-baseline"),
            SessionMode::EnterpriseUi,
            1,
        );
        baseline.windows[0].window_id = WindowId::from("wnd-baseline");
        baseline.windows[0].root.element_id = ElementId::from("elt-baseline");
        baseline.windows[0].root.locator.window_fingerprint = "1cv8.exe:1c:baseline".to_owned();
        baseline.windows[0].root.native_window_handle = Some(77);
        baseline.windows[0].root.children.push(sample_editor_node(
            "1cv8.exe:1c:baseline",
            "elt-baseline-editor",
            "elt-baseline",
            "Поиск",
        ));
        let bytes = serde_json::to_vec(&baseline).expect("baseline json");

        let mut state = state.write().await;
        state
            .artifacts
            .write_bytes(None, "snapshot-json", "application/json", &bytes)
            .expect("write baseline artifact")
            .artifact_id
            .to_string()
    };

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
    let _ = inbound
        .message()
        .await
        .expect("snapshot read")
        .expect("snapshot payload");
    let _ = inbound
        .message()
        .await
        .expect("diff read")
        .expect("diff payload");

    let action = encode_action_request(&ActionRequest {
        action_id: ActionId::from("diag-1"),
        timeout_ms: 2_000,
        target: ActionTarget::Window(WindowLocator {
            window_id: Some(WindowId::from("wnd-1")),
            title: None,
            pid: None,
        }),
        kind: domain::ActionKind::CollectDiagnosticBundle(DiagnosticBundleOptions {
            step_id: Some("step-7".to_owned()),
            step_label: "Открытие формы".to_owned(),
            test_verdict: DiagnosticStepVerdict::Failed,
            note: Some("runner=standard-1c".to_owned()),
            baseline_artifact_id: Some(baseline_artifact_id.clone().into()),
            max_tree_depth: Some(1),
        }),
        capture_policy: CapturePolicy::Never,
    })
    .expect("encode action");
    outbound_tx
        .send(pb::ClientMsg {
            payload: Some(client_msg::Payload::ActionRequest(action)),
        })
        .await
        .expect("send action");
    drop(outbound_tx);

    let result = inbound
        .message()
        .await
        .expect("action result read")
        .expect("action result payload");

    let artifacts = match result.payload.expect("payload") {
        server_msg::Payload::ActionResult(result) => {
            assert!(result.ok);
            assert_eq!(result.status, "completed");
            assert!(result.message.contains("Открытие формы"));
            result.artifacts
        }
        other => panic!("unexpected payload: {other:?}"),
    };

    let diagnostic_artifact = artifacts
        .iter()
        .find(|artifact| artifact.kind == "diagnostic-json")
        .expect("diagnostic artifact");
    let diff_artifact = artifacts
        .iter()
        .find(|artifact| artifact.kind == "diff-json")
        .expect("diff artifact");
    let baseline_artifact = artifacts
        .iter()
        .find(|artifact| artifact.kind == "baseline-comparison-json")
        .expect("baseline comparison artifact");

    let diagnostic_json =
        read_artifact_json(&mut client, diagnostic_artifact.artifact_id.clone()).await;
    let diff_json = read_artifact_json(&mut client, diff_artifact.artifact_id.clone()).await;
    let baseline_json =
        read_artifact_json(&mut client, baseline_artifact.artifact_id.clone()).await;

    assert_eq!(
        diagnostic_json["diagnostic_context"]["test_verdict"],
        serde_json::Value::String("failed".to_owned())
    );
    assert_eq!(
        diagnostic_json["diagnostic_context"]["baseline_artifact_id"],
        serde_json::Value::String(baseline_artifact_id)
    );
    assert!(diagnostic_json["target_tree"].is_object());
    assert!(diagnostic_json["fallback_surfaces"].is_array());
    assert_eq!(diff_json.as_array().map(Vec::len), Some(1));
    assert_eq!(baseline_json["status"], "compared");
    assert_eq!(baseline_json["matches"], false);
    assert_eq!(
        baseline_json["matched_windows"].as_array().map(Vec::len),
        Some(1)
    );
    assert_eq!(
        baseline_json["added_windows"].as_array().map(Vec::len),
        Some(0)
    );
    assert_eq!(
        baseline_json["removed_windows"].as_array().map(Vec::len),
        Some(0)
    );
    assert_eq!(
        baseline_json["changed_windows"].as_array().map(Vec::len),
        Some(1)
    );
    assert!(baseline_json["changed_windows"][0]["changed_fields"]
        .as_array()
        .expect("changed_fields array")
        .iter()
        .any(|value| value == "tree_state"));

    let _ = shutdown.send(());
}

#[test]
fn compare_snapshots_matches_cross_session_windows_semantically() {
    let mut expected = sample_snapshot(
        SessionId::from("sess-expected"),
        SessionMode::EnterpriseUi,
        1,
    );
    expected.windows[0].window_id = WindowId::from("wnd-expected");
    expected.windows[0].root.element_id = ElementId::from("elt-expected");
    expected.windows[0].root.locator.window_fingerprint = "1cv8.exe:1c:expected".to_owned();
    expected.windows[0].root.native_window_handle = Some(55);
    expected.windows[0].root.children.push(sample_editor_node(
        "1cv8.exe:1c:expected",
        "elt-expected-editor",
        "elt-expected",
        "Поиск",
    ));

    let actual = sample_snapshot(SessionId::from("sess-actual"), SessionMode::EnterpriseUi, 1);

    let comparison = compare_snapshots(&ArtifactId::from("artifact-baseline"), &expected, &actual);

    assert_eq!(comparison["matches"], false);
    assert_eq!(
        comparison["matched_windows"].as_array().map(Vec::len),
        Some(1)
    );
    assert_eq!(
        comparison["added_windows"].as_array().map(Vec::len),
        Some(0)
    );
    assert_eq!(
        comparison["removed_windows"].as_array().map(Vec::len),
        Some(0)
    );
    assert_eq!(
        comparison["changed_windows"].as_array().map(Vec::len),
        Some(1)
    );
    assert!(comparison["changed_windows"][0]["changed_fields"]
        .as_array()
        .expect("changed_fields array")
        .iter()
        .any(|value| value == "tree_state"));
}

#[tokio::test]
async fn get_runtime_status_returns_structured_health_artifact() {
    let dir = tempdir().expect("tempdir");
    let (mut client, shutdown, state) = spawn_test_server_with_state(dir.path().to_path_buf())
        .await
        .expect("spawn server");
    let action = encode_action_request(&ActionRequest {
        action_id: ActionId::from("runtime-1"),
        timeout_ms: 1_000,
        target: ActionTarget::Desktop,
        kind: domain::ActionKind::GetRuntimeStatus(RuntimeStatusRequest::default()),
        capture_policy: CapturePolicy::Never,
    })
    .expect("encode action");

    {
        let mut state = state.write().await;
        state.record_warning("backend_degraded", "observer restart required");
        state.record_resync("stale diff");
        state.record_recovery("observer restarted", true);
        state.record_snapshot_observation(4);
    }

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
            assert!(result.ok);
            assert_eq!(result.artifacts.len(), 1);
            assert_eq!(result.artifacts[0].kind, "runtime-status-json");
            result.artifacts[0].artifact_id.clone()
        }
        other => panic!("unexpected payload: {other:?}"),
    };

    let payload = read_artifact_json(&mut client, artifact_id).await;
    assert_eq!(payload["health"]["status"], "degraded");
    assert_eq!(payload["recoveries"]["resync_count"], 1);
    assert_eq!(payload["recoveries"]["continuity_invalidated"], true);
    assert_eq!(payload["warnings"]["by_class"]["backend"], 1);
    assert_eq!(payload["observations"]["fallback_heavy_snapshot_count"], 1);

    let runtime_status = {
        let state = state.read().await;
        state.runtime_status()
    };
    assert_eq!(runtime_status.health.status, RuntimeHealthState::Degraded);
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
    let recovery_warning = inbound
        .message()
        .await
        .expect("warning read")
        .expect("warning payload");

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

    match recovery_warning.payload.expect("payload") {
        server_msg::Payload::Warning(warning) => {
            assert_eq!(warning.code, "session_state_recovered");
            assert!(warning.message.contains("continuity_invalidated=true"));
        }
        other => panic!("unexpected payload: {other:?}"),
    }

    let (current_rev, runtime_status) = {
        let state = state.read().await;
        let session_id = state
            .sessions
            .session_ids()
            .into_iter()
            .next()
            .expect("session id");
        (
            state
                .sessions
                .runtime(&session_id)
                .and_then(|runtime| runtime.last_revision)
                .expect("last revision"),
            state.runtime_status(),
        )
    };

    assert_eq!(current_rev, 11);
    assert_eq!(runtime_status.recoveries.resync_count, 1);
    assert_eq!(runtime_status.recoveries.recovery_count, 1);
    assert!(runtime_status.recoveries.continuity_invalidated);
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
) -> Result<(UiAgentClient<Channel>, oneshot::Sender<()>, SharedState), Box<dyn std::error::Error>>
{
    spawn_test_server_with_backend(artifact_dir, StubBackend::default()).await
}

async fn spawn_test_server_with_backend<B>(
    artifact_dir: std::path::PathBuf,
    backend: B,
) -> Result<(UiAgentClient<Channel>, oneshot::Sender<()>, SharedState), Box<dyn std::error::Error>>
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
            artifact_retention: ArtifactRetentionPolicy::default(),
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

fn sample_editor_node(
    window_fingerprint: &str,
    element_id: &str,
    parent_id: &str,
    name: &str,
) -> ElementNode {
    ElementNode {
        element_id: ElementId::from(element_id),
        parent_id: Some(ElementId::from(parent_id)),
        backend: BackendKind::Mixed,
        control_type: "Edit".to_owned(),
        class_name: Some("V8Edit".to_owned()),
        name: Some(name.to_owned()),
        automation_id: None,
        native_window_handle: None,
        bounds: Rect {
            left: 16,
            top: 24,
            width: 120,
            height: 28,
        },
        locator: Locator {
            window_fingerprint: window_fingerprint.to_owned(),
            path: vec![vmui_protocol::LocatorSegment {
                control_type: "Edit".to_owned(),
                class_name: Some("V8Edit".to_owned()),
                automation_id: None,
                name: Some(name.to_owned()),
                sibling_ordinal: None,
            }],
        },
        properties: BTreeMap::from([(
            "onec_profile".to_owned(),
            PropertyValue::String("ordinary_form_text_input".to_owned()),
        )]),
        states: ElementStates {
            enabled: true,
            visible: true,
            focused: false,
            selected: false,
            expanded: false,
            toggled: false,
        },
        children: Vec::new(),
        confidence: 0.9,
    }
}

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
    ElementId, ElementLocator, ElementNode, ElementStates, Locator, Rect, SessionMode, TreeRequest,
    UiDiffBatch, UiSnapshot, WaitCondition, WaitForOptions, WindowId, WindowLocator, WindowState,
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

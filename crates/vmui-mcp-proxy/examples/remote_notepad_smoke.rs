use std::{env, time::Duration};

use anyhow::{bail, Context, Result};
use tokio::{
    sync::mpsc,
    time::{sleep, timeout},
};
use tokio_stream::wrappers::ReceiverStream;
use tonic::transport::Endpoint;
use vmui_protocol as domain;
use vmui_transport_grpc::{encode_action_request, pb};

const STARTUP_TIMEOUT: Duration = Duration::from_secs(10);
const ACTION_TIMEOUT: Duration = Duration::from_secs(10);

#[tokio::main]
async fn main() -> Result<()> {
    let typed_text = env::args()
        .nth(1)
        .or_else(|| env::var("VMUI_NOTEPAD_SMOKE_TEXT").ok())
        .unwrap_or_else(|| "vmui-remote-notepad-smoke".to_owned());
    let daemon_addr = normalize_daemon_addr(
        &env::var("VMUI_DAEMON_ADDR").unwrap_or_else(|_| "127.0.0.1:50051".to_owned()),
    );

    let channel = Endpoint::from_shared(daemon_addr.clone())
        .context("invalid VMUI_DAEMON_ADDR")?
        .connect()
        .await
        .with_context(|| format!("failed to connect to daemon at `{daemon_addr}`"))?;
    let mut client = pb::ui_agent_client::UiAgentClient::new(channel);
    let (outbound_tx, outbound_rx) = mpsc::channel(16);
    let response = client
        .session(ReceiverStream::new(outbound_rx))
        .await
        .context("failed to open daemon session")?;
    let mut inbound = response.into_inner();

    send_client_message(
        &outbound_tx,
        pb::ClientMsg {
            payload: Some(pb::client_msg::Payload::Hello(pb::Hello {
                client_name: "remote-notepad-smoke".to_owned(),
                client_version: env!("CARGO_PKG_VERSION").to_owned(),
                requested_profile: Some(pb::SessionProfile::from(notepad_profile())),
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

    let (ack, snapshot) = wait_for_startup(&mut inbound).await?;
    if ack.negotiated_profile != notepad_profile() {
        bail!(
            "daemon negotiated unexpected profile: {:?}",
            ack.negotiated_profile
        );
    }
    if snapshot.windows.len() != 1 {
        bail!(
            "expected exactly one attached Notepad window, got {}; start Notepad in the interactive VM session first",
            snapshot.windows.len()
        );
    }

    let window = &snapshot.windows[0];
    let target = domain::ActionTarget::Window(domain::WindowLocator {
        window_id: Some(window.window_id.clone()),
        title: Some(window.title.clone()),
        pid: Some(window.pid),
        process_name: window.process_name.clone(),
        class_name: window.root.class_name.clone(),
    });

    perform_action(
        &outbound_tx,
        &mut inbound,
        domain::ActionRequest {
            action_id: domain::ActionId::from("remote-notepad-focus"),
            timeout_ms: 5_000,
            target: target.clone(),
            kind: domain::ActionKind::FocusWindow,
            capture_policy: domain::CapturePolicy::Never,
        },
    )
    .await?;
    sleep(Duration::from_millis(200)).await;
    perform_action(
        &outbound_tx,
        &mut inbound,
        domain::ActionRequest {
            action_id: domain::ActionId::from("remote-notepad-send-keys"),
            timeout_ms: 5_000,
            target,
            kind: domain::ActionKind::SendKeys(domain::SendKeysOptions {
                keys: typed_text.clone(),
            }),
            capture_policy: domain::CapturePolicy::Never,
        },
    )
    .await?;

    println!(
        "{{\"session_id\":\"{}\",\"window_id\":\"{}\",\"title\":{:?},\"typed_text\":{:?}}}",
        ack.session_id, window.window_id, window.title, typed_text
    );

    Ok(())
}

fn normalize_daemon_addr(raw: &str) -> String {
    let raw = raw.trim();
    if raw.starts_with("http://") || raw.starts_with("https://") {
        raw.to_owned()
    } else {
        format!("http://{raw}")
    }
}

fn notepad_profile() -> domain::SessionProfile {
    domain::SessionProfile::attached_windows(
        domain::DomainProfile::Generic,
        domain::WindowLocator {
            window_id: None,
            title: None,
            pid: None,
            process_name: Some("notepad.exe".to_owned()),
            class_name: Some("Notepad".to_owned()),
        },
    )
}

async fn send_client_message(
    tx: &mpsc::Sender<pb::ClientMsg>,
    message: pb::ClientMsg,
) -> Result<()> {
    tx.send(message)
        .await
        .context("daemon client stream closed unexpectedly")
}

async fn wait_for_startup(
    inbound: &mut tonic::Streaming<pb::ServerMsg>,
) -> Result<(domain::HelloAck, domain::UiSnapshot)> {
    let mut hello_ack = None;
    let mut snapshot = None;

    while hello_ack.is_none() || snapshot.is_none() {
        match recv_server_message(inbound).await? {
            domain::ServerMessage::HelloAck(message) => hello_ack = Some(message),
            domain::ServerMessage::InitialSnapshot(message) => snapshot = Some(message),
            domain::ServerMessage::Warning(warning) => {
                eprintln!("warning: {}: {}", warning.code, warning.message);
            }
            domain::ServerMessage::DiffBatch(_)
            | domain::ServerMessage::ActionResult(_)
            | domain::ServerMessage::ArtifactReady(_)
            | domain::ServerMessage::Pong => {}
        }
    }

    Ok((hello_ack.expect("hello ack"), snapshot.expect("snapshot")))
}

async fn perform_action(
    outbound_tx: &mpsc::Sender<pb::ClientMsg>,
    inbound: &mut tonic::Streaming<pb::ServerMsg>,
    request: domain::ActionRequest,
) -> Result<domain::ActionResult> {
    let expected_action_id = request.action_id.clone();
    send_client_message(
        outbound_tx,
        pb::ClientMsg {
            payload: Some(pb::client_msg::Payload::ActionRequest(
                encode_action_request(&request).context("failed to encode daemon action")?,
            )),
        },
    )
    .await?;

    loop {
        match recv_server_message(inbound).await? {
            domain::ServerMessage::ActionResult(result)
                if result.action_id == expected_action_id =>
            {
                if !result.ok || result.status != domain::ActionStatus::Completed {
                    bail!(
                        "action `{}` failed with status {:?}: {}",
                        result.action_id,
                        result.status,
                        result.message
                    );
                }
                return Ok(result);
            }
            domain::ServerMessage::Warning(warning) => {
                eprintln!("warning: {}: {}", warning.code, warning.message);
            }
            domain::ServerMessage::DiffBatch(_)
            | domain::ServerMessage::HelloAck(_)
            | domain::ServerMessage::InitialSnapshot(_)
            | domain::ServerMessage::ArtifactReady(_)
            | domain::ServerMessage::Pong => {}
            domain::ServerMessage::ActionResult(_) => {}
        }
    }
}

async fn recv_server_message(
    inbound: &mut tonic::Streaming<pb::ServerMsg>,
) -> Result<domain::ServerMessage> {
    let message = timeout(ACTION_TIMEOUT.max(STARTUP_TIMEOUT), inbound.message())
        .await
        .context("timed out waiting for daemon session message")?
        .context("failed to read daemon session message")?
        .context("daemon session ended unexpectedly")?;
    domain::ServerMessage::try_from(message).context("failed to decode daemon session message")
}

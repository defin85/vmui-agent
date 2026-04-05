use std::{env, fs, path::PathBuf, time::Duration};

use anyhow::{bail, Context, Result};
use serde::Serialize;
use serde_json::Value;
use tokio::{sync::mpsc, time::timeout};
use tokio_stream::wrappers::ReceiverStream;
use tonic::transport::Endpoint;
use vmui_protocol as domain;
use vmui_transport_grpc::{encode_action_request, pb};

const STARTUP_TIMEOUT: Duration = Duration::from_secs(10);
const ACTION_TIMEOUT: Duration = Duration::from_secs(10);

#[derive(Debug)]
struct Config {
    daemon_addr: String,
    profile: domain::SessionProfile,
    keys: Option<String>,
    window_index: usize,
    expect_window_count: Option<usize>,
    click_element_id: Option<domain::ElementId>,
    capture_element_id: Option<domain::ElementId>,
    dump_tree: bool,
    tree_raw: bool,
    tree_max_depth: Option<u32>,
    capture_path: Option<PathBuf>,
    capture_format: domain::CaptureFormat,
}

#[derive(Debug, Serialize)]
struct WindowSummary<'a> {
    index: usize,
    window_id: &'a domain::WindowId,
    pid: u32,
    process_name: Option<&'a str>,
    title: &'a str,
    class_name: Option<&'a str>,
}

#[tokio::main]
async fn main() -> Result<()> {
    let config = Config::from_env()?;
    let requested_profile = config.profile.clone();
    let channel = Endpoint::from_shared(config.daemon_addr.clone())
        .context("invalid VMUI_DAEMON_ADDR")?
        .connect()
        .await
        .with_context(|| format!("failed to connect to daemon at `{}`", config.daemon_addr))?;
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
                client_name: "remote-session-smoke".to_owned(),
                client_version: env!("CARGO_PKG_VERSION").to_owned(),
                requested_profile: Some(pb::SessionProfile::from(requested_profile.clone())),
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
    if ack.negotiated_profile != requested_profile {
        bail!(
            "daemon negotiated unexpected profile: {:?}",
            ack.negotiated_profile
        );
    }

    if let Some(expected) = config.expect_window_count {
        if snapshot.windows.len() != expected {
            bail!(
                "expected {} windows for profile {:?}, got {}",
                expected,
                snapshot.profile,
                snapshot.windows.len()
            );
        }
    }

    let windows = snapshot
        .windows
        .iter()
        .enumerate()
        .map(|(index, window)| WindowSummary {
            index,
            window_id: &window.window_id,
            pid: window.pid,
            process_name: window.process_name.as_deref(),
            title: &window.title,
            class_name: window.root.class_name.as_deref(),
        })
        .collect::<Vec<_>>();
    println!("{}", serde_json::to_string_pretty(&windows)?);

    let Some(window) = snapshot.windows.get(config.window_index) else {
        bail!(
            "window_index {} is out of range for {} windows",
            config.window_index,
            snapshot.windows.len()
        );
    };
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
            action_id: domain::ActionId::from("remote-session-focus"),
            timeout_ms: 5_000,
            target: target.clone(),
            kind: domain::ActionKind::FocusWindow,
            capture_policy: domain::CapturePolicy::Never,
        },
    )
    .await?;

    if let Some(element_id) = &config.click_element_id {
        let result = perform_action(
            &outbound_tx,
            &mut inbound,
            domain::ActionRequest {
                action_id: domain::ActionId::from("remote-session-click-element"),
                timeout_ms: 5_000,
                target: domain::ActionTarget::Element(domain::ElementLocator {
                    element_id: Some(element_id.clone()),
                    locator: None,
                }),
                kind: domain::ActionKind::ClickElement(domain::ClickOptions {
                    button: domain::MouseButton::Left,
                    clicks: 1,
                }),
                capture_policy: domain::CapturePolicy::Never,
            },
        )
        .await?;
        eprintln!("click_element: {}", result.message);
    }

    if let Some(keys) = config.keys {
        let result = perform_action(
            &outbound_tx,
            &mut inbound,
            domain::ActionRequest {
                action_id: domain::ActionId::from("remote-session-send-keys"),
                timeout_ms: 5_000,
                target: target.clone(),
                kind: domain::ActionKind::SendKeys(domain::SendKeysOptions { keys }),
                capture_policy: domain::CapturePolicy::Never,
            },
        )
        .await?;
        eprintln!("send_keys: {}", result.message);
    }

    if config.dump_tree {
        let tree = read_tree_artifact(
            &config.daemon_addr,
            &outbound_tx,
            &mut inbound,
            target.clone(),
            config.tree_raw,
            config.tree_max_depth,
        )
        .await?;
        println!("{}", serde_json::to_string_pretty(&tree)?);
    }

    if let Some(path) = &config.capture_path {
        let capture_target = if let Some(element_id) = config
            .capture_element_id
            .as_ref()
            .or(config.click_element_id.as_ref())
        {
            domain::ActionTarget::Element(domain::ElementLocator {
                element_id: Some(element_id.clone()),
                locator: None,
            })
        } else {
            target.clone()
        };
        let bytes = capture_region_artifact(
            &config.daemon_addr,
            &outbound_tx,
            &mut inbound,
            capture_target,
            config.capture_format.clone(),
        )
        .await?;
        fs::write(path, bytes)
            .with_context(|| format!("failed to write capture to `{}`", path.display()))?;
        eprintln!("capture_region: {}", path.display());
    }

    Ok(())
}

impl Config {
    fn from_env() -> Result<Self> {
        let daemon_addr = normalize_daemon_addr(
            &env::var("VMUI_DAEMON_ADDR").unwrap_or_else(|_| "127.0.0.1:50051".to_owned()),
        );
        let scope = match env::var("VMUI_REMOTE_SCOPE")
            .unwrap_or_else(|_| "attached_windows".to_owned())
            .trim()
            .to_ascii_lowercase()
            .as_str()
        {
            "desktop" => domain::ObservationScope::Desktop,
            "attached_windows" | "attached" => domain::ObservationScope::AttachedWindows,
            other => bail!("unsupported VMUI_REMOTE_SCOPE `{other}`"),
        };
        let domain_profile = match env::var("VMUI_REMOTE_DOMAIN_PROFILE")
            .unwrap_or_else(|_| "generic".to_owned())
            .trim()
            .to_ascii_lowercase()
            .as_str()
        {
            "generic" => domain::DomainProfile::Generic,
            "onec_enterprise_ui" | "enterprise_ui" => domain::DomainProfile::OnecEnterpriseUi,
            "onec_configurator" | "configurator" => domain::DomainProfile::OnecConfigurator,
            other => bail!("unsupported VMUI_REMOTE_DOMAIN_PROFILE `{other}`"),
        };
        let target_filter = domain::WindowLocator {
            window_id: env::var("VMUI_REMOTE_WINDOW_ID")
                .ok()
                .filter(|value| !value.is_empty())
                .map(domain::WindowId::from),
            title: env::var("VMUI_REMOTE_TITLE")
                .ok()
                .filter(|value| !value.is_empty()),
            pid: env::var("VMUI_REMOTE_PID")
                .ok()
                .filter(|value| !value.is_empty())
                .map(|value| value.parse())
                .transpose()
                .context("invalid VMUI_REMOTE_PID")?,
            process_name: env::var("VMUI_REMOTE_PROCESS_NAME")
                .ok()
                .filter(|value| !value.is_empty()),
            class_name: env::var("VMUI_REMOTE_CLASS_NAME")
                .ok()
                .filter(|value| !value.is_empty()),
        };
        let profile = domain::SessionProfile {
            observation_scope: scope,
            domain_profile,
            target_filter: (!target_filter.is_empty()).then_some(target_filter),
        }
        .normalized();
        let keys = env::args()
            .nth(1)
            .or_else(|| env::var("VMUI_REMOTE_KEYS").ok())
            .filter(|value| !value.is_empty());
        let window_index = env::var("VMUI_REMOTE_WINDOW_INDEX")
            .ok()
            .filter(|value| !value.is_empty())
            .map(|value| value.parse())
            .transpose()
            .context("invalid VMUI_REMOTE_WINDOW_INDEX")?
            .unwrap_or(0usize);
        let expect_window_count = env::var("VMUI_REMOTE_EXPECT_WINDOW_COUNT")
            .ok()
            .filter(|value| !value.is_empty())
            .map(|value| value.parse())
            .transpose()
            .context("invalid VMUI_REMOTE_EXPECT_WINDOW_COUNT")?;
        let click_element_id = env::var("VMUI_REMOTE_CLICK_ELEMENT_ID")
            .ok()
            .filter(|value| !value.is_empty())
            .map(domain::ElementId::from);
        let dump_tree = parse_bool_env("VMUI_REMOTE_GET_TREE");
        let tree_raw = parse_bool_env("VMUI_REMOTE_TREE_RAW");
        let tree_max_depth = env::var("VMUI_REMOTE_TREE_MAX_DEPTH")
            .ok()
            .filter(|value| !value.is_empty())
            .map(|value| value.parse())
            .transpose()
            .context("invalid VMUI_REMOTE_TREE_MAX_DEPTH")?;
        let capture_path = env::var("VMUI_REMOTE_CAPTURE_PATH")
            .ok()
            .filter(|value| !value.is_empty())
            .map(PathBuf::from);
        let capture_element_id = env::var("VMUI_REMOTE_CAPTURE_ELEMENT_ID")
            .ok()
            .filter(|value| !value.is_empty())
            .map(domain::ElementId::from);
        let capture_format = match env::var("VMUI_REMOTE_CAPTURE_FORMAT")
            .unwrap_or_else(|_| "png".to_owned())
            .trim()
            .to_ascii_lowercase()
            .as_str()
        {
            "png" => domain::CaptureFormat::Png,
            "jpeg" | "jpg" => domain::CaptureFormat::Jpeg,
            other => bail!("unsupported VMUI_REMOTE_CAPTURE_FORMAT `{other}`"),
        };

        if matches!(profile.observation_scope, domain::ObservationScope::AttachedWindows)
            && profile.target_filter.is_none()
        {
            bail!("attached_windows scope requires at least one VMUI_REMOTE_* filter");
        }

        Ok(Self {
            daemon_addr,
            profile,
            keys,
            window_index,
            expect_window_count,
            click_element_id,
            capture_element_id,
            dump_tree,
            tree_raw,
            tree_max_depth,
            capture_path,
            capture_format,
        })
    }
}

fn normalize_daemon_addr(raw: &str) -> String {
    let raw = raw.trim();
    if raw.starts_with("http://") || raw.starts_with("https://") {
        raw.to_owned()
    } else {
        format!("http://{raw}")
    }
}

fn parse_bool_env(name: &str) -> bool {
    env::var(name)
        .ok()
        .map(|value| matches!(value.trim().to_ascii_lowercase().as_str(), "1" | "true" | "yes"))
        .unwrap_or(false)
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

async fn read_tree_artifact(
    daemon_addr: &str,
    outbound_tx: &mpsc::Sender<pb::ClientMsg>,
    inbound: &mut tonic::Streaming<pb::ServerMsg>,
    target: domain::ActionTarget,
    raw: bool,
    max_depth: Option<u32>,
) -> Result<Value> {
    let result = perform_action(
        outbound_tx,
        inbound,
        domain::ActionRequest {
            action_id: domain::ActionId::from("remote-session-get-tree"),
            timeout_ms: 15_000,
            target,
            kind: domain::ActionKind::GetTree(domain::TreeRequest { raw, max_depth }),
            capture_policy: domain::CapturePolicy::Never,
        },
    )
    .await?;

    let artifact = result
        .artifacts
        .first()
        .context("get_tree did not produce an artifact")?;
    read_json_artifact(daemon_addr, &artifact.artifact_id).await
}

async fn capture_region_artifact(
    daemon_addr: &str,
    outbound_tx: &mpsc::Sender<pb::ClientMsg>,
    inbound: &mut tonic::Streaming<pb::ServerMsg>,
    target: domain::ActionTarget,
    format: domain::CaptureFormat,
) -> Result<Vec<u8>> {
    let result = perform_action(
        outbound_tx,
        inbound,
        domain::ActionRequest {
            action_id: domain::ActionId::from("remote-session-capture-region"),
            timeout_ms: 15_000,
            target,
            kind: domain::ActionKind::CaptureRegion(domain::CaptureOptions { format }),
            capture_policy: domain::CapturePolicy::Never,
        },
    )
    .await?;
    let artifact = result
        .artifacts
        .first()
        .context("capture_region did not produce an artifact")?;
    read_artifact_bytes(daemon_addr, &artifact.artifact_id).await
}

async fn read_json_artifact(daemon_addr: &str, artifact_id: &domain::ArtifactId) -> Result<Value> {
    let bytes = read_artifact_bytes(daemon_addr, artifact_id).await?;
    serde_json::from_slice(&bytes)
        .with_context(|| format!("failed to decode artifact `{artifact_id}` as JSON"))
}

async fn read_artifact_bytes(daemon_addr: &str, artifact_id: &domain::ArtifactId) -> Result<Vec<u8>> {
    let channel = Endpoint::from_shared(daemon_addr.to_owned())
        .context("invalid VMUI_DAEMON_ADDR")?
        .connect()
        .await
        .with_context(|| format!("failed to connect to daemon at `{daemon_addr}`"))?;
    let mut client = pb::ui_agent_client::UiAgentClient::new(channel);
    let response = client
        .read_artifact(pb::ReadArtifactRequest {
            artifact_id: artifact_id.to_string(),
        })
        .await
        .with_context(|| format!("failed to read artifact `{artifact_id}`"))?;
    let mut stream = response.into_inner();
    let mut bytes = Vec::new();
    while let Some(chunk) = stream
        .message()
        .await
        .with_context(|| format!("failed to stream artifact `{artifact_id}`"))?
    {
        bytes.extend_from_slice(&chunk.data);
    }
    Ok(bytes)
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

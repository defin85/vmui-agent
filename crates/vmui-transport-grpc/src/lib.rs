use chrono::{DateTime, TimeZone, Utc};
use serde::{de::DeserializeOwned, Serialize};
use thiserror::Error;
use vmui_protocol as domain;

pub mod pb {
    tonic::include_proto!("vmui.v1");
}

#[derive(Debug, Error)]
pub enum ConvertError {
    #[error("missing payload in stream message")]
    MissingPayload,
    #[error("missing required field `{0}`")]
    MissingField(&'static str),
    #[error("invalid enum value for `{field}`: {value}")]
    InvalidEnumValue { field: &'static str, value: i32 },
    #[error("unsupported action kind `{0}`")]
    UnsupportedActionKind(String),
    #[error("unsupported action status `{0}`")]
    UnsupportedActionStatus(String),
    #[error("unsupported diff op `{0}`")]
    UnsupportedDiffOp(String),
    #[error("invalid json in `{field}`: {source}")]
    InvalidJson {
        field: &'static str,
        source: serde_json::Error,
    },
    #[error("invalid timestamp millis `{0}`")]
    InvalidTimestamp(i64),
}

impl TryFrom<pb::SessionMode> for domain::SessionMode {
    type Error = ConvertError;

    fn try_from(value: pb::SessionMode) -> Result<Self, Self::Error> {
        match value {
            pb::SessionMode::EnterpriseUi => Ok(Self::EnterpriseUi),
            pb::SessionMode::Configurator => Ok(Self::Configurator),
            pb::SessionMode::Unspecified => Err(ConvertError::InvalidEnumValue {
                field: "session_mode",
                value: value as i32,
            }),
        }
    }
}

impl From<domain::SessionMode> for pb::SessionMode {
    fn from(value: domain::SessionMode) -> Self {
        match value {
            domain::SessionMode::EnterpriseUi => Self::EnterpriseUi,
            domain::SessionMode::Configurator => Self::Configurator,
        }
    }
}

impl From<domain::CapturePolicy> for pb::CapturePolicy {
    fn from(value: domain::CapturePolicy) -> Self {
        match value {
            domain::CapturePolicy::Never => Self::Never,
            domain::CapturePolicy::OnFailure => Self::OnFailure,
            domain::CapturePolicy::Always => Self::Always,
        }
    }
}

impl TryFrom<pb::Hello> for domain::Hello {
    type Error = ConvertError;

    fn try_from(value: pb::Hello) -> Result<Self, Self::Error> {
        Ok(Self {
            client_name: value.client_name,
            client_version: value.client_version,
            requested_mode: decode_session_mode(value.requested_mode)?,
        })
    }
}

impl From<domain::HelloAck> for pb::HelloAck {
    fn from(value: domain::HelloAck) -> Self {
        Self {
            session_id: value.session_id.into_inner(),
            server_version: value.server_version,
            backend_id: value.backend_id,
            capabilities: value.capabilities,
            negotiated_mode: pb::SessionMode::from(value.negotiated_mode) as i32,
        }
    }
}

impl TryFrom<pb::HelloAck> for domain::HelloAck {
    type Error = ConvertError;

    fn try_from(value: pb::HelloAck) -> Result<Self, Self::Error> {
        Ok(Self {
            session_id: value.session_id.into(),
            server_version: value.server_version,
            backend_id: value.backend_id,
            capabilities: value.capabilities,
            negotiated_mode: decode_session_mode(value.negotiated_mode)?,
        })
    }
}

impl TryFrom<pb::Subscribe> for domain::Subscribe {
    type Error = ConvertError;

    fn try_from(value: pb::Subscribe) -> Result<Self, Self::Error> {
        Ok(Self {
            include_initial_snapshot: value.include_initial_snapshot,
            include_diff_stream: value.include_diff_stream,
            shallow: value.shallow,
        })
    }
}

impl TryFrom<pb::ReadArtifactRequest> for domain::ReadArtifactRequest {
    type Error = ConvertError;

    fn try_from(value: pb::ReadArtifactRequest) -> Result<Self, Self::Error> {
        Ok(Self {
            artifact_id: value.artifact_id.into(),
        })
    }
}

impl From<domain::ReadArtifactRequest> for pb::ReadArtifactRequest {
    fn from(value: domain::ReadArtifactRequest) -> Self {
        Self {
            artifact_id: value.artifact_id.into_inner(),
        }
    }
}

impl From<domain::ArtifactDescriptor> for pb::ArtifactReady {
    fn from(value: domain::ArtifactDescriptor) -> Self {
        Self {
            artifact_id: value.artifact_id.into_inner(),
            kind: value.kind,
            mime_type: value.mime_type,
            size_bytes: value.size_bytes,
        }
    }
}

impl TryFrom<pb::ArtifactReady> for domain::ArtifactDescriptor {
    type Error = ConvertError;

    fn try_from(value: pb::ArtifactReady) -> Result<Self, Self::Error> {
        Ok(Self {
            artifact_id: value.artifact_id.into(),
            kind: value.kind,
            mime_type: value.mime_type,
            size_bytes: value.size_bytes,
        })
    }
}

impl From<domain::WarningEvent> for pb::Warning {
    fn from(value: domain::WarningEvent) -> Self {
        Self {
            code: value.code,
            message: value.message,
        }
    }
}

impl TryFrom<pb::Warning> for domain::WarningEvent {
    type Error = ConvertError;

    fn try_from(value: pb::Warning) -> Result<Self, Self::Error> {
        Ok(Self {
            code: value.code,
            message: value.message,
        })
    }
}

impl TryFrom<pb::ClientMsg> for domain::ClientMessage {
    type Error = ConvertError;

    fn try_from(value: pb::ClientMsg) -> Result<Self, Self::Error> {
        match value.payload.ok_or(ConvertError::MissingPayload)? {
            pb::client_msg::Payload::Hello(hello) => Ok(Self::Hello(hello.try_into()?)),
            pb::client_msg::Payload::Subscribe(subscribe) => {
                Ok(Self::Subscribe(subscribe.try_into()?))
            }
            pb::client_msg::Payload::ActionRequest(action_request) => {
                Ok(Self::ActionRequest(action_request.try_into()?))
            }
            pb::client_msg::Payload::ReadArtifact(read_artifact) => {
                Ok(Self::ReadArtifact(read_artifact.try_into()?))
            }
            pb::client_msg::Payload::Ping(_) => Ok(Self::Ping),
        }
    }
}

impl From<domain::ServerMessage> for pb::ServerMsg {
    fn from(value: domain::ServerMessage) -> Self {
        let payload = match value {
            domain::ServerMessage::HelloAck(hello_ack) => {
                pb::server_msg::Payload::HelloAck(hello_ack.into())
            }
            domain::ServerMessage::InitialSnapshot(snapshot) => {
                pb::server_msg::Payload::InitialSnapshot(snapshot.into())
            }
            domain::ServerMessage::DiffBatch(diff_batch) => {
                pb::server_msg::Payload::DiffBatch(diff_batch.into())
            }
            domain::ServerMessage::ActionResult(action_result) => {
                pb::server_msg::Payload::ActionResult(action_result.into())
            }
            domain::ServerMessage::ArtifactReady(artifact) => {
                pb::server_msg::Payload::ArtifactReady(artifact.into())
            }
            domain::ServerMessage::Warning(warning) => {
                pb::server_msg::Payload::Warning(warning.into())
            }
            domain::ServerMessage::Pong => pb::server_msg::Payload::Pong(pb::Empty {}),
        };
        Self {
            payload: Some(payload),
        }
    }
}

impl TryFrom<pb::ServerMsg> for domain::ServerMessage {
    type Error = ConvertError;

    fn try_from(value: pb::ServerMsg) -> Result<Self, Self::Error> {
        match value.payload.ok_or(ConvertError::MissingPayload)? {
            pb::server_msg::Payload::HelloAck(hello_ack) => {
                Ok(Self::HelloAck(hello_ack.try_into()?))
            }
            pb::server_msg::Payload::InitialSnapshot(snapshot) => {
                Ok(Self::InitialSnapshot(snapshot.try_into()?))
            }
            pb::server_msg::Payload::DiffBatch(diff_batch) => {
                Ok(Self::DiffBatch(diff_batch.try_into()?))
            }
            pb::server_msg::Payload::ActionResult(action_result) => {
                Ok(Self::ActionResult(action_result.try_into()?))
            }
            pb::server_msg::Payload::ArtifactReady(artifact) => {
                Ok(Self::ArtifactReady(artifact.try_into()?))
            }
            pb::server_msg::Payload::Warning(warning) => Ok(Self::Warning(warning.try_into()?)),
            pb::server_msg::Payload::Pong(_) => Ok(Self::Pong),
        }
    }
}

impl From<domain::UiSnapshot> for pb::InitialSnapshot {
    fn from(value: domain::UiSnapshot) -> Self {
        Self {
            session_id: value.session_id.into_inner(),
            rev: value.rev,
            mode: pb::SessionMode::from(value.mode) as i32,
            captured_at_unix_ms: value.captured_at.timestamp_millis(),
            windows: value.windows.into_iter().map(Into::into).collect(),
        }
    }
}

impl TryFrom<pb::InitialSnapshot> for domain::UiSnapshot {
    type Error = ConvertError;

    fn try_from(value: pb::InitialSnapshot) -> Result<Self, Self::Error> {
        Ok(Self {
            session_id: value.session_id.into(),
            rev: value.rev,
            mode: decode_session_mode(value.mode)?,
            captured_at: utc_from_millis(value.captured_at_unix_ms)?,
            windows: value
                .windows
                .into_iter()
                .map(TryInto::try_into)
                .collect::<Result<Vec<_>, _>>()?,
        })
    }
}

impl From<domain::WindowState> for pb::WindowNode {
    fn from(value: domain::WindowState) -> Self {
        Self {
            window_id: value.window_id.into_inner(),
            pid: value.pid,
            process_name: value.process_name.unwrap_or_default(),
            title: value.title,
            bounds: Some(value.bounds.into()),
            root: Some(value.root.into()),
            backend: encode_backend_kind(value.backend),
            confidence: value.confidence,
        }
    }
}

impl TryFrom<pb::WindowNode> for domain::WindowState {
    type Error = ConvertError;

    fn try_from(value: pb::WindowNode) -> Result<Self, Self::Error> {
        Ok(Self {
            window_id: value.window_id.into(),
            pid: value.pid,
            process_name: if value.process_name.is_empty() {
                None
            } else {
                Some(value.process_name)
            },
            title: value.title,
            bounds: value
                .bounds
                .ok_or(ConvertError::MissingField("window.bounds"))?
                .into(),
            backend: decode_backend_kind(&value.backend)?,
            confidence: value.confidence,
            root: value
                .root
                .ok_or(ConvertError::MissingField("window.root"))?
                .try_into()?,
        })
    }
}

impl From<domain::Rect> for pb::Bounds {
    fn from(value: domain::Rect) -> Self {
        Self {
            left: value.left,
            top: value.top,
            width: value.width,
            height: value.height,
        }
    }
}

impl From<pb::Bounds> for domain::Rect {
    fn from(value: pb::Bounds) -> Self {
        Self {
            left: value.left,
            top: value.top,
            width: value.width,
            height: value.height,
        }
    }
}

impl From<domain::ElementStates> for pb::ElementStates {
    fn from(value: domain::ElementStates) -> Self {
        Self {
            enabled: value.enabled,
            visible: value.visible,
            focused: value.focused,
            selected: value.selected,
            expanded: value.expanded,
            toggled: value.toggled,
        }
    }
}

impl From<pb::ElementStates> for domain::ElementStates {
    fn from(value: pb::ElementStates) -> Self {
        Self {
            enabled: value.enabled,
            visible: value.visible,
            focused: value.focused,
            selected: value.selected,
            expanded: value.expanded,
            toggled: value.toggled,
        }
    }
}

impl From<domain::LocatorSegment> for pb::LocatorSegment {
    fn from(value: domain::LocatorSegment) -> Self {
        Self {
            control_type: value.control_type,
            class_name: value.class_name.unwrap_or_default(),
            automation_id: value.automation_id.unwrap_or_default(),
            name: value.name.unwrap_or_default(),
            sibling_ordinal: value.sibling_ordinal.unwrap_or_default(),
        }
    }
}

impl From<pb::LocatorSegment> for domain::LocatorSegment {
    fn from(value: pb::LocatorSegment) -> Self {
        Self {
            control_type: value.control_type,
            class_name: none_if_empty(value.class_name),
            automation_id: none_if_empty(value.automation_id),
            name: none_if_empty(value.name),
            sibling_ordinal: if value.sibling_ordinal == 0 {
                None
            } else {
                Some(value.sibling_ordinal)
            },
        }
    }
}

impl From<domain::Locator> for pb::Locator {
    fn from(value: domain::Locator) -> Self {
        Self {
            window_fingerprint: value.window_fingerprint,
            path: value.path.into_iter().map(Into::into).collect(),
        }
    }
}

impl From<pb::Locator> for domain::Locator {
    fn from(value: pb::Locator) -> Self {
        Self {
            window_fingerprint: value.window_fingerprint,
            path: value.path.into_iter().map(Into::into).collect(),
        }
    }
}

impl From<domain::ElementNode> for pb::ElementNode {
    fn from(value: domain::ElementNode) -> Self {
        Self {
            element_id: value.element_id.into_inner(),
            parent_id: value
                .parent_id
                .map(|id| id.into_inner())
                .unwrap_or_default(),
            backend: encode_backend_kind(value.backend),
            control_type: value.control_type,
            class_name: value.class_name.unwrap_or_default(),
            name: value.name.unwrap_or_default(),
            automation_id: value.automation_id.unwrap_or_default(),
            native_window_handle: value.native_window_handle.unwrap_or_default(),
            bounds: Some(value.bounds.into()),
            locator: Some(value.locator.into()),
            properties: value
                .properties
                .into_iter()
                .map(|(key, value)| pb::Property {
                    key,
                    value_json: encode_json(&value).expect("property values must serialize"),
                })
                .collect(),
            states: Some(value.states.into()),
            children: value.children.into_iter().map(Into::into).collect(),
            confidence: value.confidence,
        }
    }
}

impl TryFrom<pb::ElementNode> for domain::ElementNode {
    type Error = ConvertError;

    fn try_from(value: pb::ElementNode) -> Result<Self, Self::Error> {
        Ok(Self {
            element_id: value.element_id.into(),
            parent_id: none_if_empty(value.parent_id).map(Into::into),
            backend: decode_backend_kind(&value.backend)?,
            control_type: value.control_type,
            class_name: none_if_empty(value.class_name),
            name: none_if_empty(value.name),
            automation_id: none_if_empty(value.automation_id),
            native_window_handle: if value.native_window_handle == 0 {
                None
            } else {
                Some(value.native_window_handle)
            },
            bounds: value
                .bounds
                .ok_or(ConvertError::MissingField("element.bounds"))?
                .into(),
            locator: value
                .locator
                .ok_or(ConvertError::MissingField("element.locator"))?
                .into(),
            properties: value
                .properties
                .into_iter()
                .map(|property| {
                    Ok((
                        property.key,
                        decode_json(&property.value_json, "property.value_json")?,
                    ))
                })
                .collect::<Result<_, ConvertError>>()?,
            states: value.states.unwrap_or_default().into(),
            children: value
                .children
                .into_iter()
                .map(TryInto::try_into)
                .collect::<Result<Vec<_>, _>>()?,
            confidence: value.confidence,
        })
    }
}

impl From<domain::UiDiffBatch> for pb::DiffBatch {
    fn from(value: domain::UiDiffBatch) -> Self {
        Self {
            base_rev: value.base_rev,
            new_rev: value.new_rev,
            emitted_at_unix_ms: value.emitted_at.timestamp_millis(),
            ops: value.ops.into_iter().map(Into::into).collect(),
        }
    }
}

impl TryFrom<pb::DiffBatch> for domain::UiDiffBatch {
    type Error = ConvertError;

    fn try_from(value: pb::DiffBatch) -> Result<Self, Self::Error> {
        Ok(Self {
            base_rev: value.base_rev,
            new_rev: value.new_rev,
            emitted_at: utc_from_millis(value.emitted_at_unix_ms)?,
            ops: value
                .ops
                .into_iter()
                .map(TryInto::try_into)
                .collect::<Result<Vec<_>, _>>()?,
        })
    }
}

impl From<domain::DiffOp> for pb::DiffOp {
    fn from(value: domain::DiffOp) -> Self {
        match value {
            domain::DiffOp::WindowAdded { window } => Self {
                op: "window_added".to_owned(),
                element_id: String::new(),
                parent_id: String::new(),
                field: String::new(),
                value_json: String::new(),
                node: None,
                window: Some(window.into()),
                window_id: String::new(),
                reason: String::new(),
            },
            domain::DiffOp::WindowRemoved { window_id } => Self {
                op: "window_removed".to_owned(),
                element_id: String::new(),
                parent_id: String::new(),
                field: String::new(),
                value_json: String::new(),
                node: None,
                window: None,
                window_id: window_id.into_inner(),
                reason: String::new(),
            },
            domain::DiffOp::NodeAdded { parent_id, node } => Self {
                op: "node_added".to_owned(),
                element_id: String::new(),
                parent_id: parent_id.into_inner(),
                field: String::new(),
                value_json: String::new(),
                node: Some(node.into()),
                window: None,
                window_id: String::new(),
                reason: String::new(),
            },
            domain::DiffOp::NodeRemoved { element_id } => Self {
                op: "node_removed".to_owned(),
                element_id: element_id.into_inner(),
                parent_id: String::new(),
                field: String::new(),
                value_json: String::new(),
                node: None,
                window: None,
                window_id: String::new(),
                reason: String::new(),
            },
            domain::DiffOp::NodeReplaced { element_id, node } => Self {
                op: "node_replaced".to_owned(),
                element_id: element_id.into_inner(),
                parent_id: String::new(),
                field: String::new(),
                value_json: String::new(),
                node: Some(node.into()),
                window: None,
                window_id: String::new(),
                reason: String::new(),
            },
            domain::DiffOp::PropertyChanged {
                element_id,
                field,
                value,
            } => Self {
                op: "property_changed".to_owned(),
                element_id: element_id.into_inner(),
                parent_id: String::new(),
                field,
                value_json: encode_json(&value).expect("property change must serialize"),
                node: None,
                window: None,
                window_id: String::new(),
                reason: String::new(),
            },
            domain::DiffOp::FocusChanged {
                window_id,
                element_id,
            } => Self {
                op: "focus_changed".to_owned(),
                element_id: element_id.map(|id| id.into_inner()).unwrap_or_default(),
                parent_id: String::new(),
                field: String::new(),
                value_json: String::new(),
                node: None,
                window: None,
                window_id: window_id.into_inner(),
                reason: String::new(),
            },
            domain::DiffOp::SnapshotResync { reason } => Self {
                op: "snapshot_resync".to_owned(),
                element_id: String::new(),
                parent_id: String::new(),
                field: String::new(),
                value_json: String::new(),
                node: None,
                window: None,
                window_id: String::new(),
                reason,
            },
        }
    }
}

impl TryFrom<pb::DiffOp> for domain::DiffOp {
    type Error = ConvertError;

    fn try_from(value: pb::DiffOp) -> Result<Self, Self::Error> {
        match value.op.as_str() {
            "window_added" => Ok(Self::WindowAdded {
                window: value
                    .window
                    .ok_or(ConvertError::MissingField("diff.window"))?
                    .try_into()?,
            }),
            "window_removed" => Ok(Self::WindowRemoved {
                window_id: value.window_id.into(),
            }),
            "node_added" => Ok(Self::NodeAdded {
                parent_id: value.parent_id.into(),
                node: value
                    .node
                    .ok_or(ConvertError::MissingField("diff.node"))?
                    .try_into()?,
            }),
            "node_removed" => Ok(Self::NodeRemoved {
                element_id: value.element_id.into(),
            }),
            "node_replaced" => Ok(Self::NodeReplaced {
                element_id: value.element_id.into(),
                node: value
                    .node
                    .ok_or(ConvertError::MissingField("diff.node"))?
                    .try_into()?,
            }),
            "property_changed" => Ok(Self::PropertyChanged {
                element_id: value.element_id.into(),
                field: value.field,
                value: decode_json(&value.value_json, "diff.value_json")?,
            }),
            "focus_changed" => Ok(Self::FocusChanged {
                window_id: value.window_id.into(),
                element_id: none_if_empty(value.element_id).map(Into::into),
            }),
            "snapshot_resync" => Ok(Self::SnapshotResync {
                reason: value.reason,
            }),
            other => Err(ConvertError::UnsupportedDiffOp(other.to_owned())),
        }
    }
}

impl TryFrom<pb::ActionRequest> for domain::ActionRequest {
    type Error = ConvertError;

    fn try_from(value: pb::ActionRequest) -> Result<Self, Self::Error> {
        Ok(Self {
            action_id: value.action_id.into(),
            timeout_ms: value.timeout_ms,
            target: decode_json(&value.target_json, "action_request.target_json")?,
            kind: parse_action_kind(&value.kind, &value.payload_json)?,
            capture_policy: decode_capture_policy(value.capture_policy)?,
        })
    }
}

impl From<domain::ActionResult> for pb::ActionResult {
    fn from(value: domain::ActionResult) -> Self {
        Self {
            action_id: value.action_id.into_inner(),
            ok: value.ok,
            status: match value.status {
                domain::ActionStatus::Completed => "completed",
                domain::ActionStatus::Failed => "failed",
                domain::ActionStatus::TimedOut => "timed_out",
                domain::ActionStatus::Unsupported => "unsupported",
            }
            .to_owned(),
            message: value.message,
            artifacts: value.artifacts.into_iter().map(Into::into).collect(),
        }
    }
}

impl From<domain::ArtifactChunk> for pb::ArtifactChunk {
    fn from(value: domain::ArtifactChunk) -> Self {
        Self {
            artifact_id: value.artifact_id.into_inner(),
            data: value.data,
        }
    }
}

impl TryFrom<pb::ActionResult> for domain::ActionResult {
    type Error = ConvertError;

    fn try_from(value: pb::ActionResult) -> Result<Self, Self::Error> {
        Ok(Self {
            action_id: value.action_id.into(),
            ok: value.ok,
            status: decode_action_status(&value.status)?,
            message: value.message,
            artifacts: value
                .artifacts
                .into_iter()
                .map(TryInto::try_into)
                .collect::<Result<Vec<_>, _>>()?,
        })
    }
}

fn parse_action_kind(kind: &str, payload_json: &str) -> Result<domain::ActionKind, ConvertError> {
    match kind {
        "list_windows" => Ok(domain::ActionKind::ListWindows),
        "get_tree" => Ok(domain::ActionKind::GetTree(decode_json(
            payload_json,
            "action_request.payload_json",
        )?)),
        "get_runtime_status" => Ok(domain::ActionKind::GetRuntimeStatus(decode_json(
            payload_json,
            "action_request.payload_json",
        )?)),
        "focus_window" => Ok(domain::ActionKind::FocusWindow),
        "click_element" => Ok(domain::ActionKind::ClickElement(decode_json(
            payload_json,
            "action_request.payload_json",
        )?)),
        "set_value" => Ok(domain::ActionKind::SetValue(decode_json(
            payload_json,
            "action_request.payload_json",
        )?)),
        "invoke" => Ok(domain::ActionKind::Invoke),
        "send_keys" => Ok(domain::ActionKind::SendKeys(decode_json(
            payload_json,
            "action_request.payload_json",
        )?)),
        "wait_for" => Ok(domain::ActionKind::WaitFor(decode_json(
            payload_json,
            "action_request.payload_json",
        )?)),
        "capture_region" => Ok(domain::ActionKind::CaptureRegion(decode_json(
            payload_json,
            "action_request.payload_json",
        )?)),
        "ocr_region" => Ok(domain::ActionKind::OcrRegion(decode_json(
            payload_json,
            "action_request.payload_json",
        )?)),
        "write_artifact" => Ok(domain::ActionKind::WriteArtifact(decode_json(
            payload_json,
            "action_request.payload_json",
        )?)),
        "collect_diagnostic_bundle" => Ok(domain::ActionKind::CollectDiagnosticBundle(
            decode_json(payload_json, "action_request.payload_json")?,
        )),
        other => Err(ConvertError::UnsupportedActionKind(other.to_owned())),
    }
}

fn decode_session_mode(value: i32) -> Result<domain::SessionMode, ConvertError> {
    pb::SessionMode::try_from(value)
        .map_err(|_| ConvertError::InvalidEnumValue {
            field: "session_mode",
            value,
        })?
        .try_into()
}

fn decode_capture_policy(value: i32) -> Result<domain::CapturePolicy, ConvertError> {
    match pb::CapturePolicy::try_from(value).map_err(|_| ConvertError::InvalidEnumValue {
        field: "capture_policy",
        value,
    })? {
        pb::CapturePolicy::Never => Ok(domain::CapturePolicy::Never),
        pb::CapturePolicy::OnFailure => Ok(domain::CapturePolicy::OnFailure),
        pb::CapturePolicy::Always => Ok(domain::CapturePolicy::Always),
        pb::CapturePolicy::Unspecified => Err(ConvertError::InvalidEnumValue {
            field: "capture_policy",
            value,
        }),
    }
}

fn decode_action_status(value: &str) -> Result<domain::ActionStatus, ConvertError> {
    match value {
        "completed" => Ok(domain::ActionStatus::Completed),
        "failed" => Ok(domain::ActionStatus::Failed),
        "timed_out" => Ok(domain::ActionStatus::TimedOut),
        "unsupported" => Ok(domain::ActionStatus::Unsupported),
        other => Err(ConvertError::UnsupportedActionStatus(other.to_owned())),
    }
}

fn encode_backend_kind(value: domain::BackendKind) -> String {
    match value {
        domain::BackendKind::Uia => "uia".to_owned(),
        domain::BackendKind::Msaa => "msaa".to_owned(),
        domain::BackendKind::Ocr => "ocr".to_owned(),
        domain::BackendKind::Mixed => "mixed".to_owned(),
    }
}

fn decode_backend_kind(value: &str) -> Result<domain::BackendKind, ConvertError> {
    match value {
        "uia" => Ok(domain::BackendKind::Uia),
        "msaa" => Ok(domain::BackendKind::Msaa),
        "ocr" => Ok(domain::BackendKind::Ocr),
        "mixed" => Ok(domain::BackendKind::Mixed),
        _ => Err(ConvertError::MissingField("backend")),
    }
}

pub fn encode_action_request(
    value: &domain::ActionRequest,
) -> Result<pb::ActionRequest, ConvertError> {
    let (kind, payload_json) = match &value.kind {
        domain::ActionKind::ListWindows => ("list_windows".to_owned(), String::new()),
        domain::ActionKind::GetTree(payload) => ("get_tree".to_owned(), encode_json(payload)?),
        domain::ActionKind::GetRuntimeStatus(payload) => {
            ("get_runtime_status".to_owned(), encode_json(payload)?)
        }
        domain::ActionKind::FocusWindow => ("focus_window".to_owned(), String::new()),
        domain::ActionKind::ClickElement(payload) => {
            ("click_element".to_owned(), encode_json(payload)?)
        }
        domain::ActionKind::SetValue(payload) => ("set_value".to_owned(), encode_json(payload)?),
        domain::ActionKind::Invoke => ("invoke".to_owned(), String::new()),
        domain::ActionKind::SendKeys(payload) => ("send_keys".to_owned(), encode_json(payload)?),
        domain::ActionKind::WaitFor(payload) => ("wait_for".to_owned(), encode_json(payload)?),
        domain::ActionKind::CaptureRegion(payload) => {
            ("capture_region".to_owned(), encode_json(payload)?)
        }
        domain::ActionKind::OcrRegion(payload) => ("ocr_region".to_owned(), encode_json(payload)?),
        domain::ActionKind::WriteArtifact(payload) => {
            ("write_artifact".to_owned(), encode_json(payload)?)
        }
        domain::ActionKind::CollectDiagnosticBundle(payload) => (
            "collect_diagnostic_bundle".to_owned(),
            encode_json(payload)?,
        ),
    };

    Ok(pb::ActionRequest {
        action_id: value.action_id.to_string(),
        kind,
        target_json: encode_json(&value.target)?,
        payload_json,
        timeout_ms: value.timeout_ms,
        capture_policy: pb::CapturePolicy::from(value.capture_policy.clone()) as i32,
    })
}

fn encode_json<T: Serialize>(value: &T) -> Result<String, ConvertError> {
    serde_json::to_string(value).map_err(|source| ConvertError::InvalidJson {
        field: "json",
        source,
    })
}

fn decode_json<T: DeserializeOwned>(payload: &str, field: &'static str) -> Result<T, ConvertError> {
    let source = if payload.is_empty() { "null" } else { payload };
    serde_json::from_str(source).map_err(|source| ConvertError::InvalidJson { field, source })
}

fn none_if_empty(value: String) -> Option<String> {
    if value.is_empty() {
        None
    } else {
        Some(value)
    }
}

fn utc_from_millis(value: i64) -> Result<DateTime<Utc>, ConvertError> {
    Utc.timestamp_millis_opt(value)
        .single()
        .ok_or(ConvertError::InvalidTimestamp(value))
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use chrono::TimeZone;
    use vmui_protocol::{
        ActionId, ActionKind, ActionRequest, ActionTarget, BackendKind, CapturePolicy,
        DiagnosticBundleOptions, DiagnosticStepVerdict, ElementId, ElementNode, ElementStates,
        Locator, PropertyValue, Rect, RuntimeStatusRequest, SessionId, SessionMode, TreeRequest,
        UiSnapshot, WindowId, WindowState,
    };

    use super::*;

    fn sample_snapshot() -> UiSnapshot {
        UiSnapshot {
            session_id: SessionId::from("sess-1"),
            rev: 7,
            mode: SessionMode::Configurator,
            captured_at: Utc
                .timestamp_millis_opt(1_700_000_000_123)
                .single()
                .unwrap(),
            windows: vec![WindowState {
                window_id: WindowId::from("wnd-1"),
                pid: 7,
                process_name: Some("1cv8c.exe".to_owned()),
                title: "Configurator".to_owned(),
                bounds: Rect {
                    left: 10,
                    top: 20,
                    width: 300,
                    height: 200,
                },
                backend: BackendKind::Mixed,
                confidence: 0.82,
                root: ElementNode {
                    element_id: ElementId::from("elt-1"),
                    parent_id: None,
                    backend: BackendKind::Mixed,
                    control_type: "Window".to_owned(),
                    class_name: Some("V8TopLevelFrame".to_owned()),
                    name: Some("Configurator".to_owned()),
                    automation_id: Some("root".to_owned()),
                    native_window_handle: Some(42),
                    bounds: Rect {
                        left: 10,
                        top: 20,
                        width: 300,
                        height: 200,
                    },
                    locator: Locator {
                        window_fingerprint: "1cv8c.exe:Configurator".to_owned(),
                        path: vec![],
                    },
                    properties: BTreeMap::from([(
                        "role".to_owned(),
                        PropertyValue::String("window".to_owned()),
                    )]),
                    states: ElementStates {
                        enabled: true,
                        visible: true,
                        focused: false,
                        selected: false,
                        expanded: false,
                        toggled: false,
                    },
                    children: vec![],
                    confidence: 0.9,
                },
            }],
        }
    }

    #[test]
    fn snapshot_roundtrip_preserves_foundation_fields() {
        let snapshot = sample_snapshot();
        let proto: pb::InitialSnapshot = snapshot.clone().into();
        let decoded: UiSnapshot = proto.try_into().expect("decode snapshot");

        assert_eq!(decoded, snapshot);
    }

    #[test]
    fn action_request_roundtrip_preserves_capture_policy() {
        let request = ActionRequest {
            action_id: ActionId::from("act-1"),
            timeout_ms: 1_000,
            target: ActionTarget::Desktop,
            kind: ActionKind::GetTree(TreeRequest {
                raw: true,
                max_depth: Some(2),
            }),
            capture_policy: CapturePolicy::OnFailure,
        };

        let proto = encode_action_request(&request).expect("encode request");
        let decoded: ActionRequest = proto.try_into().expect("decode request");

        assert_eq!(decoded, request);
    }

    #[test]
    fn collect_diagnostic_bundle_roundtrip_preserves_baseline_reference() {
        let request = ActionRequest {
            action_id: ActionId::from("diag-1"),
            timeout_ms: 5_000,
            target: ActionTarget::Desktop,
            kind: ActionKind::CollectDiagnosticBundle(DiagnosticBundleOptions {
                step_id: Some("step-42".to_owned()),
                step_label: "Failed login check".to_owned(),
                test_verdict: DiagnosticStepVerdict::Failed,
                note: Some("runner=standard-1c".to_owned()),
                baseline_artifact_id: Some("art-baseline".into()),
                max_tree_depth: Some(3),
            }),
            capture_policy: CapturePolicy::Never,
        };

        let proto = encode_action_request(&request).expect("encode request");
        let decoded: ActionRequest = proto.try_into().expect("decode request");

        assert_eq!(decoded, request);
    }

    #[test]
    fn runtime_status_action_roundtrip_preserves_empty_payload() {
        let request = ActionRequest {
            action_id: ActionId::from("runtime-1"),
            timeout_ms: 500,
            target: ActionTarget::Desktop,
            kind: ActionKind::GetRuntimeStatus(RuntimeStatusRequest::default()),
            capture_policy: CapturePolicy::Never,
        };

        let proto = encode_action_request(&request).expect("encode request");
        let decoded: ActionRequest = proto.try_into().expect("decode request");

        assert_eq!(decoded, request);
    }

    #[test]
    fn server_message_wraps_snapshot_payload() {
        let message = domain::ServerMessage::InitialSnapshot(sample_snapshot());
        let wire: pb::ServerMsg = message.into();

        match wire.payload.expect("payload") {
            pb::server_msg::Payload::InitialSnapshot(snapshot) => {
                assert_eq!(snapshot.rev, 7);
                assert_eq!(snapshot.mode, pb::SessionMode::Configurator as i32);
            }
            other => panic!("unexpected payload: {other:?}"),
        }
    }

    #[test]
    fn client_message_decodes_hello() {
        let wire = pb::ClientMsg {
            payload: Some(pb::client_msg::Payload::Hello(pb::Hello {
                client_name: "test".to_owned(),
                client_version: "0.1.0".to_owned(),
                requested_mode: pb::SessionMode::EnterpriseUi as i32,
            })),
        };

        let decoded = domain::ClientMessage::try_from(wire).expect("decode hello");

        match decoded {
            domain::ClientMessage::Hello(hello) => {
                assert_eq!(hello.client_name, "test");
                assert_eq!(hello.requested_mode, SessionMode::EnterpriseUi);
            }
            other => panic!("unexpected message: {other:?}"),
        }
    }

    #[test]
    fn server_message_decodes_action_result() {
        let wire = pb::ServerMsg {
            payload: Some(pb::server_msg::Payload::ActionResult(pb::ActionResult {
                action_id: "act-1".to_owned(),
                ok: false,
                status: "timed_out".to_owned(),
                message: "expired".to_owned(),
                artifacts: vec![pb::ArtifactReady {
                    artifact_id: "art-1".to_owned(),
                    kind: "runtime-status-json".to_owned(),
                    mime_type: "application/json".to_owned(),
                    size_bytes: 42,
                }],
            })),
        };

        let decoded = domain::ServerMessage::try_from(wire).expect("decode server message");

        match decoded {
            domain::ServerMessage::ActionResult(result) => {
                assert_eq!(result.action_id, ActionId::from("act-1"));
                assert!(!result.ok);
                assert_eq!(result.status, domain::ActionStatus::TimedOut);
                assert_eq!(result.artifacts.len(), 1);
                assert_eq!(result.artifacts[0].kind, "runtime-status-json");
            }
            other => panic!("unexpected message: {other:?}"),
        }
    }
}

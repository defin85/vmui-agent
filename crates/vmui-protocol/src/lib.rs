use std::{collections::BTreeMap, fmt};

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

pub type Revision = u64;

macro_rules! id_type {
    ($name:ident) => {
        #[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq, PartialOrd, Ord, Hash)]
        pub struct $name(pub String);

        impl $name {
            pub fn new(prefix: &str) -> Self {
                Self(format!("{prefix}-{}", Uuid::new_v4()))
            }

            pub fn as_str(&self) -> &str {
                &self.0
            }

            pub fn into_inner(self) -> String {
                self.0
            }
        }

        impl fmt::Display for $name {
            fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                f.write_str(&self.0)
            }
        }

        impl From<String> for $name {
            fn from(value: String) -> Self {
                Self(value)
            }
        }

        impl From<&str> for $name {
            fn from(value: &str) -> Self {
                Self(value.to_owned())
            }
        }
    };
}

id_type!(ActionId);
id_type!(ArtifactId);
id_type!(ElementId);
id_type!(RequestId);
id_type!(SessionId);
id_type!(WindowId);

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum SessionMode {
    EnterpriseUi,
    Configurator,
}

impl Default for SessionMode {
    fn default() -> Self {
        Self::EnterpriseUi
    }
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum BackendKind {
    Uia,
    Msaa,
    Ocr,
    Mixed,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct ClientEnvelope {
    pub request_id: RequestId,
    pub payload: ClientMessage,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "snake_case")]
pub enum ClientMessage {
    Hello(Hello),
    Subscribe(Subscribe),
    ActionRequest(ActionRequest),
    ReadArtifact(ReadArtifactRequest),
    Ping,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "snake_case")]
pub enum ServerMessage {
    HelloAck(HelloAck),
    InitialSnapshot(UiSnapshot),
    DiffBatch(UiDiffBatch),
    ActionResult(ActionResult),
    ArtifactReady(ArtifactDescriptor),
    Warning(WarningEvent),
    Pong,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct Hello {
    pub client_name: String,
    pub client_version: String,
    pub requested_mode: SessionMode,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct HelloAck {
    pub session_id: SessionId,
    pub server_version: String,
    pub backend_id: String,
    pub capabilities: Vec<String>,
    pub negotiated_mode: SessionMode,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct Subscribe {
    pub include_initial_snapshot: bool,
    pub include_diff_stream: bool,
    pub shallow: bool,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct ReadArtifactRequest {
    pub artifact_id: ArtifactId,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct ArtifactChunk {
    pub artifact_id: ArtifactId,
    pub data: Vec<u8>,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct WarningEvent {
    pub code: String,
    pub message: String,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct UiSnapshot {
    pub session_id: SessionId,
    pub rev: Revision,
    pub mode: SessionMode,
    pub captured_at: DateTime<Utc>,
    pub windows: Vec<WindowState>,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct WindowState {
    pub window_id: WindowId,
    pub pid: u32,
    pub process_name: Option<String>,
    pub title: String,
    pub bounds: Rect,
    pub backend: BackendKind,
    pub confidence: f32,
    pub root: ElementNode,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct ElementNode {
    pub element_id: ElementId,
    pub parent_id: Option<ElementId>,
    pub backend: BackendKind,
    pub control_type: String,
    pub class_name: Option<String>,
    pub name: Option<String>,
    pub automation_id: Option<String>,
    pub native_window_handle: Option<u64>,
    pub bounds: Rect,
    pub locator: Locator,
    pub properties: BTreeMap<String, PropertyValue>,
    pub states: ElementStates,
    pub children: Vec<ElementNode>,
    pub confidence: f32,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq, Default)]
pub struct ElementStates {
    pub enabled: bool,
    pub visible: bool,
    pub focused: bool,
    pub selected: bool,
    pub expanded: bool,
    pub toggled: bool,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct Locator {
    pub window_fingerprint: String,
    pub path: Vec<LocatorSegment>,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct LocatorSegment {
    pub control_type: String,
    pub class_name: Option<String>,
    pub automation_id: Option<String>,
    pub name: Option<String>,
    pub sibling_ordinal: Option<u32>,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
#[serde(tag = "kind", content = "value", rename_all = "snake_case")]
pub enum PropertyValue {
    Bool(bool),
    I64(i64),
    F64(f64),
    String(String),
    StringList(Vec<String>),
    Rect(Rect),
    Null,
}

#[derive(Clone, Copy, Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct Rect {
    pub left: i32,
    pub top: i32,
    pub width: i32,
    pub height: i32,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct UiDiffBatch {
    pub base_rev: Revision,
    pub new_rev: Revision,
    pub emitted_at: DateTime<Utc>,
    pub ops: Vec<DiffOp>,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
#[serde(tag = "op", rename_all = "snake_case")]
pub enum DiffOp {
    WindowAdded {
        window: WindowState,
    },
    WindowRemoved {
        window_id: WindowId,
    },
    NodeAdded {
        parent_id: ElementId,
        node: ElementNode,
    },
    NodeRemoved {
        element_id: ElementId,
    },
    NodeReplaced {
        element_id: ElementId,
        node: ElementNode,
    },
    PropertyChanged {
        element_id: ElementId,
        field: String,
        value: PropertyValue,
    },
    FocusChanged {
        window_id: WindowId,
        element_id: Option<ElementId>,
    },
    SnapshotResync {
        reason: String,
    },
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct ActionRequest {
    pub action_id: ActionId,
    pub timeout_ms: u64,
    pub target: ActionTarget,
    pub kind: ActionKind,
    pub capture_policy: CapturePolicy,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum ActionTarget {
    Desktop,
    Window(WindowLocator),
    Element(ElementLocator),
    Region(RegionTarget),
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct WindowLocator {
    pub window_id: Option<WindowId>,
    pub title: Option<String>,
    pub pid: Option<u32>,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct ElementLocator {
    pub element_id: Option<ElementId>,
    pub locator: Option<Locator>,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct RegionTarget {
    pub window_id: Option<WindowId>,
    pub bounds: Rect,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
#[serde(tag = "action", content = "payload", rename_all = "snake_case")]
pub enum ActionKind {
    ListWindows,
    GetTree(TreeRequest),
    FocusWindow,
    ClickElement(ClickOptions),
    SetValue(SetValueOptions),
    Invoke,
    SendKeys(SendKeysOptions),
    WaitFor(WaitForOptions),
    CaptureRegion(CaptureOptions),
    OcrRegion(OcrOptions),
    WriteArtifact(WriteArtifactOptions),
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct TreeRequest {
    pub raw: bool,
    pub max_depth: Option<u32>,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct ClickOptions {
    pub button: MouseButton,
    pub clicks: u8,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum MouseButton {
    Left,
    Right,
    Middle,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct SetValueOptions {
    pub value: String,
    pub clear_first: bool,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct SendKeysOptions {
    pub keys: String,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct WaitForOptions {
    pub condition: WaitCondition,
    pub stable_for_ms: u64,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum WaitCondition {
    Exists,
    Visible,
    Enabled,
    Focused,
    Gone,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct CaptureOptions {
    pub format: CaptureFormat,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum CaptureFormat {
    Png,
    Jpeg,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct OcrOptions {
    pub language_hint: Option<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct WriteArtifactOptions {
    pub kind: String,
    pub note: Option<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum CapturePolicy {
    Never,
    OnFailure,
    Always,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct ActionResult {
    pub action_id: ActionId,
    pub ok: bool,
    pub status: ActionStatus,
    pub message: String,
    pub artifacts: Vec<ArtifactDescriptor>,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ActionStatus {
    Completed,
    Failed,
    TimedOut,
    Unsupported,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct ArtifactDescriptor {
    pub artifact_id: ArtifactId,
    pub kind: String,
    pub mime_type: String,
    pub size_bytes: u64,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn action_request_serializes_roundtrip() {
        let request = ActionRequest {
            action_id: ActionId::new("act"),
            timeout_ms: 5_000,
            target: ActionTarget::Desktop,
            kind: ActionKind::ListWindows,
            capture_policy: CapturePolicy::OnFailure,
        };

        let json = serde_json::to_string(&request).expect("serialize request");
        let decoded: ActionRequest = serde_json::from_str(&json).expect("deserialize request");

        assert_eq!(decoded, request);
    }
}

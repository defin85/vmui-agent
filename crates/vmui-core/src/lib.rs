use std::{
    collections::{BTreeMap, VecDeque},
    fs,
    path::{Path, PathBuf},
};

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use thiserror::Error;
use vmui_protocol::{
    ArtifactDescriptor, ArtifactId, DiffOp, ElementId, ElementNode, PropertyValue, Revision,
    SessionId, SessionMode, UiDiffBatch, UiSnapshot, WindowId, WindowState,
};

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct AgentConfig {
    pub bind_addr: String,
    pub artifact_dir: PathBuf,
    pub default_mode: SessionMode,
}

impl Default for AgentConfig {
    fn default() -> Self {
        Self {
            bind_addr: "127.0.0.1:50051".to_owned(),
            artifact_dir: PathBuf::from("var/artifacts"),
            default_mode: SessionMode::EnterpriseUi,
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct SessionRuntime {
    pub session_id: SessionId,
    pub mode: SessionMode,
    pub backend_id: String,
    pub connected_at: DateTime<Utc>,
    pub subscribed_at: Option<DateTime<Utc>>,
    pub closed_at: Option<DateTime<Utc>>,
    pub shallow: Option<bool>,
    pub last_revision: Option<Revision>,
}

impl SessionRuntime {
    pub fn new(session_id: SessionId, mode: SessionMode, backend_id: impl Into<String>) -> Self {
        Self {
            session_id,
            mode,
            backend_id: backend_id.into(),
            connected_at: Utc::now(),
            subscribed_at: None,
            closed_at: None,
            shallow: None,
            last_revision: None,
        }
    }
}

#[derive(Clone, Debug, Default)]
pub struct UiStateStore {
    snapshot: Option<UiSnapshot>,
    rev: Revision,
    resync_required: Option<String>,
}

impl UiStateStore {
    pub fn revision(&self) -> Revision {
        self.rev
    }

    pub fn snapshot(&self) -> Option<&UiSnapshot> {
        self.snapshot.as_ref()
    }

    pub fn resync_reason(&self) -> Option<&str> {
        self.resync_required.as_deref()
    }

    pub fn replace_snapshot(&mut self, snapshot: UiSnapshot) {
        self.rev = snapshot.rev;
        self.snapshot = Some(snapshot);
        self.resync_required = None;
    }

    pub fn mark_resync_required(&mut self, reason: impl Into<String>) {
        self.resync_required = Some(reason.into());
    }

    pub fn apply_diff(&mut self, diff: &UiDiffBatch) -> Result<(), StateError> {
        if self.snapshot.is_none() {
            return Err(StateError::MissingSnapshot);
        }

        if diff.base_rev != self.rev {
            let reason = format!("expected base rev {}, got {}", self.rev, diff.base_rev);
            self.mark_resync_required(reason.clone());
            return Err(StateError::StaleDiff {
                expected_base_rev: self.rev,
                actual_base_rev: diff.base_rev,
            });
        }

        if diff.new_rev <= diff.base_rev {
            self.mark_resync_required("non-monotonic diff revision");
            return Err(StateError::NonMonotonicDiff {
                current_rev: self.rev,
                next_rev: diff.new_rev,
            });
        }

        if let Some(reason) = diff.ops.iter().find_map(|op| match op {
            DiffOp::SnapshotResync { reason } => Some(reason.clone()),
            _ => None,
        }) {
            self.mark_resync_required(reason);
        }

        let snapshot = self.snapshot.as_mut().expect("snapshot checked above");
        for op in &diff.ops {
            apply_diff_op(snapshot, op)?;
        }

        snapshot.rev = diff.new_rev;
        snapshot.captured_at = diff.emitted_at;
        sort_windows(&mut snapshot.windows);
        self.rev = diff.new_rev;
        Ok(())
    }
}

#[derive(Debug, Error, PartialEq, Eq)]
pub enum StateError {
    #[error("cannot apply diff before initial snapshot")]
    MissingSnapshot,
    #[error("diff base revision mismatch: expected {expected_base_rev}, got {actual_base_rev}")]
    StaleDiff {
        expected_base_rev: Revision,
        actual_base_rev: Revision,
    },
    #[error("diff revision is not monotonic: current {current_rev}, next {next_rev}")]
    NonMonotonicDiff {
        current_rev: Revision,
        next_rev: Revision,
    },
    #[error("diff references unknown window `{0}`")]
    MissingWindow(WindowId),
    #[error("diff references unknown element `{0}`")]
    MissingElement(ElementId),
    #[error("diff contains invalid property update for `{field}`")]
    InvalidPropertyUpdate { field: String },
}

#[derive(Clone, Debug)]
pub struct SessionRecord {
    pub runtime: SessionRuntime,
    pub state: UiStateStore,
    recent_diffs: VecDeque<UiDiffBatch>,
}

const MAX_RECENT_DIFFS: usize = 16;

#[derive(Default)]
pub struct SessionRegistry {
    sessions: BTreeMap<SessionId, SessionRecord>,
}

impl SessionRegistry {
    pub fn open_session(&mut self, runtime: SessionRuntime) {
        self.sessions.insert(
            runtime.session_id.clone(),
            SessionRecord {
                runtime,
                state: UiStateStore::default(),
                recent_diffs: VecDeque::new(),
            },
        );
    }

    pub fn mark_subscribed(
        &mut self,
        session_id: &SessionId,
        shallow: bool,
    ) -> Result<(), SessionError> {
        let record = self
            .sessions
            .get_mut(session_id)
            .ok_or_else(|| SessionError::NotFound(session_id.clone()))?;
        record.runtime.subscribed_at = Some(Utc::now());
        record.runtime.shallow = Some(shallow);
        Ok(())
    }

    pub fn apply_snapshot(
        &mut self,
        session_id: &SessionId,
        snapshot: UiSnapshot,
    ) -> Result<(), SessionError> {
        let record = self
            .sessions
            .get_mut(session_id)
            .ok_or_else(|| SessionError::NotFound(session_id.clone()))?;
        record.runtime.last_revision = Some(snapshot.rev);
        record.state.replace_snapshot(snapshot);
        record.recent_diffs.clear();
        Ok(())
    }

    pub fn apply_diff(
        &mut self,
        session_id: &SessionId,
        diff: &UiDiffBatch,
    ) -> Result<(), SessionError> {
        let record = self
            .sessions
            .get_mut(session_id)
            .ok_or_else(|| SessionError::NotFound(session_id.clone()))?;
        record.state.apply_diff(diff).map_err(SessionError::State)?;
        record.runtime.last_revision = Some(record.state.revision());
        if record.recent_diffs.len() == MAX_RECENT_DIFFS {
            record.recent_diffs.pop_front();
        }
        record.recent_diffs.push_back(diff.clone());
        Ok(())
    }

    pub fn close_session(&mut self, session_id: &SessionId) -> Result<(), SessionError> {
        let record = self
            .sessions
            .get_mut(session_id)
            .ok_or_else(|| SessionError::NotFound(session_id.clone()))?;
        record.runtime.closed_at = Some(Utc::now());
        Ok(())
    }

    pub fn runtime(&self, session_id: &SessionId) -> Option<&SessionRuntime> {
        self.sessions.get(session_id).map(|record| &record.runtime)
    }

    pub fn state(&self, session_id: &SessionId) -> Option<&UiStateStore> {
        self.sessions.get(session_id).map(|record| &record.state)
    }

    pub fn recent_diffs(&self, session_id: &SessionId) -> Option<Vec<UiDiffBatch>> {
        self.sessions
            .get(session_id)
            .map(|record| record.recent_diffs.iter().cloned().collect())
    }

    pub fn len(&self) -> usize {
        self.sessions.len()
    }

    pub fn is_empty(&self) -> bool {
        self.sessions.is_empty()
    }

    pub fn session_ids(&self) -> Vec<SessionId> {
        self.sessions.keys().cloned().collect()
    }
}

#[derive(Debug, Error)]
pub enum SessionError {
    #[error("unknown session `{0}`")]
    NotFound(SessionId),
    #[error(transparent)]
    State(#[from] StateError),
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct ArtifactRecord {
    pub descriptor: ArtifactDescriptor,
    pub session_id: Option<SessionId>,
    pub path: PathBuf,
    pub created_at: DateTime<Utc>,
}

pub struct ArtifactStore {
    root: PathBuf,
    artifacts: BTreeMap<ArtifactId, ArtifactRecord>,
}

impl ArtifactStore {
    pub fn new(root: impl Into<PathBuf>) -> Result<Self, ArtifactError> {
        let root = root.into();
        fs::create_dir_all(&root).map_err(|source| ArtifactError::CreateDir {
            path: root.clone(),
            source,
        })?;
        Ok(Self {
            root,
            artifacts: BTreeMap::new(),
        })
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    pub fn write_bytes(
        &mut self,
        session_id: Option<SessionId>,
        kind: impl Into<String>,
        mime_type: impl Into<String>,
        bytes: &[u8],
    ) -> Result<ArtifactDescriptor, ArtifactError> {
        let kind = kind.into();
        let mime_type = mime_type.into();
        let artifact_id = ArtifactId::new("art");
        let extension = extension_for_mime(&mime_type);
        let path = self.root.join(format!("{}.{}", artifact_id, extension));
        fs::write(&path, bytes).map_err(|source| ArtifactError::WriteFile {
            path: path.clone(),
            source,
        })?;

        let descriptor = ArtifactDescriptor {
            artifact_id: artifact_id.clone(),
            kind,
            mime_type,
            size_bytes: bytes.len() as u64,
        };
        let record = ArtifactRecord {
            descriptor: descriptor.clone(),
            session_id,
            path,
            created_at: Utc::now(),
        };
        self.artifacts.insert(artifact_id, record);
        Ok(descriptor)
    }

    pub fn descriptor(&self, artifact_id: &ArtifactId) -> Option<&ArtifactDescriptor> {
        self.artifacts
            .get(artifact_id)
            .map(|record| &record.descriptor)
    }

    pub fn read_bytes(&self, artifact_id: &ArtifactId) -> Result<Vec<u8>, ArtifactError> {
        let record = self
            .artifacts
            .get(artifact_id)
            .ok_or_else(|| ArtifactError::NotFound(artifact_id.clone()))?;
        fs::read(&record.path).map_err(|source| ArtifactError::ReadFile {
            path: record.path.clone(),
            source,
        })
    }
}

#[derive(Debug, Error)]
pub enum ArtifactError {
    #[error("failed to create artifact dir `{path}`: {source}")]
    CreateDir {
        path: PathBuf,
        source: std::io::Error,
    },
    #[error("artifact `{0}` was not found")]
    NotFound(ArtifactId),
    #[error("failed to write artifact file `{path}`: {source}")]
    WriteFile {
        path: PathBuf,
        source: std::io::Error,
    },
    #[error("failed to read artifact file `{path}`: {source}")]
    ReadFile {
        path: PathBuf,
        source: std::io::Error,
    },
}

pub struct AgentRuntimeState {
    pub sessions: SessionRegistry,
    pub artifacts: ArtifactStore,
}

impl AgentRuntimeState {
    pub fn new(config: &AgentConfig) -> Result<Self, ArtifactError> {
        Ok(Self {
            sessions: SessionRegistry::default(),
            artifacts: ArtifactStore::new(config.artifact_dir.clone())?,
        })
    }
}

fn extension_for_mime(mime_type: &str) -> &'static str {
    match mime_type {
        "application/json" => "json",
        "image/jpeg" => "jpg",
        "image/png" => "png",
        "text/plain" => "txt",
        _ => "bin",
    }
}

fn apply_diff_op(snapshot: &mut UiSnapshot, op: &DiffOp) -> Result<(), StateError> {
    match op {
        DiffOp::WindowAdded { window } => upsert_window(snapshot, window.clone()),
        DiffOp::WindowRemoved { window_id } => remove_window(snapshot, window_id)?,
        DiffOp::NodeAdded { parent_id, node } => add_node(snapshot, parent_id, node.clone())?,
        DiffOp::NodeRemoved { element_id } => remove_node(snapshot, element_id)?,
        DiffOp::NodeReplaced { element_id, node } => {
            replace_node(snapshot, element_id, node.clone())?
        }
        DiffOp::PropertyChanged {
            element_id,
            field,
            value,
        } => apply_property_change(snapshot, element_id, field, value.clone())?,
        DiffOp::FocusChanged {
            window_id,
            element_id,
        } => apply_focus_change(snapshot, window_id, element_id.as_ref())?,
        DiffOp::SnapshotResync { .. } => {}
    }

    Ok(())
}

fn upsert_window(snapshot: &mut UiSnapshot, window: WindowState) {
    if let Some(existing) = snapshot
        .windows
        .iter_mut()
        .find(|candidate| candidate.window_id == window.window_id)
    {
        *existing = window;
    } else {
        snapshot.windows.push(window);
    }
}

fn remove_window(snapshot: &mut UiSnapshot, window_id: &WindowId) -> Result<(), StateError> {
    let Some(index) = snapshot
        .windows
        .iter()
        .position(|window| &window.window_id == window_id)
    else {
        return Err(StateError::MissingWindow(window_id.clone()));
    };

    snapshot.windows.remove(index);
    Ok(())
}

fn add_node(
    snapshot: &mut UiSnapshot,
    parent_id: &ElementId,
    mut node: ElementNode,
) -> Result<(), StateError> {
    let parent = find_node_mut(snapshot, parent_id)
        .ok_or_else(|| StateError::MissingElement(parent_id.clone()))?;
    reparent_subtree(&mut node, Some(&parent.element_id));

    if let Some(existing) = parent
        .children
        .iter_mut()
        .find(|candidate| candidate.element_id == node.element_id)
    {
        *existing = node;
    } else {
        parent.children.push(node);
    }

    Ok(())
}

fn remove_node(snapshot: &mut UiSnapshot, element_id: &ElementId) -> Result<(), StateError> {
    for window in &mut snapshot.windows {
        if remove_node_from_children(&mut window.root.children, element_id) {
            return Ok(());
        }
    }

    Err(StateError::MissingElement(element_id.clone()))
}

fn replace_node(
    snapshot: &mut UiSnapshot,
    element_id: &ElementId,
    mut node: ElementNode,
) -> Result<(), StateError> {
    for window in &mut snapshot.windows {
        if &window.root.element_id == element_id {
            reparent_subtree(&mut node, None);
            window.root = node;
            return Ok(());
        }

        if replace_node_in_tree(&mut window.root, element_id, &mut node) {
            return Ok(());
        }
    }

    Err(StateError::MissingElement(element_id.clone()))
}

fn apply_property_change(
    snapshot: &mut UiSnapshot,
    element_id: &ElementId,
    field: &str,
    value: PropertyValue,
) -> Result<(), StateError> {
    let node = find_node_mut(snapshot, element_id)
        .ok_or_else(|| StateError::MissingElement(element_id.clone()))?;

    match field {
        "name" => node.name = string_property(value, field)?,
        "class_name" => node.class_name = string_property(value, field)?,
        "automation_id" => node.automation_id = string_property(value, field)?,
        "control_type" => {
            node.control_type =
                string_property(value, field)?.ok_or_else(|| StateError::InvalidPropertyUpdate {
                    field: field.to_owned(),
                })?
        }
        "native_window_handle" => node.native_window_handle = u64_property(value)?,
        "bounds" => {
            node.bounds =
                rect_property(value)?.ok_or_else(|| StateError::InvalidPropertyUpdate {
                    field: field.to_owned(),
                })?
        }
        "confidence" => {
            node.confidence =
                f32_property(value)?.ok_or_else(|| StateError::InvalidPropertyUpdate {
                    field: field.to_owned(),
                })?
        }
        "states.enabled" => node.states.enabled = bool_property(value, field)?,
        "states.visible" => node.states.visible = bool_property(value, field)?,
        "states.focused" => node.states.focused = bool_property(value, field)?,
        "states.selected" => node.states.selected = bool_property(value, field)?,
        "states.expanded" => node.states.expanded = bool_property(value, field)?,
        "states.toggled" => node.states.toggled = bool_property(value, field)?,
        _ => {
            node.properties.insert(field.to_owned(), value);
        }
    }

    Ok(())
}

fn apply_focus_change(
    snapshot: &mut UiSnapshot,
    window_id: &WindowId,
    element_id: Option<&ElementId>,
) -> Result<(), StateError> {
    let window = snapshot
        .windows
        .iter_mut()
        .find(|window| &window.window_id == window_id)
        .ok_or_else(|| StateError::MissingWindow(window_id.clone()))?;
    clear_focus(&mut window.root);

    if let Some(element_id) = element_id {
        let node = find_node_mut_in_tree(&mut window.root, element_id)
            .ok_or_else(|| StateError::MissingElement(element_id.clone()))?;
        node.states.focused = true;
    }

    Ok(())
}

fn sort_windows(windows: &mut [WindowState]) {
    windows.sort_by(|left, right| left.window_id.cmp(&right.window_id));
}

fn reparent_subtree(node: &mut ElementNode, parent_id: Option<&ElementId>) {
    node.parent_id = parent_id.cloned();
    for child in &mut node.children {
        reparent_subtree(child, Some(&node.element_id));
    }
}

fn find_node_mut<'a>(
    snapshot: &'a mut UiSnapshot,
    element_id: &ElementId,
) -> Option<&'a mut ElementNode> {
    snapshot
        .windows
        .iter_mut()
        .find_map(|window| find_node_mut_in_tree(&mut window.root, element_id))
}

fn find_node_mut_in_tree<'a>(
    node: &'a mut ElementNode,
    element_id: &ElementId,
) -> Option<&'a mut ElementNode> {
    if &node.element_id == element_id {
        return Some(node);
    }

    for child in &mut node.children {
        if let Some(found) = find_node_mut_in_tree(child, element_id) {
            return Some(found);
        }
    }

    None
}

fn remove_node_from_children(children: &mut Vec<ElementNode>, element_id: &ElementId) -> bool {
    if let Some(index) = children
        .iter()
        .position(|child| &child.element_id == element_id)
    {
        children.remove(index);
        return true;
    }

    for child in children {
        if remove_node_from_children(&mut child.children, element_id) {
            return true;
        }
    }

    false
}

fn replace_node_in_tree(
    node: &mut ElementNode,
    element_id: &ElementId,
    replacement: &mut ElementNode,
) -> bool {
    if let Some(index) = node
        .children
        .iter()
        .position(|child| &child.element_id == element_id)
    {
        reparent_subtree(replacement, Some(&node.element_id));
        node.children[index] = replacement.clone();
        return true;
    }

    for child in &mut node.children {
        if replace_node_in_tree(child, element_id, replacement) {
            return true;
        }
    }

    false
}

fn clear_focus(node: &mut ElementNode) {
    node.states.focused = false;
    for child in &mut node.children {
        clear_focus(child);
    }
}

fn string_property(value: PropertyValue, field: &str) -> Result<Option<String>, StateError> {
    match value {
        PropertyValue::String(value) => Ok(Some(value)),
        PropertyValue::Null => Ok(None),
        _ => Err(StateError::InvalidPropertyUpdate {
            field: field.to_owned(),
        }),
    }
}

fn u64_property(value: PropertyValue) -> Result<Option<u64>, StateError> {
    match value {
        PropertyValue::I64(value) if value >= 0 => Ok(Some(value as u64)),
        PropertyValue::Null => Ok(None),
        _ => Err(StateError::InvalidPropertyUpdate {
            field: "native_window_handle".to_owned(),
        }),
    }
}

fn rect_property(value: PropertyValue) -> Result<Option<vmui_protocol::Rect>, StateError> {
    match value {
        PropertyValue::Rect(value) => Ok(Some(value)),
        PropertyValue::Null => Ok(None),
        _ => Err(StateError::InvalidPropertyUpdate {
            field: "bounds".to_owned(),
        }),
    }
}

fn f32_property(value: PropertyValue) -> Result<Option<f32>, StateError> {
    match value {
        PropertyValue::F64(value) => Ok(Some(value as f32)),
        PropertyValue::I64(value) => Ok(Some(value as f32)),
        PropertyValue::Null => Ok(None),
        _ => Err(StateError::InvalidPropertyUpdate {
            field: "confidence".to_owned(),
        }),
    }
}

fn bool_property(value: PropertyValue, field: &str) -> Result<bool, StateError> {
    match value {
        PropertyValue::Bool(value) => Ok(value),
        _ => Err(StateError::InvalidPropertyUpdate {
            field: field.to_owned(),
        }),
    }
}

#[cfg(test)]
mod tests {
    use chrono::{TimeZone, Utc};
    use tempfile::tempdir;
    use vmui_protocol::{
        BackendKind, DiffOp, ElementId, ElementNode, ElementStates, Locator, Rect, SessionMode,
        UiDiffBatch, WindowId, WindowState,
    };

    use super::*;

    fn snapshot(session_id: SessionId, rev: Revision) -> UiSnapshot {
        UiSnapshot {
            session_id,
            rev,
            mode: SessionMode::EnterpriseUi,
            captured_at: Utc
                .timestamp_millis_opt(1_700_000_000_123)
                .single()
                .unwrap(),
            windows: vec![WindowState {
                window_id: WindowId::from("wnd-1"),
                pid: 42,
                process_name: Some("1cv8.exe".to_owned()),
                title: "Test".to_owned(),
                bounds: Rect::default(),
                backend: BackendKind::Uia,
                confidence: 1.0,
                root: ElementNode {
                    element_id: ElementId::from("elt-root"),
                    parent_id: None,
                    backend: BackendKind::Uia,
                    control_type: "Window".to_owned(),
                    class_name: None,
                    name: Some("Test".to_owned()),
                    automation_id: None,
                    native_window_handle: None,
                    bounds: Rect::default(),
                    locator: Locator {
                        window_fingerprint: "1cv8.exe:Test".to_owned(),
                        path: Vec::new(),
                    },
                    properties: Default::default(),
                    states: ElementStates::default(),
                    children: vec![ElementNode {
                        element_id: ElementId::from("elt-child"),
                        parent_id: Some(ElementId::from("elt-root")),
                        backend: BackendKind::Uia,
                        control_type: "Button".to_owned(),
                        class_name: Some("Button".to_owned()),
                        name: Some("Open".to_owned()),
                        automation_id: Some("open".to_owned()),
                        native_window_handle: None,
                        bounds: Rect {
                            left: 10,
                            top: 10,
                            width: 40,
                            height: 20,
                        },
                        locator: Locator {
                            window_fingerprint: "1cv8.exe:Test".to_owned(),
                            path: Vec::new(),
                        },
                        properties: Default::default(),
                        states: ElementStates::default(),
                        children: Vec::new(),
                        confidence: 1.0,
                    }],
                    confidence: 1.0,
                },
            }],
        }
    }

    #[test]
    fn state_store_rejects_diff_without_snapshot() {
        let mut store = UiStateStore::default();
        let diff = UiDiffBatch {
            base_rev: 1,
            new_rev: 2,
            emitted_at: Utc::now(),
            ops: Vec::new(),
        };

        let err = store.apply_diff(&diff).expect_err("diff must fail");
        assert_eq!(err, StateError::MissingSnapshot);
    }

    #[test]
    fn state_store_tracks_revisions() {
        let mut store = UiStateStore::default();
        store.replace_snapshot(snapshot(SessionId::new("sess"), 3));

        let diff = UiDiffBatch {
            base_rev: 3,
            new_rev: 4,
            emitted_at: Utc::now(),
            ops: Vec::new(),
        };

        store
            .apply_diff(&diff)
            .expect("diff metadata must be accepted");

        assert_eq!(store.revision(), 4);
        assert_eq!(store.resync_reason(), None);
    }

    #[test]
    fn state_store_marks_resync_from_diff() {
        let mut store = UiStateStore::default();
        store.replace_snapshot(snapshot(SessionId::new("sess"), 3));

        let diff = UiDiffBatch {
            base_rev: 3,
            new_rev: 4,
            emitted_at: Utc::now(),
            ops: vec![DiffOp::SnapshotResync {
                reason: "client gap".to_owned(),
            }],
        };

        store
            .apply_diff(&diff)
            .expect("diff metadata must be accepted");

        assert_eq!(store.resync_reason(), Some("client gap"));
    }

    #[test]
    fn state_store_applies_focus_changed_to_snapshot_tree() {
        let session_id = SessionId::new("sess");
        let mut store = UiStateStore::default();
        store.replace_snapshot(snapshot(session_id, 3));

        let diff = UiDiffBatch {
            base_rev: 3,
            new_rev: 4,
            emitted_at: Utc::now(),
            ops: vec![DiffOp::FocusChanged {
                window_id: WindowId::from("wnd-1"),
                element_id: Some(ElementId::from("elt-child")),
            }],
        };

        store.apply_diff(&diff).expect("focus diff must apply");

        let snapshot = store.snapshot().expect("snapshot must exist");
        assert!(!snapshot.windows[0].root.states.focused);
        assert!(snapshot.windows[0].root.children[0].states.focused);
    }

    #[test]
    fn state_store_replaces_nodes_from_diff() {
        let session_id = SessionId::new("sess");
        let mut store = UiStateStore::default();
        store.replace_snapshot(snapshot(session_id, 3));
        let replacement = ElementNode {
            element_id: ElementId::from("elt-child"),
            parent_id: Some(ElementId::from("elt-root")),
            backend: BackendKind::Mixed,
            control_type: "Edit".to_owned(),
            class_name: Some("V8Edit".to_owned()),
            name: Some("Search".to_owned()),
            automation_id: Some("search".to_owned()),
            native_window_handle: None,
            bounds: Rect {
                left: 15,
                top: 10,
                width: 80,
                height: 20,
            },
            locator: Locator {
                window_fingerprint: "1cv8.exe:Test".to_owned(),
                path: Vec::new(),
            },
            properties: Default::default(),
            states: ElementStates {
                enabled: true,
                visible: true,
                focused: false,
                selected: false,
                expanded: false,
                toggled: false,
            },
            children: Vec::new(),
            confidence: 0.8,
        };

        let diff = UiDiffBatch {
            base_rev: 3,
            new_rev: 4,
            emitted_at: Utc::now(),
            ops: vec![DiffOp::NodeReplaced {
                element_id: ElementId::from("elt-child"),
                node: replacement,
            }],
        };

        store.apply_diff(&diff).expect("replace diff must apply");

        let snapshot = store.snapshot().expect("snapshot must exist");
        assert_eq!(snapshot.windows[0].root.children[0].control_type, "Edit");
        assert_eq!(
            snapshot.windows[0].root.children[0]
                .automation_id
                .as_deref(),
            Some("search")
        );
        assert_eq!(
            snapshot.windows[0].root.children[0].backend,
            BackendKind::Mixed
        );
    }

    #[test]
    fn session_registry_persists_runtime_metadata() {
        let session_id = SessionId::new("sess");
        let mut registry = SessionRegistry::default();
        registry.open_session(SessionRuntime::new(
            session_id.clone(),
            SessionMode::Configurator,
            "test-backend",
        ));
        registry
            .mark_subscribed(&session_id, true)
            .expect("session must exist");
        registry
            .apply_snapshot(&session_id, snapshot(session_id.clone(), 10))
            .expect("snapshot must apply");
        registry
            .close_session(&session_id)
            .expect("close must succeed");

        let runtime = registry.runtime(&session_id).expect("runtime must exist");
        assert_eq!(runtime.mode, SessionMode::Configurator);
        assert_eq!(runtime.backend_id, "test-backend");
        assert_eq!(runtime.shallow, Some(true));
        assert_eq!(runtime.last_revision, Some(10));
        assert!(runtime.subscribed_at.is_some());
        assert!(runtime.closed_at.is_some());
    }

    #[test]
    fn session_registry_keeps_recent_diffs_for_diagnostics() {
        let session_id = SessionId::new("sess");
        let mut registry = SessionRegistry::default();
        registry.open_session(SessionRuntime::new(
            session_id.clone(),
            SessionMode::EnterpriseUi,
            "test-backend",
        ));
        registry
            .apply_snapshot(&session_id, snapshot(session_id.clone(), 3))
            .expect("snapshot must apply");

        let diff = UiDiffBatch {
            base_rev: 3,
            new_rev: 4,
            emitted_at: Utc::now(),
            ops: vec![DiffOp::FocusChanged {
                window_id: WindowId::from("wnd-1"),
                element_id: Some(ElementId::from("elt-child")),
            }],
        };

        registry
            .apply_diff(&session_id, &diff)
            .expect("diff must apply");

        let recent = registry.recent_diffs(&session_id).expect("recent diffs");
        assert_eq!(recent.len(), 1);
        assert_eq!(recent[0], diff);
    }

    #[test]
    fn artifact_store_writes_and_reads_bytes() {
        let dir = tempdir().expect("tempdir");
        let mut store = ArtifactStore::new(dir.path()).expect("artifact store");
        let descriptor = store
            .write_bytes(
                Some(SessionId::new("sess")),
                "snapshot-json",
                "application/json",
                br#"{"ok":true}"#,
            )
            .expect("artifact write");

        let bytes = store
            .read_bytes(&descriptor.artifact_id)
            .expect("artifact read");

        assert_eq!(bytes, br#"{"ok":true}"#);
        assert_eq!(
            store
                .descriptor(&descriptor.artifact_id)
                .expect("descriptor must exist"),
            &descriptor
        );
    }
}

use std::{
    collections::BTreeMap,
    fs,
    path::{Path, PathBuf},
};

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use thiserror::Error;
use vmui_protocol::{
    ArtifactDescriptor, ArtifactId, DiffOp, Revision, SessionId, SessionMode, UiDiffBatch,
    UiSnapshot,
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

    pub fn apply_diff_metadata(&mut self, diff: &UiDiffBatch) -> Result<(), StateError> {
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
}

#[derive(Clone, Debug)]
pub struct SessionRecord {
    pub runtime: SessionRuntime,
    pub state: UiStateStore,
}

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
        Ok(())
    }

    pub fn apply_diff_metadata(
        &mut self,
        session_id: &SessionId,
        diff: &UiDiffBatch,
    ) -> Result<(), SessionError> {
        let record = self
            .sessions
            .get_mut(session_id)
            .ok_or_else(|| SessionError::NotFound(session_id.clone()))?;
        record
            .state
            .apply_diff_metadata(diff)
            .map_err(SessionError::State)?;
        record.runtime.last_revision = Some(record.state.revision());
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

#[cfg(test)]
mod tests {
    use chrono::{TimeZone, Utc};
    use tempfile::tempdir;
    use vmui_protocol::{
        BackendKind, ElementId, ElementNode, ElementStates, Locator, Rect, SessionMode,
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
                window_id: WindowId::new("wnd"),
                pid: 42,
                process_name: Some("1cv8.exe".to_owned()),
                title: "Test".to_owned(),
                bounds: Rect::default(),
                backend: BackendKind::Uia,
                confidence: 1.0,
                root: ElementNode {
                    element_id: ElementId::new("elt"),
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
                    children: Vec::new(),
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

        let err = store
            .apply_diff_metadata(&diff)
            .expect_err("diff must fail");
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
            .apply_diff_metadata(&diff)
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
            .apply_diff_metadata(&diff)
            .expect("diff metadata must be accepted");

        assert_eq!(store.resync_reason(), Some("client gap"));
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

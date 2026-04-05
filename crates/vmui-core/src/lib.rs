use std::{
    collections::{BTreeMap, VecDeque},
    fs,
    path::{Path, PathBuf},
};

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use thiserror::Error;
use vmui_protocol::{
    ActionOutcomeSummary, ActionStatus, ArtifactDescriptor, ArtifactId, ArtifactStorePressureState,
    ArtifactStoreStatus, DiffOp, ElementId, ElementNode, PropertyValue, Revision,
    RuntimeHealthState, RuntimeHealthSummary, RuntimeObservationSummary, RuntimeRecoverySummary,
    RuntimeStatusReport, RuntimeWarningSummary, SessionId, SessionMode, UiDiffBatch, UiSnapshot,
    WindowId, WindowState,
};

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct AgentConfig {
    pub bind_addr: String,
    pub artifact_dir: PathBuf,
    pub default_mode: SessionMode,
    pub artifact_retention: ArtifactRetentionPolicy,
}

impl Default for AgentConfig {
    fn default() -> Self {
        Self {
            bind_addr: "127.0.0.1:50051".to_owned(),
            artifact_dir: PathBuf::from("var/artifacts"),
            default_mode: SessionMode::EnterpriseUi,
            artifact_retention: ArtifactRetentionPolicy::default(),
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct ArtifactRetentionPolicy {
    pub max_age_seconds: u64,
    pub max_bytes: u64,
    pub max_count: usize,
    pub cleanup_interval_seconds: u64,
}

impl Default for ArtifactRetentionPolicy {
    fn default() -> Self {
        Self {
            max_age_seconds: 24 * 60 * 60,
            max_bytes: 256 * 1024 * 1024,
            max_count: 512,
            cleanup_interval_seconds: 5 * 60,
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

    pub fn active_len(&self) -> usize {
        self.sessions
            .values()
            .filter(|record| record.runtime.closed_at.is_none())
            .count()
    }

    pub fn resync_required_count(&self) -> usize {
        self.sessions
            .values()
            .filter(|record| record.state.resync_reason().is_some())
            .count()
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

#[derive(Clone, Debug)]
pub struct ArtifactCleanupOutcome {
    pub removed_count: usize,
    pub removed_bytes: u64,
    pub remaining_count: usize,
    pub remaining_bytes: u64,
    pub ran_at: DateTime<Utc>,
    pub reason: String,
}

#[derive(Clone, Debug, Default)]
struct ArtifactStoreMetrics {
    total_written_bytes: u64,
    deleted_artifact_count: u64,
    deleted_bytes: u64,
    cleanup_runs: u64,
    last_cleanup_at: Option<DateTime<Utc>>,
    last_cleanup_reason: Option<String>,
}

pub struct ArtifactStore {
    root: PathBuf,
    artifacts: BTreeMap<ArtifactId, ArtifactRecord>,
    retention: ArtifactRetentionPolicy,
    metrics: ArtifactStoreMetrics,
    total_bytes: u64,
}

impl ArtifactStore {
    pub fn new(
        root: impl Into<PathBuf>,
        retention: ArtifactRetentionPolicy,
    ) -> Result<Self, ArtifactError> {
        let root = root.into();
        fs::create_dir_all(&root).map_err(|source| ArtifactError::CreateDir {
            path: root.clone(),
            source,
        })?;
        let mut store = Self {
            root,
            artifacts: BTreeMap::new(),
            retention,
            metrics: ArtifactStoreMetrics::default(),
            total_bytes: 0,
        };
        store.startup_sweep()?;
        Ok(store)
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    pub fn retention(&self) -> &ArtifactRetentionPolicy {
        &self.retention
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
        self.total_bytes += descriptor.size_bytes;
        self.metrics.total_written_bytes += descriptor.size_bytes;
        self.artifacts.insert(artifact_id, record);
        let _ = self.cleanup("retention_enforced_after_write")?;
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

    pub fn cleanup(
        &mut self,
        reason: impl Into<String>,
    ) -> Result<ArtifactCleanupOutcome, ArtifactError> {
        let reason = reason.into();
        let now = Utc::now();
        let mut records = self
            .artifacts
            .iter()
            .map(|(artifact_id, record)| (artifact_id.clone(), record.clone()))
            .collect::<Vec<_>>();
        records.sort_by(|left, right| {
            left.1
                .created_at
                .cmp(&right.1.created_at)
                .then_with(|| left.0.cmp(&right.0))
        });

        let max_age_cutoff = now
            - chrono::Duration::seconds(self.retention.max_age_seconds.min(i64::MAX as u64) as i64);
        let mut remove_ids = Vec::new();
        let mut remaining_count = records.len();
        let mut remaining_bytes = self.total_bytes;

        for (artifact_id, record) in &records {
            if record.created_at < max_age_cutoff {
                remaining_count = remaining_count.saturating_sub(1);
                remaining_bytes = remaining_bytes.saturating_sub(record.descriptor.size_bytes);
                remove_ids.push(artifact_id.clone());
            }
        }

        for (artifact_id, record) in &records {
            if remove_ids.iter().any(|candidate| candidate == artifact_id) {
                continue;
            }

            let over_count = remaining_count > self.retention.max_count;
            let over_bytes = remaining_bytes > self.retention.max_bytes;
            if !over_count && !over_bytes {
                continue;
            }

            remaining_count = remaining_count.saturating_sub(1);
            remaining_bytes = remaining_bytes.saturating_sub(record.descriptor.size_bytes);
            remove_ids.push(artifact_id.clone());
        }

        let mut removed_count = 0usize;
        let mut removed_bytes = 0u64;
        for artifact_id in remove_ids {
            let Some(record) = self.artifacts.remove(&artifact_id) else {
                continue;
            };
            remove_artifact_path(&record.path)?;
            removed_count += 1;
            removed_bytes += record.descriptor.size_bytes;
        }

        self.total_bytes = self
            .artifacts
            .values()
            .map(|record| record.descriptor.size_bytes)
            .sum();
        self.metrics.cleanup_runs += 1;
        self.metrics.deleted_artifact_count += removed_count as u64;
        self.metrics.deleted_bytes += removed_bytes;
        self.metrics.last_cleanup_at = Some(now);
        self.metrics.last_cleanup_reason = Some(reason.clone());

        Ok(ArtifactCleanupOutcome {
            removed_count,
            removed_bytes,
            remaining_count: self.artifacts.len(),
            remaining_bytes: self.total_bytes,
            ran_at: now,
            reason,
        })
    }

    pub fn status(&self) -> ArtifactStoreStatus {
        ArtifactStoreStatus {
            artifact_count: self.artifacts.len(),
            total_bytes: self.total_bytes,
            max_count: self.retention.max_count,
            max_bytes: self.retention.max_bytes,
            max_age_seconds: self.retention.max_age_seconds,
            cleanup_interval_seconds: self.retention.cleanup_interval_seconds,
            cleanup_runs: self.metrics.cleanup_runs,
            deleted_artifact_count: self.metrics.deleted_artifact_count,
            deleted_bytes: self.metrics.deleted_bytes,
            last_cleanup_at: self.metrics.last_cleanup_at,
            last_cleanup_reason: self.metrics.last_cleanup_reason.clone(),
            pressure_state: artifact_pressure_state(
                self.artifacts.len(),
                self.total_bytes,
                &self.retention,
            ),
        }
    }

    fn startup_sweep(&mut self) -> Result<(), ArtifactError> {
        let mut removed_count = 0u64;
        let mut removed_bytes = 0u64;
        let entries = fs::read_dir(&self.root).map_err(|source| ArtifactError::ReadDir {
            path: self.root.clone(),
            source,
        })?;

        for entry in entries {
            let entry = entry.map_err(|source| ArtifactError::ReadDirEntry {
                path: self.root.clone(),
                source,
            })?;
            let path = entry.path();
            if !path.is_file() {
                continue;
            }
            let metadata = fs::metadata(&path).map_err(|source| ArtifactError::Metadata {
                path: path.clone(),
                source,
            })?;
            remove_artifact_path(&path)?;
            removed_count += 1;
            removed_bytes += metadata.len();
        }

        if removed_count > 0 {
            self.metrics.cleanup_runs += 1;
            self.metrics.deleted_artifact_count += removed_count;
            self.metrics.deleted_bytes += removed_bytes;
            self.metrics.last_cleanup_at = Some(Utc::now());
            self.metrics.last_cleanup_reason = Some("startup_orphan_sweep".to_owned());
        }

        Ok(())
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
    #[error("failed to list artifact dir `{path}`: {source}")]
    ReadDir {
        path: PathBuf,
        source: std::io::Error,
    },
    #[error("failed to inspect artifact dir entry in `{path}`: {source}")]
    ReadDirEntry {
        path: PathBuf,
        source: std::io::Error,
    },
    #[error("failed to read artifact file `{path}`: {source}")]
    ReadFile {
        path: PathBuf,
        source: std::io::Error,
    },
    #[error("failed to inspect artifact file `{path}`: {source}")]
    Metadata {
        path: PathBuf,
        source: std::io::Error,
    },
    #[error("failed to delete artifact file `{path}`: {source}")]
    DeleteFile {
        path: PathBuf,
        source: std::io::Error,
    },
}

#[derive(Clone, Debug, Default)]
pub struct RuntimeTelemetry {
    resync_count: u64,
    last_resync_at: Option<DateTime<Utc>>,
    last_resync_reason: Option<String>,
    recovery_count: u64,
    continuity_invalidated_count: u64,
    last_recovery_at: Option<DateTime<Utc>>,
    last_recovery_reason: Option<String>,
    warning_counts_by_code: BTreeMap<String, u64>,
    warning_counts_by_class: BTreeMap<String, u64>,
    total_warning_count: u64,
    last_warning_at: Option<DateTime<Utc>>,
    last_warning_code: Option<String>,
    last_warning_message: Option<String>,
    action_outcomes: BTreeMap<String, ActionOutcomeSummary>,
    snapshot_count: u64,
    fallback_heavy_snapshot_count: u64,
    last_fallback_heavy_at: Option<DateTime<Utc>>,
    last_fallback_surface_count: usize,
    continuity_invalidated: bool,
}

pub struct AgentRuntimeState {
    pub sessions: SessionRegistry,
    pub artifacts: ArtifactStore,
    pub telemetry: RuntimeTelemetry,
}

impl AgentRuntimeState {
    pub fn new(config: &AgentConfig) -> Result<Self, ArtifactError> {
        Ok(Self {
            sessions: SessionRegistry::default(),
            artifacts: ArtifactStore::new(
                config.artifact_dir.clone(),
                config.artifact_retention.clone(),
            )?,
            telemetry: RuntimeTelemetry::default(),
        })
    }

    pub fn cleanup_artifacts(
        &mut self,
        reason: impl Into<String>,
    ) -> Result<ArtifactCleanupOutcome, ArtifactError> {
        self.artifacts.cleanup(reason)
    }

    pub fn record_warning(&mut self, code: impl Into<String>, message: impl Into<String>) {
        let code = code.into();
        let message = message.into();
        let class = classify_warning_code(&code);
        let now = Utc::now();
        *self
            .telemetry
            .warning_counts_by_code
            .entry(code.clone())
            .or_default() += 1;
        *self
            .telemetry
            .warning_counts_by_class
            .entry(class.to_owned())
            .or_default() += 1;
        self.telemetry.total_warning_count += 1;
        self.telemetry.last_warning_at = Some(now);
        self.telemetry.last_warning_code = Some(code);
        self.telemetry.last_warning_message = Some(message);
    }

    pub fn record_resync(&mut self, reason: impl Into<String>) {
        self.telemetry.resync_count += 1;
        self.telemetry.last_resync_at = Some(Utc::now());
        self.telemetry.last_resync_reason = Some(reason.into());
    }

    pub fn record_recovery(&mut self, reason: impl Into<String>, continuity_invalidated: bool) {
        self.telemetry.recovery_count += 1;
        self.telemetry.last_recovery_at = Some(Utc::now());
        self.telemetry.last_recovery_reason = Some(reason.into());
        self.telemetry.continuity_invalidated = continuity_invalidated;
        if continuity_invalidated {
            self.telemetry.continuity_invalidated_count += 1;
        }
    }

    pub fn record_action_result(&mut self, action: &str, status: &ActionStatus) {
        let entry = self
            .telemetry
            .action_outcomes
            .entry(action.to_owned())
            .or_insert_with(|| ActionOutcomeSummary {
                action: action.to_owned(),
                completed: 0,
                failed: 0,
                timed_out: 0,
                unsupported: 0,
            });

        match status {
            ActionStatus::Completed => entry.completed += 1,
            ActionStatus::Failed => entry.failed += 1,
            ActionStatus::TimedOut => entry.timed_out += 1,
            ActionStatus::Unsupported => entry.unsupported += 1,
        }
    }

    pub fn record_snapshot_observation(&mut self, fallback_surface_count: usize) {
        const FALLBACK_HEAVY_SURFACE_THRESHOLD: usize = 2;

        self.telemetry.snapshot_count += 1;
        if fallback_surface_count >= FALLBACK_HEAVY_SURFACE_THRESHOLD {
            self.telemetry.fallback_heavy_snapshot_count += 1;
            self.telemetry.last_fallback_heavy_at = Some(Utc::now());
            self.telemetry.last_fallback_surface_count = fallback_surface_count;
        }
    }

    pub fn runtime_status(&self) -> RuntimeStatusReport {
        let artifact_store = self.artifacts.status();
        let mut reasons = Vec::new();
        if self.sessions.resync_required_count() > 0 {
            reasons.push("session_resync_required".to_owned());
        }
        if self.telemetry.resync_count > 0 {
            reasons.push("resyncs_observed".to_owned());
        }
        if self.telemetry.fallback_heavy_snapshot_count > 0 {
            reasons.push("fallback_heavy_observation".to_owned());
        }
        if self
            .telemetry
            .warning_counts_by_class
            .get("backend")
            .copied()
            .unwrap_or_default()
            > 0
        {
            reasons.push("backend_degradation".to_owned());
        }
        if artifact_store.pressure_state != ArtifactStorePressureState::Healthy {
            reasons.push("artifact_store_pressure".to_owned());
        }
        reasons.sort();
        reasons.dedup();

        let status = if reasons.is_empty() {
            RuntimeHealthState::Healthy
        } else {
            RuntimeHealthState::Degraded
        };

        let mut actions = self
            .telemetry
            .action_outcomes
            .values()
            .cloned()
            .collect::<Vec<_>>();
        actions.sort_by(|left, right| left.action.cmp(&right.action));

        RuntimeStatusReport {
            generated_at: Utc::now(),
            session_count: self.sessions.len(),
            active_session_count: self.sessions.active_len(),
            active_resync_sessions: self.sessions.resync_required_count(),
            health: RuntimeHealthSummary { status, reasons },
            recoveries: RuntimeRecoverySummary {
                resync_count: self.telemetry.resync_count,
                last_resync_at: self.telemetry.last_resync_at,
                last_resync_reason: self.telemetry.last_resync_reason.clone(),
                recovery_count: self.telemetry.recovery_count,
                continuity_invalidated_count: self.telemetry.continuity_invalidated_count,
                last_recovery_at: self.telemetry.last_recovery_at,
                last_recovery_reason: self.telemetry.last_recovery_reason.clone(),
                continuity_invalidated: self.telemetry.continuity_invalidated,
            },
            warnings: RuntimeWarningSummary {
                total_count: self.telemetry.total_warning_count,
                by_code: self.telemetry.warning_counts_by_code.clone(),
                by_class: self.telemetry.warning_counts_by_class.clone(),
                last_warning_at: self.telemetry.last_warning_at,
                last_warning_code: self.telemetry.last_warning_code.clone(),
                last_warning_message: self.telemetry.last_warning_message.clone(),
            },
            observations: RuntimeObservationSummary {
                snapshot_count: self.telemetry.snapshot_count,
                fallback_heavy_snapshot_count: self.telemetry.fallback_heavy_snapshot_count,
                last_fallback_heavy_at: self.telemetry.last_fallback_heavy_at,
                last_fallback_surface_count: self.telemetry.last_fallback_surface_count,
            },
            actions,
            artifact_store,
        }
    }
}

fn remove_artifact_path(path: &Path) -> Result<(), ArtifactError> {
    match fs::remove_file(path) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(source) => Err(ArtifactError::DeleteFile {
            path: path.to_path_buf(),
            source,
        }),
    }
}

fn artifact_pressure_state(
    artifact_count: usize,
    total_bytes: u64,
    retention: &ArtifactRetentionPolicy,
) -> ArtifactStorePressureState {
    if artifact_count > retention.max_count {
        ArtifactStorePressureState::MaxCountExceeded
    } else if total_bytes > retention.max_bytes {
        ArtifactStorePressureState::MaxBytesExceeded
    } else {
        ArtifactStorePressureState::Healthy
    }
}

fn classify_warning_code(code: &str) -> &'static str {
    if code.contains("resync") || code.contains("recover") {
        "resync"
    } else if code.starts_with("backend_") {
        "backend"
    } else if code.starts_with("artifact_") {
        "artifact_store"
    } else if code.starts_with("session_") {
        "session"
    } else {
        "general"
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
        let mut store = ArtifactStore::new(dir.path(), ArtifactRetentionPolicy::default())
            .expect("artifact store");
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

    #[test]
    fn artifact_store_enforces_retention_limits() {
        let dir = tempdir().expect("tempdir");
        let mut store = ArtifactStore::new(
            dir.path(),
            ArtifactRetentionPolicy {
                max_age_seconds: 24 * 60 * 60,
                max_bytes: 8,
                max_count: 1,
                cleanup_interval_seconds: 1,
            },
        )
        .expect("artifact store");

        let first = store
            .write_bytes(None, "log-text", "text/plain", b"1234")
            .expect("write first");
        let second = store
            .write_bytes(None, "log-text", "text/plain", b"567890")
            .expect("write second");

        assert!(store.descriptor(&first.artifact_id).is_none());
        assert!(store.descriptor(&second.artifact_id).is_some());
        assert_eq!(store.status().artifact_count, 1);
        assert_eq!(store.status().total_bytes, 6);
    }

    #[test]
    fn runtime_status_reports_recovery_and_warning_summaries() {
        let dir = tempdir().expect("tempdir");
        let mut state = AgentRuntimeState::new(&AgentConfig {
            bind_addr: "127.0.0.1:50051".to_owned(),
            artifact_dir: dir.path().to_path_buf(),
            default_mode: SessionMode::EnterpriseUi,
            artifact_retention: ArtifactRetentionPolicy::default(),
        })
        .expect("runtime state");
        let session_id = SessionId::new("sess");

        state.sessions.open_session(SessionRuntime::new(
            session_id.clone(),
            SessionMode::EnterpriseUi,
            "test-backend",
        ));
        state
            .sessions
            .apply_snapshot(&session_id, snapshot(session_id.clone(), 1))
            .expect("apply snapshot");
        state
            .sessions
            .state(&session_id)
            .expect("state exists")
            .snapshot()
            .expect("snapshot exists");

        state.record_warning("backend_degraded", "uia observer unavailable");
        state.record_resync("stale diff");
        state.record_recovery("observer restarted", true);
        state.record_snapshot_observation(3);
        state.record_action_result("wait_for", &ActionStatus::TimedOut);

        let report = state.runtime_status();

        assert_eq!(report.health.status, RuntimeHealthState::Degraded);
        assert!(report
            .health
            .reasons
            .iter()
            .any(|reason| reason == "backend_degradation"));
        assert_eq!(report.recoveries.resync_count, 1);
        assert_eq!(report.recoveries.recovery_count, 1);
        assert!(report.recoveries.continuity_invalidated);
        assert_eq!(report.warnings.total_count, 1);
        assert_eq!(report.warnings.by_class.get("backend"), Some(&1));
        assert_eq!(report.observations.fallback_heavy_snapshot_count, 1);
        assert_eq!(report.actions.len(), 1);
        assert_eq!(report.actions[0].action, "wait_for");
        assert_eq!(report.actions[0].timed_out, 1);
    }
}

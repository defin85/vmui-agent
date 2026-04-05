use std::pin::Pin;

use anyhow::Result;
use async_trait::async_trait;
use futures_core::Stream;
use vmui_protocol::{
    ActionId, ActionRequest, ActionResult, ActionStatus, ArtifactDescriptor, SessionId,
    SessionProfile, UiDiffBatch, UiSnapshot,
};

pub type BackendEventStream = Pin<Box<dyn Stream<Item = BackendEvent> + Send>>;

pub struct BackendSession {
    pub initial_snapshot: UiSnapshot,
    pub events: BackendEventStream,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BackendArtifact {
    pub kind: String,
    pub mime_type: String,
    pub bytes: Vec<u8>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BackendActionResult {
    pub action_id: ActionId,
    pub ok: bool,
    pub status: ActionStatus,
    pub message: String,
    pub artifacts: Vec<BackendArtifact>,
}

impl BackendActionResult {
    pub fn into_action_result(self, artifacts: Vec<ArtifactDescriptor>) -> ActionResult {
        ActionResult {
            action_id: self.action_id,
            ok: self.ok,
            status: self.status,
            message: self.message,
            artifacts,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BackendCapabilities {
    pub supports_live_observer: bool,
    pub supports_uia: bool,
    pub supports_msaa: bool,
    pub supports_ocr_fallback: bool,
    pub supports_artifacts: bool,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BackendSessionParams {
    pub session_id: SessionId,
    pub profile: SessionProfile,
    pub shallow: bool,
}

#[derive(Clone, Debug, PartialEq)]
pub enum BackendEvent {
    Diff(UiDiffBatch),
    Warning { code: String, message: String },
}

#[async_trait]
pub trait UiBackend: Send + Sync {
    fn backend_id(&self) -> &'static str;

    fn capabilities(&self) -> BackendCapabilities;

    async fn open_session(&self, params: BackendSessionParams) -> Result<BackendSession>;

    async fn capture_snapshot(&self, params: BackendSessionParams) -> Result<UiSnapshot>;

    async fn perform_action(&self, action: ActionRequest) -> Result<BackendActionResult>;
}

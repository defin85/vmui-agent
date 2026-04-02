use anyhow::Result;
use async_trait::async_trait;
use chrono::Utc;
use futures_util::stream;
use vmui_platform::{
    BackendActionResult, BackendCapabilities, BackendSession, BackendSessionParams, UiBackend,
};
use vmui_protocol::{ActionRequest, ActionStatus, UiSnapshot};

#[derive(Debug, Default, Clone, Copy)]
pub struct WindowsBackend;

impl WindowsBackend {
    pub fn new() -> Self {
        Self
    }

    pub fn supported_on_current_host() -> bool {
        cfg!(windows)
    }
}

#[async_trait]
impl UiBackend for WindowsBackend {
    fn backend_id(&self) -> &'static str {
        "windows-uia"
    }

    fn capabilities(&self) -> BackendCapabilities {
        BackendCapabilities {
            supports_uia: false,
            supports_msaa: false,
            supports_ocr_fallback: false,
            supports_artifacts: true,
        }
    }

    async fn open_session(&self, params: BackendSessionParams) -> Result<BackendSession> {
        let initial_snapshot = self.capture_snapshot(params).await?;
        Ok(BackendSession {
            initial_snapshot,
            events: Box::pin(stream::empty()),
        })
    }

    async fn capture_snapshot(&self, params: BackendSessionParams) -> Result<UiSnapshot> {
        Ok(UiSnapshot {
            session_id: params.session_id,
            rev: 1,
            mode: params.mode,
            captured_at: Utc::now(),
            windows: Vec::new(),
        })
    }

    async fn perform_action(&self, action: ActionRequest) -> Result<BackendActionResult> {
        Ok(BackendActionResult {
            action_id: action.action_id,
            ok: false,
            status: ActionStatus::Unsupported,
            message: "Windows backend action executor is not implemented yet".to_owned(),
            artifacts: Vec::new(),
        })
    }
}

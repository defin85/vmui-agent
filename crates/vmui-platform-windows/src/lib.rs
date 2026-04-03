use std::{
    collections::{BTreeMap, BTreeSet},
    hash::{Hash, Hasher},
    sync::Arc,
    time::Duration,
};

#[cfg(windows)]
use anyhow::Context;
use anyhow::Result;
use async_trait::async_trait;
use chrono::Utc;
use tokio::{sync::mpsc, time::sleep};
use tokio_stream::wrappers::UnboundedReceiverStream;
use vmui_platform::{
    BackendActionResult, BackendCapabilities, BackendEvent, BackendSession, BackendSessionParams,
    UiBackend,
};
use vmui_protocol::{
    ActionRequest, ActionStatus, DiffOp, ElementId, ElementNode, LocatorSegment, UiDiffBatch,
    UiSnapshot, WindowId, WindowState,
};

pub struct WindowsBackend {
    source: Arc<dyn ObservationSource>,
}

impl WindowsBackend {
    pub fn new() -> Self {
        Self {
            source: default_source(),
        }
    }

    #[cfg(test)]
    fn from_source(source: Arc<dyn ObservationSource>) -> Self {
        Self { source }
    }
}

impl Default for WindowsBackend {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg_attr(not(any(test, windows)), allow(dead_code))]
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
enum HintSource {
    Uia,
    WinEvent,
    Msaa,
}

#[cfg_attr(not(any(test, windows)), allow(dead_code))]
#[derive(Clone, Debug, PartialEq, Eq)]
enum RefreshScope {
    Desktop,
    Window { hwnd: usize },
}

#[cfg_attr(not(any(test, windows)), allow(dead_code))]
#[derive(Clone, Debug, PartialEq, Eq)]
struct RefreshRequest {
    scope: RefreshScope,
    source: HintSource,
    reason: String,
}

#[cfg_attr(not(any(test, windows)), allow(dead_code))]
impl RefreshRequest {
    fn desktop(source: HintSource, reason: impl Into<String>) -> Self {
        Self {
            scope: RefreshScope::Desktop,
            source,
            reason: reason.into(),
        }
    }

    fn window(hwnd: usize, source: HintSource, reason: impl Into<String>) -> Self {
        Self {
            scope: RefreshScope::Window { hwnd },
            source,
            reason: reason.into(),
        }
    }
}

#[cfg_attr(not(any(test, windows)), allow(dead_code))]
struct RefreshSubscription {
    receiver: mpsc::UnboundedReceiver<RefreshRequest>,
    shutdown: Option<Box<dyn FnOnce() + Send + 'static>>,
}

#[cfg_attr(not(any(test, windows)), allow(dead_code))]
impl RefreshSubscription {
    #[cfg_attr(not(windows), allow(dead_code))]
    fn new(
        receiver: mpsc::UnboundedReceiver<RefreshRequest>,
        shutdown: impl FnOnce() + Send + 'static,
    ) -> Self {
        Self {
            receiver,
            shutdown: Some(Box::new(shutdown)),
        }
    }

    #[cfg(test)]
    fn from_receiver(receiver: mpsc::UnboundedReceiver<RefreshRequest>) -> Self {
        Self {
            receiver,
            shutdown: None,
        }
    }
}

impl Drop for RefreshSubscription {
    fn drop(&mut self) {
        if let Some(shutdown) = self.shutdown.take() {
            shutdown();
        }
    }
}

trait ObservationSource: Send + Sync {
    fn backend_id(&self) -> &'static str;

    fn capabilities(&self) -> BackendCapabilities;

    fn availability_warning(&self) -> Option<String>;

    fn capture_desktop(&self, params: &BackendSessionParams) -> Result<Vec<WindowState>>;

    fn capture_window(
        &self,
        params: &BackendSessionParams,
        hwnd: usize,
        hint: HintSource,
    ) -> Result<Option<WindowState>>;

    fn subscribe(&self, params: &BackendSessionParams) -> Result<Option<RefreshSubscription>>;
}

#[async_trait]
impl UiBackend for WindowsBackend {
    fn backend_id(&self) -> &'static str {
        self.source.backend_id()
    }

    fn capabilities(&self) -> BackendCapabilities {
        self.source.capabilities()
    }

    async fn open_session(&self, params: BackendSessionParams) -> Result<BackendSession> {
        let windows = self.source.capture_desktop(&params)?;
        let initial_snapshot = snapshot_from_windows(&params, 1, windows);
        let (tx, rx) = mpsc::unbounded_channel();

        if let Some(message) = self.source.availability_warning() {
            let _ = tx.send(BackendEvent::Warning {
                code: "observer_unavailable".to_owned(),
                message,
            });
        }

        let subscription = self.source.subscribe(&params)?;
        if subscription.is_none() && self.source.capabilities().supports_live_observer {
            let _ = tx.send(BackendEvent::Warning {
                code: "observer_unavailable".to_owned(),
                message: "live observer hooks are unavailable; snapshot capture remains available"
                    .to_owned(),
            });
        }

        if let Some(subscription) = subscription {
            let source = Arc::clone(&self.source);
            let params = params.clone();
            let initial_snapshot_for_task = initial_snapshot.clone();
            tokio::spawn(async move {
                observe_refreshes(source, params, initial_snapshot_for_task, subscription, tx)
                    .await;
            });
        }

        Ok(BackendSession {
            initial_snapshot,
            events: Box::pin(UnboundedReceiverStream::new(rx)),
        })
    }

    async fn capture_snapshot(&self, params: BackendSessionParams) -> Result<UiSnapshot> {
        let windows = self.source.capture_desktop(&params)?;
        Ok(snapshot_from_windows(&params, 1, windows))
    }

    async fn perform_action(&self, action: ActionRequest) -> Result<BackendActionResult> {
        #[cfg(windows)]
        {
            if self.backend_id() != "windows-uia" {
                return Ok(unsupported_action(
                    action,
                    "semantic actions require an interactive Windows desktop session",
                ));
            }

            return tokio::task::spawn_blocking(move || windows_impl::perform_action(action))
                .await
                .context("windows action task failed")?;
        }

        #[cfg(not(windows))]
        {
            Ok(unsupported_action(
                action,
                "semantic actions require a Windows host and interactive desktop session",
            ))
        }
    }
}

fn unsupported_action(action: ActionRequest, message: impl Into<String>) -> BackendActionResult {
    BackendActionResult {
        action_id: action.action_id,
        ok: false,
        status: ActionStatus::Unsupported,
        message: message.into(),
        artifacts: Vec::new(),
    }
}

async fn observe_refreshes(
    source: Arc<dyn ObservationSource>,
    params: BackendSessionParams,
    mut current_snapshot: UiSnapshot,
    mut subscription: RefreshSubscription,
    tx: mpsc::UnboundedSender<BackendEvent>,
) {
    while let Some(requests) = next_refresh_batch(&mut subscription.receiver).await {
        let mut next_windows = current_snapshot.windows.clone();

        for request in requests {
            if let Err(error) =
                apply_refresh_request(source.as_ref(), &params, &mut next_windows, &request)
            {
                if tx
                    .send(BackendEvent::Warning {
                        code: "refresh_failed".to_owned(),
                        message: error.to_string(),
                    })
                    .is_err()
                {
                    return;
                }
            }
        }

        sort_windows(&mut next_windows);
        let next_rev = current_snapshot.rev.saturating_add(1);
        let next_snapshot = snapshot_from_windows(&params, next_rev, next_windows);
        let ops = diff_snapshots(&current_snapshot, &next_snapshot);
        if ops.is_empty() {
            continue;
        }

        let diff = UiDiffBatch {
            base_rev: current_snapshot.rev,
            new_rev: next_snapshot.rev,
            emitted_at: Utc::now(),
            ops,
        };

        current_snapshot = next_snapshot;

        if tx.send(BackendEvent::Diff(diff)).is_err() {
            return;
        }
    }
}

async fn next_refresh_batch(
    receiver: &mut mpsc::UnboundedReceiver<RefreshRequest>,
) -> Option<Vec<RefreshRequest>> {
    let first = receiver.recv().await?;
    let mut batch = vec![first];
    let deadline = sleep(Duration::from_millis(75));
    tokio::pin!(deadline);

    loop {
        tokio::select! {
            maybe = receiver.recv() => match maybe {
                Some(request) => batch.push(request),
                None => break,
            },
            _ = &mut deadline => break,
        }
    }

    Some(coalesce_refresh_requests(batch))
}

fn coalesce_refresh_requests(requests: Vec<RefreshRequest>) -> Vec<RefreshRequest> {
    let mut desktop_request: Option<RefreshRequest> = None;
    let mut windows: BTreeMap<usize, RefreshRequest> = BTreeMap::new();

    for request in requests {
        match request.scope {
            RefreshScope::Desktop => {
                desktop_request = Some(match desktop_request.take() {
                    Some(existing) => merge_refresh_request(existing, request),
                    None => request,
                });
            }
            RefreshScope::Window { hwnd } => {
                windows
                    .entry(hwnd)
                    .and_modify(|existing| {
                        *existing = merge_refresh_request(existing.clone(), request.clone())
                    })
                    .or_insert(request);
            }
        }
    }

    if let Some(request) = desktop_request {
        vec![request]
    } else {
        windows.into_values().collect()
    }
}

fn merge_refresh_request(mut current: RefreshRequest, incoming: RefreshRequest) -> RefreshRequest {
    if incoming.source > current.source {
        current.source = incoming.source;
    }
    current.reason = merge_reason(&current.reason, &incoming.reason);
    current
}

fn merge_reason(current: &str, incoming: &str) -> String {
    if current == incoming || incoming.is_empty() {
        current.to_owned()
    } else if current.is_empty() {
        incoming.to_owned()
    } else {
        format!("{current}; {incoming}")
    }
}

fn apply_refresh_request(
    source: &dyn ObservationSource,
    params: &BackendSessionParams,
    windows: &mut Vec<WindowState>,
    request: &RefreshRequest,
) -> Result<()> {
    match request.scope {
        RefreshScope::Desktop => {
            let mut refreshed = source.capture_desktop(params)?;
            stabilize_windows(windows, &mut refreshed);
            *windows = refreshed;
        }
        RefreshScope::Window { hwnd } => {
            match source.capture_window(params, hwnd, request.source)? {
                Some(mut window) => {
                    if let Some(existing) = windows
                        .iter_mut()
                        .find(|candidate| window_matches_hwnd(candidate, hwnd))
                    {
                        stabilize_window(existing, &mut window);
                        *existing = window;
                    } else {
                        windows.push(window);
                    }
                }
                None => windows.retain(|candidate| !window_matches_hwnd(candidate, hwnd)),
            }
        }
    }

    Ok(())
}

fn sort_windows(windows: &mut [WindowState]) {
    windows.sort_by(|left, right| left.window_id.cmp(&right.window_id));
}

fn snapshot_from_windows(
    params: &BackendSessionParams,
    rev: u64,
    mut windows: Vec<WindowState>,
) -> UiSnapshot {
    sort_windows(&mut windows);
    UiSnapshot {
        session_id: params.session_id.clone(),
        rev,
        mode: params.mode.clone(),
        captured_at: Utc::now(),
        windows,
    }
}

fn diff_snapshots(previous: &UiSnapshot, next: &UiSnapshot) -> Vec<DiffOp> {
    let previous_windows = previous
        .windows
        .iter()
        .map(|window| (window.window_id.clone(), window))
        .collect::<BTreeMap<_, _>>();
    let next_windows = next
        .windows
        .iter()
        .map(|window| (window.window_id.clone(), window))
        .collect::<BTreeMap<_, _>>();
    let window_ids = previous_windows
        .keys()
        .chain(next_windows.keys())
        .cloned()
        .collect::<BTreeSet<_>>();
    let mut ops = Vec::new();

    for window_id in window_ids {
        match (
            previous_windows.get(&window_id),
            next_windows.get(&window_id),
        ) {
            (None, Some(window)) => ops.push(DiffOp::WindowAdded {
                window: (*window).clone(),
            }),
            (Some(_), None) => ops.push(DiffOp::WindowRemoved { window_id }),
            (Some(previous_window), Some(next_window)) => {
                let shell_changed = !same_window_shell(previous_window, next_window);
                if shell_changed {
                    ops.push(DiffOp::WindowRemoved {
                        window_id: window_id.clone(),
                    });
                    ops.push(DiffOp::WindowAdded {
                        window: (*next_window).clone(),
                    });
                }

                if !shell_changed && previous_window.root != next_window.root {
                    ops.push(DiffOp::NodeReplaced {
                        element_id: previous_window.root.element_id.clone(),
                        node: next_window.root.clone(),
                    });
                }

                let previous_focus = focused_element_id(&previous_window.root);
                let next_focus = focused_element_id(&next_window.root);
                if previous_focus != next_focus {
                    ops.push(DiffOp::FocusChanged {
                        window_id: window_id.clone(),
                        element_id: next_focus,
                    });
                }
            }
            (None, None) => {}
        }
    }

    ops
}

fn same_window_shell(left: &WindowState, right: &WindowState) -> bool {
    left.window_id == right.window_id
        && left.pid == right.pid
        && left.process_name == right.process_name
        && left.title == right.title
        && left.bounds == right.bounds
        && left.backend == right.backend
        && left.confidence.to_bits() == right.confidence.to_bits()
}

fn focused_element_id(node: &ElementNode) -> Option<ElementId> {
    if node.states.focused {
        return Some(node.element_id.clone());
    }

    node.children.iter().find_map(focused_element_id)
}

#[cfg_attr(not(any(test, windows)), allow(dead_code))]
fn window_id_from_fingerprint(window_fingerprint: &str) -> WindowId {
    WindowId::from(format!("wnd-{:016x}", stable_hash(window_fingerprint)))
}

#[cfg_attr(not(any(test, windows)), allow(dead_code))]
fn element_id_from_locator(window_fingerprint: &str, path: &[LocatorSegment]) -> ElementId {
    let key = format!("{window_fingerprint}::{}", locator_path_key(path));
    ElementId::from(format!("elt-{:016x}", stable_hash(&key)))
}

#[cfg_attr(not(any(test, windows)), allow(dead_code))]
fn locator_path_key(path: &[LocatorSegment]) -> String {
    path.iter()
        .map(locator_segment_key)
        .collect::<Vec<_>>()
        .join("/")
}

#[cfg_attr(not(any(test, windows)), allow(dead_code))]
fn locator_segment_key(segment: &LocatorSegment) -> String {
    format!(
        "type={}|class={}|auto={}|name={}|ord={}",
        normalize_for_key(&segment.control_type),
        normalize_optional_for_key(segment.class_name.as_deref()),
        normalize_optional_for_key(segment.automation_id.as_deref()),
        normalize_optional_for_key(segment.name.as_deref()),
        segment
            .sibling_ordinal
            .map(|value| value.to_string())
            .unwrap_or_else(|| "-".to_owned())
    )
}

#[cfg_attr(not(any(test, windows)), allow(dead_code))]
fn normalize_for_key(value: &str) -> String {
    value.trim().to_lowercase()
}

#[cfg_attr(not(any(test, windows)), allow(dead_code))]
fn normalize_optional_for_key(value: Option<&str>) -> String {
    value
        .map(normalize_for_key)
        .filter(|value| !value.is_empty())
        .unwrap_or_else(|| "-".to_owned())
}

#[cfg_attr(not(any(test, windows)), allow(dead_code))]
fn stable_hash(value: &str) -> u64 {
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    value.hash(&mut hasher);
    hasher.finish()
}

fn window_native_handle(window: &WindowState) -> Option<u64> {
    window.root.native_window_handle
}

fn window_matches_hwnd(window: &WindowState, hwnd: usize) -> bool {
    window_native_handle(window) == Some(hwnd as u64)
}

fn stabilize_windows(previous: &[WindowState], current: &mut [WindowState]) {
    let mut used_previous = BTreeSet::new();

    for current_window in current.iter_mut() {
        let best_match = previous
            .iter()
            .enumerate()
            .filter(|(index, _)| !used_previous.contains(index))
            .filter_map(|(index, previous_window)| {
                window_match_score(previous_window, current_window).map(|score| (index, score))
            })
            .max_by_key(|(_, score)| *score);

        if let Some((index, _)) = best_match {
            used_previous.insert(index);
            stabilize_window(&previous[index], current_window);
        }
    }
}

fn window_match_score(previous: &WindowState, current: &WindowState) -> Option<i32> {
    let mut score = 0;

    match (
        window_native_handle(previous),
        window_native_handle(current),
    ) {
        (Some(left), Some(right)) if left == right => score += 1_000,
        (Some(_), Some(_)) => return None,
        _ => {}
    }

    if previous.pid != 0 && current.pid != 0 {
        if previous.pid == current.pid {
            score += 200;
        } else {
            return None;
        }
    }

    if previous.root.control_type != current.root.control_type {
        return None;
    }

    if previous.root.class_name == current.root.class_name && previous.root.class_name.is_some() {
        score += 80;
    }
    if previous.title == current.title && !previous.title.is_empty() {
        score += 60;
    }
    if previous.root.name == current.root.name && previous.root.name.is_some() {
        score += 60;
    }
    if previous.root.automation_id == current.root.automation_id
        && previous.root.automation_id.is_some()
    {
        score += 120;
    }

    (score >= 200).then_some(score)
}

fn stabilize_window(previous: &WindowState, current: &mut WindowState) {
    current.window_id = previous.window_id.clone();
    stabilize_element(&previous.root, &mut current.root, None);
}

fn stabilize_element(
    previous: &ElementNode,
    current: &mut ElementNode,
    parent_id: Option<&ElementId>,
) {
    current.element_id = previous.element_id.clone();
    current.parent_id = parent_id.cloned();

    let mut used_previous = BTreeSet::new();
    for current_child in current.children.iter_mut() {
        let best_match = previous
            .children
            .iter()
            .enumerate()
            .filter(|(index, _)| !used_previous.contains(index))
            .filter_map(|(index, previous_child)| {
                element_match_score(previous_child, current_child).map(|score| (index, score))
            })
            .max_by_key(|(_, score)| *score);

        if let Some((index, _)) = best_match {
            used_previous.insert(index);
            stabilize_element(
                &previous.children[index],
                current_child,
                Some(&current.element_id),
            );
        } else {
            reparent_subtree(current_child, Some(&current.element_id));
        }
    }
}

fn reparent_subtree(node: &mut ElementNode, parent_id: Option<&ElementId>) {
    node.parent_id = parent_id.cloned();
    for child in node.children.iter_mut() {
        reparent_subtree(child, Some(&node.element_id));
    }
}

fn element_match_score(previous: &ElementNode, current: &ElementNode) -> Option<i32> {
    if previous.control_type != current.control_type {
        return None;
    }

    let mut score = 100;
    match (previous.native_window_handle, current.native_window_handle) {
        (Some(left), Some(right)) if left == right => score += 1_000,
        (Some(_), Some(_)) => return None,
        _ => {}
    }

    if previous.automation_id == current.automation_id && previous.automation_id.is_some() {
        score += 300;
    }
    if previous.class_name == current.class_name && previous.class_name.is_some() {
        score += 120;
    }
    if previous.name == current.name && previous.name.is_some() {
        score += 120;
    }
    if previous.locator.path.last() == current.locator.path.last() {
        score += 40;
    }
    if previous.bounds == current.bounds {
        score += 40;
    }

    (score >= 220).then_some(score)
}

fn default_source() -> Arc<dyn ObservationSource> {
    #[cfg(windows)]
    {
        if windows_impl::interactive_desktop_available() {
            Arc::new(windows_impl::WindowsObservationSource)
        } else {
            Arc::new(UnsupportedWindowsSource::windows_session())
        }
    }

    #[cfg(not(windows))]
    {
        Arc::new(UnsupportedWindowsSource::non_windows())
    }
}

#[derive(Clone)]
struct UnsupportedWindowsSource {
    backend_id: &'static str,
    warning: &'static str,
}

impl UnsupportedWindowsSource {
    #[cfg(not(windows))]
    fn non_windows() -> Self {
        Self {
            backend_id: "windows-observer-unavailable",
            warning:
                "live Windows observer requires a Windows host and an interactive desktop session",
        }
    }

    #[cfg(windows)]
    fn windows_session() -> Self {
        Self {
            backend_id: "windows-observer-unavailable",
            warning: "live Windows observer requires an interactive Windows desktop session",
        }
    }
}

impl ObservationSource for UnsupportedWindowsSource {
    fn backend_id(&self) -> &'static str {
        self.backend_id
    }

    fn capabilities(&self) -> BackendCapabilities {
        BackendCapabilities {
            supports_live_observer: false,
            supports_uia: false,
            supports_msaa: false,
            supports_ocr_fallback: false,
            supports_artifacts: true,
        }
    }

    fn availability_warning(&self) -> Option<String> {
        Some(self.warning.to_owned())
    }

    fn capture_desktop(&self, _params: &BackendSessionParams) -> Result<Vec<WindowState>> {
        Ok(Vec::new())
    }

    fn capture_window(
        &self,
        _params: &BackendSessionParams,
        _hwnd: usize,
        _hint: HintSource,
    ) -> Result<Option<WindowState>> {
        Ok(None)
    }

    fn subscribe(&self, _params: &BackendSessionParams) -> Result<Option<RefreshSubscription>> {
        Ok(None)
    }
}

#[cfg(windows)]
mod windows_impl {
    use std::{
        collections::BTreeMap,
        ffi::c_void,
        mem::ManuallyDrop,
        ptr,
        sync::{Mutex, OnceLock},
        thread,
    };

    use anyhow::{anyhow, Context, Result};
    use image::{
        codecs::{jpeg::JpegEncoder, png::PngEncoder},
        ColorType, ImageEncoder,
    };
    use tokio::sync::mpsc;
    use vmui_platform::{BackendActionResult, BackendArtifact, BackendCapabilities};
    use vmui_protocol::{
        ActionKind, ActionRequest, ActionStatus, ActionTarget, BackendKind, CaptureFormat,
        ClickOptions, ElementId, ElementLocator, ElementNode, ElementStates, Locator,
        LocatorSegment, MouseButton, PropertyValue, Rect, RegionTarget, SendKeysOptions,
        SetValueOptions, WindowLocator, WindowState,
    };
    use windows::{
        core::{Interface, BOOL, BSTR},
        Win32::{
            Foundation::{HWND, LPARAM, RECT, RPC_E_CHANGED_MODE, WPARAM},
            Graphics::Gdi::{
                BitBlt, CreateCompatibleBitmap, CreateCompatibleDC, DeleteDC, DeleteObject, GetDC,
                GetDIBits, ReleaseDC, SelectObject, BITMAPINFO, BITMAPINFOHEADER, BI_RGB,
                DIB_RGB_COLORS, HGDIOBJ, SRCCOPY,
            },
            System::{
                Com::{
                    CoCreateInstance, CoInitializeEx, CoUninitialize, CLSCTX_INPROC_SERVER,
                    COINIT_MULTITHREADED,
                },
                StationsAndDesktops::{
                    CloseDesktop, OpenInputDesktop, DESKTOP_READOBJECTS, DESKTOP_SWITCHDESKTOP,
                },
                Threading::GetCurrentThreadId,
                Variant::{VariantClear, VARIANT, VARIANT_0_0, VARIANT_0_0_0, VT_I4},
            },
            UI::{
                Accessibility::{
                    AccessibleObjectFromEvent, AccessibleObjectFromWindow, CUIAutomation,
                    IAccessible, IUIAutomation, IUIAutomationElement, IUIAutomationInvokePattern,
                    IUIAutomationLegacyIAccessiblePattern, IUIAutomationTreeWalker,
                    IUIAutomationValuePattern, SetWinEventHook, UIA_ButtonControlTypeId,
                    UIA_CustomControlTypeId, UIA_EditControlTypeId, UIA_InvokePatternId,
                    UIA_LegacyIAccessiblePatternId, UIA_ListControlTypeId, UIA_MenuControlTypeId,
                    UIA_PaneControlTypeId, UIA_TabControlTypeId, UIA_TextControlTypeId,
                    UIA_TreeControlTypeId, UIA_ValuePatternId, UIA_WindowControlTypeId,
                    UnhookWinEvent,
                },
                Input::KeyboardAndMouse::{
                    SendInput, INPUT, INPUT_0, INPUT_KEYBOARD, INPUT_MOUSE, KEYBDINPUT,
                    KEYEVENTF_KEYUP, KEYEVENTF_UNICODE, MOUSEEVENTF_LEFTDOWN, MOUSEEVENTF_LEFTUP,
                    MOUSEEVENTF_MIDDLEDOWN, MOUSEEVENTF_MIDDLEUP, MOUSEEVENTF_RIGHTDOWN,
                    MOUSEEVENTF_RIGHTUP, MOUSEINPUT, VIRTUAL_KEY, VK_A, VK_BACK, VK_CONTROL,
                    VK_DELETE, VK_DOWN, VK_ESCAPE, VK_F4, VK_LEFT, VK_RETURN, VK_RIGHT, VK_TAB,
                    VK_UP,
                },
                WindowsAndMessaging::{
                    EnumWindows, GetMessageW, GetWindowRect, GetWindowTextW,
                    GetWindowThreadProcessId, IsWindowVisible, PostThreadMessageW, SetCursorPos,
                    SetForegroundWindow, CHILDID_SELF, EVENT_OBJECT_FOCUS, EVENT_OBJECT_HIDE,
                    EVENT_OBJECT_NAMECHANGE, EVENT_OBJECT_SELECTION, EVENT_OBJECT_SHOW,
                    EVENT_OBJECT_STATECHANGE, EVENT_SYSTEM_FOREGROUND, EVENT_SYSTEM_MINIMIZEEND,
                    MSG, OBJID_CLIENT, WINEVENT_OUTOFCONTEXT, WINEVENT_SKIPOWNPROCESS, WM_QUIT,
                },
            },
        },
    };

    use super::{
        element_id_from_locator, normalize_for_key, normalize_optional_for_key,
        window_id_from_fingerprint, HintSource, ObservationSource, RefreshRequest, RefreshScope,
        RefreshSubscription,
    };

    static HOOK_SENDERS: OnceLock<Mutex<BTreeMap<isize, mpsc::UnboundedSender<RefreshRequest>>>> =
        OnceLock::new();

    pub struct WindowsObservationSource;

    impl ObservationSource for WindowsObservationSource {
        fn backend_id(&self) -> &'static str {
            "windows-uia"
        }

        fn capabilities(&self) -> BackendCapabilities {
            BackendCapabilities {
                supports_live_observer: true,
                supports_uia: true,
                supports_msaa: true,
                supports_ocr_fallback: false,
                supports_artifacts: true,
            }
        }

        fn availability_warning(&self) -> Option<String> {
            None
        }

        fn capture_desktop(
            &self,
            params: &vmui_platform::BackendSessionParams,
        ) -> Result<Vec<WindowState>> {
            let hwnds = enumerate_windows()?;
            let mut windows = Vec::new();

            for hwnd in hwnds {
                if let Some(window) = self.capture_window(params, hwnd, HintSource::Uia)? {
                    windows.push(window);
                }
            }

            Ok(windows)
        }

        fn capture_window(
            &self,
            params: &vmui_platform::BackendSessionParams,
            hwnd: usize,
            hint: HintSource,
        ) -> Result<Option<WindowState>> {
            let hwnd = HWND(hwnd as *mut c_void);
            let metadata = match read_window_metadata(hwnd)? {
                Some(metadata) => metadata,
                None => return Ok(None),
            };
            let max_depth = if params.shallow { 1 } else { 4 };

            match capture_uia_window(hwnd, &metadata, max_depth, hint) {
                Ok(window) => Ok(Some(window)),
                Err(uia_error) => match capture_msaa_window(hwnd, &metadata) {
                    Ok(Some(window)) => Ok(Some(window)),
                    Ok(None) => Err(uia_error),
                    Err(msaa_error) => Err(anyhow!(
                        "UIA capture failed: {uia_error}; MSAA fallback failed: {msaa_error}"
                    )),
                },
            }
        }

        fn subscribe(
            &self,
            _params: &vmui_platform::BackendSessionParams,
        ) -> Result<Option<RefreshSubscription>> {
            let (tx, rx) = mpsc::unbounded_channel();
            let (thread_tx, thread_rx) = std::sync::mpsc::channel();
            let join = thread::spawn(move || run_hook_thread(tx, thread_tx));
            let ready = thread_rx
                .recv()
                .context("failed to receive WinEvent hook thread id")??;
            if ready.hook_count == 0 {
                let _ = join.join();
                return Ok(None);
            }

            Ok(Some(RefreshSubscription::new(rx, move || {
                let _ =
                    unsafe { PostThreadMessageW(ready.thread_id, WM_QUIT, WPARAM(0), LPARAM(0)) };
                let _ = join.join();
            })))
        }
    }

    pub fn interactive_desktop_available() -> bool {
        unsafe {
            match OpenInputDesktop(
                Default::default(),
                false,
                windows::Win32::System::StationsAndDesktops::DESKTOP_ACCESS_FLAGS(
                    DESKTOP_READOBJECTS.0 | DESKTOP_SWITCHDESKTOP.0,
                ),
            ) {
                Ok(handle) => {
                    let _ = CloseDesktop(handle);
                    true
                }
                Err(_) => false,
            }
        }
    }

    #[derive(Clone)]
    struct WindowMetadata {
        hwnd: usize,
        pid: u32,
        title: String,
        bounds: Rect,
    }

    #[derive(Clone)]
    struct RawObservedNode {
        backend: BackendKind,
        control_type: String,
        class_name: Option<String>,
        name: Option<String>,
        automation_id: Option<String>,
        native_window_handle: Option<u64>,
        bounds: Rect,
        properties: BTreeMap<String, PropertyValue>,
        states: ElementStates,
        children: Vec<RawObservedNode>,
        confidence: f32,
    }

    struct HookThreadReady {
        thread_id: u32,
        hook_count: usize,
    }

    fn enumerate_windows() -> Result<Vec<usize>> {
        unsafe extern "system" fn callback(hwnd: HWND, lparam: LPARAM) -> BOOL {
            let windows = &mut *(lparam.0 as *mut Vec<usize>);
            windows.push(hwnd.0 as usize);
            BOOL(1)
        }

        let mut windows = Vec::new();
        unsafe {
            EnumWindows(
                Some(callback),
                LPARAM((&mut windows as *mut Vec<usize>) as isize),
            )
            .context("EnumWindows failed")?;
        }
        Ok(windows)
    }

    fn read_window_metadata(hwnd: HWND) -> Result<Option<WindowMetadata>> {
        unsafe {
            if !IsWindowVisible(hwnd).as_bool() {
                return Ok(None);
            }

            let mut rect = RECT::default();
            GetWindowRect(hwnd, &mut rect).context("GetWindowRect failed")?;
            let bounds = rect_to_bounds(rect);
            if bounds.width <= 0 || bounds.height <= 0 {
                return Ok(None);
            }

            let mut title_buffer = vec![0u16; 512];
            let title_len = GetWindowTextW(hwnd, &mut title_buffer);
            let title = String::from_utf16_lossy(&title_buffer[..title_len as usize])
                .trim()
                .to_owned();

            let mut pid = 0u32;
            let _ = GetWindowThreadProcessId(hwnd, Some(&mut pid));

            Ok(Some(WindowMetadata {
                hwnd: hwnd.0 as usize,
                pid,
                title,
                bounds,
            }))
        }
    }

    fn capture_uia_window(
        hwnd: HWND,
        metadata: &WindowMetadata,
        max_depth: usize,
        _hint: HintSource,
    ) -> Result<WindowState> {
        let _com = ComApartment::mta()?;
        let automation: IUIAutomation =
            unsafe { CoCreateInstance(&CUIAutomation, None, CLSCTX_INPROC_SERVER) }
                .context("CoCreateInstance(CUIAutomation) failed")?;
        let element =
            unsafe { automation.ElementFromHandle(hwnd) }.context("ElementFromHandle failed")?;
        let walker =
            unsafe { automation.ControlViewWalker() }.context("ControlViewWalker failed")?;
        let raw_root = read_uia_tree(&walker, &element, max_depth)?;
        let title = preferred_window_title(
            metadata.title.as_str(),
            raw_root.name.as_deref(),
            raw_root.class_name.as_deref(),
            metadata.hwnd,
        );
        let window_fingerprint = build_window_fingerprint(metadata, &raw_root, &title);
        let root = materialize_root(raw_root, &window_fingerprint);

        Ok(WindowState {
            window_id: window_id_from_fingerprint(&window_fingerprint),
            pid: metadata.pid,
            process_name: None,
            title,
            bounds: metadata.bounds,
            backend: BackendKind::Uia,
            confidence: 1.0,
            root,
        })
    }

    fn read_uia_tree(
        walker: &IUIAutomationTreeWalker,
        element: &IUIAutomationElement,
        max_depth: usize,
    ) -> Result<RawObservedNode> {
        let control_type = control_type_name(
            unsafe { element.CurrentControlType() }
                .context("CurrentControlType failed")?
                .0,
        );
        let class_name = read_bstr(|| unsafe { element.CurrentClassName() });
        let name = read_bstr(|| unsafe { element.CurrentName() });
        let automation_id = read_bstr(|| unsafe { element.CurrentAutomationId() });
        let framework_id = read_bstr(|| unsafe { element.CurrentFrameworkId() });
        let native_window_handle = unsafe { element.CurrentNativeWindowHandle() }
            .ok()
            .map(|value| value.0 as u64)
            .filter(|value| *value != 0);
        let bounds = unsafe { element.CurrentBoundingRectangle() }
            .ok()
            .map(rect_to_bounds)
            .unwrap_or_default();
        let enabled = unsafe { element.CurrentIsEnabled() }
            .map(|value| value.as_bool())
            .unwrap_or(true);
        let visible = unsafe { element.CurrentIsOffscreen() }
            .map(|value| !value.as_bool())
            .unwrap_or(true);
        let focused = unsafe { element.CurrentHasKeyboardFocus() }
            .map(|value| value.as_bool())
            .unwrap_or(false);

        let mut properties = BTreeMap::new();
        if let Some(framework_id) = framework_id {
            properties.insert(
                "framework_id".to_owned(),
                PropertyValue::String(framework_id),
            );
        }

        let mut node = RawObservedNode {
            backend: BackendKind::Uia,
            control_type,
            class_name,
            name,
            automation_id,
            native_window_handle,
            bounds,
            properties,
            states: ElementStates {
                enabled,
                visible,
                focused,
                selected: false,
                expanded: false,
                toggled: false,
            },
            children: Vec::new(),
            confidence: 1.0,
        };

        if max_depth == 0 {
            return Ok(node);
        }

        let mut current = unsafe { walker.GetFirstChildElement(element) }.ok();
        while let Some(child) = current {
            let child_node = read_uia_tree(walker, &child, max_depth - 1)?;
            node.children.push(child_node);
            current = unsafe { walker.GetNextSiblingElement(&child) }.ok();
        }

        Ok(node)
    }

    fn capture_msaa_window(hwnd: HWND, metadata: &WindowMetadata) -> Result<Option<WindowState>> {
        let _com = ComApartment::mta()?;
        let mut raw = ptr::null_mut();
        unsafe {
            AccessibleObjectFromWindow(hwnd, OBJID_CLIENT.0 as u32, &IAccessible::IID, &mut raw)
        }
        .context("AccessibleObjectFromWindow failed")?;

        let accessible = unsafe { IAccessible::from_raw(raw as _) };
        let self_child = child_self_variant();
        let name = unsafe { accessible.get_accName(&self_child) }
            .ok()
            .and_then(bstr_to_option);
        let role_variant = unsafe { accessible.get_accRole(&self_child) }.ok();
        let state_variant = unsafe { accessible.get_accState(&self_child) }.ok();
        let role_name = role_variant
            .as_ref()
            .and_then(variant_i32)
            .map(|value| format!("msaa_role_{value}"))
            .unwrap_or_else(|| "MsaaAccessible".to_owned());
        let state_bits = state_variant
            .as_ref()
            .and_then(variant_i32)
            .unwrap_or_default();

        let mut properties = BTreeMap::new();
        properties.insert(
            "msaa_role".to_owned(),
            vmui_protocol::PropertyValue::String(role_name.clone()),
        );
        properties.insert(
            "msaa_state".to_owned(),
            vmui_protocol::PropertyValue::I64(state_bits as i64),
        );

        let title = preferred_window_title(
            metadata.title.as_str(),
            name.as_deref(),
            Some(&role_name),
            metadata.hwnd,
        );
        let window_fingerprint = build_msaa_window_fingerprint(metadata, &role_name, &title);
        let root = ElementNode {
            element_id: element_id_from_locator(&window_fingerprint, &[]),
            parent_id: None,
            backend: BackendKind::Msaa,
            control_type: role_name.clone(),
            class_name: None,
            name,
            automation_id: None,
            native_window_handle: Some(metadata.hwnd as u64),
            bounds: metadata.bounds,
            locator: Locator {
                window_fingerprint: window_fingerprint.clone(),
                path: Vec::new(),
            },
            properties,
            states: ElementStates {
                enabled: true,
                visible: true,
                focused: false,
                selected: false,
                expanded: false,
                toggled: false,
            },
            children: Vec::new(),
            confidence: 0.45,
        };

        Ok(Some(WindowState {
            window_id: window_id_from_fingerprint(&window_fingerprint),
            pid: metadata.pid,
            process_name: None,
            title,
            bounds: metadata.bounds,
            backend: BackendKind::Msaa,
            confidence: 0.45,
            root,
        }))
    }

    fn preferred_window_title(
        explicit_title: &str,
        semantic_name: Option<&str>,
        fallback_label: Option<&str>,
        hwnd: usize,
    ) -> String {
        [Some(explicit_title), semantic_name, fallback_label]
            .into_iter()
            .flatten()
            .map(str::trim)
            .find(|value| !value.is_empty())
            .map(ToOwned::to_owned)
            .unwrap_or_else(|| format!("Window {hwnd:016x}"))
    }

    fn build_window_fingerprint(
        metadata: &WindowMetadata,
        root: &RawObservedNode,
        title: &str,
    ) -> String {
        format!(
            "pid={}:hwnd={:016x}:class={}:title={}:root_name={}",
            metadata.pid,
            metadata.hwnd,
            normalize_optional_for_key(root.class_name.as_deref()),
            normalize_for_key(title),
            normalize_optional_for_key(root.name.as_deref()),
        )
    }

    fn build_msaa_window_fingerprint(
        metadata: &WindowMetadata,
        role_name: &str,
        title: &str,
    ) -> String {
        format!(
            "pid={}:hwnd={:016x}:msaa_role={}:title={}",
            metadata.pid,
            metadata.hwnd,
            normalize_for_key(role_name),
            normalize_for_key(title),
        )
    }

    fn materialize_root(raw_root: RawObservedNode, window_fingerprint: &str) -> ElementNode {
        materialize_node(raw_root, window_fingerprint, None, Vec::new())
    }

    fn materialize_node(
        raw: RawObservedNode,
        window_fingerprint: &str,
        parent_id: Option<ElementId>,
        path: Vec<LocatorSegment>,
    ) -> ElementNode {
        let element_id = element_id_from_locator(window_fingerprint, &path);
        let parent_id_for_children = Some(element_id.clone());
        let child_segments = build_child_locator_segments(&raw.children);
        let mut children = Vec::with_capacity(raw.children.len());

        for (child_raw, segment) in raw.children.into_iter().zip(child_segments.into_iter()) {
            let mut child_path = path.clone();
            child_path.push(segment);
            children.push(materialize_node(
                child_raw,
                window_fingerprint,
                parent_id_for_children.clone(),
                child_path,
            ));
        }

        ElementNode {
            element_id,
            parent_id,
            backend: raw.backend,
            control_type: raw.control_type,
            class_name: raw.class_name,
            name: raw.name,
            automation_id: raw.automation_id,
            native_window_handle: raw.native_window_handle,
            bounds: raw.bounds,
            locator: Locator {
                window_fingerprint: window_fingerprint.to_owned(),
                path,
            },
            properties: raw.properties,
            states: raw.states,
            children,
            confidence: raw.confidence,
        }
    }

    fn build_child_locator_segments(children: &[RawObservedNode]) -> Vec<LocatorSegment> {
        let mut counts = BTreeMap::new();
        for child in children {
            *counts
                .entry(locator_signature_for_raw(child))
                .or_insert(0usize) += 1;
        }

        let mut seen = BTreeMap::new();
        children
            .iter()
            .map(|child| {
                let signature = locator_signature_for_raw(child);
                let total = counts.get(&signature).copied().unwrap_or(1);
                let ordinal = if total > 1 {
                    let next = seen.entry(signature).or_insert(0usize);
                    let ordinal = *next as u32;
                    *next += 1;
                    Some(ordinal)
                } else {
                    None
                };

                LocatorSegment {
                    control_type: child.control_type.clone(),
                    class_name: child.class_name.clone(),
                    automation_id: child.automation_id.clone(),
                    name: child.name.clone(),
                    sibling_ordinal: ordinal,
                }
            })
            .collect()
    }

    fn locator_signature_for_raw(node: &RawObservedNode) -> String {
        format!(
            "type={}|class={}|auto={}|name={}",
            normalize_for_key(&node.control_type),
            normalize_optional_for_key(node.class_name.as_deref()),
            normalize_optional_for_key(node.automation_id.as_deref()),
            normalize_optional_for_key(node.name.as_deref()),
        )
    }

    pub(super) fn perform_action(action: ActionRequest) -> Result<BackendActionResult> {
        match action.kind.clone() {
            ActionKind::FocusWindow => focus_window_action(action),
            ActionKind::Invoke => invoke_action(action),
            ActionKind::ClickElement(options) => click_element_action(action, options),
            ActionKind::SetValue(options) => set_value_action(action, options),
            ActionKind::SendKeys(options) => send_keys_action(action, options),
            ActionKind::CaptureRegion(options) => capture_region_action(action, options.format),
            ActionKind::OcrRegion(_) => Ok(super::unsupported_action(
                action,
                "ocr fallback is not available on the current Windows backend",
            )),
            ActionKind::ListWindows
            | ActionKind::GetTree(_)
            | ActionKind::WaitFor(_)
            | ActionKind::WriteArtifact(_) => Ok(super::unsupported_action(
                action,
                "this action is handled by the daemon state executor",
            )),
        }
    }

    struct ActionWindow {
        hwnd: HWND,
        window: WindowState,
    }

    struct ResolvedActionElement<'a> {
        window: &'a ActionWindow,
        node: &'a ElementNode,
    }

    struct AutomationContext {
        _apartment: ComApartment,
        automation: IUIAutomation,
        walker: IUIAutomationTreeWalker,
    }

    impl AutomationContext {
        fn new() -> Result<Self> {
            let apartment = ComApartment::mta()?;
            let automation: IUIAutomation =
                unsafe { CoCreateInstance(&CUIAutomation, None, CLSCTX_INPROC_SERVER) }
                    .context("CoCreateInstance(CUIAutomation) failed")?;
            let walker =
                unsafe { automation.ControlViewWalker() }.context("ControlViewWalker failed")?;
            Ok(Self {
                _apartment: apartment,
                automation,
                walker,
            })
        }

        fn root_element_from_handle(&self, hwnd: HWND) -> Result<IUIAutomationElement> {
            unsafe { self.automation.ElementFromHandle(hwnd) }.context("ElementFromHandle failed")
        }

        fn resolve_element_for_locator(
            &self,
            hwnd: HWND,
            locator: &Locator,
        ) -> Result<Option<IUIAutomationElement>> {
            let root = self.root_element_from_handle(hwnd)?;
            if locator.path.is_empty() {
                return Ok(Some(root));
            }

            let mut current = root;
            for segment in &locator.path {
                let matches = matching_children(&self.walker, &current, segment)?;
                let index = segment.sibling_ordinal.unwrap_or(0) as usize;
                let Some(next) = matches.into_iter().nth(index) else {
                    return Ok(None);
                };
                current = next;
            }

            Ok(Some(current))
        }
    }

    fn focus_window_action(action: ActionRequest) -> Result<BackendActionResult> {
        let automation = AutomationContext::new()?;
        let windows = capture_action_windows(4)?;
        let Some(target) = resolve_action_window_from_target(&windows, &action.target) else {
            return Ok(failed_action(action, "window target was not found"));
        };

        if focus_action_window(target, Some(&automation)).is_ok() {
            Ok(completed_action(
                action.action_id,
                "focused target window via hwnd/UIA",
            ))
        } else {
            Ok(failed_action(
                action,
                "failed to bring target window to the foreground",
            ))
        }
    }

    fn invoke_action(action: ActionRequest) -> Result<BackendActionResult> {
        let automation = AutomationContext::new()?;
        let windows = capture_action_windows(8)?;
        let Some(target) = resolve_action_node_from_target(&windows, &action.target) else {
            return Ok(failed_action(action, "invoke target was not found"));
        };
        let semantic_message = if let Some(element) =
            automation.resolve_element_for_locator(target.window.hwnd, &target.node.locator)?
        {
            try_invoke_semantically(&element)?
        } else {
            None
        };

        if let Some(message) = semantic_message {
            return Ok(completed_action(action.action_id, message));
        }

        focus_action_target(&automation, &windows, &action.target)?;
        click_bounds_center(target.node.bounds, MouseButton::Left, 1)?;
        Ok(completed_action(
            action.action_id,
            "fallback=coordinate-click reason=semantic-invoke-unavailable",
        ))
    }

    fn click_element_action(
        action: ActionRequest,
        options: ClickOptions,
    ) -> Result<BackendActionResult> {
        let automation = AutomationContext::new()?;
        let windows = capture_action_windows(8)?;
        let Some(target) = resolve_action_node_from_target(&windows, &action.target) else {
            return Ok(failed_action(action, "click target was not found"));
        };

        if options.button == MouseButton::Left && options.clicks == 1 {
            if let Some(element) =
                automation.resolve_element_for_locator(target.window.hwnd, &target.node.locator)?
            {
                if let Some(message) = try_invoke_semantically(&element)? {
                    return Ok(completed_action(action.action_id, message));
                }
            }
        }

        focus_action_target(&automation, &windows, &action.target)?;
        click_bounds_center(target.node.bounds, options.button, options.clicks)?;
        Ok(completed_action(
            action.action_id,
            "fallback=coordinate-click",
        ))
    }

    fn set_value_action(
        action: ActionRequest,
        options: SetValueOptions,
    ) -> Result<BackendActionResult> {
        let automation = AutomationContext::new()?;
        let windows = capture_action_windows(8)?;
        let Some(target) = resolve_action_node_from_target(&windows, &action.target) else {
            return Ok(failed_action(action, "set_value target was not found"));
        };

        if let Some(element) =
            automation.resolve_element_for_locator(target.window.hwnd, &target.node.locator)?
        {
            if try_set_value_pattern(&element, &options.value)? {
                return Ok(completed_action(action.action_id, "semantic=value-pattern"));
            }
        }
        focus_action_target(&automation, &windows, &action.target)?;

        if options.clear_first {
            send_key_chord(&[VK_CONTROL, VK_A])?;
            send_virtual_key(VK_DELETE)?;
        }
        send_text(&options.value)?;
        Ok(completed_action(
            action.action_id,
            "fallback=send-keys reason=value-pattern-unavailable",
        ))
    }

    fn send_keys_action(
        action: ActionRequest,
        options: SendKeysOptions,
    ) -> Result<BackendActionResult> {
        let automation = AutomationContext::new()?;
        let windows = capture_action_windows(8)?;
        focus_action_target(&automation, &windows, &action.target)?;
        send_key_sequence(&options.keys)?;
        Ok(completed_action(
            action.action_id,
            "sent keys via SendInput",
        ))
    }

    fn capture_region_action(
        action: ActionRequest,
        format: CaptureFormat,
    ) -> Result<BackendActionResult> {
        let windows = capture_action_windows(8)?;
        let Some(bounds) = resolve_capture_bounds(&windows, &action.target) else {
            return Ok(failed_action(action, "capture target was not found"));
        };
        let artifact = capture_screen_region(bounds, format)?;
        Ok(BackendActionResult {
            action_id: action.action_id,
            ok: true,
            status: ActionStatus::Completed,
            message: "captured scoped region".to_owned(),
            artifacts: vec![artifact],
        })
    }

    fn completed_action(
        action_id: vmui_protocol::ActionId,
        message: impl Into<String>,
    ) -> BackendActionResult {
        BackendActionResult {
            action_id,
            ok: true,
            status: ActionStatus::Completed,
            message: message.into(),
            artifacts: Vec::new(),
        }
    }

    fn failed_action(action: ActionRequest, message: impl Into<String>) -> BackendActionResult {
        BackendActionResult {
            action_id: action.action_id,
            ok: false,
            status: ActionStatus::Failed,
            message: message.into(),
            artifacts: Vec::new(),
        }
    }

    fn capture_action_windows(max_depth: usize) -> Result<Vec<ActionWindow>> {
        let hwnds = enumerate_windows()?;
        let mut windows = Vec::new();

        for raw_hwnd in hwnds {
            let hwnd = HWND(raw_hwnd as *mut c_void);
            let Some(metadata) = read_window_metadata(hwnd)? else {
                continue;
            };

            let window = match capture_uia_window(hwnd, &metadata, max_depth, HintSource::Uia) {
                Ok(window) => window,
                Err(_) => match capture_msaa_window(hwnd, &metadata)? {
                    Some(window) => window,
                    None => continue,
                },
            };

            windows.push(ActionWindow { hwnd, window });
        }

        Ok(windows)
    }

    fn resolve_action_window_from_target<'a>(
        windows: &'a [ActionWindow],
        target: &ActionTarget,
    ) -> Option<&'a ActionWindow> {
        match target {
            ActionTarget::Window(locator) => resolve_action_window(windows, locator),
            ActionTarget::Element(locator) => {
                resolve_action_element(windows, locator).map(|resolved| resolved.window)
            }
            ActionTarget::Region(RegionTarget {
                window_id: Some(window_id),
                ..
            }) => windows
                .iter()
                .find(|window| &window.window.window_id == window_id),
            ActionTarget::Region(_) => None,
            ActionTarget::Desktop => (windows.len() == 1).then_some(&windows[0]),
        }
    }

    fn resolve_action_window<'a>(
        windows: &'a [ActionWindow],
        locator: &WindowLocator,
    ) -> Option<&'a ActionWindow> {
        if let Some(window_id) = &locator.window_id {
            return windows
                .iter()
                .find(|window| &window.window.window_id == window_id);
        }

        let mut matches = windows.iter().filter(|window| {
            locator
                .title
                .as_ref()
                .map(|title| &window.window.title == title)
                .unwrap_or(true)
                && locator
                    .pid
                    .map(|pid| window.window.pid == pid)
                    .unwrap_or(true)
        });

        match (
            locator.title.is_some() || locator.pid.is_some(),
            windows.len(),
        ) {
            (true, _) => matches.next(),
            (false, 1) => windows.first(),
            _ => None,
        }
    }

    fn resolve_action_node_from_target<'a>(
        windows: &'a [ActionWindow],
        target: &ActionTarget,
    ) -> Option<ResolvedActionElement<'a>> {
        match target {
            ActionTarget::Element(locator) => resolve_action_element(windows, locator),
            ActionTarget::Window(locator) => {
                resolve_action_window(windows, locator).map(|window| ResolvedActionElement {
                    window,
                    node: &window.window.root,
                })
            }
            _ => None,
        }
    }

    fn resolve_action_element<'a>(
        windows: &'a [ActionWindow],
        locator: &ElementLocator,
    ) -> Option<ResolvedActionElement<'a>> {
        if let Some(element_id) = &locator.element_id {
            if let Some(found) = windows.iter().find_map(|window| {
                find_node_by_id(&window.window.root, element_id)
                    .map(|node| ResolvedActionElement { window, node })
            }) {
                return Some(found);
            }
        }

        locator.locator.as_ref().and_then(|locator| {
            windows.iter().find_map(|window| {
                if window.window.root.locator.window_fingerprint != locator.window_fingerprint {
                    return None;
                }

                find_node_by_locator(&window.window.root, locator)
                    .map(|node| ResolvedActionElement { window, node })
            })
        })
    }

    fn find_node_by_id<'a>(
        node: &'a ElementNode,
        element_id: &ElementId,
    ) -> Option<&'a ElementNode> {
        if &node.element_id == element_id {
            return Some(node);
        }

        node.children
            .iter()
            .find_map(|child| find_node_by_id(child, element_id))
    }

    fn find_node_by_locator<'a>(
        node: &'a ElementNode,
        locator: &Locator,
    ) -> Option<&'a ElementNode> {
        if &node.locator == locator {
            return Some(node);
        }

        node.children
            .iter()
            .find_map(|child| find_node_by_locator(child, locator))
    }

    fn matching_children(
        walker: &IUIAutomationTreeWalker,
        parent: &IUIAutomationElement,
        segment: &LocatorSegment,
    ) -> Result<Vec<IUIAutomationElement>> {
        let mut current = unsafe { walker.GetFirstChildElement(parent) }.ok();
        let mut matches = Vec::new();

        while let Some(child) = current {
            if element_matches_segment(&child, segment)? {
                matches.push(child.clone());
            }
            current = unsafe { walker.GetNextSiblingElement(&child) }.ok();
        }

        Ok(matches)
    }

    fn element_matches_segment(
        element: &IUIAutomationElement,
        segment: &LocatorSegment,
    ) -> Result<bool> {
        let control_type = control_type_name(
            unsafe { element.CurrentControlType() }
                .context("CurrentControlType failed")?
                .0,
        );
        if control_type != segment.control_type {
            return Ok(false);
        }

        let class_name = read_bstr(|| unsafe { element.CurrentClassName() });
        let automation_id = read_bstr(|| unsafe { element.CurrentAutomationId() });
        let name = read_bstr(|| unsafe { element.CurrentName() });

        Ok(segment
            .class_name
            .as_ref()
            .map(|value| class_name.as_ref() == Some(value))
            .unwrap_or(true)
            && segment
                .automation_id
                .as_ref()
                .map(|value| automation_id.as_ref() == Some(value))
                .unwrap_or(true)
            && segment
                .name
                .as_ref()
                .map(|value| name.as_ref() == Some(value))
                .unwrap_or(true))
    }

    fn try_invoke_semantically(element: &IUIAutomationElement) -> Result<Option<String>> {
        if let Ok(pattern) = unsafe {
            element.GetCurrentPatternAs::<IUIAutomationInvokePattern>(UIA_InvokePatternId)
        } {
            unsafe { pattern.Invoke() }.context("InvokePattern::Invoke failed")?;
            return Ok(Some("semantic=invoke-pattern".to_owned()));
        }

        if let Ok(pattern) = unsafe {
            element.GetCurrentPatternAs::<IUIAutomationLegacyIAccessiblePattern>(
                UIA_LegacyIAccessiblePatternId,
            )
        } {
            unsafe { pattern.DoDefaultAction() }
                .context("LegacyIAccessible::DoDefaultAction failed")?;
            return Ok(Some("semantic=legacy-default-action".to_owned()));
        }

        Ok(None)
    }

    fn try_set_value_pattern(element: &IUIAutomationElement, value: &str) -> Result<bool> {
        let Ok(pattern) = (unsafe {
            element.GetCurrentPatternAs::<IUIAutomationValuePattern>(UIA_ValuePatternId)
        }) else {
            return Ok(false);
        };

        if unsafe { pattern.CurrentIsReadOnly() }
            .map(|value| value.as_bool())
            .unwrap_or(false)
        {
            return Ok(false);
        }

        let value = BSTR::from(value);
        unsafe { pattern.SetValue(&value) }.context("ValuePattern::SetValue failed")?;
        Ok(true)
    }

    fn focus_action_target(
        automation: &AutomationContext,
        windows: &[ActionWindow],
        target: &ActionTarget,
    ) -> Result<()> {
        match target {
            ActionTarget::Element(locator) => {
                let Some(resolved) = resolve_action_element(windows, locator) else {
                    return Err(anyhow!("element target was not found"));
                };
                if let Some(element) = automation
                    .resolve_element_for_locator(resolved.window.hwnd, &resolved.node.locator)?
                {
                    unsafe { element.SetFocus() }.context("SetFocus failed")?;
                } else {
                    focus_action_window(resolved.window, Some(automation))?;
                }
            }
            _ => {
                let Some(window) = resolve_action_window_from_target(windows, target) else {
                    return Err(anyhow!("window target was not found"));
                };
                focus_action_window(window, Some(automation))?;
            }
        }

        thread::sleep(std::time::Duration::from_millis(50));
        Ok(())
    }

    fn focus_action_window(
        window: &ActionWindow,
        automation: Option<&AutomationContext>,
    ) -> Result<()> {
        let foreground_ok = unsafe { SetForegroundWindow(window.hwnd) }.as_bool();
        let semantic_ok = automation
            .and_then(|automation| {
                automation
                    .root_element_from_handle(window.hwnd)
                    .and_then(|element| unsafe { element.SetFocus() }.context("SetFocus failed"))
                    .ok()
            })
            .is_some();

        if foreground_ok || semantic_ok {
            Ok(())
        } else {
            Err(anyhow!("failed to focus target window"))
        }
    }

    fn click_bounds_center(bounds: Rect, button: MouseButton, clicks: u8) -> Result<()> {
        if bounds.width <= 0 || bounds.height <= 0 {
            return Err(anyhow!("target bounds are invalid for coordinate fallback"));
        }

        let x = bounds.left + bounds.width / 2;
        let y = bounds.top + bounds.height / 2;
        unsafe { SetCursorPos(x, y) }.context("SetCursorPos failed")?;
        thread::sleep(std::time::Duration::from_millis(40));

        for _ in 0..clicks.max(1) {
            let (down, up) = match button {
                MouseButton::Left => (MOUSEEVENTF_LEFTDOWN, MOUSEEVENTF_LEFTUP),
                MouseButton::Right => (MOUSEEVENTF_RIGHTDOWN, MOUSEEVENTF_RIGHTUP),
                MouseButton::Middle => (MOUSEEVENTF_MIDDLEDOWN, MOUSEEVENTF_MIDDLEUP),
            };
            send_inputs(&[mouse_input(down), mouse_input(up)])?;
            thread::sleep(std::time::Duration::from_millis(50));
        }

        Ok(())
    }

    fn resolve_capture_bounds(windows: &[ActionWindow], target: &ActionTarget) -> Option<Rect> {
        match target {
            ActionTarget::Region(region) => {
                let mut bounds = region.bounds;
                if let Some(window_id) = &region.window_id {
                    let window = windows
                        .iter()
                        .find(|window| &window.window.window_id == window_id)?;
                    bounds.left += window.window.bounds.left;
                    bounds.top += window.window.bounds.top;
                }
                Some(bounds)
            }
            ActionTarget::Window(locator) => {
                resolve_action_window(windows, locator).map(|window| window.window.bounds)
            }
            ActionTarget::Element(locator) => {
                resolve_action_element(windows, locator).map(|resolved| resolved.node.bounds)
            }
            ActionTarget::Desktop => None,
        }
    }

    fn capture_screen_region(bounds: Rect, format: CaptureFormat) -> Result<BackendArtifact> {
        if bounds.width <= 0 || bounds.height <= 0 {
            return Err(anyhow!("capture bounds are invalid"));
        }

        let screen_dc = unsafe { GetDC(None) };
        if screen_dc.0.is_null() {
            return Err(anyhow!("GetDC failed"));
        }

        let memory_dc = unsafe { CreateCompatibleDC(Some(screen_dc)) };
        if memory_dc.0.is_null() {
            unsafe {
                let _ = ReleaseDC(None, screen_dc);
            }
            return Err(anyhow!("CreateCompatibleDC failed"));
        }

        let bitmap = unsafe { CreateCompatibleBitmap(screen_dc, bounds.width, bounds.height) };
        if bitmap.0.is_null() {
            unsafe {
                let _ = DeleteDC(memory_dc);
                let _ = ReleaseDC(None, screen_dc);
            }
            return Err(anyhow!("CreateCompatibleBitmap failed"));
        }

        let old_object = unsafe { SelectObject(memory_dc, HGDIOBJ(bitmap.0)) };
        let copy_result = unsafe {
            BitBlt(
                memory_dc,
                0,
                0,
                bounds.width,
                bounds.height,
                Some(screen_dc),
                bounds.left,
                bounds.top,
                SRCCOPY,
            )
        };

        let mut bitmap_info = BITMAPINFO::default();
        bitmap_info.bmiHeader = BITMAPINFOHEADER {
            biSize: std::mem::size_of::<BITMAPINFOHEADER>() as u32,
            biWidth: bounds.width,
            biHeight: -bounds.height,
            biPlanes: 1,
            biBitCount: 32,
            biCompression: BI_RGB.0,
            ..Default::default()
        };
        let mut pixels = vec![0u8; (bounds.width * bounds.height * 4) as usize];
        let scan_lines = unsafe {
            GetDIBits(
                memory_dc,
                bitmap,
                0,
                bounds.height as u32,
                Some(pixels.as_mut_ptr() as *mut c_void),
                &mut bitmap_info,
                DIB_RGB_COLORS,
            )
        };

        unsafe {
            let _ = SelectObject(memory_dc, old_object);
            let _ = DeleteObject(HGDIOBJ(bitmap.0));
            let _ = DeleteDC(memory_dc);
            let _ = ReleaseDC(None, screen_dc);
        }

        copy_result.context("BitBlt failed")?;
        if scan_lines == 0 {
            return Err(anyhow!("GetDIBits failed"));
        }

        encode_capture_artifact(bounds.width as u32, bounds.height as u32, &pixels, format)
    }

    fn encode_capture_artifact(
        width: u32,
        height: u32,
        pixels: &[u8],
        format: CaptureFormat,
    ) -> Result<BackendArtifact> {
        let rgba = pixels
            .chunks_exact(4)
            .flat_map(|chunk| [chunk[2], chunk[1], chunk[0], 0xff])
            .collect::<Vec<_>>();

        match format {
            CaptureFormat::Png => {
                let mut bytes = Vec::new();
                PngEncoder::new(&mut bytes)
                    .write_image(&rgba, width, height, ColorType::Rgba8.into())
                    .context("failed to encode PNG screenshot")?;
                Ok(BackendArtifact {
                    kind: "screenshot-png".to_owned(),
                    mime_type: "image/png".to_owned(),
                    bytes,
                })
            }
            CaptureFormat::Jpeg => {
                let rgb = rgba
                    .chunks_exact(4)
                    .flat_map(|chunk| [chunk[0], chunk[1], chunk[2]])
                    .collect::<Vec<_>>();
                let mut bytes = Vec::new();
                JpegEncoder::new_with_quality(&mut bytes, 90)
                    .encode(&rgb, width, height, ColorType::Rgb8.into())
                    .context("failed to encode JPEG screenshot")?;
                Ok(BackendArtifact {
                    kind: "screenshot-jpeg".to_owned(),
                    mime_type: "image/jpeg".to_owned(),
                    bytes,
                })
            }
        }
    }

    fn send_key_sequence(keys: &str) -> Result<()> {
        let mut chars = keys.chars().peekable();
        while let Some(ch) = chars.next() {
            if ch == '{' {
                let mut token = String::new();
                let mut closed = false;
                while let Some(next) = chars.next() {
                    if next == '}' {
                        closed = true;
                        break;
                    }
                    token.push(next);
                }
                if !closed {
                    return Err(anyhow!("unterminated key token"));
                }
                send_named_key(&token)?;
                continue;
            }

            match ch {
                '\n' => send_virtual_key(VK_RETURN)?,
                '\t' => send_virtual_key(VK_TAB)?,
                _ => send_text_char(ch)?,
            }
        }

        Ok(())
    }

    fn send_named_key(token: &str) -> Result<()> {
        let token = token.trim().to_ascii_uppercase();
        match token.as_str() {
            "ENTER" => send_virtual_key(VK_RETURN),
            "TAB" => send_virtual_key(VK_TAB),
            "ESC" | "ESCAPE" => send_virtual_key(VK_ESCAPE),
            "BACKSPACE" => send_virtual_key(VK_BACK),
            "DELETE" | "DEL" => send_virtual_key(VK_DELETE),
            "LEFT" => send_virtual_key(VK_LEFT),
            "RIGHT" => send_virtual_key(VK_RIGHT),
            "UP" => send_virtual_key(VK_UP),
            "DOWN" => send_virtual_key(VK_DOWN),
            "F4" => send_virtual_key(VK_F4),
            _ => Err(anyhow!("unsupported key token `{token}`")),
        }
    }

    fn send_text(text: &str) -> Result<()> {
        for ch in text.chars() {
            send_text_char(ch)?;
        }
        Ok(())
    }

    fn send_text_char(ch: char) -> Result<()> {
        let mut encoded = [0u16; 2];
        for unit in ch.encode_utf16(&mut encoded) {
            send_inputs(&[
                keyboard_unicode_input(*unit, false),
                keyboard_unicode_input(*unit, true),
            ])?;
        }
        Ok(())
    }

    fn send_key_chord(keys: &[VIRTUAL_KEY]) -> Result<()> {
        let mut inputs = Vec::with_capacity(keys.len() * 2);
        for key in keys {
            inputs.push(keyboard_virtual_input(*key, false));
        }
        for key in keys.iter().rev() {
            inputs.push(keyboard_virtual_input(*key, true));
        }
        send_inputs(&inputs)
    }

    fn send_virtual_key(key: VIRTUAL_KEY) -> Result<()> {
        send_inputs(&[
            keyboard_virtual_input(key, false),
            keyboard_virtual_input(key, true),
        ])
    }

    fn send_inputs(inputs: &[INPUT]) -> Result<()> {
        let sent = unsafe { SendInput(inputs, std::mem::size_of::<INPUT>() as i32) };
        if sent != inputs.len() as u32 {
            return Err(anyhow!("SendInput sent {sent} of {} events", inputs.len()));
        }
        Ok(())
    }

    fn keyboard_virtual_input(key: VIRTUAL_KEY, key_up: bool) -> INPUT {
        INPUT {
            r#type: INPUT_KEYBOARD,
            Anonymous: INPUT_0 {
                ki: KEYBDINPUT {
                    wVk: key,
                    wScan: 0,
                    dwFlags: if key_up {
                        KEYEVENTF_KEYUP
                    } else {
                        Default::default()
                    },
                    time: 0,
                    dwExtraInfo: 0,
                },
            },
        }
    }

    fn keyboard_unicode_input(code_unit: u16, key_up: bool) -> INPUT {
        INPUT {
            r#type: INPUT_KEYBOARD,
            Anonymous: INPUT_0 {
                ki: KEYBDINPUT {
                    wVk: VIRTUAL_KEY(0),
                    wScan: code_unit,
                    dwFlags: if key_up {
                        KEYEVENTF_UNICODE | KEYEVENTF_KEYUP
                    } else {
                        KEYEVENTF_UNICODE
                    },
                    time: 0,
                    dwExtraInfo: 0,
                },
            },
        }
    }

    fn mouse_input(flags: windows::Win32::UI::Input::KeyboardAndMouse::MOUSE_EVENT_FLAGS) -> INPUT {
        INPUT {
            r#type: INPUT_MOUSE,
            Anonymous: INPUT_0 {
                mi: MOUSEINPUT {
                    dx: 0,
                    dy: 0,
                    mouseData: 0,
                    dwFlags: flags,
                    time: 0,
                    dwExtraInfo: 0,
                },
            },
        }
    }

    fn run_hook_thread(
        sender: mpsc::UnboundedSender<RefreshRequest>,
        ready: std::sync::mpsc::Sender<Result<HookThreadReady>>,
    ) {
        let apartment = match ComApartment::mta() {
            Ok(apartment) => apartment,
            Err(error) => {
                let _ = ready.send(Err(error));
                return;
            }
        };

        let _apartment = apartment;
        let thread_id = unsafe { GetCurrentThreadId() };

        let events = [
            EVENT_SYSTEM_FOREGROUND,
            EVENT_OBJECT_FOCUS,
            EVENT_OBJECT_NAMECHANGE,
            EVENT_OBJECT_SHOW,
            EVENT_OBJECT_HIDE,
            EVENT_OBJECT_STATECHANGE,
            EVENT_OBJECT_SELECTION,
            EVENT_SYSTEM_MINIMIZEEND,
        ];

        let mut hooks = Vec::new();
        for event in events {
            let hook = unsafe {
                SetWinEventHook(
                    event,
                    event,
                    None,
                    Some(winevent_callback),
                    0,
                    0,
                    WINEVENT_OUTOFCONTEXT | WINEVENT_SKIPOWNPROCESS,
                )
            };
            if !hook.is_invalid() {
                hook_sender_map()
                    .lock()
                    .expect("winevent sender map poisoned")
                    .insert(hook.0 as isize, sender.clone());
                hooks.push(hook);
            }
        }

        if ready
            .send(Ok(HookThreadReady {
                thread_id,
                hook_count: hooks.len(),
            }))
            .is_err()
        {
            let mut map = hook_sender_map()
                .lock()
                .expect("winevent sender map poisoned");
            for hook in hooks {
                map.remove(&(hook.0 as isize));
                unsafe {
                    let _ = UnhookWinEvent(hook);
                }
            }
            return;
        }

        if hooks.is_empty() {
            return;
        }

        let mut message = MSG::default();
        loop {
            let status = unsafe { GetMessageW(&mut message, None, 0, 0) };
            let code = status.0;
            if code <= 0 {
                break;
            }
        }

        let mut map = hook_sender_map()
            .lock()
            .expect("winevent sender map poisoned");
        for hook in hooks {
            map.remove(&(hook.0 as isize));
            unsafe {
                let _ = UnhookWinEvent(hook);
            }
        }
    }

    unsafe extern "system" fn winevent_callback(
        hook: windows::Win32::UI::Accessibility::HWINEVENTHOOK,
        event: u32,
        hwnd: HWND,
        idobject: i32,
        idchild: i32,
        _ideventthread: u32,
        _dwmseventtime: u32,
    ) {
        let sender = {
            let map = hook_sender_map()
                .lock()
                .expect("winevent sender map poisoned");
            map.get(&(hook.0 as isize)).cloned()
        };

        let Some(sender) = sender else {
            return;
        };

        let scope = if hwnd.is_invalid() {
            RefreshScope::Desktop
        } else {
            RefreshScope::Window {
                hwnd: hwnd.0 as usize,
            }
        };
        let hint = classify_event_source(hwnd, idobject as u32, idchild as u32);
        let reason = format!("winevent:{event}");
        let request = match scope {
            RefreshScope::Desktop => RefreshRequest::desktop(hint, reason),
            RefreshScope::Window { hwnd } => RefreshRequest::window(hwnd, hint, reason),
        };

        let _ = sender.send(request);
    }

    fn classify_event_source(hwnd: HWND, idobject: u32, idchild: u32) -> HintSource {
        let mut accessible = None;
        let mut child = VARIANT::default();
        let resolved = unsafe {
            AccessibleObjectFromEvent(hwnd, idobject, idchild, &mut accessible, &mut child)
        };
        if resolved.is_ok() {
            unsafe {
                let _ = VariantClear(&mut child);
            }
            HintSource::Msaa
        } else if idobject as i32 == OBJID_CLIENT.0 {
            HintSource::WinEvent
        } else {
            HintSource::WinEvent
        }
    }

    fn hook_sender_map() -> &'static Mutex<BTreeMap<isize, mpsc::UnboundedSender<RefreshRequest>>> {
        HOOK_SENDERS.get_or_init(|| Mutex::new(BTreeMap::new()))
    }

    fn rect_to_bounds(rect: RECT) -> Rect {
        Rect {
            left: rect.left,
            top: rect.top,
            width: rect.right - rect.left,
            height: rect.bottom - rect.top,
        }
    }

    fn read_bstr<F>(reader: F) -> Option<String>
    where
        F: FnOnce() -> windows::core::Result<BSTR>,
    {
        reader().ok().and_then(bstr_to_option)
    }

    fn bstr_to_option(value: BSTR) -> Option<String> {
        let text = value.to_string();
        let trimmed = text.trim();
        if trimmed.is_empty() {
            None
        } else {
            Some(trimmed.to_owned())
        }
    }

    fn child_self_variant() -> VARIANT {
        VARIANT {
            Anonymous: windows::Win32::System::Variant::VARIANT_0 {
                Anonymous: ManuallyDrop::new(VARIANT_0_0 {
                    vt: VT_I4,
                    wReserved1: 0,
                    wReserved2: 0,
                    wReserved3: 0,
                    Anonymous: VARIANT_0_0_0 {
                        lVal: CHILDID_SELF as i32,
                    },
                }),
            },
        }
    }

    fn variant_i32(value: &VARIANT) -> Option<i32> {
        unsafe {
            let variant = &value.Anonymous.Anonymous;
            if variant.vt == VT_I4 {
                Some(variant.Anonymous.lVal)
            } else {
                None
            }
        }
    }

    fn control_type_name(control_type: i32) -> String {
        match control_type {
            value if value == UIA_WindowControlTypeId.0 => "Window".to_owned(),
            value if value == UIA_PaneControlTypeId.0 => "Pane".to_owned(),
            value if value == UIA_ButtonControlTypeId.0 => "Button".to_owned(),
            value if value == UIA_EditControlTypeId.0 => "Edit".to_owned(),
            value if value == UIA_TextControlTypeId.0 => "Text".to_owned(),
            value if value == UIA_ListControlTypeId.0 => "List".to_owned(),
            value if value == UIA_TreeControlTypeId.0 => "Tree".to_owned(),
            value if value == UIA_MenuControlTypeId.0 => "Menu".to_owned(),
            value if value == UIA_TabControlTypeId.0 => "Tab".to_owned(),
            value if value == UIA_CustomControlTypeId.0 => "Custom".to_owned(),
            value => format!("ControlType({value})"),
        }
    }

    struct ComApartment {
        should_uninitialize: bool,
    }

    impl ComApartment {
        fn mta() -> Result<Self> {
            unsafe {
                let result = CoInitializeEx(None, COINIT_MULTITHREADED);
                if result == RPC_E_CHANGED_MODE {
                    Ok(Self {
                        should_uninitialize: false,
                    })
                } else {
                    result.ok()?;
                    Ok(Self {
                        should_uninitialize: true,
                    })
                }
            }
        }
    }

    impl Drop for ComApartment {
        fn drop(&mut self) {
            if self.should_uninitialize {
                unsafe {
                    CoUninitialize();
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use std::{
        collections::{BTreeMap, VecDeque},
        sync::{Arc, Mutex},
    };

    use tokio::time::{timeout, Duration};
    use tokio_stream::StreamExt;
    use vmui_platform::{BackendCapabilities, BackendEvent, BackendSessionParams};
    use vmui_protocol::{BackendKind, ElementStates, Locator, LocatorSegment, Rect, SessionMode};

    use super::*;

    #[test]
    fn coalesce_refresh_requests_dedups_same_window() {
        let requests = vec![
            RefreshRequest::window(10, HintSource::WinEvent, "focus"),
            RefreshRequest::window(10, HintSource::Msaa, "msaa"),
            RefreshRequest::window(20, HintSource::Uia, "uia"),
        ];

        let merged = coalesce_refresh_requests(requests);

        assert_eq!(merged.len(), 2);
        assert_eq!(merged[0].scope, RefreshScope::Window { hwnd: 10 });
        assert_eq!(merged[0].source, HintSource::Msaa);
        assert!(merged[0].reason.contains("focus"));
        assert!(merged[0].reason.contains("msaa"));
        assert_eq!(merged[1].scope, RefreshScope::Window { hwnd: 20 });
    }

    #[test]
    fn desktop_refresh_trumps_window_scope() {
        let requests = vec![
            RefreshRequest::window(10, HintSource::WinEvent, "focus"),
            RefreshRequest::desktop(HintSource::Msaa, "desktop"),
        ];

        let merged = coalesce_refresh_requests(requests);

        assert_eq!(
            merged,
            vec![RefreshRequest::desktop(HintSource::Msaa, "desktop")]
        );
    }

    #[cfg(not(windows))]
    #[test]
    fn non_windows_backend_reports_unavailable() {
        let backend = WindowsBackend::new();
        let capabilities = backend.capabilities();

        assert!(!capabilities.supports_live_observer);
        assert_eq!(backend.backend_id(), "windows-observer-unavailable");
    }

    #[tokio::test]
    async fn targeted_refresh_uses_window_scope_without_full_rescan() {
        let (refresh_tx, refresh_rx) = mpsc::unbounded_channel();
        let initial_windows = vec![
            sample_window(0x10, "Configurator", BackendKind::Uia, 1.0, false, false),
            sample_window(0x20, "Enterprise", BackendKind::Uia, 1.0, false, false),
        ];
        let updated_window =
            sample_window(0x10, "Configurator", BackendKind::Uia, 1.0, false, true);
        let source = Arc::new(FakeObservationSource::new(
            initial_windows.clone(),
            vec![(0x10, Some(updated_window.clone()))],
            Some(refresh_rx),
        ));
        let backend = WindowsBackend::from_source(source.clone());
        let params = test_params();

        let session = backend
            .open_session(params.clone())
            .await
            .expect("open session");
        assert_eq!(session.initial_snapshot.windows.len(), 2);
        assert_eq!(
            session.initial_snapshot.windows[0].backend,
            BackendKind::Uia
        );

        refresh_tx
            .send(RefreshRequest::window(0x10, HintSource::Msaa, "focus"))
            .expect("send refresh");

        let mut stream = session.events;
        let event = timeout(Duration::from_secs(1), stream.next())
            .await
            .expect("event timeout")
            .expect("event must exist");

        match event {
            BackendEvent::Diff(diff) => {
                assert_eq!(diff.ops.len(), 2);
                assert!(diff.ops.iter().any(|op| {
                    matches!(op, DiffOp::NodeReplaced { element_id, node } if element_id == &initial_windows[0].root.element_id && node.backend == BackendKind::Uia && node.children[0].locator.path[0].sibling_ordinal.is_none())
                }));
                assert!(diff.ops.iter().any(|op| {
                    matches!(op, DiffOp::FocusChanged { window_id, element_id } if window_id == &initial_windows[0].window_id && element_id.as_ref() == Some(&initial_windows[0].root.children[0].element_id))
                }));
            }
            other => panic!("unexpected event: {other:?}"),
        }

        let calls = source.calls.lock().expect("calls mutex poisoned").clone();
        assert_eq!(
            calls,
            vec!["desktop".to_owned(), "window:0000000000000010".to_owned()]
        );
    }

    #[tokio::test]
    async fn initial_snapshot_preserves_backend_provenance_and_confidence() {
        let initial_windows = vec![sample_window(
            0x30,
            "Designer",
            BackendKind::Mixed,
            0.7,
            false,
            false,
        )];
        let source = Arc::new(FakeObservationSource::new(
            initial_windows,
            Vec::new(),
            None,
        ));
        let backend = WindowsBackend::from_source(source);
        let session = backend
            .open_session(test_params())
            .await
            .expect("open session");

        assert_eq!(
            session.initial_snapshot.windows[0].backend,
            BackendKind::Mixed
        );
        assert_eq!(session.initial_snapshot.windows[0].confidence, 0.7);
        assert_eq!(
            session.initial_snapshot.windows[0].root.backend,
            BackendKind::Mixed
        );
        assert_eq!(session.initial_snapshot.windows[0].root.confidence, 0.7);
    }

    #[test]
    fn stabilize_window_reuses_ids_when_semantic_locator_matches() {
        let previous = sample_window(0x44, "Configurator", BackendKind::Uia, 1.0, false, false);
        let mut refreshed = sample_window(0x44, "Configurator", BackendKind::Uia, 1.0, false, true);

        refreshed
            .root
            .children
            .insert(0, sibling_button(0x44, "Open"));

        let previous_child_id = previous.root.children[0].element_id.clone();
        stabilize_window(&previous, &mut refreshed);

        assert_eq!(refreshed.window_id, previous.window_id);
        assert_eq!(refreshed.root.element_id, previous.root.element_id);
        assert_eq!(refreshed.root.children[1].element_id, previous_child_id);
        assert_eq!(
            refreshed.root.children[1].locator.path[0].control_type,
            "Edit"
        );
        assert_eq!(
            refreshed.root.children[1].locator.path[0]
                .class_name
                .as_deref(),
            Some("V8Edit")
        );
        assert_eq!(
            refreshed.root.children[1].locator.path[0].sibling_ordinal,
            None
        );
    }

    struct FakeObservationSource {
        initial_windows: Mutex<Vec<WindowState>>,
        window_updates: Mutex<BTreeMap<usize, VecDeque<Option<WindowState>>>>,
        refresh_receiver: Mutex<Option<mpsc::UnboundedReceiver<RefreshRequest>>>,
        calls: Mutex<Vec<String>>,
    }

    impl FakeObservationSource {
        fn new(
            initial_windows: Vec<WindowState>,
            window_updates: Vec<(usize, Option<WindowState>)>,
            refresh_receiver: Option<mpsc::UnboundedReceiver<RefreshRequest>>,
        ) -> Self {
            let mut updates = BTreeMap::new();
            for (hwnd, window) in window_updates {
                updates
                    .entry(hwnd)
                    .or_insert_with(VecDeque::new)
                    .push_back(window);
            }
            Self {
                initial_windows: Mutex::new(initial_windows),
                window_updates: Mutex::new(updates),
                refresh_receiver: Mutex::new(refresh_receiver),
                calls: Mutex::new(Vec::new()),
            }
        }
    }

    impl ObservationSource for FakeObservationSource {
        fn backend_id(&self) -> &'static str {
            "fake-windows-source"
        }

        fn capabilities(&self) -> BackendCapabilities {
            BackendCapabilities {
                supports_live_observer: true,
                supports_uia: true,
                supports_msaa: true,
                supports_ocr_fallback: false,
                supports_artifacts: true,
            }
        }

        fn availability_warning(&self) -> Option<String> {
            None
        }

        fn capture_desktop(&self, _params: &BackendSessionParams) -> Result<Vec<WindowState>> {
            self.calls
                .lock()
                .expect("calls mutex poisoned")
                .push("desktop".to_owned());
            Ok(self
                .initial_windows
                .lock()
                .expect("initial windows mutex poisoned")
                .clone())
        }

        fn capture_window(
            &self,
            _params: &BackendSessionParams,
            hwnd: usize,
            _hint: HintSource,
        ) -> Result<Option<WindowState>> {
            self.calls
                .lock()
                .expect("calls mutex poisoned")
                .push(format!("window:{hwnd:016x}"));
            Ok(self
                .window_updates
                .lock()
                .expect("window updates mutex poisoned")
                .get_mut(&hwnd)
                .and_then(VecDeque::pop_front)
                .flatten())
        }

        fn subscribe(&self, _params: &BackendSessionParams) -> Result<Option<RefreshSubscription>> {
            Ok(self
                .refresh_receiver
                .lock()
                .expect("refresh receiver mutex poisoned")
                .take()
                .map(RefreshSubscription::from_receiver))
        }
    }

    fn test_params() -> BackendSessionParams {
        BackendSessionParams {
            session_id: vmui_protocol::SessionId::from("sess-test"),
            mode: SessionMode::EnterpriseUi,
            shallow: false,
        }
    }

    fn sample_window(
        hwnd: usize,
        title: &str,
        backend: BackendKind,
        confidence: f32,
        root_focused: bool,
        child_focused: bool,
    ) -> WindowState {
        let window_fingerprint = format!(
            "pid={}:hwnd={:016x}:class=v8toplevelframe:title={}",
            hwnd as u32,
            hwnd,
            title.to_lowercase()
        );
        let root_id = element_id_from_locator(&window_fingerprint, &[]);
        let child_locator = LocatorSegment {
            control_type: "Edit".to_owned(),
            class_name: Some("V8Edit".to_owned()),
            automation_id: None,
            name: Some("Search".to_owned()),
            sibling_ordinal: None,
        };
        let child_id =
            element_id_from_locator(&window_fingerprint, std::slice::from_ref(&child_locator));
        WindowState {
            window_id: window_id_from_fingerprint(&window_fingerprint),
            pid: hwnd as u32,
            process_name: Some("1cv8.exe".to_owned()),
            title: title.to_owned(),
            bounds: Rect {
                left: 0,
                top: 0,
                width: 100,
                height: 50,
            },
            backend: backend.clone(),
            confidence,
            root: ElementNode {
                element_id: root_id.clone(),
                parent_id: None,
                backend: backend.clone(),
                control_type: "Window".to_owned(),
                class_name: Some("V8TopLevelFrame".to_owned()),
                name: Some(title.to_owned()),
                automation_id: None,
                native_window_handle: Some(hwnd as u64),
                bounds: Rect {
                    left: 0,
                    top: 0,
                    width: 100,
                    height: 50,
                },
                locator: Locator {
                    window_fingerprint: window_fingerprint.clone(),
                    path: Vec::new(),
                },
                properties: BTreeMap::new(),
                states: ElementStates {
                    enabled: true,
                    visible: true,
                    focused: root_focused,
                    selected: false,
                    expanded: false,
                    toggled: false,
                },
                children: vec![ElementNode {
                    element_id: child_id,
                    parent_id: Some(root_id),
                    backend,
                    control_type: "Edit".to_owned(),
                    class_name: Some("V8Edit".to_owned()),
                    name: Some("Search".to_owned()),
                    automation_id: None,
                    native_window_handle: None,
                    bounds: Rect {
                        left: 10,
                        top: 10,
                        width: 80,
                        height: 20,
                    },
                    locator: Locator {
                        window_fingerprint: window_fingerprint,
                        path: vec![child_locator],
                    },
                    properties: BTreeMap::new(),
                    states: ElementStates {
                        enabled: true,
                        visible: true,
                        focused: child_focused,
                        selected: false,
                        expanded: false,
                        toggled: false,
                    },
                    children: Vec::new(),
                    confidence,
                }],
                confidence,
            },
        }
    }

    fn sibling_button(hwnd: usize, name: &str) -> ElementNode {
        let window_fingerprint = format!(
            "pid={}:hwnd={:016x}:class=v8toplevelframe:title=configurator",
            hwnd as u32, hwnd
        );
        let segment = LocatorSegment {
            control_type: "Button".to_owned(),
            class_name: Some("V8Button".to_owned()),
            automation_id: None,
            name: Some(name.to_owned()),
            sibling_ordinal: None,
        };

        ElementNode {
            element_id: element_id_from_locator(
                &window_fingerprint,
                std::slice::from_ref(&segment),
            ),
            parent_id: Some(element_id_from_locator(&window_fingerprint, &[])),
            backend: BackendKind::Uia,
            control_type: "Button".to_owned(),
            class_name: Some("V8Button".to_owned()),
            name: Some(name.to_owned()),
            automation_id: None,
            native_window_handle: None,
            bounds: Rect {
                left: 5,
                top: 8,
                width: 20,
                height: 20,
            },
            locator: Locator {
                window_fingerprint,
                path: vec![segment],
            },
            properties: BTreeMap::new(),
            states: ElementStates {
                enabled: true,
                visible: true,
                focused: false,
                selected: false,
                expanded: false,
                toggled: false,
            },
            children: Vec::new(),
            confidence: 1.0,
        }
    }
}

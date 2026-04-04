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
mod windows_impl;

#[cfg(test)]
mod tests;

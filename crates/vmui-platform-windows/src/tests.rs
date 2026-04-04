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
    let updated_window = sample_window(0x10, "Configurator", BackendKind::Uia, 1.0, false, true);
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
        element_id: element_id_from_locator(&window_fingerprint, std::slice::from_ref(&segment)),
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

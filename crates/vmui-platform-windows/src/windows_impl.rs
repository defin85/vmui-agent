use std::{
    collections::BTreeMap,
    ffi::c_void,
    mem::ManuallyDrop,
    path::Path,
    ptr,
    sync::{Mutex, OnceLock},
    thread,
};

use anyhow::{anyhow, Context, Result};
use image::{
    codecs::{jpeg::JpegEncoder, png::PngEncoder},
    ColorType, ImageEncoder,
};
use serde::Serialize;
use tokio::sync::mpsc;
use vmui_platform::{BackendActionResult, BackendArtifact, BackendCapabilities};
use vmui_protocol::{
    ActionKind, ActionRequest, ActionStatus, ActionTarget, BackendKind, CaptureFormat,
    ClickOptions, ElementId, ElementLocator, ElementNode, ElementStates, Locator, LocatorSegment,
    MouseButton, PanelProbeLayer, PanelProbeLayerKind, PanelProbeMetadata, PanelProbeOptions,
    PanelProbeSurface, PanelProbeTargetKind, ProbeLayerStatus, PropertyValue, Rect, RegionTarget,
    SendKeysOptions, SetValueOptions, WindowLocator, WindowState,
};
use windows::{
    core::{Interface, BOOL, BSTR, PWSTR},
    Win32::{
        Foundation::{CloseHandle, HWND, LPARAM, POINT, RECT, RPC_E_CHANGED_MODE, WPARAM},
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
            Threading::{
                GetCurrentThreadId, OpenProcess, QueryFullProcessImageNameW, PROCESS_NAME_WIN32,
                PROCESS_QUERY_LIMITED_INFORMATION,
            },
            Variant::{VariantClear, VARIANT, VARIANT_0_0, VARIANT_0_0_0, VT_I4},
        },
        UI::{
            Accessibility::{
                AccessibleObjectFromEvent, AccessibleObjectFromPoint, AccessibleObjectFromWindow,
                CUIAutomation, IAccessible, IUIAutomation, IUIAutomationElement,
                IUIAutomationInvokePattern, IUIAutomationLegacyIAccessiblePattern,
                IUIAutomationTreeWalker, IUIAutomationValuePattern, SetWinEventHook,
                UIA_ButtonControlTypeId, UIA_CustomControlTypeId, UIA_EditControlTypeId,
                UIA_InvokePatternId, UIA_LegacyIAccessiblePatternId, UIA_ListControlTypeId,
                UIA_MenuControlTypeId, UIA_PaneControlTypeId, UIA_TabControlTypeId,
                UIA_TextControlTypeId, UIA_TreeControlTypeId, UIA_ValuePatternId,
                UIA_WindowControlTypeId, UnhookWinEvent,
            },
            Input::KeyboardAndMouse::{
                SendInput, INPUT, INPUT_0, INPUT_KEYBOARD, INPUT_MOUSE, KEYBDINPUT,
                KEYEVENTF_KEYUP, KEYEVENTF_UNICODE, MOUSEEVENTF_LEFTDOWN, MOUSEEVENTF_LEFTUP,
                MOUSEEVENTF_MIDDLEDOWN, MOUSEEVENTF_MIDDLEUP, MOUSEEVENTF_RIGHTDOWN,
                MOUSEEVENTF_RIGHTUP, MOUSEINPUT, VIRTUAL_KEY, VK_A, VK_BACK, VK_CONTROL, VK_DELETE,
                VK_DOWN, VK_ESCAPE, VK_F4, VK_LEFT, VK_RETURN, VK_RIGHT, VK_TAB, VK_UP,
            },
            WindowsAndMessaging::{
                EnumChildWindows, EnumWindows, GetClassNameW, GetMessageW, GetParent,
                GetWindowRect, GetWindowTextW, GetWindowThreadProcessId, IsWindowVisible,
                PostThreadMessageW, SetCursorPos, SetForegroundWindow, WindowFromPoint,
                CHILDID_SELF, EVENT_OBJECT_FOCUS, EVENT_OBJECT_HIDE, EVENT_OBJECT_NAMECHANGE,
                EVENT_OBJECT_SELECTION, EVENT_OBJECT_SHOW, EVENT_OBJECT_STATECHANGE,
                EVENT_SYSTEM_FOREGROUND, EVENT_SYSTEM_MINIMIZEEND, MSG, OBJID_CLIENT,
                WINEVENT_OUTOFCONTEXT, WINEVENT_SKIPOWNPROCESS, WM_QUIT,
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

        for raw_hwnd in hwnds {
            let hwnd = HWND(raw_hwnd as *mut c_void);
            let metadata = match read_window_metadata(hwnd)? {
                Some(metadata) => metadata,
                None => continue,
            };
            if let Some(window) =
                capture_window_from_metadata(params, hwnd, &metadata, HintSource::Uia)?
            {
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
        capture_window_from_metadata(params, hwnd, &metadata, hint)
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
            let _ = unsafe { PostThreadMessageW(ready.thread_id, WM_QUIT, WPARAM(0), LPARAM(0)) };
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
    process_name: Option<String>,
    title: String,
    class_name: Option<String>,
    bounds: Rect,
}

#[derive(Clone, Serialize)]
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
        let process_name = read_process_name(pid);
        let class_name = read_window_class_name(hwnd);

        Ok(Some(WindowMetadata {
            hwnd: hwnd.0 as usize,
            pid,
            process_name,
            title,
            class_name,
            bounds,
        }))
    }
}

fn capture_window_from_metadata(
    params: &vmui_platform::BackendSessionParams,
    hwnd: HWND,
    metadata: &WindowMetadata,
    hint: HintSource,
) -> Result<Option<WindowState>> {
    let max_depth = if params.shallow { 1 } else { 4 };

    match capture_uia_window(hwnd, metadata, max_depth, hint) {
        Ok(window) => Ok(Some(window)),
        Err(uia_error) => match capture_msaa_window(hwnd, metadata) {
            Ok(Some(window)) => Ok(Some(window)),
            Ok(None) => Err(uia_error),
            Err(msaa_error) => Err(anyhow!(
                "UIA capture failed: {uia_error}; MSAA fallback failed: {msaa_error}"
            )),
        },
    }
}

fn read_window_class_name(hwnd: HWND) -> Option<String> {
    let mut class_buffer = [0u16; 256];
    let class_len = unsafe { GetClassNameW(hwnd, &mut class_buffer) };
    (class_len > 0).then(|| String::from_utf16_lossy(&class_buffer[..class_len as usize]))
}

fn read_process_name(pid: u32) -> Option<String> {
    if pid == 0 {
        return None;
    }

    let handle = unsafe { OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, false, pid) }.ok()?;
    let mut buffer = vec![0u16; 260];
    let mut len = buffer.len() as u32;
    let query_result = unsafe {
        QueryFullProcessImageNameW(
            handle,
            PROCESS_NAME_WIN32,
            PWSTR(buffer.as_mut_ptr()),
            &mut len,
        )
    };
    let _ = unsafe { CloseHandle(handle) };
    query_result.ok()?;

    let full_path = String::from_utf16_lossy(&buffer[..len as usize]);
    Path::new(&full_path)
        .file_name()
        .and_then(|name| name.to_str())
        .map(ToOwned::to_owned)
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
    let walker = unsafe { automation.ControlViewWalker() }.context("ControlViewWalker failed")?;
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
        process_name: metadata.process_name.clone(),
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
    unsafe { AccessibleObjectFromWindow(hwnd, OBJID_CLIENT.0 as u32, &IAccessible::IID, &mut raw) }
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
        class_name: metadata.class_name.clone(),
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
        process_name: metadata.process_name.clone(),
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
        ActionKind::PanelProbe(options) => panel_probe_action(action, options),
        ActionKind::OcrRegion(_) => Ok(super::unsupported_action(
            action,
            "ocr fallback is not available on the current Windows backend",
        )),
        ActionKind::ListWindows
        | ActionKind::GetTree(_)
        | ActionKind::GetRuntimeStatus(_)
        | ActionKind::WaitFor(_)
        | ActionKind::WriteArtifact(_)
        | ActionKind::CollectDiagnosticBundle(_) => Ok(super::unsupported_action(
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

#[derive(Clone)]
struct ResolvedPanelProbeTarget<'a> {
    window: &'a ActionWindow,
    target_kind: PanelProbeTargetKind,
    bounds: Rect,
    target_element_id: Option<ElementId>,
    target_locator: Option<Locator>,
    target_backend: Option<BackendKind>,
    target_hwnd: Option<HWND>,
    uia_element: Option<IUIAutomationElement>,
}

#[derive(Clone, Serialize)]
struct HwndProbeArtifact {
    root_hwnd: String,
    target_bounds: Rect,
    nodes: Vec<HwndProbeNode>,
}

#[derive(Clone, Serialize)]
struct HwndProbeNode {
    hwnd: String,
    parent_hwnd: Option<String>,
    pid: u32,
    process_name: Option<String>,
    title: String,
    class_name: Option<String>,
    bounds: Rect,
    visible: bool,
    depth: usize,
    intersects_target: bool,
}

#[derive(Clone, Serialize)]
struct UiaProbeArtifact {
    view: String,
    max_depth: usize,
    root: RawObservedNode,
}

#[derive(Clone, Serialize)]
struct MsaaProbeArtifact {
    max_depth: usize,
    root: Option<MsaaNode>,
}

#[derive(Clone, Serialize)]
struct MsaaNode {
    child_id: Option<i32>,
    name: Option<String>,
    role: Option<String>,
    role_code: Option<i32>,
    state_bits: Option<i32>,
    bounds: Option<Rect>,
    child_count: Option<i32>,
    children: Vec<MsaaNode>,
}

#[derive(Clone, Serialize)]
struct HitTestArtifact {
    target_bounds: Rect,
    sample_points: Vec<ProbePointSample>,
}

#[derive(Clone, Serialize)]
struct ProbePointSample {
    label: String,
    point: ProbePoint,
    hwnd: Option<String>,
    uia: Option<ProbeElementSummary>,
    msaa: Option<MsaaHitResult>,
}

#[derive(Clone, Serialize)]
struct ProbePoint {
    x: i32,
    y: i32,
}

#[derive(Clone, Serialize)]
struct ProbeElementSummary {
    control_type: String,
    class_name: Option<String>,
    name: Option<String>,
    automation_id: Option<String>,
    native_window_handle: Option<String>,
    bounds: Rect,
}

#[derive(Clone, Serialize)]
struct MsaaHitResult {
    child_id: Option<i32>,
    name: Option<String>,
    role: Option<String>,
    role_code: Option<i32>,
    state_bits: Option<i32>,
    bounds: Option<Rect>,
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

fn panel_probe_action(
    action: ActionRequest,
    options: PanelProbeOptions,
) -> Result<BackendActionResult> {
    let automation = AutomationContext::new()?;
    let windows = capture_action_windows(8)?;
    let Some(target) = resolve_panel_probe_target(&automation, &windows, &action.target)? else {
        return Ok(failed_action(action, "panel_probe target was not found"));
    };

    let mut artifacts = Vec::new();
    let mut layers = Vec::new();

    let (hwnd_layer, hwnd_artifact) = collect_hwnd_probe(&target)?;
    push_probe_layer(&mut layers, &mut artifacts, hwnd_layer, hwnd_artifact);

    let uia_max_depth = options.uia_max_depth.unwrap_or(6).max(1) as usize;
    let (uia_raw_layer, uia_raw_artifact) = collect_uia_probe(
        &automation,
        &target,
        unsafe { automation.automation.RawViewWalker() }.context("RawViewWalker failed")?,
        uia_max_depth,
        PanelProbeLayerKind::UiaRaw,
        "panel-probe-uia-raw-json",
        "raw",
    )?;
    push_probe_layer(&mut layers, &mut artifacts, uia_raw_layer, uia_raw_artifact);

    let (uia_control_layer, uia_control_artifact) = collect_uia_probe(
        &automation,
        &target,
        automation.walker.clone(),
        uia_max_depth,
        PanelProbeLayerKind::UiaControl,
        "panel-probe-uia-control-json",
        "control",
    )?;
    push_probe_layer(
        &mut layers,
        &mut artifacts,
        uia_control_layer,
        uia_control_artifact,
    );

    let (uia_content_layer, uia_content_artifact) = collect_uia_probe(
        &automation,
        &target,
        unsafe { automation.automation.ContentViewWalker() }.context("ContentViewWalker failed")?,
        uia_max_depth,
        PanelProbeLayerKind::UiaContent,
        "panel-probe-uia-content-json",
        "content",
    )?;
    push_probe_layer(
        &mut layers,
        &mut artifacts,
        uia_content_layer,
        uia_content_artifact,
    );

    let msaa_max_depth = options.msaa_max_depth.unwrap_or(3).max(1) as usize;
    let (msaa_layer, msaa_artifact) = collect_msaa_probe(&target, msaa_max_depth)?;
    push_probe_layer(&mut layers, &mut artifacts, msaa_layer, msaa_artifact);

    let (hit_test_layer, hit_test_artifact) = collect_hit_test_probe(&automation, &target)?;
    push_probe_layer(
        &mut layers,
        &mut artifacts,
        hit_test_layer,
        hit_test_artifact,
    );

    let capture_artifact = capture_screen_region(target.bounds, options.capture_format)?;
    let capture_kind = capture_artifact.kind.clone();
    let capture_mime_type = capture_artifact.mime_type.clone();
    artifacts.push(capture_artifact);
    layers.push(PanelProbeLayer {
        layer: PanelProbeLayerKind::Capture,
        status: ProbeLayerStatus::Observed,
        artifact_kind: Some(capture_kind),
        artifact_id: None,
        mime_type: Some(capture_mime_type),
        message: Some("captured target surface".to_owned()),
    });

    let metadata = PanelProbeMetadata {
        surface: PanelProbeSurface {
            target_kind: target.target_kind,
            window_id: Some(target.window.window.window_id.clone()),
            pid: Some(target.window.window.pid),
            process_name: target.window.window.process_name.clone(),
            title: Some(target.window.window.title.clone()),
            class_name: target.window.window.root.class_name.clone(),
            bounds: target.bounds,
            target_element_id: target.target_element_id.clone(),
            target_locator: target.target_locator.clone(),
            target_backend: target.target_backend.clone(),
        },
        layers,
    };
    artifacts.push(json_probe_artifact("panel-probe-metadata-json", &metadata)?);

    Ok(BackendActionResult {
        action_id: action.action_id,
        ok: true,
        status: ActionStatus::Completed,
        message: "collected out-of-process panel probe bundle".to_owned(),
        artifacts,
    })
}

fn push_probe_layer(
    layers: &mut Vec<PanelProbeLayer>,
    artifacts: &mut Vec<BackendArtifact>,
    layer: PanelProbeLayer,
    artifact: Option<BackendArtifact>,
) {
    if let Some(artifact) = artifact {
        layers.push(PanelProbeLayer {
            mime_type: Some(artifact.mime_type.clone()),
            ..layer
        });
        artifacts.push(artifact);
    } else {
        layers.push(layer);
    }
}

fn resolve_panel_probe_target<'a>(
    automation: &AutomationContext,
    windows: &'a [ActionWindow],
    target: &ActionTarget,
) -> Result<Option<ResolvedPanelProbeTarget<'a>>> {
    match target {
        ActionTarget::Element(locator) => {
            let Some(resolved) = resolve_action_element(windows, locator) else {
                return Ok(None);
            };
            Ok(Some(ResolvedPanelProbeTarget {
                window: resolved.window,
                target_kind: PanelProbeTargetKind::Element,
                bounds: resolved.node.bounds,
                target_element_id: Some(resolved.node.element_id.clone()),
                target_locator: Some(resolved.node.locator.clone()),
                target_backend: Some(resolved.node.backend.clone()),
                target_hwnd: resolved
                    .node
                    .native_window_handle
                    .map(|value| HWND(value as usize as *mut c_void)),
                uia_element: automation
                    .resolve_element_for_locator(resolved.window.hwnd, &resolved.node.locator)?,
            }))
        }
        ActionTarget::Window(_) | ActionTarget::Desktop => {
            let Some(window) = resolve_action_window_from_target(windows, target) else {
                return Ok(None);
            };
            Ok(Some(ResolvedPanelProbeTarget {
                window,
                target_kind: PanelProbeTargetKind::Window,
                bounds: window.window.bounds,
                target_element_id: Some(window.window.root.element_id.clone()),
                target_locator: Some(window.window.root.locator.clone()),
                target_backend: Some(window.window.backend.clone()),
                target_hwnd: Some(window.hwnd),
                uia_element: Some(automation.root_element_from_handle(window.hwnd)?),
            }))
        }
        ActionTarget::Region(region) => {
            let Some(bounds) = resolve_panel_probe_region_bounds(windows, region) else {
                return Ok(None);
            };
            let point = rect_center(bounds);
            let Some(window) = (match &region.window_id {
                Some(_) => resolve_action_window_from_target(windows, target),
                None => windows
                    .iter()
                    .find(|window| rect_contains_point(window.window.bounds, point)),
            }) else {
                return Ok(None);
            };
            let element = unsafe { automation.automation.ElementFromPoint(point) }.ok();
            let target_hwnd = unsafe { WindowFromPoint(point) };
            Ok(Some(ResolvedPanelProbeTarget {
                window,
                target_kind: PanelProbeTargetKind::Region,
                bounds,
                target_element_id: None,
                target_locator: None,
                target_backend: element
                    .as_ref()
                    .map(|_| BackendKind::Uia)
                    .or_else(|| Some(window.window.backend.clone())),
                target_hwnd: (!target_hwnd.0.is_null()).then_some(target_hwnd),
                uia_element: element,
            }))
        }
    }
}

fn resolve_panel_probe_region_bounds(
    windows: &[ActionWindow],
    region: &RegionTarget,
) -> Option<Rect> {
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

fn collect_hwnd_probe(
    target: &ResolvedPanelProbeTarget<'_>,
) -> Result<(PanelProbeLayer, Option<BackendArtifact>)> {
    let root_hwnd = target.target_hwnd.unwrap_or(target.window.hwnd);
    let root_metadata = read_probe_window_metadata(root_hwnd)?;
    let mut nodes = Vec::new();

    if let Some(root_metadata) = root_metadata {
        nodes.push(probe_hwnd_node(
            root_hwnd,
            None,
            0,
            true,
            &root_metadata,
            target.bounds,
        ));
    }

    for child in enumerate_child_hwnds(root_hwnd)? {
        let Some(metadata) = read_probe_window_metadata(child)? else {
            continue;
        };
        let intersects_target = rects_intersect(metadata.bounds, target.bounds);
        if !intersects_target {
            continue;
        }
        nodes.push(probe_hwnd_node(
            child,
            hwnd_parent(child).map(hwnd_hex),
            hwnd_depth(root_hwnd, child),
            intersects_target,
            &metadata,
            target.bounds,
        ));
    }

    let status = if nodes.len() > 1 {
        ProbeLayerStatus::Observed
    } else {
        ProbeLayerStatus::Insufficient
    };
    let message = match status {
        ProbeLayerStatus::Observed => {
            Some("enumerated child HWNDs intersecting target surface".to_owned())
        }
        ProbeLayerStatus::Insufficient => {
            Some("no intersecting child HWNDs were exposed beneath the target surface".to_owned())
        }
        ProbeLayerStatus::Unsupported => None,
    };

    let artifact = json_probe_artifact(
        "panel-probe-hwnd-json",
        &HwndProbeArtifact {
            root_hwnd: hwnd_hex(root_hwnd),
            target_bounds: target.bounds,
            nodes,
        },
    )?;

    Ok((
        PanelProbeLayer {
            layer: PanelProbeLayerKind::HwndHierarchy,
            status,
            artifact_kind: Some("panel-probe-hwnd-json".to_owned()),
            artifact_id: None,
            mime_type: None,
            message,
        },
        Some(artifact),
    ))
}

fn collect_uia_probe(
    _automation: &AutomationContext,
    target: &ResolvedPanelProbeTarget<'_>,
    walker: IUIAutomationTreeWalker,
    max_depth: usize,
    layer_kind: PanelProbeLayerKind,
    artifact_kind: &str,
    view_label: &str,
) -> Result<(PanelProbeLayer, Option<BackendArtifact>)> {
    let Some(element) = target.uia_element.as_ref() else {
        return Ok((
            PanelProbeLayer {
                layer: layer_kind,
                status: ProbeLayerStatus::Insufficient,
                artifact_kind: None,
                artifact_id: None,
                mime_type: None,
                message: Some(format!(
                    "{view_label} view could not resolve a UIA element for the target surface"
                )),
            },
            None,
        ));
    };

    let root = read_uia_tree(&walker, element, max_depth)?;
    let weak = uia_probe_looks_opaque(&root);
    let status = if weak {
        ProbeLayerStatus::Insufficient
    } else {
        ProbeLayerStatus::Observed
    };
    let artifact = json_probe_artifact(
        artifact_kind,
        &UiaProbeArtifact {
            view: view_label.to_owned(),
            max_depth,
            root,
        },
    )?;

    Ok((
        PanelProbeLayer {
            layer: layer_kind,
            status,
            artifact_kind: Some(artifact_kind.to_owned()),
            artifact_id: None,
            mime_type: None,
            message: weak.then(|| {
                format!("{view_label} view exposed only shallow/opaque semantics for the target")
            }),
        },
        Some(artifact),
    ))
}

fn collect_msaa_probe(
    target: &ResolvedPanelProbeTarget<'_>,
    max_depth: usize,
) -> Result<(PanelProbeLayer, Option<BackendArtifact>)> {
    let root_hwnd = target.target_hwnd.unwrap_or(target.window.hwnd);
    let Some(accessible) = accessible_object_from_window(root_hwnd) else {
        return Ok((
            PanelProbeLayer {
                layer: PanelProbeLayerKind::Msaa,
                status: ProbeLayerStatus::Unsupported,
                artifact_kind: None,
                artifact_id: None,
                mime_type: None,
                message: Some(
                    "AccessibleObjectFromWindow did not expose an MSAA root for the target"
                        .to_owned(),
                ),
            },
            None,
        ));
    };

    let root = msaa_accessible_node(&accessible, 0, max_depth)?;
    let weak = msaa_probe_looks_opaque(&root);
    let status = if weak {
        ProbeLayerStatus::Insufficient
    } else {
        ProbeLayerStatus::Observed
    };
    let artifact = json_probe_artifact(
        "panel-probe-msaa-json",
        &MsaaProbeArtifact {
            max_depth,
            root: Some(root),
        },
    )?;

    Ok((
        PanelProbeLayer {
            layer: PanelProbeLayerKind::Msaa,
            status,
            artifact_kind: Some("panel-probe-msaa-json".to_owned()),
            artifact_id: None,
            mime_type: None,
            message: weak.then(|| {
                "MSAA exposed only shallow or opaque data for the target surface".to_owned()
            }),
        },
        Some(artifact),
    ))
}

fn collect_hit_test_probe(
    automation: &AutomationContext,
    target: &ResolvedPanelProbeTarget<'_>,
) -> Result<(PanelProbeLayer, Option<BackendArtifact>)> {
    let bounds = target.bounds;
    let inset_x = (bounds.width / 5).max(8);
    let inset_y = (bounds.height / 5).max(8);
    let points = [
        ("center", rect_center(bounds)),
        (
            "top_left",
            POINT {
                x: bounds.left + inset_x.min(bounds.width.saturating_sub(1)),
                y: bounds.top + inset_y.min(bounds.height.saturating_sub(1)),
            },
        ),
        (
            "top_right",
            POINT {
                x: bounds.left + (bounds.width - inset_x).max(1) - 1,
                y: bounds.top + inset_y.min(bounds.height.saturating_sub(1)),
            },
        ),
        (
            "bottom_left",
            POINT {
                x: bounds.left + inset_x.min(bounds.width.saturating_sub(1)),
                y: bounds.top + (bounds.height - inset_y).max(1) - 1,
            },
        ),
        (
            "bottom_right",
            POINT {
                x: bounds.left + (bounds.width - inset_x).max(1) - 1,
                y: bounds.top + (bounds.height - inset_y).max(1) - 1,
            },
        ),
    ];

    let sample_points = points
        .into_iter()
        .map(|(label, point)| probe_point_sample(automation, point, label))
        .collect::<Result<Vec<_>>>()?;
    let rich_samples = sample_points
        .iter()
        .filter(|sample| sample.uia.is_some() || sample.msaa.is_some())
        .count();
    let status = if rich_samples > 0 {
        ProbeLayerStatus::Observed
    } else {
        ProbeLayerStatus::Insufficient
    };
    let message = match status {
        ProbeLayerStatus::Observed => {
            Some("captured point hit-test evidence for the target surface".to_owned())
        }
        ProbeLayerStatus::Insufficient => {
            Some("point hit-tests resolved HWNDs but no meaningful UIA/MSAA semantics".to_owned())
        }
        ProbeLayerStatus::Unsupported => None,
    };
    let artifact = json_probe_artifact(
        "panel-probe-hit-test-json",
        &HitTestArtifact {
            target_bounds: target.bounds,
            sample_points,
        },
    )?;

    Ok((
        PanelProbeLayer {
            layer: PanelProbeLayerKind::HitTest,
            status,
            artifact_kind: Some("panel-probe-hit-test-json".to_owned()),
            artifact_id: None,
            mime_type: None,
            message,
        },
        Some(artifact),
    ))
}

fn probe_point_sample(
    automation: &AutomationContext,
    point: POINT,
    label: &str,
) -> Result<ProbePointSample> {
    let hwnd = unsafe { WindowFromPoint(point) };
    let uia = unsafe { automation.automation.ElementFromPoint(point) }
        .ok()
        .as_ref()
        .map(summarize_uia_element)
        .transpose()?;
    let msaa = accessible_object_from_point_summary(point)?;

    Ok(ProbePointSample {
        label: label.to_owned(),
        point: ProbePoint {
            x: point.x,
            y: point.y,
        },
        hwnd: (!hwnd.0.is_null()).then(|| hwnd_hex(hwnd)),
        uia,
        msaa,
    })
}

fn summarize_uia_element(element: &IUIAutomationElement) -> Result<ProbeElementSummary> {
    Ok(ProbeElementSummary {
        control_type: control_type_name(
            unsafe { element.CurrentControlType() }
                .context("CurrentControlType failed")?
                .0,
        ),
        class_name: read_bstr(|| unsafe { element.CurrentClassName() }),
        name: read_bstr(|| unsafe { element.CurrentName() }),
        automation_id: read_bstr(|| unsafe { element.CurrentAutomationId() }),
        native_window_handle: unsafe { element.CurrentNativeWindowHandle() }
            .ok()
            .map(|value| value.0 as usize)
            .filter(|value| *value != 0)
            .map(|value| format!("0x{value:016x}")),
        bounds: unsafe { element.CurrentBoundingRectangle() }
            .ok()
            .map(rect_to_bounds)
            .unwrap_or_default(),
    })
}

fn accessible_object_from_point_summary(point: POINT) -> Result<Option<MsaaHitResult>> {
    let mut accessible = None;
    let mut child = VARIANT::default();
    let result = unsafe { AccessibleObjectFromPoint(point, &mut accessible, &mut child) };
    if result.is_err() {
        return Ok(None);
    }
    let Some(accessible) = accessible else {
        unsafe {
            let _ = VariantClear(&mut child);
        }
        return Ok(None);
    };

    let hit = msaa_hit_result_from_variant(&accessible, &child)?;
    unsafe {
        let _ = VariantClear(&mut child);
    }
    Ok(Some(hit))
}

fn json_probe_artifact(kind: &str, payload: &impl Serialize) -> Result<BackendArtifact> {
    Ok(BackendArtifact {
        kind: kind.to_owned(),
        mime_type: "application/json".to_owned(),
        bytes: serde_json::to_vec_pretty(payload)
            .with_context(|| format!("failed to encode `{kind}` probe artifact"))?,
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

fn find_node_by_id<'a>(node: &'a ElementNode, element_id: &ElementId) -> Option<&'a ElementNode> {
    if &node.element_id == element_id {
        return Some(node);
    }

    node.children
        .iter()
        .find_map(|child| find_node_by_id(child, element_id))
}

fn find_node_by_locator<'a>(node: &'a ElementNode, locator: &Locator) -> Option<&'a ElementNode> {
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
    if let Ok(pattern) =
        unsafe { element.GetCurrentPatternAs::<IUIAutomationInvokePattern>(UIA_InvokePatternId) }
    {
        if unsafe { pattern.Invoke() }.is_ok() {
            return Ok(Some("semantic=invoke-pattern".to_owned()));
        }
    }

    if let Ok(pattern) = unsafe {
        element.GetCurrentPatternAs::<IUIAutomationLegacyIAccessiblePattern>(
            UIA_LegacyIAccessiblePatternId,
        )
    } {
        if unsafe { pattern.DoDefaultAction() }.is_ok() {
            return Ok(Some("semantic=legacy-default-action".to_owned()));
        }
    }

    Ok(None)
}

fn try_set_value_pattern(element: &IUIAutomationElement, value: &str) -> Result<bool> {
    let Ok(pattern) =
        (unsafe { element.GetCurrentPatternAs::<IUIAutomationValuePattern>(UIA_ValuePatternId) })
    else {
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

fn read_probe_window_metadata(hwnd: HWND) -> Result<Option<WindowMetadata>> {
    unsafe {
        let mut rect = RECT::default();
        if GetWindowRect(hwnd, &mut rect).is_err() {
            return Ok(None);
        }
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
            process_name: read_process_name(pid),
            title,
            class_name: read_window_class_name(hwnd),
            bounds,
        }))
    }
}

fn enumerate_child_hwnds(root: HWND) -> Result<Vec<HWND>> {
    unsafe extern "system" fn callback(hwnd: HWND, lparam: LPARAM) -> BOOL {
        let hwnds = &mut *(lparam.0 as *mut Vec<HWND>);
        hwnds.push(hwnd);
        BOOL(1)
    }

    let mut hwnds = Vec::new();
    unsafe {
        let _ = EnumChildWindows(
            Some(root),
            Some(callback),
            LPARAM((&mut hwnds as *mut Vec<HWND>) as isize),
        );
    }
    Ok(hwnds)
}

fn probe_hwnd_node(
    hwnd: HWND,
    parent_hwnd: Option<String>,
    depth: usize,
    intersects_target: bool,
    metadata: &WindowMetadata,
    _target_bounds: Rect,
) -> HwndProbeNode {
    HwndProbeNode {
        hwnd: hwnd_hex(hwnd),
        parent_hwnd,
        pid: metadata.pid,
        process_name: metadata.process_name.clone(),
        title: metadata.title.clone(),
        class_name: metadata.class_name.clone(),
        bounds: metadata.bounds,
        visible: unsafe { IsWindowVisible(hwnd) }.as_bool(),
        depth,
        intersects_target,
    }
}

fn hwnd_parent(hwnd: HWND) -> Option<HWND> {
    unsafe { GetParent(hwnd).ok() }.filter(|parent| !parent.0.is_null())
}

fn hwnd_depth(root: HWND, hwnd: HWND) -> usize {
    let mut depth = 0usize;
    let mut current = hwnd_parent(hwnd);
    while let Some(parent) = current {
        depth += 1;
        if parent == root {
            break;
        }
        current = hwnd_parent(parent);
    }
    depth
}

fn hwnd_hex(hwnd: HWND) -> String {
    format!("0x{:016x}", hwnd.0 as usize)
}

fn rects_intersect(lhs: Rect, rhs: Rect) -> bool {
    let lhs_right = lhs.left + lhs.width;
    let lhs_bottom = lhs.top + lhs.height;
    let rhs_right = rhs.left + rhs.width;
    let rhs_bottom = rhs.top + rhs.height;
    lhs.left < rhs_right && lhs_right > rhs.left && lhs.top < rhs_bottom && lhs_bottom > rhs.top
}

fn rect_contains_point(rect: Rect, point: POINT) -> bool {
    point.x >= rect.left
        && point.y >= rect.top
        && point.x < rect.left + rect.width
        && point.y < rect.top + rect.height
}

fn rect_center(bounds: Rect) -> POINT {
    POINT {
        x: bounds.left + bounds.width / 2,
        y: bounds.top + bounds.height / 2,
    }
}

fn uia_probe_looks_opaque(root: &RawObservedNode) -> bool {
    root.children.is_empty()
        && root.name.is_none()
        && root.automation_id.is_none()
        && matches!(root.control_type.as_str(), "Pane" | "Custom")
}

fn accessible_object_from_window(hwnd: HWND) -> Option<IAccessible> {
    let mut raw = ptr::null_mut();
    let result = unsafe {
        AccessibleObjectFromWindow(hwnd, OBJID_CLIENT.0 as u32, &IAccessible::IID, &mut raw)
    };
    if result.is_err() || raw.is_null() {
        return None;
    }
    Some(unsafe { IAccessible::from_raw(raw as _) })
}

fn msaa_accessible_node(
    accessible: &IAccessible,
    depth: usize,
    max_depth: usize,
) -> Result<MsaaNode> {
    let self_child = child_self_variant();
    let name = unsafe { accessible.get_accName(&self_child) }
        .ok()
        .and_then(bstr_to_option);
    let role_variant = unsafe { accessible.get_accRole(&self_child) }.ok();
    let state_variant = unsafe { accessible.get_accState(&self_child) }.ok();
    let child_count = unsafe { accessible.accChildCount() }.ok();
    let role_code = role_variant.as_ref().and_then(variant_i32);
    let state_bits = state_variant.as_ref().and_then(variant_i32);

    let mut children = Vec::new();
    if depth < max_depth {
        let limit = child_count.unwrap_or_default().clamp(0, 64);
        for child_id in 1..=limit {
            let child_variant = child_id_variant(child_id);
            children.push(msaa_child_node(
                accessible,
                &child_variant,
                depth + 1,
                max_depth,
            )?);
        }
    }

    Ok(MsaaNode {
        child_id: None,
        name,
        role: role_code.map(|value| format!("msaa_role_{value}")),
        role_code,
        state_bits,
        bounds: msaa_bounds(accessible, &self_child),
        child_count,
        children,
    })
}

fn msaa_child_node(
    accessible: &IAccessible,
    child_variant: &VARIANT,
    depth: usize,
    max_depth: usize,
) -> Result<MsaaNode> {
    let name = unsafe { accessible.get_accName(child_variant) }
        .ok()
        .and_then(bstr_to_option);
    let role_variant = unsafe { accessible.get_accRole(child_variant) }.ok();
    let state_variant = unsafe { accessible.get_accState(child_variant) }.ok();
    let role_code = role_variant.as_ref().and_then(variant_i32);
    let state_bits = state_variant.as_ref().and_then(variant_i32);
    let mut node = MsaaNode {
        child_id: variant_i32(child_variant),
        name,
        role: role_code.map(|value| format!("msaa_role_{value}")),
        role_code,
        state_bits,
        bounds: msaa_bounds(accessible, child_variant),
        child_count: None,
        children: Vec::new(),
    };

    if depth < max_depth {
        if let Ok(dispatch) = unsafe { accessible.get_accChild(child_variant) } {
            if let Ok(child_accessible) = dispatch.cast::<IAccessible>() {
                let child_root = msaa_accessible_node(&child_accessible, depth, max_depth)?;
                node.name = node.name.or(child_root.name);
                node.role = node.role.or(child_root.role);
                node.role_code = node.role_code.or(child_root.role_code);
                node.state_bits = node.state_bits.or(child_root.state_bits);
                node.bounds = node.bounds.or(child_root.bounds);
                node.child_count = child_root.child_count;
                node.children = child_root.children;
            }
        }
    }

    Ok(node)
}

fn msaa_bounds(accessible: &IAccessible, child_variant: &VARIANT) -> Option<Rect> {
    let mut left = 0i32;
    let mut top = 0i32;
    let mut width = 0i32;
    let mut height = 0i32;
    unsafe { accessible.accLocation(&mut left, &mut top, &mut width, &mut height, child_variant) }
        .ok()?;
    (width > 0 && height > 0).then_some(Rect {
        left,
        top,
        width,
        height,
    })
}

fn msaa_probe_looks_opaque(root: &MsaaNode) -> bool {
    root.children.is_empty() && root.name.is_none() && root.role_code.is_none()
}

fn msaa_hit_result_from_variant(
    accessible: &IAccessible,
    child_variant: &VARIANT,
) -> Result<MsaaHitResult> {
    let name = unsafe { accessible.get_accName(child_variant) }
        .ok()
        .and_then(bstr_to_option);
    let role_variant = unsafe { accessible.get_accRole(child_variant) }.ok();
    let state_variant = unsafe { accessible.get_accState(child_variant) }.ok();
    let role_code = role_variant.as_ref().and_then(variant_i32);
    let state_bits = state_variant.as_ref().and_then(variant_i32);
    Ok(MsaaHitResult {
        child_id: variant_i32(child_variant),
        name,
        role: role_code.map(|value| format!("msaa_role_{value}")),
        role_code,
        state_bits,
        bounds: msaa_bounds(accessible, child_variant),
    })
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
    let resolved =
        unsafe { AccessibleObjectFromEvent(hwnd, idobject, idchild, &mut accessible, &mut child) };
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
    child_id_variant(CHILDID_SELF as i32)
}

fn child_id_variant(child_id: i32) -> VARIANT {
    VARIANT {
        Anonymous: windows::Win32::System::Variant::VARIANT_0 {
            Anonymous: ManuallyDrop::new(VARIANT_0_0 {
                vt: VT_I4,
                wReserved1: 0,
                wReserved2: 0,
                wReserved3: 0,
                Anonymous: VARIANT_0_0_0 { lVal: child_id },
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

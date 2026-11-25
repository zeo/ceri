// windows ui automation server-side provider wired through wm_getobject; loads
// uiautomationcore.dll and oleaut32.dll lazily on first assistive-technology query so normal
// windows and tray-only processes pay zero dll or memory overhead

use super::a11y::{AccessibilityAction, AccessibilitySnapshot, SharedAccessibility, Toggle};
use crate::hashing::HashMap;
use std::ffi::c_void;
use std::mem::offset_of;
use std::sync::atomic::{AtomicU32, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, OnceLock, PoisonError};
use windows::Win32::Foundation::{
    E_INVALIDARG, E_NOINTERFACE, E_POINTER, HWND, LPARAM, LRESULT, POINT, S_OK, WPARAM,
};
use windows::Win32::Graphics::Gdi::{ClientToScreen, ScreenToClient};
use windows::Win32::System::LibraryLoader::{GetProcAddress, LoadLibraryW};
use windows::Win32::UI::Accessibility::{
    HeadingLevel1, NavigateDirection, NavigateDirection_FirstChild, NavigateDirection_LastChild,
    NavigateDirection_NextSibling, NavigateDirection_Parent, NavigateDirection_PreviousSibling,
    ProviderOptions, ProviderOptions_ServerSideProvider, ToggleState, ToggleState_Indeterminate,
    ToggleState_Off, ToggleState_On, UIA_AriaRolePropertyId, UIA_AutomationFocusChangedEventId,
    UIA_AutomationIdPropertyId, UIA_ButtonControlTypeId, UIA_CONTROLTYPE_ID,
    UIA_CheckBoxControlTypeId, UIA_ComboBoxControlTypeId, UIA_ControlTypePropertyId,
    UIA_CustomLandmarkTypeId, UIA_DataItemControlTypeId, UIA_DocumentControlTypeId, UIA_EVENT_ID,
    UIA_EditControlTypeId, UIA_FormLandmarkTypeId, UIA_FrameworkIdPropertyId,
    UIA_GroupControlTypeId, UIA_HasKeyboardFocusPropertyId, UIA_HeaderItemControlTypeId,
    UIA_HeadingLevelPropertyId, UIA_HyperlinkControlTypeId, UIA_ImageControlTypeId,
    UIA_InvokePatternId, UIA_IsContentElementPropertyId, UIA_IsControlElementPropertyId,
    UIA_IsEnabledPropertyId, UIA_IsKeyboardFocusablePropertyId, UIA_IsOffscreenPropertyId,
    UIA_IsPasswordPropertyId, UIA_LandmarkTypePropertyId, UIA_ListControlTypeId,
    UIA_ListItemControlTypeId, UIA_LocalizedControlTypePropertyId, UIA_MainLandmarkTypeId,
    UIA_NamePropertyId, UIA_NavigationLandmarkTypeId, UIA_PATTERN_ID, UIA_PROPERTY_ID,
    UIA_PaneControlTypeId, UIA_ProgressBarControlTypeId, UIA_ProviderDescriptionPropertyId,
    UIA_RadioButtonControlTypeId, UIA_SliderControlTypeId, UIA_TabControlTypeId,
    UIA_TabItemControlTypeId, UIA_TableControlTypeId, UIA_TextControlTypeId, UIA_TogglePatternId,
    UIA_ValuePatternId, UIA_WindowControlTypeId, UiaAppendRuntimeId, UiaRect, UiaRootObjectId,
};
use windows::Win32::UI::WindowsAndMessaging::{
    CallWindowProcW, GWLP_WNDPROC, WM_GETOBJECT, WM_NCDESTROY, WNDPROC,
};
use windows::core::{BOOL, GUID, HRESULT, PCWSTR, s, w};

const VT_EMPTY: u16 = 0;
const VT_I4: u16 = 3;
const VT_BSTR: u16 = 8;
const VT_BOOL: u16 = 11;

const IID_IUNKNOWN: GUID = GUID::from_u128(0x00000000_0000_0000_c000_000000000046);
const IID_RAW_SIMPLE: GUID = GUID::from_u128(0xd6dd68d1_86fd_4332_8666_9abedea2d24c);
const IID_RAW_FRAGMENT: GUID = GUID::from_u128(0xf7063da8_8359_439c_9297_bbc5299a7d87);
const IID_RAW_FRAGMENT_ROOT: GUID = GUID::from_u128(0x620ce2a5_ab8f_40a9_86cb_de3c75599b58);
const IID_INVOKE_PROVIDER: GUID = GUID::from_u128(0x54fcb24b_e18e_47a2_b4d3_eccbe77599a2);
const IID_TOGGLE_PROVIDER: GUID = GUID::from_u128(0x56d00bd0_c4f4_433c_a836_1a52a57e0892);
const IID_VALUE_PROVIDER: GUID = GUID::from_u128(0xc7935180_6fb3_4201_b174_7df73adbf64a);

// matches win32 variant abi layout without statically importing oleaut32.dll
#[repr(C)]
#[derive(Clone, Copy)]
struct RawVariant {
    vt: u16,
    reserved: [u16; 3],
    #[cfg(target_pointer_width = "64")]
    data: [usize; 2],
    #[cfg(target_pointer_width = "32")]
    data: [u64; 1],
}

#[cfg(target_pointer_width = "64")]
const _: () = assert!(size_of::<RawVariant>() == 24);
#[cfg(target_pointer_width = "32")]
const _: () = assert!(size_of::<RawVariant>() == 16);

impl RawVariant {
    const fn empty() -> Self {
        Self {
            vt: VT_EMPTY,
            reserved: [0; 3],
            #[cfg(target_pointer_width = "64")]
            data: [0; 2],
            #[cfg(target_pointer_width = "32")]
            data: [0; 1],
        }
    }

    fn i4(value: i32) -> Self {
        let mut out = Self::empty();
        out.vt = VT_I4;
        #[cfg(target_pointer_width = "64")]
        {
            out.data[0] = (value as u32) as usize;
        }
        #[cfg(target_pointer_width = "32")]
        {
            out.data[0] = u64::from(value as u32);
        }
        out
    }

    fn bool_val(value: bool) -> Self {
        let mut out = Self::empty();
        out.vt = VT_BOOL;
        // variant_true is -1i16 (0xffff), variant_false is 0
        let raw: u16 = if value { 0xFFFF } else { 0 };
        #[cfg(target_pointer_width = "64")]
        {
            out.data[0] = usize::from(raw);
        }
        #[cfg(target_pointer_width = "32")]
        {
            out.data[0] = u64::from(raw);
        }
        out
    }

    fn bstr(text: &str) -> Self {
        let Some(ptr) = alloc_bstr(text) else {
            return Self::empty();
        };
        let mut out = Self::empty();
        out.vt = VT_BSTR;
        #[cfg(target_pointer_width = "64")]
        {
            out.data[0] = ptr as usize;
        }
        #[cfg(target_pointer_width = "32")]
        {
            out.data[0] = ptr as usize as u64;
        }
        out
    }
}

type FarProc = unsafe extern "system" fn() -> isize;
type UiaReturnFn = unsafe extern "system" fn(HWND, WPARAM, LPARAM, *mut c_void) -> LRESULT;
type UiaHostFn = unsafe extern "system" fn(HWND, *mut *mut c_void) -> HRESULT;
type UiaRaiseEventFn = unsafe extern "system" fn(*mut c_void, UIA_EVENT_ID) -> HRESULT;
type UiaDisconnectFn = unsafe extern "system" fn(*mut c_void) -> HRESULT;
type SysAllocStringLenFn = unsafe extern "system" fn(*const u16, u32) -> *mut u16;
#[cfg(test)]
type SysFreeStringFn = unsafe extern "system" fn(*mut u16);
type SafeArrayCreateVectorFn = unsafe extern "system" fn(u16, i32, u32) -> *mut c_void;
type SafeArrayPutElementFn =
    unsafe extern "system" fn(*mut c_void, *const i32, *const c_void) -> HRESULT;
type SafeArrayDestroyFn = unsafe extern "system" fn(*mut c_void) -> HRESULT;

struct UiaApi {
    return_raw: UiaReturnFn,
    host_from_hwnd: UiaHostFn,
    raise_event: UiaRaiseEventFn,
    disconnect: UiaDisconnectFn,
}

struct OleAutApi {
    sys_alloc_string_len: SysAllocStringLenFn,
    #[cfg(test)]
    sys_free_string: SysFreeStringFn,
    safe_array_create_vector: SafeArrayCreateVectorFn,
    safe_array_put_element: SafeArrayPutElementFn,
    safe_array_destroy: SafeArrayDestroyFn,
}

fn uia_api() -> Option<&'static UiaApi> {
    static API: OnceLock<Option<UiaApi>> = OnceLock::new();
    API.get_or_init(|| {
        // safety: uiautomationcore stays loaded for the process lifetime once queried
        unsafe {
            let module = LoadLibraryW(w!("uiautomationcore.dll")).ok()?;
            Some(UiaApi {
                return_raw: std::mem::transmute::<FarProc, UiaReturnFn>(GetProcAddress(
                    module,
                    s!("UiaReturnRawElementProvider"),
                )?),
                host_from_hwnd: std::mem::transmute::<FarProc, UiaHostFn>(GetProcAddress(
                    module,
                    s!("UiaHostProviderFromHwnd"),
                )?),
                raise_event: std::mem::transmute::<FarProc, UiaRaiseEventFn>(GetProcAddress(
                    module,
                    s!("UiaRaiseAutomationEvent"),
                )?),
                disconnect: std::mem::transmute::<FarProc, UiaDisconnectFn>(GetProcAddress(
                    module,
                    s!("UiaDisconnectProvider"),
                )?),
            })
        }
    })
    .as_ref()
}

fn oleaut_api() -> Option<&'static OleAutApi> {
    static API: OnceLock<Option<OleAutApi>> = OnceLock::new();
    API.get_or_init(|| {
        // safety: oleaut32 stays loaded for the process lifetime once queried
        unsafe {
            let module = LoadLibraryW(w!("oleaut32.dll")).ok()?;
            Some(OleAutApi {
                sys_alloc_string_len: std::mem::transmute::<FarProc, SysAllocStringLenFn>(
                    GetProcAddress(module, s!("SysAllocStringLen"))?,
                ),
                #[cfg(test)]
                sys_free_string: std::mem::transmute::<FarProc, SysFreeStringFn>(GetProcAddress(
                    module,
                    s!("SysFreeString"),
                )?),
                safe_array_create_vector: std::mem::transmute::<FarProc, SafeArrayCreateVectorFn>(
                    GetProcAddress(module, s!("SafeArrayCreateVector"))?,
                ),
                safe_array_put_element: std::mem::transmute::<FarProc, SafeArrayPutElementFn>(
                    GetProcAddress(module, s!("SafeArrayPutElement"))?,
                ),
                safe_array_destroy: std::mem::transmute::<FarProc, SafeArrayDestroyFn>(
                    GetProcAddress(module, s!("SafeArrayDestroy"))?,
                ),
            })
        }
    })
    .as_ref()
}

fn alloc_bstr(text: &str) -> Option<*mut u16> {
    let api = oleaut_api()?;
    let units: Vec<u16> = text.encode_utf16().collect();
    // safety: units pointer is valid for units.len() utf-16 code units
    let ptr = unsafe { (api.sys_alloc_string_len)(units.as_ptr(), units.len() as u32) };
    (!ptr.is_null()).then_some(ptr)
}

fn alloc_runtime_id(node_raw: u64) -> Option<*mut c_void> {
    let api = oleaut_api()?;
    let parts = [
        UiaAppendRuntimeId as i32,
        (node_raw & 0xffff_ffff) as i32,
        (node_raw >> 32) as i32,
    ];
    // safety: allocates a 1d safearray of 3 vt_i4 elements and writes each index in bounds
    unsafe {
        let array = (api.safe_array_create_vector)(VT_I4, 0, parts.len() as u32);
        if array.is_null() {
            return None;
        }
        for (i, value) in parts.iter().enumerate() {
            let idx = i as i32;
            if (api.safe_array_put_element)(array, &idx, (value as *const i32).cast()).is_err() {
                let _ = (api.safe_array_destroy)(array);
                return None;
            }
        }
        Some(array)
    }
}

#[repr(C)]
struct UnknownVtbl {
    query_interface:
        unsafe extern "system" fn(*mut c_void, *const GUID, *mut *mut c_void) -> HRESULT,
    add_ref: unsafe extern "system" fn(*mut c_void) -> u32,
    release: unsafe extern "system" fn(*mut c_void) -> u32,
}

#[repr(C)]
struct SimpleVtbl {
    base: UnknownVtbl,
    provider_options: unsafe extern "system" fn(*mut c_void, *mut ProviderOptions) -> HRESULT,
    get_pattern_provider:
        unsafe extern "system" fn(*mut c_void, UIA_PATTERN_ID, *mut *mut c_void) -> HRESULT,
    get_property_value:
        unsafe extern "system" fn(*mut c_void, UIA_PROPERTY_ID, *mut RawVariant) -> HRESULT,
    host_raw_element_provider: unsafe extern "system" fn(*mut c_void, *mut *mut c_void) -> HRESULT,
}

#[repr(C)]
struct FragmentVtbl {
    base: UnknownVtbl,
    navigate:
        unsafe extern "system" fn(*mut c_void, NavigateDirection, *mut *mut c_void) -> HRESULT,
    get_runtime_id: unsafe extern "system" fn(*mut c_void, *mut *mut c_void) -> HRESULT,
    bounding_rectangle: unsafe extern "system" fn(*mut c_void, *mut UiaRect) -> HRESULT,
    get_embedded_fragment_roots:
        unsafe extern "system" fn(*mut c_void, *mut *mut c_void) -> HRESULT,
    set_focus: unsafe extern "system" fn(*mut c_void) -> HRESULT,
    fragment_root: unsafe extern "system" fn(*mut c_void, *mut *mut c_void) -> HRESULT,
}

#[repr(C)]
struct FragmentRootVtbl {
    base: UnknownVtbl,
    element_provider_from_point:
        unsafe extern "system" fn(*mut c_void, f64, f64, *mut *mut c_void) -> HRESULT,
    get_focus: unsafe extern "system" fn(*mut c_void, *mut *mut c_void) -> HRESULT,
}

#[repr(C)]
struct InvokeVtbl {
    base: UnknownVtbl,
    invoke: unsafe extern "system" fn(*mut c_void) -> HRESULT,
}

#[repr(C)]
struct ToggleVtbl {
    base: UnknownVtbl,
    toggle: unsafe extern "system" fn(*mut c_void) -> HRESULT,
    toggle_state: unsafe extern "system" fn(*mut c_void, *mut ToggleState) -> HRESULT,
}

#[repr(C)]
struct ValueVtbl {
    base: UnknownVtbl,
    set_value: unsafe extern "system" fn(*mut c_void, PCWSTR) -> HRESULT,
    value: unsafe extern "system" fn(*mut c_void, *mut *mut u16) -> HRESULT,
    is_read_only: unsafe extern "system" fn(*mut c_void, *mut BOOL) -> HRESULT,
}

struct WindowUiaState {
    hwnd: isize,
    shared: Arc<SharedAccessibility>,
    // raw simple provider pointer handed to uia; owned ref disconnected on window close
    root_provider: Mutex<Option<usize>>,
    last_focused: AtomicU64,
}

impl WindowUiaState {
    fn snapshot(&self) -> Option<Arc<AccessibilitySnapshot>> {
        self.shared.activate_and_fetch()
    }

    fn current_snapshot(&self) -> Option<Arc<AccessibilitySnapshot>> {
        self.shared.current().or_else(|| self.snapshot())
    }

    fn root_simple(self: &Arc<Self>) -> *mut c_void {
        let mut slot = self
            .root_provider
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        let ptr = match *slot {
            Some(existing) => existing as *mut c_void,
            None => {
                let created = UiaNodeBox::new_simple(self.clone(), 0, true);
                *slot = Some(created as usize);
                created
            }
        };
        // caller receives its own com reference while slot keeps one until window teardown
        unsafe { add_ref_simple(ptr) };
        ptr
    }

    fn disconnect_root(&self) {
        let ptr = self
            .root_provider
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .take();
        if let Some(raw) = ptr {
            let simple = raw as *mut c_void;
            if let Some(api) = uia_api() {
                // safety: simple is a live simple provider com pointer owned by slot
                let _ = unsafe { (api.disconnect)(simple) };
            }
            // safety: releases the reference owned by root_provider slot
            unsafe { release_simple(simple) };
        }
    }
}

#[repr(C)]
struct UiaNodeBox {
    simple_vtbl: &'static SimpleVtbl,
    fragment_vtbl: &'static FragmentVtbl,
    root_vtbl: &'static FragmentRootVtbl,
    invoke_vtbl: &'static InvokeVtbl,
    toggle_vtbl: &'static ToggleVtbl,
    value_vtbl: &'static ValueVtbl,
    ref_count: AtomicU32,
    state: Arc<WindowUiaState>,
    node_raw: u64,
    is_root: bool,
}

impl UiaNodeBox {
    fn alloc(state: Arc<WindowUiaState>, node_raw: u64, is_root: bool) -> *mut Self {
        Box::into_raw(Box::new(Self {
            simple_vtbl: &SIMPLE_VTBL,
            fragment_vtbl: &FRAGMENT_VTBL,
            root_vtbl: &FRAGMENT_ROOT_VTBL,
            invoke_vtbl: &INVOKE_VTBL,
            toggle_vtbl: &TOGGLE_VTBL,
            value_vtbl: &VALUE_VTBL,
            ref_count: AtomicU32::new(1),
            state,
            node_raw,
            is_root,
        }))
    }

    fn new_simple(state: Arc<WindowUiaState>, node_raw: u64, is_root: bool) -> *mut c_void {
        Self::alloc(state, node_raw, is_root).cast()
    }

    fn new_fragment(state: Arc<WindowUiaState>, node_raw: u64, is_root: bool) -> *mut c_void {
        let ptr = Self::alloc(state, node_raw, is_root);
        // safety: fragment_vtbl lies inside the freshly allocated node box
        unsafe { (ptr as *mut u8).add(offset_of!(Self, fragment_vtbl)).cast() }
    }

    fn add_ref(&self) -> u32 {
        self.ref_count.fetch_add(1, Ordering::Relaxed) + 1
    }

    unsafe fn release(this: *mut Self) -> u32 {
        // safety: caller transfers one valid com reference to this live node box
        let prev = unsafe { (*this).ref_count.fetch_sub(1, Ordering::Release) };
        if prev == 1 {
            std::sync::atomic::fence(Ordering::Acquire);
            // safety: ref_count reached 0 so no other com references exist
            unsafe { drop(Box::from_raw(this)) };
        }
        prev - 1
    }

    fn resolve<'a>(
        &self,
        snapshot: &'a AccessibilitySnapshot,
    ) -> Option<(usize, &'a super::a11y::A11yNode)> {
        let index = if self.is_root {
            0
        } else {
            snapshot.index_of(self.node_raw)?
        };
        Some((index, snapshot.nodes.get(index)?))
    }

    unsafe fn query_interface(
        this: *mut Self,
        riid: *const GUID,
        out: *mut *mut c_void,
    ) -> HRESULT {
        if out.is_null() {
            return E_POINTER;
        }
        // safety: out is valid and non-null
        unsafe { *out = std::ptr::null_mut() };
        if riid.is_null() {
            return E_INVALIDARG;
        }
        // safety: this and riid point to live values for the duration of the call
        let (node, iid) = unsafe { (&*this, *riid) };
        let offset = if iid == IID_IUNKNOWN || iid == IID_RAW_SIMPLE {
            Some(offset_of!(Self, simple_vtbl))
        } else if iid == IID_RAW_FRAGMENT {
            Some(offset_of!(Self, fragment_vtbl))
        } else if iid == IID_RAW_FRAGMENT_ROOT && node.is_root {
            Some(offset_of!(Self, root_vtbl))
        } else if iid == IID_INVOKE_PROVIDER {
            node.state
                .current_snapshot()
                .and_then(|s| node.resolve(&s).map(|(i, _)| s.supports_invoke(i)))
                .unwrap_or(false)
                .then_some(offset_of!(Self, invoke_vtbl))
        } else if iid == IID_TOGGLE_PROVIDER {
            node.state
                .current_snapshot()
                .and_then(|s| node.resolve(&s).map(|(i, _)| s.supports_toggle(i)))
                .unwrap_or(false)
                .then_some(offset_of!(Self, toggle_vtbl))
        } else if iid == IID_VALUE_PROVIDER {
            node.state
                .current_snapshot()
                .and_then(|s| node.resolve(&s).map(|(i, _)| s.supports_value(i)))
                .unwrap_or(false)
                .then_some(offset_of!(Self, value_vtbl))
        } else {
            None
        };
        let Some(offset) = offset else {
            return E_NOINTERFACE;
        };
        node.add_ref();
        // safety: offset lies inside node box and out is valid
        unsafe { *out = (this as *mut u8).add(offset).cast() };
        S_OK
    }
}

unsafe fn from_offset<const OFFSET: usize>(this: *mut c_void) -> *mut UiaNodeBox {
    // safety: com vtable pointer sits at byte offset inside node box
    unsafe { (this as *mut u8).sub(OFFSET).cast() }
}

unsafe fn add_ref_simple(simple: *mut c_void) -> u32 {
    // safety: simple points to offset 0 of node box
    unsafe { (*simple.cast::<UiaNodeBox>()).add_ref() }
}

unsafe fn release_simple(simple: *mut c_void) -> u32 {
    // safety: simple points to offset 0 of node box
    unsafe { UiaNodeBox::release(simple.cast()) }
}

unsafe extern "system" fn qi_thunk<const OFFSET: usize>(
    this: *mut c_void,
    riid: *const GUID,
    out: *mut *mut c_void,
) -> HRESULT {
    unsafe { UiaNodeBox::query_interface(from_offset::<OFFSET>(this), riid, out) }
}

unsafe extern "system" fn add_ref_thunk<const OFFSET: usize>(this: *mut c_void) -> u32 {
    unsafe { (*from_offset::<OFFSET>(this)).add_ref() }
}

unsafe extern "system" fn release_thunk<const OFFSET: usize>(this: *mut c_void) -> u32 {
    unsafe { UiaNodeBox::release(from_offset::<OFFSET>(this)) }
}

const fn unknown_vtbl<const OFFSET: usize>() -> UnknownVtbl {
    UnknownVtbl {
        query_interface: qi_thunk::<OFFSET>,
        add_ref: add_ref_thunk::<OFFSET>,
        release: release_thunk::<OFFSET>,
    }
}

fn control_type_for(role: &str) -> UIA_CONTROLTYPE_ID {
    match role {
        "document" => UIA_DocumentControlTypeId,
        "button" => UIA_ButtonControlTypeId,
        "link" => UIA_HyperlinkControlTypeId,
        "textbox" | "searchbox" => UIA_EditControlTypeId,
        "checkbox" | "switch" => UIA_CheckBoxControlTypeId,
        "radio" => UIA_RadioButtonControlTypeId,
        "combobox" => UIA_ComboBoxControlTypeId,
        "option" | "listitem" => UIA_ListItemControlTypeId,
        "slider" => UIA_SliderControlTypeId,
        "img" => UIA_ImageControlTypeId,
        "heading" | "label" => UIA_TextControlTypeId,
        "list" | "listbox" => UIA_ListControlTypeId,
        "table" => UIA_TableControlTypeId,
        "row" | "cell" => UIA_DataItemControlTypeId,
        "columnheader" => UIA_HeaderItemControlTypeId,
        "tab" => UIA_TabItemControlTypeId,
        "tablist" => UIA_TabControlTypeId,
        "progressbar" => UIA_ProgressBarControlTypeId,
        "dialog" => UIA_WindowControlTypeId,
        "main" | "navigation" | "banner" | "contentinfo" | "complementary" | "form" => {
            UIA_GroupControlTypeId
        }
        _ => UIA_PaneControlTypeId,
    }
}

fn landmark_type_for(role: &str) -> Option<i32> {
    match role {
        "main" => Some(UIA_MainLandmarkTypeId.0),
        "navigation" => Some(UIA_NavigationLandmarkTypeId.0),
        "form" => Some(UIA_FormLandmarkTypeId.0),
        "banner" | "contentinfo" | "complementary" => Some(UIA_CustomLandmarkTypeId.0),
        _ => None,
    }
}

unsafe extern "system" fn simple_provider_options(
    _this: *mut c_void,
    out: *mut ProviderOptions,
) -> HRESULT {
    if out.is_null() {
        return E_POINTER;
    }
    unsafe { *out = ProviderOptions_ServerSideProvider };
    S_OK
}

unsafe extern "system" fn simple_get_pattern_provider(
    this: *mut c_void,
    pattern_id: UIA_PATTERN_ID,
    out: *mut *mut c_void,
) -> HRESULT {
    if out.is_null() {
        return E_POINTER;
    }
    unsafe { *out = std::ptr::null_mut() };
    let node_ptr = this.cast::<UiaNodeBox>();
    let node = unsafe { &*node_ptr };
    let Some(snapshot) = node.state.current_snapshot() else {
        return S_OK;
    };
    let Some((index, _)) = node.resolve(&snapshot) else {
        return S_OK;
    };
    let offset = match pattern_id {
        id if id == UIA_InvokePatternId && snapshot.supports_invoke(index) => {
            Some(offset_of!(UiaNodeBox, invoke_vtbl))
        }
        id if id == UIA_TogglePatternId && snapshot.supports_toggle(index) => {
            Some(offset_of!(UiaNodeBox, toggle_vtbl))
        }
        id if id == UIA_ValuePatternId && snapshot.supports_value(index) => {
            Some(offset_of!(UiaNodeBox, value_vtbl))
        }
        _ => None,
    };
    if let Some(offset) = offset {
        node.add_ref();
        unsafe { *out = (node_ptr as *mut u8).add(offset).cast() };
    }
    S_OK
}

unsafe extern "system" fn simple_get_property_value(
    this: *mut c_void,
    property_id: UIA_PROPERTY_ID,
    out: *mut RawVariant,
) -> HRESULT {
    if out.is_null() {
        return E_POINTER;
    }
    unsafe { *out = RawVariant::empty() };
    let node = unsafe { &*this.cast::<UiaNodeBox>() };
    let Some(snapshot) = node.state.current_snapshot() else {
        if property_id == UIA_ControlTypePropertyId && node.is_root {
            unsafe { *out = RawVariant::i4(UIA_DocumentControlTypeId.0) };
        }
        return S_OK;
    };
    let Some((_, entry)) = node.resolve(&snapshot) else {
        return S_OK;
    };
    let value = match property_id {
        id if id == UIA_ControlTypePropertyId => RawVariant::i4(control_type_for(&entry.role).0),
        id if id == UIA_LandmarkTypePropertyId => match landmark_type_for(&entry.role) {
            Some(landmark) => RawVariant::i4(landmark),
            None => RawVariant::empty(),
        },
        id if id == UIA_NamePropertyId => RawVariant::bstr(&entry.name),
        id if id == UIA_AutomationIdPropertyId => match &entry.automation_id {
            Some(automation_id) => RawVariant::bstr(automation_id),
            None => RawVariant::empty(),
        },
        id if id == UIA_AriaRolePropertyId => RawVariant::bstr(&entry.role),
        id if id == UIA_LocalizedControlTypePropertyId => match entry.role.as_str() {
            "heading" => RawVariant::bstr("heading"),
            "switch" => RawVariant::bstr("switch"),
            "banner" | "navigation" | "main" | "contentinfo" | "complementary" | "form" => {
                RawVariant::bstr(&entry.role)
            }
            _ => RawVariant::empty(),
        },
        id if id == UIA_HeadingLevelPropertyId => match entry.heading_level {
            Some(level) => RawVariant::i4(HeadingLevel1.0 + i32::from(level.saturating_sub(1))),
            None => RawVariant::empty(),
        },
        id if id == UIA_IsEnabledPropertyId => RawVariant::bool_val(!entry.disabled),
        id if id == UIA_IsKeyboardFocusablePropertyId => RawVariant::bool_val(entry.focusable),
        id if id == UIA_HasKeyboardFocusPropertyId => RawVariant::bool_val(entry.focused),
        id if id == UIA_IsPasswordPropertyId => RawVariant::bool_val(entry.password),
        id if id == UIA_IsControlElementPropertyId => RawVariant::bool_val(true),
        id if id == UIA_IsContentElementPropertyId => {
            RawVariant::bool_val(entry.role != "generic" || !entry.name.is_empty())
        }
        id if id == UIA_IsOffscreenPropertyId => RawVariant::bool_val(!entry.visible),
        id if id == UIA_FrameworkIdPropertyId => RawVariant::bstr("Ceri"),
        id if id == UIA_ProviderDescriptionPropertyId => RawVariant::bstr("Ceri UIA Provider"),
        _ => RawVariant::empty(),
    };
    unsafe { *out = value };
    S_OK
}

unsafe extern "system" fn simple_host_raw_element_provider(
    this: *mut c_void,
    out: *mut *mut c_void,
) -> HRESULT {
    if out.is_null() {
        return E_POINTER;
    }
    unsafe { *out = std::ptr::null_mut() };
    let node = unsafe { &*this.cast::<UiaNodeBox>() };
    if !node.is_root || node.state.hwnd == 0 {
        return S_OK;
    }
    let Some(api) = uia_api() else {
        return S_OK;
    };
    unsafe { (api.host_from_hwnd)(HWND(node.state.hwnd as *mut _), out) }
}

unsafe extern "system" fn fragment_navigate(
    this: *mut c_void,
    direction: NavigateDirection,
    out: *mut *mut c_void,
) -> HRESULT {
    if out.is_null() {
        return E_POINTER;
    }
    unsafe { *out = std::ptr::null_mut() };
    let node = unsafe { &*from_offset::<{ offset_of!(UiaNodeBox, fragment_vtbl) }>(this) };
    let Some(snapshot) = node.state.current_snapshot() else {
        return S_OK;
    };
    let Some((_, entry)) = node.resolve(&snapshot) else {
        return S_OK;
    };
    let target = match direction {
        d if d == NavigateDirection_Parent => entry.parent,
        d if d == NavigateDirection_FirstChild => entry.first_child,
        d if d == NavigateDirection_LastChild => entry.last_child,
        d if d == NavigateDirection_NextSibling => entry.next_sibling,
        d if d == NavigateDirection_PreviousSibling => entry.prev_sibling,
        _ => None,
    };
    if let Some(target_index) = target
        && let Some(target_node) = snapshot.nodes.get(target_index)
    {
        let frag = UiaNodeBox::new_fragment(
            node.state.clone(),
            target_node.node.raw(),
            target_index == 0,
        );
        unsafe { *out = frag };
    }
    S_OK
}

unsafe extern "system" fn fragment_get_runtime_id(
    this: *mut c_void,
    out: *mut *mut c_void,
) -> HRESULT {
    if out.is_null() {
        return E_POINTER;
    }
    unsafe { *out = std::ptr::null_mut() };
    let node = unsafe { &*from_offset::<{ offset_of!(UiaNodeBox, fragment_vtbl) }>(this) };
    // a fragment root hosted by an hwnd returns null so uia supplies the hwnd runtime id
    if node.is_root {
        return S_OK;
    }
    if let Some(array) = alloc_runtime_id(node.node_raw) {
        unsafe { *out = array };
    }
    S_OK
}

unsafe extern "system" fn fragment_bounding_rectangle(
    this: *mut c_void,
    out: *mut UiaRect,
) -> HRESULT {
    if out.is_null() {
        return E_POINTER;
    }
    let node = unsafe { &*from_offset::<{ offset_of!(UiaNodeBox, fragment_vtbl) }>(this) };
    let mut rect = UiaRect::default();
    if let Some(snapshot) = node.state.current_snapshot()
        && let Some((_, entry)) = node.resolve(&snapshot)
        && let Some([x, y, width, height]) = entry.bounds
    {
        let scale = f64::from(snapshot.scale.max(0.01));
        let mut origin = POINT {
            x: (f64::from(x) * scale).round() as i32,
            y: (f64::from(y) * scale).round() as i32,
        };
        if node.state.hwnd != 0 {
            // safety: hwnd belongs to the live window and origin is a valid point
            let _ = unsafe { ClientToScreen(HWND(node.state.hwnd as *mut _), &mut origin) };
        }
        rect = UiaRect {
            left: f64::from(origin.x),
            top: f64::from(origin.y),
            width: (f64::from(width) * scale).max(0.0),
            height: (f64::from(height) * scale).max(0.0),
        };
    }
    unsafe { *out = rect };
    S_OK
}

unsafe extern "system" fn fragment_get_embedded_roots(
    _this: *mut c_void,
    out: *mut *mut c_void,
) -> HRESULT {
    if out.is_null() {
        return E_POINTER;
    }
    unsafe { *out = std::ptr::null_mut() };
    S_OK
}

unsafe extern "system" fn fragment_set_focus(this: *mut c_void) -> HRESULT {
    let node = unsafe { &*from_offset::<{ offset_of!(UiaNodeBox, fragment_vtbl) }>(this) };
    if !node.is_root {
        let _ = node
            .state
            .shared
            .send_action(AccessibilityAction::Focus(node.node_raw));
    }
    S_OK
}

unsafe extern "system" fn fragment_root(this: *mut c_void, out: *mut *mut c_void) -> HRESULT {
    if out.is_null() {
        return E_POINTER;
    }
    let node_ptr = unsafe { from_offset::<{ offset_of!(UiaNodeBox, fragment_vtbl) }>(this) };
    let node = unsafe { &*node_ptr };
    let root = if node.is_root {
        node.add_ref();
        unsafe {
            (node_ptr as *mut u8)
                .add(offset_of!(UiaNodeBox, root_vtbl))
                .cast()
        }
    } else {
        let created = UiaNodeBox::alloc(node.state.clone(), 0, true);
        unsafe {
            (created as *mut u8)
                .add(offset_of!(UiaNodeBox, root_vtbl))
                .cast()
        }
    };
    unsafe { *out = root };
    S_OK
}

unsafe extern "system" fn root_element_from_point(
    this: *mut c_void,
    x: f64,
    y: f64,
    out: *mut *mut c_void,
) -> HRESULT {
    if out.is_null() {
        return E_POINTER;
    }
    unsafe { *out = std::ptr::null_mut() };
    let node = unsafe { &*from_offset::<{ offset_of!(UiaNodeBox, root_vtbl) }>(this) };
    let Some(snapshot) = node.state.current_snapshot() else {
        return S_OK;
    };
    let mut point = POINT {
        x: x.round() as i32,
        y: y.round() as i32,
    };
    if node.state.hwnd != 0 {
        // safety: hwnd belongs to the live window and point is a valid point
        let _ = unsafe { ScreenToClient(HWND(node.state.hwnd as *mut _), &mut point) };
    }
    let scale = snapshot.scale.max(0.01);
    let css_x = point.x as f32 / scale;
    let css_y = point.y as f32 / scale;
    let hit = snapshot.hit_test(css_x, css_y);
    if let Some(entry) = snapshot.nodes.get(hit) {
        let frag = UiaNodeBox::new_fragment(node.state.clone(), entry.node.raw(), hit == 0);
        unsafe { *out = frag };
    }
    S_OK
}

unsafe extern "system" fn root_get_focus(this: *mut c_void, out: *mut *mut c_void) -> HRESULT {
    if out.is_null() {
        return E_POINTER;
    }
    unsafe { *out = std::ptr::null_mut() };
    let node = unsafe { &*from_offset::<{ offset_of!(UiaNodeBox, root_vtbl) }>(this) };
    let Some(snapshot) = node.state.current_snapshot() else {
        return S_OK;
    };
    if let Some(index) = snapshot.focused
        && let Some(entry) = snapshot.nodes.get(index)
    {
        let frag = UiaNodeBox::new_fragment(node.state.clone(), entry.node.raw(), index == 0);
        unsafe { *out = frag };
    }
    S_OK
}

unsafe extern "system" fn invoke_invoke(this: *mut c_void) -> HRESULT {
    let node = unsafe { &*from_offset::<{ offset_of!(UiaNodeBox, invoke_vtbl) }>(this) };
    let _ = node
        .state
        .shared
        .send_action(AccessibilityAction::Invoke(node.node_raw));
    S_OK
}

unsafe extern "system" fn toggle_toggle(this: *mut c_void) -> HRESULT {
    let node = unsafe { &*from_offset::<{ offset_of!(UiaNodeBox, toggle_vtbl) }>(this) };
    let _ = node
        .state
        .shared
        .send_action(AccessibilityAction::Toggle(node.node_raw));
    S_OK
}

unsafe extern "system" fn toggle_state(this: *mut c_void, out: *mut ToggleState) -> HRESULT {
    if out.is_null() {
        return E_POINTER;
    }
    let node = unsafe { &*from_offset::<{ offset_of!(UiaNodeBox, toggle_vtbl) }>(this) };
    let state = node
        .state
        .current_snapshot()
        .and_then(|snapshot| {
            let (_, entry) = node.resolve(&snapshot)?;
            Some(match entry.toggle {
                Some(Toggle::On) => ToggleState_On,
                Some(Toggle::Indeterminate) => ToggleState_Indeterminate,
                Some(Toggle::Off) | None => ToggleState_Off,
            })
        })
        .unwrap_or(ToggleState_Off);
    unsafe { *out = state };
    S_OK
}

unsafe extern "system" fn value_set_value(this: *mut c_void, val: PCWSTR) -> HRESULT {
    let node = unsafe { &*from_offset::<{ offset_of!(UiaNodeBox, value_vtbl) }>(this) };
    let text = if val.is_null() {
        String::new()
    } else {
        // safety: val is a nul-terminated utf-16 string supplied by the com caller
        unsafe { val.to_string().unwrap_or_default() }
    };
    let _ = node
        .state
        .shared
        .send_action(AccessibilityAction::SetValue(node.node_raw, text));
    S_OK
}

unsafe extern "system" fn value_value(this: *mut c_void, out: *mut *mut u16) -> HRESULT {
    if out.is_null() {
        return E_POINTER;
    }
    unsafe { *out = std::ptr::null_mut() };
    let node = unsafe { &*from_offset::<{ offset_of!(UiaNodeBox, value_vtbl) }>(this) };
    let text = node
        .state
        .current_snapshot()
        .and_then(|snapshot| {
            let (_, entry) = node.resolve(&snapshot)?;
            if entry.password {
                return Some(String::new());
            }
            entry.value.clone()
        })
        .unwrap_or_default();
    if let Some(bstr) = alloc_bstr(&text) {
        unsafe { *out = bstr };
    }
    S_OK
}

unsafe extern "system" fn value_is_read_only(this: *mut c_void, out: *mut BOOL) -> HRESULT {
    if out.is_null() {
        return E_POINTER;
    }
    let node = unsafe { &*from_offset::<{ offset_of!(UiaNodeBox, value_vtbl) }>(this) };
    let read_only = node
        .state
        .current_snapshot()
        .and_then(|snapshot| {
            let (_, entry) = node.resolve(&snapshot)?;
            Some(entry.read_only || entry.disabled)
        })
        .unwrap_or(true);
    unsafe { *out = BOOL::from(read_only) };
    S_OK
}

static SIMPLE_VTBL: SimpleVtbl = SimpleVtbl {
    base: unknown_vtbl::<{ offset_of!(UiaNodeBox, simple_vtbl) }>(),
    provider_options: simple_provider_options,
    get_pattern_provider: simple_get_pattern_provider,
    get_property_value: simple_get_property_value,
    host_raw_element_provider: simple_host_raw_element_provider,
};

static FRAGMENT_VTBL: FragmentVtbl = FragmentVtbl {
    base: unknown_vtbl::<{ offset_of!(UiaNodeBox, fragment_vtbl) }>(),
    navigate: fragment_navigate,
    get_runtime_id: fragment_get_runtime_id,
    bounding_rectangle: fragment_bounding_rectangle,
    get_embedded_fragment_roots: fragment_get_embedded_roots,
    set_focus: fragment_set_focus,
    fragment_root,
};

static FRAGMENT_ROOT_VTBL: FragmentRootVtbl = FragmentRootVtbl {
    base: unknown_vtbl::<{ offset_of!(UiaNodeBox, root_vtbl) }>(),
    element_provider_from_point: root_element_from_point,
    get_focus: root_get_focus,
};

static INVOKE_VTBL: InvokeVtbl = InvokeVtbl {
    base: unknown_vtbl::<{ offset_of!(UiaNodeBox, invoke_vtbl) }>(),
    invoke: invoke_invoke,
};

static TOGGLE_VTBL: ToggleVtbl = ToggleVtbl {
    base: unknown_vtbl::<{ offset_of!(UiaNodeBox, toggle_vtbl) }>(),
    toggle: toggle_toggle,
    toggle_state,
};

static VALUE_VTBL: ValueVtbl = ValueVtbl {
    base: unknown_vtbl::<{ offset_of!(UiaNodeBox, value_vtbl) }>(),
    set_value: value_set_value,
    value: value_value,
    is_read_only: value_is_read_only,
};

struct SubclassEntry {
    prev_proc: WNDPROC,
    state: Arc<WindowUiaState>,
}

fn subclasses() -> &'static Mutex<HashMap<isize, SubclassEntry>> {
    static MAP: OnceLock<Mutex<HashMap<isize, SubclassEntry>>> = OnceLock::new();
    MAP.get_or_init(|| Mutex::new(HashMap::default()))
}

#[cfg(target_pointer_width = "64")]
unsafe fn set_wndproc(hwnd: HWND, proc: isize) -> isize {
    use windows::Win32::UI::WindowsAndMessaging::SetWindowLongPtrW;
    unsafe { SetWindowLongPtrW(hwnd, GWLP_WNDPROC, proc) }
}

#[cfg(target_pointer_width = "32")]
unsafe fn set_wndproc(hwnd: HWND, proc: isize) -> isize {
    use windows::Win32::UI::WindowsAndMessaging::SetWindowLongW;
    unsafe { SetWindowLongW(hwnd, GWLP_WNDPROC, proc as i32) as isize }
}

#[cfg(target_pointer_width = "64")]
unsafe fn get_wndproc(hwnd: HWND) -> isize {
    use windows::Win32::UI::WindowsAndMessaging::GetWindowLongPtrW;
    unsafe { GetWindowLongPtrW(hwnd, GWLP_WNDPROC) }
}

#[cfg(target_pointer_width = "32")]
unsafe fn get_wndproc(hwnd: HWND) -> isize {
    use windows::Win32::UI::WindowsAndMessaging::GetWindowLongW;
    unsafe { GetWindowLongW(hwnd, GWLP_WNDPROC) as isize }
}

unsafe extern "system" fn uia_wndproc(
    hwnd: HWND,
    msg: u32,
    wparam: WPARAM,
    lparam: LPARAM,
) -> LRESULT {
    let key = hwnd.0 as isize;
    if msg == WM_GETOBJECT && (lparam.0 as i32) == UiaRootObjectId {
        let state = subclasses()
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .get(&key)
            .map(|entry| entry.state.clone());
        if let Some(state) = state
            && let Some(api) = uia_api()
        {
            let _ = state.snapshot();
            let simple = state.root_simple();
            // safety: uia return helper addrefs simple if needed; we release our temp ref
            let result = unsafe { (api.return_raw)(hwnd, wparam, lparam, simple) };
            unsafe { release_simple(simple) };
            return result;
        }
    }
    let prev = if msg == WM_NCDESTROY {
        let removed = subclasses()
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .remove(&key);
        if let Some(entry) = removed {
            entry.state.disconnect_root();
            entry.prev_proc
        } else {
            None
        }
    } else {
        subclasses()
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .get(&key)
            .and_then(|entry| entry.prev_proc)
    };
    unsafe { CallWindowProcW(prev, hwnd, msg, wparam, lparam) }
}

// owns the hwnd wm_getobject subclass for one native window and cleans it up on drop
pub(crate) struct WindowUiaBridge {
    hwnd: isize,
    state: Arc<WindowUiaState>,
}

impl WindowUiaBridge {
    pub(crate) fn attach(hwnd: isize, shared: Arc<SharedAccessibility>) -> Option<Self> {
        if hwnd == 0 {
            return None;
        }
        let state = Arc::new(WindowUiaState {
            hwnd,
            shared,
            root_provider: Mutex::new(None),
            last_focused: AtomicU64::new(0),
        });
        let raw_hwnd = HWND(hwnd as *mut _);
        let proc_addr = uia_wndproc as *const () as usize as isize;
        // safety: installs our wm_getobject wrapper while chaining every other message to winit
        let previous = unsafe { set_wndproc(raw_hwnd, proc_addr) };
        if previous == 0 {
            return None;
        }
        // safety: previous is a valid wndproc pointer returned by setwindowlongptrw
        let prev_proc: WNDPROC = unsafe { std::mem::transmute::<isize, WNDPROC>(previous) };
        subclasses()
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .insert(
                hwnd,
                SubclassEntry {
                    prev_proc,
                    state: state.clone(),
                },
            );
        Some(Self { hwnd, state })
    }

    pub(crate) fn rebind(&mut self, shared: Arc<SharedAccessibility>) {
        let next = Arc::new(WindowUiaState {
            hwnd: self.hwnd,
            shared,
            root_provider: Mutex::new(None),
            last_focused: AtomicU64::new(0),
        });
        self.state.disconnect_root();
        if let Some(entry) = subclasses()
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .get_mut(&self.hwnd)
        {
            entry.state = next.clone();
        }
        self.state = next;
    }

    // notifies assistive technology of keyboard focus movement only when uia is already active
    pub(crate) fn sync_focus(&self) {
        if !self.state.shared.is_active() {
            return;
        }
        let Some(snapshot) = self.state.shared.current() else {
            return;
        };
        let focused_raw = snapshot
            .focused
            .and_then(|idx| snapshot.nodes.get(idx))
            .map_or(0, |entry| entry.node.raw());
        let prev = self.state.last_focused.swap(focused_raw, Ordering::AcqRel);
        if focused_raw == 0 || focused_raw == prev {
            return;
        }
        let Some(api) = uia_api() else {
            return;
        };
        let simple = UiaNodeBox::new_simple(self.state.clone(), focused_raw, false);
        // safety: simple is a live simple provider released right after raising the event
        let _ = unsafe { (api.raise_event)(simple, UIA_AutomationFocusChangedEventId) };
        unsafe { release_simple(simple) };
    }
}

impl Drop for WindowUiaBridge {
    fn drop(&mut self) {
        let removed = subclasses()
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .remove(&self.hwnd);
        if let Some(entry) = removed {
            let raw_hwnd = HWND(self.hwnd as *mut _);
            let ours = uia_wndproc as *const () as usize as isize;
            // safety: only restore previous wndproc if our thunk is still at the head of the chain
            unsafe {
                if get_wndproc(raw_hwnd) == ours
                    && let Some(prev) = entry.prev_proc
                {
                    let _ = set_wndproc(raw_hwnd, prev as usize as isize);
                }
            }
            entry.state.disconnect_root();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::host::{EngineHost, EngineWorker};
    use crate::layout::Size;
    use crate::navigation::{Browser, ResourceBundle};
    use windows::Win32::System::LibraryLoader::GetModuleHandleW;

    unsafe fn bstr_to_string(ptr: *mut u16) -> String {
        if ptr.is_null() {
            return String::new();
        }
        let mut len = 0usize;
        while unsafe { *ptr.add(len) } != 0 {
            len += 1;
        }
        let text = String::from_utf16_lossy(unsafe { std::slice::from_raw_parts(ptr, len) });
        if let Some(api) = oleaut_api() {
            unsafe { (api.sys_free_string)(ptr) };
        }
        text
    }

    unsafe fn variant_string(variant: RawVariant) -> String {
        assert_eq!(variant.vt, VT_BSTR);
        #[cfg(target_pointer_width = "64")]
        let ptr = variant.data[0] as *mut u16;
        #[cfg(target_pointer_width = "32")]
        let ptr = variant.data[0] as usize as *mut u16;
        unsafe { bstr_to_string(ptr) }
    }

    fn variant_i4(variant: RawVariant) -> i32 {
        assert_eq!(variant.vt, VT_I4);
        variant.data[0] as u32 as i32
    }

    fn variant_bool(variant: RawVariant) -> bool {
        assert_eq!(variant.vt, VT_BOOL);
        (variant.data[0] as u16) != 0
    }

    #[test]
    fn uia_com_provider_exposes_tree_properties_patterns_and_actions_headless() {
        // verify dynamic loading: uiautomationcore is not statically imported into the test binary
        assert!(
            unsafe { GetModuleHandleW(w!("uiautomationcore.dll")) }.is_err(),
            "uiautomationcore.dll must stay unloaded until a uia query arrives"
        );

        let html = r#"<!doctype html><title>Panel</title><body style="margin:0">
            <nav><a id="lnk" href="/next" style="display:block;width:100px;height:24px">Docs</a></nav>
            <main>
                <h1 id="title" style="margin:0;height:28px">Controls</h1>
                <button id="act" style="display:block;width:120px;height:32px">Run</button>
                <input id="opt" type="checkbox" style="display:block;width:24px;height:24px">
                <input id="txt" type="text" value="hello" style="display:block;width:160px;height:28px">
                <p id="log">idle</p>
            </main>
            <script>
                document.getElementById('act').onclick = () => {
                    document.getElementById('log').textContent = 'ran';
                };
            </script>
        </body>"#;

        let mut resources = ResourceBundle::default();
        resources
            .insert("/", "text/html; charset=utf-8", html.as_bytes().to_vec())
            .expect("html resource");
        let (tx, _rx) = std::sync::mpsc::sync_channel(8);
        let shared = Arc::new(SharedAccessibility::new(tx));
        let mut host = EngineHost::new(
            Browser::new(resources.clone()),
            Size {
                width: 640.0,
                height: 480.0,
            },
        );
        host.navigate("https://app.ceri.invalid/")
            .expect("navigate");
        let snapshot = Arc::new(host.accessibility_snapshot().expect("snapshot"));
        shared.publish(Some(snapshot.clone()));

        let state = Arc::new(WindowUiaState {
            hwnd: 0,
            shared: shared.clone(),
            root_provider: Mutex::new(None),
            last_focused: AtomicU64::new(0),
        });
        let root_simple = UiaNodeBox::new_simple(state.clone(), 0, true);

        // safety: exercises our com vtables directly through standard com pointer rules
        unsafe {
            let vtbl = &*(*root_simple.cast::<*const SimpleVtbl>());
            let mut opts = ProviderOptions(0);
            assert_eq!((vtbl.provider_options)(root_simple, &mut opts), S_OK);
            assert_eq!(opts, ProviderOptions_ServerSideProvider);

            let mut prop = RawVariant::empty();
            assert_eq!(
                (vtbl.get_property_value)(root_simple, UIA_ControlTypePropertyId, &mut prop),
                S_OK
            );
            assert_eq!(variant_i4(prop), UIA_DocumentControlTypeId.0);

            assert_eq!(
                (vtbl.get_property_value)(root_simple, UIA_NamePropertyId, &mut prop),
                S_OK
            );
            assert_eq!(variant_string(prop), "Panel");

            assert_eq!(
                (vtbl.get_property_value)(root_simple, UIA_FrameworkIdPropertyId, &mut prop),
                S_OK
            );
            assert_eq!(variant_string(prop), "Ceri");

            // queryinterface identity and fragmentroot support on root
            let mut root_frag_root = std::ptr::null_mut();
            assert_eq!(
                (vtbl.base.query_interface)(
                    root_simple,
                    &IID_RAW_FRAGMENT_ROOT,
                    &mut root_frag_root
                ),
                S_OK
            );
            assert!(!root_frag_root.is_null());

            let mut unk_from_root = std::ptr::null_mut();
            let root_vtbl = &*(*root_frag_root.cast::<*const FragmentRootVtbl>());
            assert_eq!(
                (root_vtbl.base.query_interface)(root_frag_root, &IID_IUNKNOWN, &mut unk_from_root),
                S_OK
            );
            assert_eq!(unk_from_root, root_simple, "com identity must match");
            (vtbl.base.release)(unk_from_root);

            // navigate root -> first child (nav) -> next sibling (main) -> children
            let mut root_frag = std::ptr::null_mut();
            assert_eq!(
                (vtbl.base.query_interface)(root_simple, &IID_RAW_FRAGMENT, &mut root_frag),
                S_OK
            );
            let frag_vtbl = &*(*root_frag.cast::<*const FragmentVtbl>());
            // root runtime id returns null
            let mut root_rid = std::ptr::null_mut();
            assert_eq!((frag_vtbl.get_runtime_id)(root_frag, &mut root_rid), S_OK);
            assert!(root_rid.is_null());

            let mut nav_frag = std::ptr::null_mut();
            assert_eq!(
                (frag_vtbl.navigate)(root_frag, NavigateDirection_FirstChild, &mut nav_frag),
                S_OK
            );
            assert!(!nav_frag.is_null());

            let nav_fvtbl = &*(*nav_frag.cast::<*const FragmentVtbl>());
            // non-root runtime id returns a valid safearray and must not qi fragmentroot
            let mut nav_rid = std::ptr::null_mut();
            assert_eq!((nav_fvtbl.get_runtime_id)(nav_frag, &mut nav_rid), S_OK);
            assert!(!nav_rid.is_null());
            let _ = (oleaut_api().expect("oleaut").safe_array_destroy)(nav_rid);

            let mut bad_root = std::ptr::null_mut();
            assert_eq!(
                (nav_fvtbl.base.query_interface)(nav_frag, &IID_RAW_FRAGMENT_ROOT, &mut bad_root),
                E_NOINTERFACE
            );

            let mut main_frag = std::ptr::null_mut();
            assert_eq!(
                (nav_fvtbl.navigate)(nav_frag, NavigateDirection_NextSibling, &mut main_frag),
                S_OK
            );
            assert!(!main_frag.is_null());
            let main_fvtbl = &*(*main_frag.cast::<*const FragmentVtbl>());

            // h1 -> button -> checkbox -> textbox
            let mut h1_frag = std::ptr::null_mut();
            assert_eq!(
                (main_fvtbl.navigate)(main_frag, NavigateDirection_FirstChild, &mut h1_frag),
                S_OK
            );
            let mut h1_simple = std::ptr::null_mut();
            assert_eq!(
                ((*(*h1_frag.cast::<*const FragmentVtbl>()))
                    .base
                    .query_interface)(h1_frag, &IID_RAW_SIMPLE, &mut h1_simple),
                S_OK
            );
            let h1_svtbl = &*(*h1_simple.cast::<*const SimpleVtbl>());
            assert_eq!(
                (h1_svtbl.get_property_value)(h1_simple, UIA_HeadingLevelPropertyId, &mut prop),
                S_OK
            );
            assert_eq!(variant_i4(prop), HeadingLevel1.0);

            let mut btn_frag = std::ptr::null_mut();
            assert_eq!(
                ((*(*h1_frag.cast::<*const FragmentVtbl>())).navigate)(
                    h1_frag,
                    NavigateDirection_NextSibling,
                    &mut btn_frag
                ),
                S_OK
            );
            let mut btn_rect = UiaRect::default();
            assert_eq!(
                ((*(*btn_frag.cast::<*const FragmentVtbl>())).bounding_rectangle)(
                    btn_frag,
                    &mut btn_rect
                ),
                S_OK
            );
            assert!(btn_rect.width >= 100.0 && btn_rect.height >= 28.0);

            // element from point hits the button at its center
            let mut hit_frag = std::ptr::null_mut();
            assert_eq!(
                (root_vtbl.element_provider_from_point)(
                    root_frag_root,
                    btn_rect.left + btn_rect.width * 0.5,
                    btn_rect.top + btn_rect.height * 0.5,
                    &mut hit_frag
                ),
                S_OK
            );
            assert!(!hit_frag.is_null());
            let mut hit_simple = std::ptr::null_mut();
            assert_eq!(
                ((*(*hit_frag.cast::<*const FragmentVtbl>()))
                    .base
                    .query_interface)(hit_frag, &IID_RAW_SIMPLE, &mut hit_simple),
                S_OK
            );
            assert_eq!(
                ((*(*hit_simple.cast::<*const SimpleVtbl>())).get_property_value)(
                    hit_simple,
                    UIA_NamePropertyId,
                    &mut prop
                ),
                S_OK
            );
            assert_eq!(variant_string(prop), "Run");

            // invoke pattern on button, toggle pattern on checkbox, value pattern on textbox
            let mut invoke_pat = std::ptr::null_mut();
            assert_eq!(
                ((*(*hit_simple.cast::<*const SimpleVtbl>())).get_pattern_provider)(
                    hit_simple,
                    UIA_InvokePatternId,
                    &mut invoke_pat
                ),
                S_OK
            );
            assert!(!invoke_pat.is_null());
            ((*(*invoke_pat.cast::<*const InvokeVtbl>())).base.release)(invoke_pat);

            let mut chk_frag = std::ptr::null_mut();
            assert_eq!(
                ((*(*btn_frag.cast::<*const FragmentVtbl>())).navigate)(
                    btn_frag,
                    NavigateDirection_NextSibling,
                    &mut chk_frag
                ),
                S_OK
            );
            let mut chk_simple = std::ptr::null_mut();
            assert_eq!(
                ((*(*chk_frag.cast::<*const FragmentVtbl>()))
                    .base
                    .query_interface)(chk_frag, &IID_RAW_SIMPLE, &mut chk_simple),
                S_OK
            );
            let mut toggle_pat = std::ptr::null_mut();
            assert_eq!(
                ((*(*chk_simple.cast::<*const SimpleVtbl>())).get_pattern_provider)(
                    chk_simple,
                    UIA_TogglePatternId,
                    &mut toggle_pat
                ),
                S_OK
            );
            assert!(!toggle_pat.is_null());
            let mut tstate = ToggleState_On;
            assert_eq!(
                ((*(*toggle_pat.cast::<*const ToggleVtbl>())).toggle_state)(
                    toggle_pat,
                    &mut tstate
                ),
                S_OK
            );
            assert_eq!(tstate, ToggleState_Off);
            ((*(*toggle_pat.cast::<*const ToggleVtbl>())).base.release)(toggle_pat);

            let mut txt_frag = std::ptr::null_mut();
            assert_eq!(
                ((*(*chk_frag.cast::<*const FragmentVtbl>())).navigate)(
                    chk_frag,
                    NavigateDirection_NextSibling,
                    &mut txt_frag
                ),
                S_OK
            );
            let mut txt_simple = std::ptr::null_mut();
            assert_eq!(
                ((*(*txt_frag.cast::<*const FragmentVtbl>()))
                    .base
                    .query_interface)(txt_frag, &IID_RAW_SIMPLE, &mut txt_simple),
                S_OK
            );
            let mut value_pat = std::ptr::null_mut();
            assert_eq!(
                ((*(*txt_simple.cast::<*const SimpleVtbl>())).get_pattern_provider)(
                    txt_simple,
                    UIA_ValuePatternId,
                    &mut value_pat
                ),
                S_OK
            );
            assert!(!value_pat.is_null());
            let mut bstr_val = std::ptr::null_mut();
            assert_eq!(
                ((*(*value_pat.cast::<*const ValueVtbl>())).value)(value_pat, &mut bstr_val),
                S_OK
            );
            assert_eq!(bstr_to_string(bstr_val), "hello");
            let mut ro = BOOL(1);
            assert_eq!(
                ((*(*value_pat.cast::<*const ValueVtbl>())).is_read_only)(value_pat, &mut ro),
                S_OK
            );
            assert!(!ro.as_bool());
            assert_eq!(
                ((*(*txt_simple.cast::<*const SimpleVtbl>())).get_property_value)(
                    txt_simple,
                    UIA_IsEnabledPropertyId,
                    &mut prop
                ),
                S_OK
            );
            assert!(variant_bool(prop));

            // clean com release of every reference
            ((*(*value_pat.cast::<*const ValueVtbl>())).base.release)(value_pat);
            (vtbl.base.release)(txt_simple);
            ((*(*txt_frag.cast::<*const FragmentVtbl>())).base.release)(txt_frag);
            (vtbl.base.release)(chk_simple);
            ((*(*chk_frag.cast::<*const FragmentVtbl>())).base.release)(chk_frag);
            (vtbl.base.release)(hit_simple);
            ((*(*hit_frag.cast::<*const FragmentVtbl>())).base.release)(hit_frag);
            ((*(*btn_frag.cast::<*const FragmentVtbl>())).base.release)(btn_frag);
            (vtbl.base.release)(h1_simple);
            ((*(*h1_frag.cast::<*const FragmentVtbl>())).base.release)(h1_frag);
            ((*(*main_frag.cast::<*const FragmentVtbl>())).base.release)(main_frag);
            ((*(*nav_frag.cast::<*const FragmentVtbl>())).base.release)(nav_frag);
            ((*(*root_frag.cast::<*const FragmentVtbl>())).base.release)(root_frag);
            (root_vtbl.base.release)(root_frag_root);
            assert_eq!((vtbl.base.release)(root_simple), 0);
        }

        // end-to-end live engine worker test covering activate, invoke, toggle, and setvalue
        let mut worker = EngineWorker::spawn_live(
            Size {
                width: 640.0,
                height: 480.0,
            },
            move || Ok(Browser::new(resources)),
            Arc::new(|| {}),
        )
        .expect("spawn worker");
        worker.navigate("https://app.ceri.invalid/").expect("nav");
        let a11y = worker.accessibility();
        assert!(!a11y.is_active(), "accessibility starts inactive");
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(2);
        let snap = loop {
            if let Some(s) = a11y.activate_and_fetch()
                && s.nodes
                    .iter()
                    .any(|n| n.automation_id.as_deref() == Some("act"))
            {
                break s;
            }
            assert!(
                std::time::Instant::now() < deadline,
                "timed out fetching snapshot"
            );
            std::thread::sleep(std::time::Duration::from_millis(5));
        };
        assert!(a11y.is_active());
        let act_id = snap
            .nodes
            .iter()
            .find(|n| n.automation_id.as_deref() == Some("act"))
            .expect("act")
            .node
            .raw();
        let opt_id = snap
            .nodes
            .iter()
            .find(|n| n.automation_id.as_deref() == Some("opt"))
            .expect("opt")
            .node
            .raw();
        let txt_id = snap
            .nodes
            .iter()
            .find(|n| n.automation_id.as_deref() == Some("txt"))
            .expect("txt")
            .node
            .raw();
        let live_state = Arc::new(WindowUiaState {
            hwnd: 0,
            shared: a11y.clone(),
            root_provider: Mutex::new(None),
            last_focused: AtomicU64::new(0),
        });
        let btn_simple = UiaNodeBox::new_simple(live_state.clone(), act_id, false);
        let chk_simple = UiaNodeBox::new_simple(live_state.clone(), opt_id, false);
        let txt_simple = UiaNodeBox::new_simple(live_state, txt_id, false);
        unsafe {
            let mut pat = std::ptr::null_mut();
            assert_eq!(
                ((*(*btn_simple.cast::<*const SimpleVtbl>())).get_pattern_provider)(
                    btn_simple,
                    UIA_InvokePatternId,
                    &mut pat
                ),
                S_OK
            );
            assert!(!pat.is_null());
            assert_eq!(((*(*pat.cast::<*const InvokeVtbl>())).invoke)(pat), S_OK);
            ((*(*pat.cast::<*const InvokeVtbl>())).base.release)(pat);

            let mut toggle_pat = std::ptr::null_mut();
            assert_eq!(
                ((*(*chk_simple.cast::<*const SimpleVtbl>())).get_pattern_provider)(
                    chk_simple,
                    UIA_TogglePatternId,
                    &mut toggle_pat
                ),
                S_OK
            );
            assert!(!toggle_pat.is_null());
            assert_eq!(
                ((*(*toggle_pat.cast::<*const ToggleVtbl>())).toggle)(toggle_pat),
                S_OK
            );
            ((*(*toggle_pat.cast::<*const ToggleVtbl>())).base.release)(toggle_pat);

            let mut val_pat = std::ptr::null_mut();
            assert_eq!(
                ((*(*txt_simple.cast::<*const SimpleVtbl>())).get_pattern_provider)(
                    txt_simple,
                    UIA_ValuePatternId,
                    &mut val_pat
                ),
                S_OK
            );
            assert!(!val_pat.is_null());
            let next_val: Vec<u16> = "world".encode_utf16().chain([0]).collect();
            assert_eq!(
                ((*(*val_pat.cast::<*const ValueVtbl>())).set_value)(
                    val_pat,
                    PCWSTR(next_val.as_ptr())
                ),
                S_OK
            );
            ((*(*val_pat.cast::<*const ValueVtbl>())).base.release)(val_pat);

            release_simple(txt_simple);
            release_simple(chk_simple);
            release_simple(btn_simple);
        }
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(2);
        loop {
            if let Some(updated) = a11y.current()
                && updated
                    .nodes
                    .iter()
                    .any(|n| n.automation_id.as_deref() == Some("log") && n.name == "ran")
                && updated.nodes.iter().any(|n| {
                    n.automation_id.as_deref() == Some("opt") && n.toggle == Some(Toggle::On)
                })
                && updated.nodes.iter().any(|n| {
                    n.automation_id.as_deref() == Some("txt") && n.value.as_deref() == Some("world")
                })
            {
                break;
            }
            assert!(
                std::time::Instant::now() < deadline,
                "uia actions did not update live dom"
            );
            std::thread::sleep(std::time::Duration::from_millis(5));
        }
        worker.shutdown();
    }
}

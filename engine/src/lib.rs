#[cfg(feature = "mimalloc")]
struct Allocator;

#[cfg(feature = "mimalloc")]
#[global_allocator]
static ALLOCATOR: Allocator = Allocator;

#[cfg(feature = "mimalloc")]
mod allocator {
    #[cfg(feature = "windowed")]
    use std::cell::Cell;
    use std::sync::atomic::{AtomicU8, Ordering};

    // mimalloc v3 option indices from the header libmimalloc-sys builds
    const ARENA_EAGER_COMMIT: libmimalloc_sys::mi_option_t = 4;
    const PURGE_DELAY: libmimalloc_sys::mi_option_t = 15;
    const ARENA_RESERVE: libmimalloc_sys::mi_option_t = 23;
    const PAGE_RECLAIM_ON_FREE: libmimalloc_sys::mi_option_t = 35;
    const PAGE_FULL_RETAIN: libmimalloc_sys::mi_option_t = 36;
    const PAGE_COMMIT_ON_DEMAND: libmimalloc_sys::mi_option_t = 40;
    const ACTIVE_PURGE_DELAY_MS: std::ffi::c_long = 25;
    // 64 MiB arenas keep unreclaimable slice metadata at 128 KiB instead of 2 MiB per 1 GiB arena
    const ARENA_RESERVE_KIB: std::ffi::c_long = 65_536;

    // 0 unconfigured, 1 configuring, 2 ready
    static STATE: AtomicU8 = AtomicU8::new(0);

    #[cfg(feature = "windowed")]
    thread_local! {
        // only engine worker threads allocate from mimalloc in windowed apps so host and tray
        // threads stay on the 4 kb system heap and never pin 64 kb mimalloc slices when idle
        static ENGINE_THREAD: Cell<bool> = const { Cell::new(false) };
    }

    #[cfg(feature = "windowed")]
    #[inline]
    pub(super) fn engine_thread() -> bool {
        ENGINE_THREAD.get()
    }

    #[cfg(feature = "windowed")]
    pub(super) fn set_engine_thread(enabled: bool) -> bool {
        ENGINE_THREAD.replace(enabled)
    }

    #[cfg(feature = "windowed")]
    #[inline]
    pub(super) fn owns(ptr: *mut u8) -> bool {
        if STATE.load(Ordering::Acquire) != 2 {
            return false;
        }
        // safety: ready state guarantees mi_process_init has published mimalloc's page map
        unsafe { libmimalloc_sys::mi_is_in_heap_region(ptr.cast()) }
    }

    #[inline]
    pub(super) fn is_ready() -> bool {
        STATE.load(Ordering::Acquire) == 2
    }

    #[inline]
    pub(super) fn configure() {
        if STATE.load(Ordering::Acquire) == 2 {
            return;
        }
        configure_slow();
    }

    #[cold]
    fn configure_slow() {
        if STATE
            .compare_exchange(0, 1, Ordering::AcqRel, Ordering::Acquire)
            .is_ok()
        {
            // safety: options apply before mi_process_init allocates mimalloc's page map
            unsafe {
                libmimalloc_sys::mi_option_set(ARENA_EAGER_COMMIT, 0);
                libmimalloc_sys::mi_option_set(ARENA_RESERVE, ARENA_RESERVE_KIB);
                libmimalloc_sys::mi_option_set(PURGE_DELAY, ACTIVE_PURGE_DELAY_MS);
                libmimalloc_sys::mi_option_set(PAGE_RECLAIM_ON_FREE, 0);
                libmimalloc_sys::mi_option_set(PAGE_FULL_RETAIN, 0);
                libmimalloc_sys::mi_option_set(PAGE_COMMIT_ON_DEMAND, 1);
                libmimalloc_sys::mi_process_init();
            }
            STATE.store(2, Ordering::Release);
        } else {
            while STATE.load(Ordering::Acquire) != 2 {
                std::hint::spin_loop();
            }
        }
    }

    #[cfg(feature = "windowed")]
    pub(super) fn set_active() {
        configure();
        // safety: option descriptors update without allocating
        unsafe {
            libmimalloc_sys::mi_option_set(PURGE_DELAY, ACTIVE_PURGE_DELAY_MS);
        }
    }

    #[cfg(feature = "windowed")]
    pub(super) fn purge_immediately() {
        if !is_ready() {
            return;
        }
        // safety: positive delay lets mi_arenas_try_purge flush already-scheduled slices across
        // every arena; switching to zero afterward decommits late cross-thread frees immediately
        unsafe {
            libmimalloc_sys::mi_option_set(PURGE_DELAY, ACTIVE_PURGE_DELAY_MS);
            libmimalloc_sys::mi_thread_init();
            libmimalloc_sys::mi_collect(true);
            libmimalloc_sys::mi_option_set(PURGE_DELAY, 0);
        }
    }
}

#[cfg(all(feature = "mimalloc", not(feature = "windowed")))]
// safety: forwards every operation to mimalloc after applying process-wide sizing options once
unsafe impl std::alloc::GlobalAlloc for Allocator {
    unsafe fn alloc(&self, layout: std::alloc::Layout) -> *mut u8 {
        allocator::configure();
        // safety: caller upholds GlobalAlloc layout preconditions
        unsafe { mimalloc::MiMalloc.alloc(layout) }
    }

    unsafe fn alloc_zeroed(&self, layout: std::alloc::Layout) -> *mut u8 {
        allocator::configure();
        // safety: caller upholds GlobalAlloc layout preconditions
        unsafe { mimalloc::MiMalloc.alloc_zeroed(layout) }
    }

    unsafe fn dealloc(&self, ptr: *mut u8, layout: std::alloc::Layout) {
        // safety: caller upholds GlobalAlloc dealloc preconditions
        unsafe { mimalloc::MiMalloc.dealloc(ptr, layout) }
    }

    unsafe fn realloc(&self, ptr: *mut u8, layout: std::alloc::Layout, new_size: usize) -> *mut u8 {
        // safety: caller upholds GlobalAlloc realloc preconditions
        unsafe { mimalloc::MiMalloc.realloc(ptr, layout, new_size) }
    }
}

#[cfg(all(feature = "mimalloc", feature = "windowed"))]
// safety: engine threads allocate from mimalloc and other threads from system; dealloc and realloc
// dispatch by mimalloc's page map so pointers may freely cross threads
unsafe impl std::alloc::GlobalAlloc for Allocator {
    unsafe fn alloc(&self, layout: std::alloc::Layout) -> *mut u8 {
        if allocator::engine_thread() {
            allocator::configure();
            // safety: caller upholds global alloc layout preconditions
            unsafe { mimalloc::MiMalloc.alloc(layout) }
        } else {
            // safety: caller upholds global alloc layout preconditions
            unsafe { std::alloc::System.alloc(layout) }
        }
    }

    unsafe fn alloc_zeroed(&self, layout: std::alloc::Layout) -> *mut u8 {
        if allocator::engine_thread() {
            allocator::configure();
            // safety: caller upholds global alloc layout preconditions
            unsafe { mimalloc::MiMalloc.alloc_zeroed(layout) }
        } else {
            // safety: caller upholds global alloc layout preconditions
            unsafe { std::alloc::System.alloc_zeroed(layout) }
        }
    }

    unsafe fn dealloc(&self, ptr: *mut u8, layout: std::alloc::Layout) {
        if allocator::owns(ptr) {
            // safety: ptr belongs to mimalloc's heap region and caller upholds dealloc preconditions
            unsafe { mimalloc::MiMalloc.dealloc(ptr, layout) }
        } else {
            // safety: ptr was allocated by system and caller upholds dealloc preconditions
            unsafe { std::alloc::System.dealloc(ptr, layout) }
        }
    }

    unsafe fn realloc(&self, ptr: *mut u8, layout: std::alloc::Layout, new_size: usize) -> *mut u8 {
        let in_mi = allocator::owns(ptr);
        let want_mi = allocator::engine_thread();
        if in_mi == want_mi {
            if in_mi {
                // safety: ptr belongs to mimalloc and caller upholds realloc preconditions
                return unsafe { mimalloc::MiMalloc.realloc(ptr, layout, new_size) };
            }
            // safety: ptr belongs to system and caller upholds realloc preconditions
            return unsafe { std::alloc::System.realloc(ptr, layout, new_size) };
        }
        let Ok(new_layout) = std::alloc::Layout::from_size_align(new_size, layout.align()) else {
            return std::ptr::null_mut();
        };
        // safety: caller guarantees new_size > 0 and layout matches ptr
        let next = unsafe { self.alloc(new_layout) };
        if !next.is_null() {
            // safety: both buffers are valid for at least min(layout.size(), new_size) bytes
            unsafe {
                std::ptr::copy_nonoverlapping(ptr, next, layout.size().min(new_size));
                if in_mi {
                    mimalloc::MiMalloc.dealloc(ptr, layout);
                } else {
                    std::alloc::System.dealloc(ptr, layout);
                }
            }
        }
        next
    }
}

// opts the current engine worker thread into mimalloc in windowed builds
#[doc(hidden)]
pub struct EngineAllocator {
    #[cfg(all(feature = "mimalloc", feature = "windowed"))]
    previous: bool,
}

#[doc(hidden)]
pub fn enter_engine_allocator() -> EngineAllocator {
    EngineAllocator {
        #[cfg(all(feature = "mimalloc", feature = "windowed"))]
        previous: allocator::set_engine_thread(true),
    }
}

impl Drop for EngineAllocator {
    fn drop(&mut self) {
        #[cfg(all(feature = "mimalloc", feature = "windowed"))]
        allocator::set_engine_thread(self.previous);
    }
}

// hands freed memory back to the system a few milliseconds after it is freed, so a page that
// peaks while it loads settles back down as it would on the system allocator without paying
// for a release on every free
pub(crate) fn settle_allocator() {
    #[cfg(all(feature = "mimalloc", not(feature = "windowed")))]
    allocator::configure();
}

// hands back what is still held for later once the page rests, as a thread that stops
// allocating never reaches its delayed release
pub(crate) fn release_memory() {
    #[cfg(feature = "mimalloc")]
    if allocator::is_ready() {
        // safety: a collection is a plain call on the calling thread's own heap
        unsafe {
            libmimalloc_sys::mi_collect(true);
        }
    }
}

// flushes abandoned worker arenas from a join thread whose own mimalloc theap was never initialized
#[cfg(feature = "mimalloc")]
pub(crate) fn release_after_worker_exit() {
    if allocator::is_ready() {
        // safety: mi_thread_init enables mi_collect on this short-lived join thread so it flushes
        // every arena before exiting
        unsafe {
            libmimalloc_sys::mi_thread_init();
            libmimalloc_sys::mi_collect(true);
        }
    }
}

#[cfg(not(feature = "mimalloc"))]
pub(crate) fn release_after_worker_exit() {}

// restores batched allocator purging once a window opens after tray idle
#[cfg(feature = "windowed")]
pub(crate) fn resume_active_memory() {
    #[cfg(feature = "mimalloc")]
    allocator::set_active();
}

// flushes deferred allocator purges and trims the process working set once no window is open
#[cfg(feature = "windowed")]
pub(crate) fn trim_idle_memory() {
    #[cfg(feature = "mimalloc")]
    allocator::purge_immediately();
    #[cfg(target_os = "windows")]
    {
        use windows::Win32::System::Memory::{
            SETPROCESSWORKINGSETSIZEEX_FLAGS, SetProcessWorkingSetSizeEx,
        };
        use windows::Win32::System::Threading::GetCurrentProcess;
        // safety: passing usize::MAX for both bounds asks windows to trim the current process working set
        unsafe {
            let _ = SetProcessWorkingSetSizeEx(
                GetCurrentProcess(),
                usize::MAX,
                usize::MAX,
                SETPROCESSWORKINGSETSIZEEX_FLAGS(0),
            );
        }
    }
}

mod animated;
mod audio;
mod autohint;
mod bidi;
mod border;
mod bridge;
mod cff;
mod clip_path;
mod color_glyph;
mod colors;
mod csp;
mod css;
mod deflate;
mod devtools;
mod dom;
#[cfg(windows)]
mod dwrite;
mod filter;
mod font;
mod gif;
mod grid;
pub mod hashing;
mod host;
mod image;
mod inflate;
mod intern;
mod interpolate;
mod jpeg;
mod layout;
mod linebreak;
mod media;
#[cfg(feature = "native-http")]
mod native_http;
mod navigation;
mod netlog;
mod paint;
mod png;
#[cfg(all(feature = "windowed", target_os = "windows"))]
mod presenter;
pub mod profile;
mod range;
mod script;
mod segment;
mod selection;
mod shaping;
mod sound;
mod svg;
mod text;
mod text_tables;
mod transform;
mod unicode;
mod webp;
#[rustfmt::skip]
mod unicode_tables;
mod variation;
mod video;
mod wasm;
mod weburl;
#[cfg(feature = "windowed")]
mod window;
mod woff;

pub use audio::{AudioDevice, AudioOutput, AudioRender};
pub use bridge::{
    Capabilities, CommandCall, CommandError, CommandReply, CommandResult, CommandRouter, Grant,
};
pub use css::{Color, ColorScheme, Cursor, Display, Stylesheet};
pub use deflate::{gzip, zlib_compress};
pub use devtools::{devtools_serving, serve_devtools};
pub use dom::{
    AccessibilityNode, Document, DomError, Element, Namespace, NodeId, NodeKind, ParseError,
    ScriptSource, StylesheetSource,
};
pub use font::{
    FontFace, FontSet, FontSource, FontStack, FontStyle, GlyphBitmap, intern_font_stack,
};
pub use host::{
    ENGINE_STACK, EngineHost, EngineWorker, EngineWorkerEvent, Frame, InputEvent, InputResult,
    KeyModifiers, PointerButton, RegionPress,
};
pub use image::{decode_rgba, icon_png, rasterize_icon};

// eight bit RGBA pixels, row after row, as a PNG file
pub fn png_from_rgba(width: u32, height: u32, rgba: &[u8]) -> Vec<u8> {
    png::encode_png(width, height, 4, rgba)
}
pub use inflate::{InflateError, zlib_decompress};
pub use layout::{LayoutDocument, Rect, Size, ThumbDrag, layout};
#[cfg(feature = "native-http")]
pub use native_http::{NativeHttpProvider, request_external};
pub use navigation::{
    APP_ORIGIN, AppUrl, Browser, ClipboardProvider, ExternalEvent, ExternalReport, ExternalRequest,
    FILES_ORIGIN, FilePicker, FilePickerRequest, FileSelection, LinkOpener, NavigationError,
    NavigationFailure, NavigationStatus, Resource, ResourceBundle, ResourceCacheMode,
    ResourceProvider, ResourceRequest, ResourceResponse, ScriptDialogs, SocketConnector,
    SocketStream, UserPreferences, external_url, mime_type_for,
};
pub use paint::Canvas;
pub use script::{
    HostBridge, HostRuntimeInfo, ScriptError, ScriptRuntime, TimeZone, seed_local_storage,
};
pub use sound::{AudioChunk, AudioInfo, AudioReader};
pub use video::{
    MediaBytes, MediaDecoder, MediaInfo, MediaSource, VideoFrame, VideoReader, VideoSurface,
};
#[cfg(feature = "windowed")]
pub use window::{
    Lifecycle, Monitor, NativeHandle, WindowAction, WindowConfig, WindowContext, WindowControl,
    WindowState, run as run_window, run_with as run_window_with,
};
#[cfg(feature = "windowed")]
pub use winit::window::ResizeDirection;

#[cfg(all(test, feature = "mimalloc", feature = "windowed"))]
mod allocator_tests {
    use super::{allocator, enter_engine_allocator};

    #[test]
    fn cross_allocator_alloc_realloc_and_drop_preserve_contents() {
        let mut system_vec = Vec::with_capacity(64);
        system_vec.extend((0..64u8).map(|v| v.wrapping_mul(3)));
        assert!(!allocator::owns(system_vec.as_mut_ptr()));

        let mut engine_vec = {
            let _guard = enter_engine_allocator();
            let mut vec = vec![0u8; 128];
            for (index, slot) in vec.iter_mut().enumerate() {
                *slot = (index as u8).wrapping_add(11);
            }
            assert!(allocator::owns(vec.as_mut_ptr()));
            // grow a system-owned allocation while on an engine thread
            system_vec.extend((0..256usize).map(|v| v as u8));
            assert!(allocator::owns(system_vec.as_mut_ptr()));
            vec
        };

        assert!(allocator::owns(engine_vec.as_mut_ptr()));
        // grow a mimalloc-owned allocation back on a non-engine thread
        engine_vec.extend((0..512usize).map(|v| v as u8));
        assert!(!allocator::owns(engine_vec.as_mut_ptr()));
        assert_eq!(engine_vec.len(), 640);
        assert_eq!(engine_vec[0], 11);
        assert_eq!(engine_vec[127], 138);
        assert_eq!(engine_vec[128], 0);
        assert_eq!(engine_vec[639], 255);
        assert_eq!(system_vec.len(), 320);
        assert_eq!(system_vec[0], 0);
        assert_eq!(system_vec[63], 189);
        assert_eq!(system_vec[64], 0);
    }
}

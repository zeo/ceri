#[cfg(feature = "mimalloc")]
#[global_allocator]
static ALLOCATOR: mimalloc::MiMalloc = mimalloc::MiMalloc;

// hands freed memory back to the system a few milliseconds after it is freed, so a page that
// peaks while it loads settles back down as it would on the system allocator without paying
// for a release on every free
pub(crate) fn settle_allocator() {
    #[cfg(feature = "mimalloc")]
    {
        // mi_option_purge_delay in the mimalloc v3 header the crate builds
        const PURGE_DELAY: libmimalloc_sys::mi_option_t = 15;
        static SETTLED: std::sync::Once = std::sync::Once::new();
        // SAFETY: setting an option is a plain call that mimalloc allows at any time
        SETTLED.call_once(|| unsafe { libmimalloc_sys::mi_option_set(PURGE_DELAY, 25) });
    }
}

// hands back what is still held for later once the page rests, as a thread that stops
// allocating never reaches its delayed release
pub(crate) fn release_memory() {
    #[cfg(feature = "mimalloc")]
    // SAFETY: a collection is a plain call on the calling thread's own heap
    unsafe {
        libmimalloc_sys::mi_collect(true);
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

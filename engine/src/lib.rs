mod colors;
mod css;
mod dom;
mod font;
mod gif;
mod host;
mod image;
mod inflate;
mod interpolate;
mod jpeg;
mod layout;
#[cfg(feature = "native-http")]
mod native_http;
mod navigation;
mod paint;
mod png;
mod script;
mod selection;
mod text;
mod transform;
mod unicode;
#[rustfmt::skip]
mod unicode_tables;
mod wasm;
#[cfg(feature = "windowed")]
mod window;

pub use css::{Color, ColorScheme, Display, Stylesheet};
pub use dom::{
    AccessibilityNode, Document, DomError, Element, Namespace, NodeId, NodeKind, ParseError,
    ScriptSource, StylesheetSource,
};
pub use font::{FontFace, FontSet, FontSource, FontStack, GlyphBitmap, intern_font_stack};
pub use host::{
    EngineHost, EngineWorker, EngineWorkerEvent, Frame, InputEvent, InputResult, KeyModifiers,
    PointerButton,
};
pub use image::rasterize_icon;
pub use layout::{LayoutDocument, Rect, Size, ThumbDrag, layout};
#[cfg(feature = "native-http")]
pub use native_http::{NativeHttpProvider, request_external};
pub use navigation::{
    APP_ORIGIN, AppUrl, Browser, ClipboardProvider, ExternalRequest, FilePicker, FilePickerRequest,
    FileSelection, NavigationError, NavigationFailure, NavigationStatus, Resource, ResourceBundle,
    ResourceCacheMode, ResourceProvider, ResourceRequest, ResourceResponse, UserPreferences,
    external_url, mime_type_for,
};
pub use paint::Canvas;
pub use script::{HostRuntimeInfo, ScriptError, ScriptRuntime, TimeZone};
#[cfg(feature = "windowed")]
pub use window::{WindowConfig, run as run_window};

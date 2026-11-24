// WebGL 1 and 2 drawn in software: the interfaces script sees, the canvas contexts behind
// them and the extensions; the state machine is in context.rs, the pipeline in raster.rs and
// the shader compiler in glsl

mod calls;
mod constants;
mod context;
pub(crate) mod glsl;
mod raster;
mod texture;

use super::object::{JsObject, ObjectClass, ObjectHandle, Property, Slot, new_object};
use super::realm::{Builder, Native};
use super::*;
use constants::*;
use context::Context;
use std::rc::Weak;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(super) enum GlMember {
    // an index into METHODS
    Method(u16),
    Canvas,
    DrawingBufferWidth,
    DrawingBufferHeight,
    ColorSpace(bool),
    SetColorSpace(bool),
    InfoName,
    InfoSize,
    InfoType,
    RangeMin,
    RangeMax,
    Precision,
    // an index into EXTENSION_METHODS
    Extension(u8),
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum Kind {
    Buffer,
    Texture,
    Renderbuffer,
    Framebuffer,
    Shader,
    Program,
    VertexArray,
    Query,
    Sampler,
    Sync,
    TransformFeedback,
}

impl Kind {
    fn class(self) -> &'static str {
        match self {
            Kind::Buffer => "WebGLBuffer",
            Kind::Texture => "WebGLTexture",
            Kind::Renderbuffer => "WebGLRenderbuffer",
            Kind::Framebuffer => "WebGLFramebuffer",
            Kind::Shader => "WebGLShader",
            Kind::Program => "WebGLProgram",
            Kind::VertexArray => "WebGLVertexArrayObject",
            Kind::Query => "WebGLQuery",
            Kind::Sampler => "WebGLSampler",
            Kind::Sync => "WebGLSync",
            Kind::TransformFeedback => "WebGLTransformFeedback",
        }
    }
}

pub(crate) struct Options {
    alpha: bool,
    depth: bool,
    stencil: bool,
    premultiplied: bool,
    preserve: bool,
    power: String,
    fail_if_caveat: bool,
    desynchronized: bool,
    xr: bool,
}

pub(crate) struct GlContext {
    pub(crate) gl: Context,
    canvas: Value,
    node: NodeId,
    options: Options,
    // the script object of each live WebGL object, so a binding query returns the same one
    wrappers: HashMap<u32, ObjectHandle>,
    extensions: HashMap<&'static str, ObjectHandle>,
    // what paint showed last
    snapshot: Option<std::sync::Arc<crate::image::Image>>,
    color_space: [crate::script::JsStr; 2],
}

pub(crate) enum GlObject {
    Context(Rc<RefCell<GlContext>>),
    Object {
        context: Weak<RefCell<GlContext>>,
        id: u32,
        kind: Kind,
    },
    Location {
        context: Weak<RefCell<GlContext>>,
        program: u32,
        generation: u32,
        index: u32,
    },
    Info {
        name: String,
        size: i32,
        kind: u32,
    },
    Precision([i32; 3]),
    Extension(Weak<RefCell<GlContext>>),
}

pub(super) type WebGlHandle = (ObjectHandle, Rc<RefCell<GlContext>>);

pub(super) struct WebGlInterfaces {
    prototypes: HashMap<&'static str, ObjectHandle>,
}

// the methods of WebGLRenderingContext, then those WebGL2RenderingContext adds: name and
// the arguments WebIDL requires
pub(super) const METHODS: &[(&str, u8)] = &[
    ("activeTexture", 1),
    ("attachShader", 2),
    ("bindAttribLocation", 3),
    ("bindBuffer", 2),
    ("bindFramebuffer", 2),
    ("bindRenderbuffer", 2),
    ("bindTexture", 2),
    ("blendColor", 4),
    ("blendEquation", 1),
    ("blendEquationSeparate", 2),
    ("blendFunc", 2),
    ("blendFuncSeparate", 4),
    ("bufferData", 3),
    ("bufferSubData", 3),
    ("checkFramebufferStatus", 1),
    ("clear", 1),
    ("clearColor", 4),
    ("clearDepth", 1),
    ("clearStencil", 1),
    ("colorMask", 4),
    ("compileShader", 1),
    ("compressedTexImage2D", 7),
    ("compressedTexSubImage2D", 8),
    ("copyTexImage2D", 8),
    ("copyTexSubImage2D", 8),
    ("createBuffer", 0),
    ("createFramebuffer", 0),
    ("createProgram", 0),
    ("createRenderbuffer", 0),
    ("createShader", 1),
    ("createTexture", 0),
    ("cullFace", 1),
    ("deleteBuffer", 1),
    ("deleteFramebuffer", 1),
    ("deleteProgram", 1),
    ("deleteRenderbuffer", 1),
    ("deleteShader", 1),
    ("deleteTexture", 1),
    ("depthFunc", 1),
    ("depthMask", 1),
    ("depthRange", 2),
    ("detachShader", 2),
    ("disable", 1),
    ("disableVertexAttribArray", 1),
    ("drawArrays", 3),
    ("drawElements", 4),
    ("enable", 1),
    ("enableVertexAttribArray", 1),
    ("finish", 0),
    ("flush", 0),
    ("framebufferRenderbuffer", 4),
    ("framebufferTexture2D", 5),
    ("frontFace", 1),
    ("generateMipmap", 1),
    ("getActiveAttrib", 2),
    ("getActiveUniform", 2),
    ("getAttachedShaders", 1),
    ("getAttribLocation", 2),
    ("getBufferParameter", 2),
    ("getContextAttributes", 0),
    ("getError", 0),
    ("getExtension", 1),
    ("getFramebufferAttachmentParameter", 3),
    ("getParameter", 1),
    ("getProgramInfoLog", 1),
    ("getProgramParameter", 2),
    ("getRenderbufferParameter", 2),
    ("getShaderInfoLog", 1),
    ("getShaderParameter", 2),
    ("getShaderPrecisionFormat", 2),
    ("getShaderSource", 1),
    ("getSupportedExtensions", 0),
    ("getTexParameter", 2),
    ("getUniform", 2),
    ("getUniformLocation", 2),
    ("getVertexAttrib", 2),
    ("getVertexAttribOffset", 2),
    ("hint", 2),
    ("isBuffer", 1),
    ("isContextLost", 0),
    ("isEnabled", 1),
    ("isFramebuffer", 1),
    ("isProgram", 1),
    ("isRenderbuffer", 1),
    ("isShader", 1),
    ("isTexture", 1),
    ("lineWidth", 1),
    ("linkProgram", 1),
    ("makeXRCompatible", 0),
    ("pixelStorei", 2),
    ("polygonOffset", 2),
    ("readPixels", 7),
    ("renderbufferStorage", 4),
    ("sampleCoverage", 2),
    ("scissor", 4),
    ("shaderSource", 2),
    ("stencilFunc", 3),
    ("stencilFuncSeparate", 4),
    ("stencilMask", 1),
    ("stencilMaskSeparate", 2),
    ("stencilOp", 3),
    ("stencilOpSeparate", 4),
    ("texImage2D", 6),
    ("texParameterf", 3),
    ("texParameteri", 3),
    ("texSubImage2D", 7),
    ("uniform1f", 2),
    ("uniform1fv", 2),
    ("uniform1i", 2),
    ("uniform1iv", 2),
    ("uniform2f", 3),
    ("uniform2fv", 2),
    ("uniform2i", 3),
    ("uniform2iv", 2),
    ("uniform3f", 4),
    ("uniform3fv", 2),
    ("uniform3i", 4),
    ("uniform3iv", 2),
    ("uniform4f", 5),
    ("uniform4fv", 2),
    ("uniform4i", 5),
    ("uniform4iv", 2),
    ("uniformMatrix2fv", 3),
    ("uniformMatrix3fv", 3),
    ("uniformMatrix4fv", 3),
    ("useProgram", 1),
    ("validateProgram", 1),
    ("vertexAttrib1f", 2),
    ("vertexAttrib1fv", 2),
    ("vertexAttrib2f", 3),
    ("vertexAttrib2fv", 2),
    ("vertexAttrib3f", 4),
    ("vertexAttrib3fv", 2),
    ("vertexAttrib4f", 5),
    ("vertexAttrib4fv", 2),
    ("vertexAttribPointer", 6),
    ("viewport", 4),
    // WebGL 2
    ("beginQuery", 2),
    ("beginTransformFeedback", 1),
    ("bindBufferBase", 3),
    ("bindBufferRange", 5),
    ("bindSampler", 2),
    ("bindTransformFeedback", 2),
    ("bindVertexArray", 1),
    ("blitFramebuffer", 10),
    ("clearBufferfi", 4),
    ("clearBufferfv", 3),
    ("clearBufferiv", 3),
    ("clearBufferuiv", 3),
    ("clientWaitSync", 3),
    ("compressedTexImage3D", 8),
    ("compressedTexSubImage3D", 10),
    ("copyBufferSubData", 5),
    ("copyTexSubImage3D", 9),
    ("createQuery", 0),
    ("createSampler", 0),
    ("createTransformFeedback", 0),
    ("createVertexArray", 0),
    ("deleteQuery", 1),
    ("deleteSampler", 1),
    ("deleteSync", 1),
    ("deleteTransformFeedback", 1),
    ("deleteVertexArray", 1),
    ("drawArraysInstanced", 4),
    ("drawBuffers", 1),
    ("drawElementsInstanced", 5),
    ("drawRangeElements", 6),
    ("endQuery", 1),
    ("endTransformFeedback", 0),
    ("fenceSync", 2),
    ("framebufferTextureLayer", 5),
    ("getActiveUniformBlockName", 2),
    ("getActiveUniformBlockParameter", 3),
    ("getActiveUniforms", 3),
    ("getBufferSubData", 3),
    ("getFragDataLocation", 2),
    ("getIndexedParameter", 2),
    ("getInternalformatParameter", 3),
    ("getQuery", 2),
    ("getQueryParameter", 2),
    ("getSamplerParameter", 2),
    ("getSyncParameter", 2),
    ("getTransformFeedbackVarying", 2),
    ("getUniformBlockIndex", 2),
    ("getUniformIndices", 2),
    ("invalidateFramebuffer", 2),
    ("invalidateSubFramebuffer", 6),
    ("isQuery", 1),
    ("isSampler", 1),
    ("isSync", 1),
    ("isTransformFeedback", 1),
    ("isVertexArray", 1),
    ("pauseTransformFeedback", 0),
    ("readBuffer", 1),
    ("renderbufferStorageMultisample", 5),
    ("resumeTransformFeedback", 0),
    ("samplerParameterf", 3),
    ("samplerParameteri", 3),
    ("texImage3D", 10),
    ("texStorage2D", 5),
    ("texStorage3D", 6),
    ("texSubImage3D", 11),
    ("transformFeedbackVaryings", 3),
    ("uniform1ui", 2),
    ("uniform1uiv", 2),
    ("uniform2ui", 3),
    ("uniform2uiv", 2),
    ("uniform3ui", 4),
    ("uniform3uiv", 2),
    ("uniform4ui", 5),
    ("uniform4uiv", 2),
    ("uniformBlockBinding", 3),
    ("uniformMatrix2x3fv", 3),
    ("uniformMatrix2x4fv", 3),
    ("uniformMatrix3x2fv", 3),
    ("uniformMatrix3x4fv", 3),
    ("uniformMatrix4x2fv", 3),
    ("uniformMatrix4x3fv", 3),
    ("vertexAttribDivisor", 2),
    ("vertexAttribI4i", 5),
    ("vertexAttribI4iv", 2),
    ("vertexAttribI4ui", 5),
    ("vertexAttribI4uiv", 2),
    ("vertexAttribIPointer", 5),
    ("waitSync", 3),
];

// where the WebGL 2 methods start in METHODS
fn first_webgl2_method() -> usize {
    METHODS
        .iter()
        .position(|(name, _)| *name == "beginQuery")
        .expect("the WebGL 2 methods are listed")
}

// the extensions each version offers, with the constants they define
type Extension = (&'static str, bool, bool, &'static [(&'static str, u32)]);

pub(super) const EXTENSIONS: &[Extension] = &[
    // name, in WebGL 1, in WebGL 2, constants
    (
        "ANGLE_instanced_arrays",
        true,
        false,
        &[(
            "VERTEX_ATTRIB_ARRAY_DIVISOR_ANGLE",
            VERTEX_ATTRIB_ARRAY_DIVISOR,
        )],
    ),
    (
        "EXT_blend_minmax",
        true,
        false,
        &[("MIN_EXT", MIN), ("MAX_EXT", MAX)],
    ),
    ("EXT_color_buffer_float", false, true, &[]),
    (
        "EXT_color_buffer_half_float",
        true,
        true,
        &[
            ("RGBA16F_EXT", RGBA16F),
            ("RGB16F_EXT", RGB16F),
            (
                "FRAMEBUFFER_ATTACHMENT_COMPONENT_TYPE_EXT",
                FRAMEBUFFER_ATTACHMENT_COMPONENT_TYPE,
            ),
            ("UNSIGNED_NORMALIZED_EXT", UNSIGNED_NORMALIZED),
        ],
    ),
    ("EXT_float_blend", true, true, &[]),
    ("EXT_frag_depth", true, false, &[]),
    ("EXT_shader_texture_lod", true, false, &[]),
    (
        "EXT_sRGB",
        true,
        false,
        &[
            ("SRGB_EXT", SRGB),
            ("SRGB_ALPHA_EXT", SRGB_ALPHA_EXT),
            ("SRGB8_ALPHA8_EXT", SRGB8_ALPHA8),
            (
                "FRAMEBUFFER_ATTACHMENT_COLOR_ENCODING_EXT",
                FRAMEBUFFER_ATTACHMENT_COLOR_ENCODING,
            ),
        ],
    ),
    (
        "EXT_texture_filter_anisotropic",
        true,
        true,
        &[
            ("TEXTURE_MAX_ANISOTROPY_EXT", TEXTURE_MAX_ANISOTROPY_EXT),
            (
                "MAX_TEXTURE_MAX_ANISOTROPY_EXT",
                MAX_TEXTURE_MAX_ANISOTROPY_EXT,
            ),
        ],
    ),
    (
        "KHR_parallel_shader_compile",
        true,
        true,
        &[("COMPLETION_STATUS_KHR", COMPLETION_STATUS_KHR)],
    ),
    ("OES_element_index_uint", true, false, &[]),
    ("OES_fbo_render_mipmap", true, false, &[]),
    (
        "OES_standard_derivatives",
        true,
        false,
        &[(
            "FRAGMENT_SHADER_DERIVATIVE_HINT_OES",
            FRAGMENT_SHADER_DERIVATIVE_HINT,
        )],
    ),
    ("OES_texture_float", true, false, &[]),
    ("OES_texture_float_linear", true, true, &[]),
    (
        "OES_texture_half_float",
        true,
        false,
        &[("HALF_FLOAT_OES", HALF_FLOAT_OES)],
    ),
    ("OES_texture_half_float_linear", true, false, &[]),
    (
        "OES_vertex_array_object",
        true,
        false,
        &[("VERTEX_ARRAY_BINDING_OES", VERTEX_ARRAY_BINDING)],
    ),
    (
        "WEBGL_color_buffer_float",
        true,
        false,
        &[
            ("RGBA32F_EXT", RGBA32F),
            (
                "FRAMEBUFFER_ATTACHMENT_COMPONENT_TYPE_EXT",
                FRAMEBUFFER_ATTACHMENT_COMPONENT_TYPE,
            ),
            ("UNSIGNED_NORMALIZED_EXT", UNSIGNED_NORMALIZED),
        ],
    ),
    (
        "WEBGL_debug_renderer_info",
        true,
        true,
        &[
            ("UNMASKED_VENDOR_WEBGL", UNMASKED_VENDOR_WEBGL),
            ("UNMASKED_RENDERER_WEBGL", UNMASKED_RENDERER_WEBGL),
        ],
    ),
    (
        "WEBGL_depth_texture",
        true,
        false,
        &[("UNSIGNED_INT_24_8_WEBGL", UNSIGNED_INT_24_8)],
    ),
    ("WEBGL_draw_buffers", true, false, &[]),
    ("WEBGL_lose_context", true, true, &[]),
];

// the interface name an extension object reports, which drops the underscores
fn extension_interface(name: &str) -> String {
    let (vendor, rest) = name.split_once('_').unwrap_or((name, ""));
    let vendor = if vendor == "WEBGL" { "WebGL" } else { vendor };
    let words: String = rest
        .split('_')
        .map(|word| match word {
            "sRGB" | "minmax" => word.replace("minmax", "MinMax"),
            "lod" => "LOD".to_string(),
            "uint" => "Uint".to_string(),
            _ => {
                let mut characters = word.chars();
                characters.next().map_or(String::new(), |first| {
                    first.to_ascii_uppercase().to_string()
                        + &characters.as_str().to_ascii_lowercase()
                })
            }
        })
        .collect();
    format!("{vendor}{words}")
}

pub(super) const EXTENSION_METHODS: &[(&str, &str, u8)] = &[
    ("ANGLE_instanced_arrays", "drawArraysInstancedANGLE", 4),
    ("ANGLE_instanced_arrays", "drawElementsInstancedANGLE", 5),
    ("ANGLE_instanced_arrays", "vertexAttribDivisorANGLE", 2),
    ("OES_vertex_array_object", "createVertexArrayOES", 0),
    ("OES_vertex_array_object", "deleteVertexArrayOES", 1),
    ("OES_vertex_array_object", "isVertexArrayOES", 1),
    ("OES_vertex_array_object", "bindVertexArrayOES", 1),
    ("WEBGL_draw_buffers", "drawBuffersWEBGL", 1),
    ("WEBGL_lose_context", "loseContext", 0),
    ("WEBGL_lose_context", "restoreContext", 0),
];

const OBJECT_CLASSES: &[&str] = &[
    "WebGLBuffer",
    "WebGLFramebuffer",
    "WebGLProgram",
    "WebGLRenderbuffer",
    "WebGLShader",
    "WebGLTexture",
    "WebGLUniformLocation",
    "WebGLActiveInfo",
    "WebGLShaderPrecisionFormat",
    "WebGLQuery",
    "WebGLSampler",
    "WebGLSync",
    "WebGLTransformFeedback",
    "WebGLVertexArrayObject",
];

pub(super) fn create(
    build: &Builder,
    object_prototype: &ObjectHandle,
    globals: &mut Vec<(&'static str, Value)>,
) -> WebGlInterfaces {
    let mut prototypes: HashMap<&'static str, ObjectHandle> = HashMap::default();
    let accessor = |target: &ObjectHandle, name: &str, get: GlMember, set: Option<GlMember>| {
        let get = build.function(Native::WebGl(get), &format!("get {name}"), 0);
        let set = set.map(|set| {
            Value::Object(build.function(Native::WebGl(set), &format!("set {name}"), 1))
        });
        target.borrow_mut().define(
            name.to_string(),
            Property {
                slot: Slot::accessor(Some(Value::Object(get)), set),
                enumerable: true,
                writable: false,
                configurable: true,
            },
        );
    };
    let constant = |target: &ObjectHandle, name: &str, value: u32| {
        target.borrow_mut().define(
            name.to_string(),
            Property {
                writable: false,
                configurable: false,
                ..Property::data(Value::Number(f64::from(value)))
            },
        );
    };
    let mut class = |name: &'static str, parent: Option<&ObjectHandle>| {
        let prototype = new_object(JsObject::with_prototype(
            Some(parent.unwrap_or(object_prototype).clone()),
            ObjectClass::Ordinary,
        ));
        prototype.borrow_mut().define(
            "@@toStringTag".to_string(),
            Property {
                writable: false,
                ..Property::hidden(Value::String(name.into()))
            },
        );
        let constructor = build.constructor(Native::IllegalConstructor, name, 0, &prototype);
        globals.push((name, Value::Object(constructor.clone())));
        prototypes.insert(name, prototype.clone());
        (prototype, constructor)
    };
    let webgl2_start = first_webgl2_method();
    for (name, webgl2) in [
        ("WebGLRenderingContext", false),
        ("WebGL2RenderingContext", true),
    ] {
        let (prototype, constructor) = class(name, None);
        accessor(&prototype, "canvas", GlMember::Canvas, None);
        accessor(
            &prototype,
            "drawingBufferWidth",
            GlMember::DrawingBufferWidth,
            None,
        );
        accessor(
            &prototype,
            "drawingBufferHeight",
            GlMember::DrawingBufferHeight,
            None,
        );
        accessor(
            &prototype,
            "drawingBufferColorSpace",
            GlMember::ColorSpace(false),
            Some(GlMember::SetColorSpace(false)),
        );
        accessor(
            &prototype,
            "unpackColorSpace",
            GlMember::ColorSpace(true),
            Some(GlMember::SetColorSpace(true)),
        );
        let tables: &[&[(&str, u32)]] = if webgl2 { &[WEBGL1, WEBGL2] } else { &[WEBGL1] };
        for table in tables {
            for &(constant_name, value) in *table {
                constant(&prototype, constant_name, value);
                constant(&constructor, constant_name, value);
            }
        }
        let count = if webgl2 { METHODS.len() } else { webgl2_start };
        for (index, (method, length)) in METHODS.iter().enumerate().take(count) {
            let function = build.function(
                Native::WebGl(GlMember::Method(index as u16)),
                method,
                u32::from(*length),
            );
            prototype.borrow_mut().define(
                method.to_string(),
                Property {
                    enumerable: true,
                    ..Property::data(Value::Object(function))
                },
            );
        }
    }
    let (object, _) = class("WebGLObject", None);
    for name in OBJECT_CLASSES {
        let parent = (!matches!(
            *name,
            "WebGLUniformLocation" | "WebGLActiveInfo" | "WebGLShaderPrecisionFormat"
        ))
        .then_some(&object);
        let (prototype, _) = class(name, parent);
        match *name {
            "WebGLActiveInfo" => {
                accessor(&prototype, "size", GlMember::InfoSize, None);
                accessor(&prototype, "type", GlMember::InfoType, None);
                accessor(&prototype, "name", GlMember::InfoName, None);
            }
            "WebGLShaderPrecisionFormat" => {
                accessor(&prototype, "rangeMin", GlMember::RangeMin, None);
                accessor(&prototype, "rangeMax", GlMember::RangeMax, None);
                accessor(&prototype, "precision", GlMember::Precision, None);
            }
            _ => {}
        }
    }
    // extension objects have interfaces of their own but no global names
    for (name, _, _, constants) in EXTENSIONS {
        let prototype = new_object(JsObject::with_prototype(
            Some(object_prototype.clone()),
            ObjectClass::Ordinary,
        ));
        prototype.borrow_mut().define(
            "@@toStringTag".to_string(),
            Property {
                writable: false,
                ..Property::hidden(Value::String(extension_interface(name).into()))
            },
        );
        for &(constant_name, value) in *constants {
            constant(&prototype, constant_name, value);
        }
        if *name == "WEBGL_draw_buffers" {
            for index in 0..16 {
                constant(
                    &prototype,
                    &format!("COLOR_ATTACHMENT{index}_WEBGL"),
                    COLOR_ATTACHMENT0 + index,
                );
                constant(
                    &prototype,
                    &format!("DRAW_BUFFER{index}_WEBGL"),
                    DRAW_BUFFER0 + index,
                );
            }
            constant(
                &prototype,
                "MAX_COLOR_ATTACHMENTS_WEBGL",
                MAX_COLOR_ATTACHMENTS,
            );
            constant(&prototype, "MAX_DRAW_BUFFERS_WEBGL", MAX_DRAW_BUFFERS);
        }
        for (index, (extension, method, length)) in EXTENSION_METHODS.iter().enumerate() {
            if extension == name {
                let function = build.function(
                    Native::WebGl(GlMember::Extension(index as u8)),
                    method,
                    u32::from(*length),
                );
                prototype.borrow_mut().define(
                    method.to_string(),
                    Property {
                        enumerable: true,
                        ..Property::data(Value::Object(function))
                    },
                );
            }
        }
        prototypes.insert(name, prototype);
    }
    WebGlInterfaces { prototypes }
}

impl ScriptRuntime {
    fn webgl_object(&self, prototype: &str, object: GlObject) -> ObjectHandle {
        let prototype = self.intrinsics.webgl.prototypes[prototype].clone();
        new_object(JsObject::with_prototype(
            Some(prototype),
            ObjectClass::WebGl(Rc::new(object)),
        ))
    }

    fn webgl_of(value: &Value) -> Option<Rc<GlObject>> {
        match value {
            Value::Object(object) => match &object.borrow().class {
                ObjectClass::WebGl(found) => Some(found.clone()),
                _ => None,
            },
            _ => None,
        }
    }

    fn context_options(&mut self, options: &Value) -> Result<Options, ScriptError> {
        let mut read = |name: &str, default: bool| -> Result<bool, ScriptError> {
            if !matches!(options, Value::Object(_)) {
                return Ok(default);
            }
            Ok(match self.get_property(options, name)? {
                Value::Undefined => default,
                value => value.truthy(),
            })
        };
        let alpha = read("alpha", true)?;
        let depth = read("depth", true)?;
        let stencil = read("stencil", false)?;
        let premultiplied = read("premultipliedAlpha", true)?;
        let preserve = read("preserveDrawingBuffer", false)?;
        let fail_if_caveat = read("failIfMajorPerformanceCaveat", false)?;
        let desynchronized = read("desynchronized", false)?;
        let xr = read("xrCompatible", false)?;
        let power = if matches!(options, Value::Object(_)) {
            match self.get_property(options, "powerPreference")? {
                Value::Undefined => "default".to_string(),
                value => {
                    let text = self.coerce_string(value)?;
                    if !matches!(text.as_str(), "default" | "low-power" | "high-performance") {
                        return Err(self.type_error(format!(
                            "Failed to execute 'getContext' on 'HTMLCanvasElement': Failed to read the 'powerPreference' property from 'WebGLContextAttributes': The provided value '{text}' is not a valid enum value of type WebGLPowerPreference."
                        )));
                    }
                    text
                }
            }
        } else {
            "default".to_string()
        };
        Ok(Options {
            alpha,
            depth,
            stencil,
            premultiplied,
            preserve,
            power,
            fail_if_caveat,
            desynchronized,
            xr,
        })
    }

    // getContext("webgl"), "experimental-webgl" or "webgl2": the canvas's context of that kind,
    // made on first use, or null when the canvas already has another kind
    pub(super) fn webgl_get_context(
        &mut self,
        canvas: Value,
        node: NodeId,
        kind: &str,
        options: &Value,
    ) -> Result<Value, ScriptError> {
        let webgl2 = kind == "webgl2";
        if let Some((object, context)) = self.webgl_contexts.get(&node) {
            return Ok(if context.borrow().gl.webgl2 == webgl2 {
                Value::Object(object.clone())
            } else {
                Value::Null
            });
        }
        if self.canvas_contexts.contains_key(&node) {
            return Ok(Value::Null);
        }
        let options = self.context_options(options)?;
        let (width, height) = super::canvas::canvas_size(&self.document.borrow(), node);
        let gl = Context::new(
            webgl2,
            width,
            height,
            options.alpha,
            options.depth,
            options.stencil,
            options.premultiplied,
            options.preserve,
        );
        let context = Rc::new(RefCell::new(GlContext {
            gl,
            canvas,
            node,
            options,
            wrappers: HashMap::default(),
            extensions: HashMap::default(),
            snapshot: None,
            color_space: ["srgb".into(), "srgb".into()],
        }));
        let prototype = if webgl2 {
            "WebGL2RenderingContext"
        } else {
            "WebGLRenderingContext"
        };
        let object = self.webgl_object(prototype, GlObject::Context(context.clone()));
        self.webgl_contexts.insert(node, (object.clone(), context));
        Ok(Value::Object(object))
    }

    pub(super) fn has_webgl_context(&self, node: NodeId) -> bool {
        self.webgl_contexts.contains_key(&node)
    }

    // the canvas was given a new width or height
    pub(super) fn resize_webgl(&mut self, node: NodeId) {
        let Some((_, context)) = self.webgl_contexts.get(&node) else {
            return;
        };
        let (width, height) = super::canvas::canvas_size(&self.document.borrow(), node);
        let mut context = context.borrow_mut();
        context.gl.resize(width, height);
        context.snapshot = None;
    }

    // what paint shows for each connected WebGL canvas, taken when the drawing buffer changed
    pub(super) fn webgl_layers(&self) -> Vec<crate::layout::ImageLayer> {
        let document = self.document.borrow();
        self.webgl_contexts
            .iter()
            .filter(|(node, _)| document.is_connected(**node))
            .filter_map(|(node, (_, context))| {
                let mut context = context.borrow_mut();
                if context.gl.width == 0 || context.gl.height == 0 {
                    return None;
                }
                if context.gl.dirty || context.snapshot.is_none() {
                    let image = context.gl.present();
                    context.snapshot = Some(std::sync::Arc::new(image));
                }
                Some(crate::layout::ImageLayer {
                    node: *node,
                    image: context.snapshot.clone()?,
                    background: None,
                })
            })
            .collect()
    }

    // the pixels of a WebGL canvas for drawImage, toDataURL and the like: the drawing buffer,
    // or what paint showed when the buffer has since been cleared for the next frame
    pub(super) fn webgl_image(&self, node: NodeId) -> Option<crate::image::Image> {
        let (_, context) = self.webgl_contexts.get(&node)?;
        let context = context.borrow();
        if context.gl.clear_pending {
            return context.snapshot.as_deref().cloned();
        }
        Some(context.gl.image())
    }

    pub(super) fn forget_webgl(&mut self, gone: &dyn Fn(NodeId) -> bool) {
        self.webgl_contexts.retain(|node, _| !gone(*node));
    }

    // the script object for a GL object, the same one each time
    fn wrap_gl(&self, context: &Rc<RefCell<GlContext>>, id: u32, kind: Kind) -> Value {
        if id == 0 {
            return Value::Null;
        }
        if let Some(object) = context.borrow().wrappers.get(&id) {
            return Value::Object(object.clone());
        }
        let object = self.webgl_object(
            kind.class(),
            GlObject::Object {
                context: Rc::downgrade(context),
                id,
                kind,
            },
        );
        context.borrow_mut().wrappers.insert(id, object.clone());
        Value::Object(object)
    }

    pub(super) fn call_webgl(
        &mut self,
        member: GlMember,
        this: Value,
        arguments: Vec<Value>,
    ) -> Result<Value, ScriptError> {
        let object = Self::webgl_of(&this);
        match member {
            GlMember::InfoName | GlMember::InfoSize | GlMember::InfoType => {
                let Some(GlObject::Info { name, size, kind }) = object.as_deref() else {
                    return Err(self.type_error("Illegal invocation"));
                };
                return Ok(match member {
                    GlMember::InfoName => Value::String(name.as_str().into()),
                    GlMember::InfoSize => Value::Number(f64::from(*size)),
                    _ => Value::Number(f64::from(*kind)),
                });
            }
            GlMember::RangeMin | GlMember::RangeMax | GlMember::Precision => {
                let Some(GlObject::Precision(values)) = object.as_deref() else {
                    return Err(self.type_error("Illegal invocation"));
                };
                let index = match member {
                    GlMember::RangeMin => 0,
                    GlMember::RangeMax => 1,
                    _ => 2,
                };
                return Ok(Value::Number(f64::from(values[index])));
            }
            GlMember::Extension(index) => {
                let Some(GlObject::Extension(context)) = object.as_deref() else {
                    return Err(self.type_error("Illegal invocation"));
                };
                let Some(context) = context.upgrade() else {
                    return Ok(Value::Undefined);
                };
                return self.call_extension(&context, index as usize, arguments);
            }
            _ => {}
        }
        let Some(GlObject::Context(context)) = object.as_deref() else {
            return Err(self.type_error("Illegal invocation"));
        };
        let context = context.clone();
        match member {
            GlMember::Canvas => Ok(context.borrow().canvas.clone()),
            GlMember::DrawingBufferWidth => Ok(Value::Number(f64::from(context.borrow().gl.width))),
            GlMember::DrawingBufferHeight => {
                Ok(Value::Number(f64::from(context.borrow().gl.height)))
            }
            GlMember::ColorSpace(unpack) => Ok(Value::String(
                context.borrow().color_space[usize::from(unpack)].clone(),
            )),
            GlMember::SetColorSpace(unpack) => {
                let value =
                    self.coerce_string(arguments.first().cloned().unwrap_or(Value::Undefined))?;
                if matches!(value.as_str(), "srgb" | "display-p3") {
                    context.borrow_mut().color_space[usize::from(unpack)] = value.as_str().into();
                }
                Ok(Value::Undefined)
            }
            GlMember::Method(index) => {
                let (name, required) = METHODS[index as usize];
                let interface = if context.borrow().gl.webgl2 {
                    "WebGL2RenderingContext"
                } else {
                    "WebGLRenderingContext"
                };
                if arguments.len() < usize::from(required) {
                    return Err(self.type_error(format!(
                        "Failed to execute '{name}' on '{interface}': {required} argument{} required, but only {} present.",
                        if required == 1 { "" } else { "s" },
                        arguments.len()
                    )));
                }
                self.call_gl_method(&context, name, interface, &arguments)
            }
            _ => Ok(Value::Undefined),
        }
    }

    fn call_extension(
        &mut self,
        context: &Rc<RefCell<GlContext>>,
        index: usize,
        arguments: Vec<Value>,
    ) -> Result<Value, ScriptError> {
        let (extension, method, required) = EXTENSION_METHODS[index];
        if arguments.len() < usize::from(required) {
            return Err(self.type_error(format!(
                "Failed to execute '{method}' on '{extension}': {required} argument{} required, but only {} present.",
                if required == 1 { "" } else { "s" },
                arguments.len()
            )));
        }
        let interface = "WebGLRenderingContext";
        let target = match method {
            "drawArraysInstancedANGLE" => "drawArraysInstanced",
            "drawElementsInstancedANGLE" => "drawElementsInstanced",
            "vertexAttribDivisorANGLE" => "vertexAttribDivisor",
            "createVertexArrayOES" => "createVertexArray",
            "deleteVertexArrayOES" => "deleteVertexArray",
            "isVertexArrayOES" => "isVertexArray",
            "bindVertexArrayOES" => "bindVertexArray",
            "drawBuffersWEBGL" => "drawBuffers",
            "loseContext" => return self.lose_webgl(context, true),
            _ => return self.lose_webgl(context, false),
        };
        self.call_gl_method(context, target, interface, &arguments)
    }

    // WEBGL_lose_context: losing drops every object, restoring starts the context afresh
    fn lose_webgl(
        &mut self,
        context: &Rc<RefCell<GlContext>>,
        lose: bool,
    ) -> Result<Value, ScriptError> {
        let node = {
            let mut state = context.borrow_mut();
            if state.gl.lost == lose {
                state.gl.error(INVALID_OPERATION);
                return Ok(Value::Undefined);
            }
            if lose {
                state.gl.lost = true;
                state.gl.errors = vec![CONTEXT_LOST_WEBGL];
                state.wrappers.clear();
            } else {
                let options = &state.options;
                let (webgl2, width, height) = (state.gl.webgl2, state.gl.width, state.gl.height);
                state.gl = Context::new(
                    webgl2,
                    width,
                    height,
                    options.alpha,
                    options.depth,
                    options.stencil,
                    options.premultiplied,
                    options.preserve,
                );
                state.extensions.clear();
            }
            matches!(state.canvas, Value::Element(_)).then_some(state.node)
        };
        if let Some(node) = node {
            self.dispatch_node_event(
                node,
                if lose {
                    "webglcontextlost"
                } else {
                    "webglcontextrestored"
                },
            )?;
        }
        Ok(Value::Undefined)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn script_draws_reads_back_and_shares_webgl_pixels() {
        let document = crate::dom::Document::parse(r#"<canvas id="c" width="4" height="4"></canvas><canvas id="d" width="4" height="4"></canvas><p id="out"></p>"#).expect("document should parse");
        let document = Rc::new(RefCell::new(document));
        let mut runtime = ScriptRuntime::new(document.clone());
        runtime
            .execute(
                r##"
                const seen = [];
                const canvas = document.getElementById("c");
                const gl = canvas.getContext("webgl", { preserveDrawingBuffer: true });
                seen.push(gl instanceof WebGLRenderingContext, canvas.getContext("2d"), gl.getParameter(gl.VERSION).startsWith("WebGL 1.0"));
                const shader = (type, source) => { const s = gl.createShader(type); gl.shaderSource(s, source); gl.compileShader(s); return s; };
                const program = gl.createProgram();
                gl.attachShader(program, shader(gl.VERTEX_SHADER, "attribute vec2 p; void main() { gl_Position = vec4(p, 0.0, 1.0); }"));
                gl.attachShader(program, shader(gl.FRAGMENT_SHADER, "precision mediump float; uniform vec4 color; void main() { gl_FragColor = color; }"));
                gl.linkProgram(program);
                gl.useProgram(program);
                seen.push(gl.getProgramParameter(program, gl.LINK_STATUS), gl.getActiveUniform(program, 0).name);
                gl.bindBuffer(gl.ARRAY_BUFFER, gl.createBuffer());
                gl.bufferData(gl.ARRAY_BUFFER, new Float32Array([-1, -1, 1, -1, -1, 1]), gl.STATIC_DRAW);
                gl.enableVertexAttribArray(0);
                gl.vertexAttribPointer(0, 2, gl.FLOAT, false, 0, 0);
                gl.uniform4fv(gl.getUniformLocation(program, "color"), [0, 1, 0, 1]);
                gl.clearColor(0, 0, 1, 1);
                gl.clear(gl.COLOR_BUFFER_BIT);
                gl.drawArrays(gl.TRIANGLES, 0, 3);
                const pixel = new Uint8Array(4);
                gl.readPixels(0, 0, 1, 1, gl.RGBA, gl.UNSIGNED_BYTE, pixel);
                seen.push(Array.from(pixel).join(), gl.getError());
                gl.drawArrays(gl.TRIANGLES, 0, 4);
                seen.push(gl.getError(), gl.getError());
                // the bottom-left green corner is the top-left of a 2D canvas's rows flipped
                const context = document.getElementById("d").getContext("2d");
                context.drawImage(canvas, 0, 0);
                seen.push(Array.from(context.getImageData(0, 3, 1, 1).data).join(), Array.from(context.getImageData(3, 0, 1, 1).data).join());
                const gl2 = document.createElement("canvas").getContext("webgl2");
                seen.push(gl2 instanceof WebGL2RenderingContext, gl2.getExtension("WEBGL_lose_context") !== null);
                gl2.getExtension("WEBGL_lose_context").loseContext();
                seen.push(gl2.isContextLost(), gl2.getError() === gl2.CONTEXT_LOST_WEBGL, gl2.createBuffer());
                try { gl.bindBuffer(gl.ARRAY_BUFFER, {}); } catch (error) { seen.push(error.name); }
                document.getElementById("out").textContent = seen.join("|");
                "##,
            )
            .expect("script should run");
        let out = document.borrow().find_element_by_id("out").unwrap();
        assert_eq!(
            document.borrow().element_text_content(out).as_deref(),
            Some(
                "true||true|true|color|0,255,0,255|0|1282|0|0,255,0,255|0,0,255,255|true|true|true|true||TypeError"
            )
        );
    }
}

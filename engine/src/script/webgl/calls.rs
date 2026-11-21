// each WebGL method: its arguments converted the way WebIDL converts them, then the state
// machine called; objects from another context count as INVALID_OPERATION, the wrong kind of
// object as a TypeError

use super::context::{self, Attachment, AttribArray, Capability, Setter};
use super::texture::{Format, Kind as TexelKind, Level, Texture};
use super::*;
use crate::script::typed::{ElementKind, byte_range, new_typed};

type Location = Option<(u32, u32, usize)>;

enum Arg {
    Null,
    Id(u32),
    Foreign,
}

// what a texImage or texSubImage call was given to upload
enum Pixels {
    None,
    Bytes(Vec<u8>),
    Texels(Vec<[f32; 4]>),
}

fn float_array(values: &[f32]) -> Value {
    let bytes: Vec<u8> = values
        .iter()
        .flat_map(|value| value.to_le_bytes())
        .collect();
    let length = bytes.len();
    new_typed(
        ElementKind::Float32,
        Rc::new(RefCell::new(bytes.into())),
        0,
        length,
    )
}

fn int_array(values: &[i32]) -> Value {
    let bytes: Vec<u8> = values
        .iter()
        .flat_map(|value| value.to_le_bytes())
        .collect();
    let length = bytes.len();
    new_typed(
        ElementKind::Int32,
        Rc::new(RefCell::new(bytes.into())),
        0,
        length,
    )
}

fn uint_array(values: &[u32]) -> Value {
    let bytes: Vec<u8> = values
        .iter()
        .flat_map(|value| value.to_le_bytes())
        .collect();
    let length = bytes.len();
    new_typed(
        ElementKind::Uint32,
        Rc::new(RefCell::new(bytes.into())),
        0,
        length,
    )
}

fn number(value: impl Into<f64>) -> Value {
    Value::Number(value.into())
}

// the typed array kinds a pixel type reads from and writes to
fn view_fits(kind: ElementKind, pixel_type: u32) -> bool {
    match pixel_type {
        UNSIGNED_BYTE => matches!(kind, ElementKind::Uint8 | ElementKind::Uint8Clamped),
        BYTE => kind == ElementKind::Int8,
        UNSIGNED_SHORT
        | UNSIGNED_SHORT_5_6_5
        | UNSIGNED_SHORT_4_4_4_4
        | UNSIGNED_SHORT_5_5_5_1
        | HALF_FLOAT
        | HALF_FLOAT_OES => kind == ElementKind::Uint16,
        SHORT => kind == ElementKind::Int16,
        UNSIGNED_INT
        | UNSIGNED_INT_2_10_10_10_REV
        | UNSIGNED_INT_10F_11F_11F_REV
        | UNSIGNED_INT_5_9_9_9_REV
        | UNSIGNED_INT_24_8 => kind == ElementKind::Uint32,
        INT => kind == ElementKind::Int32,
        FLOAT => kind == ElementKind::Float32,
        FLOAT_32_UNSIGNED_INT_24_8_REV => false,
        _ => false,
    }
}

impl ScriptRuntime {
    fn gl_u32(&mut self, arguments: &[Value], index: usize) -> Result<u32, ScriptError> {
        self.coerce_uint32(arguments.get(index).cloned().unwrap_or(Value::Undefined))
    }

    fn gl_i32(&mut self, arguments: &[Value], index: usize) -> Result<i32, ScriptError> {
        self.coerce_int32(arguments.get(index).cloned().unwrap_or(Value::Undefined))
    }

    fn gl_f32(&mut self, arguments: &[Value], index: usize) -> Result<f32, ScriptError> {
        Ok(self.coerce_number(arguments.get(index).cloned().unwrap_or(Value::Undefined))? as f32)
    }

    // GLintptr and GLsizeiptr, WebIDL long long
    fn gl_i64(&mut self, arguments: &[Value], index: usize) -> Result<i64, ScriptError> {
        let value =
            self.coerce_number(arguments.get(index).cloned().unwrap_or(Value::Undefined))?;
        Ok(if value.is_finite() {
            value.trunc().clamp(i64::MIN as f64, i64::MAX as f64) as i64
        } else {
            0
        })
    }

    fn gl_bool(arguments: &[Value], index: usize) -> bool {
        arguments.get(index).is_some_and(Value::truthy)
    }

    #[allow(clippy::too_many_arguments)]
    fn gl_arg(
        &mut self,
        context: &Rc<RefCell<GlContext>>,
        arguments: &[Value],
        index: usize,
        kind: Kind,
        nullable: bool,
        method: &str,
        interface: &str,
    ) -> Result<Arg, ScriptError> {
        let value = arguments.get(index).cloned().unwrap_or(Value::Undefined);
        if nullable && matches!(value, Value::Null | Value::Undefined) {
            return Ok(Arg::Null);
        }
        match Self::webgl_of(&value).as_deref() {
            Some(GlObject::Object {
                context: owner,
                id,
                kind: found,
            }) if *found == kind => Ok(if Weak::ptr_eq(owner, &Rc::downgrade(context)) {
                Arg::Id(*id)
            } else {
                Arg::Foreign
            }),
            _ => Err(self.type_error(format!(
                "Failed to execute '{method}' on '{interface}': parameter {} is not of type '{}'.",
                index + 1,
                kind.class()
            ))),
        }
    }

    // a uniform location argument: (program, generation, index), or None for null
    fn gl_location(
        &mut self,
        context: &Rc<RefCell<GlContext>>,
        value: &Value,
        method: &str,
        interface: &str,
    ) -> Result<Option<Location>, ScriptError> {
        if matches!(value, Value::Null | Value::Undefined) {
            return Ok(Some(None));
        }
        match Self::webgl_of(value).as_deref() {
            Some(GlObject::Location { context: owner, program, generation, index }) => {
                if Weak::ptr_eq(owner, &Rc::downgrade(context)) {
                    Ok(Some(Some((*program, *generation, *index as usize))))
                } else {
                    Ok(None)
                }
            }
            _ => Err(self.type_error(format!("Failed to execute '{method}' on '{interface}': parameter 1 is not of type 'WebGLUniformLocation'."))),
        }
    }

    // the bytes of a view or buffer and the size of its elements
    fn view_bytes(value: &Value) -> Option<(Vec<u8>, usize, Option<ElementKind>)> {
        match value {
            Value::Bytes(bytes) => Some((bytes.borrow().to_vec(), 1, None)),
            Value::TypedArray(array) => {
                let array = array.borrow();
                let bytes = array
                    .buffer
                    .borrow()
                    .get(byte_range(&array))
                    .map(<[u8]>::to_vec)
                    .unwrap_or_default();
                Some((bytes, array.kind.size(), Some(array.kind)))
            }
            Value::Object(object) => match &object.borrow().class {
                ObjectClass::DataView {
                    buffer,
                    start,
                    end,
                    tracking,
                } => {
                    let buffer = buffer.borrow();
                    let range =
                        super::super::typed::view_range(&buffer, *start, *end, *tracking, 1);
                    Some((
                        range.map_or(Vec::new(), |(start, end)| buffer[start..end].to_vec()),
                        1,
                        None,
                    ))
                }
                _ => None,
            },
            _ => None,
        }
    }

    // a sequence of numbers or a typed array of the expected kind, as the uniform and
    // vertexAttrib *v methods take them
    #[allow(clippy::too_many_arguments)]
    fn gl_list(
        &mut self,
        value: &Value,
        expected: ElementKind,
        offset: usize,
        length: usize,
        method: &str,
        interface: &str,
    ) -> Result<Option<Vec<u32>>, ScriptError> {
        let words: Vec<u32> = match value {
            Value::TypedArray(array) => {
                let kind = array.borrow().kind;
                if kind != expected {
                    return Err(self.type_error(format!("Failed to execute '{method}' on '{interface}': parameter 2 is not of type '{}'.", expected.name())));
                }
                let (bytes, _, _) = Self::view_bytes(value).expect("a typed array has bytes");
                bytes.as_chunks::<4>().0.iter().map(|word| u32::from_le_bytes(*word)).collect()
            }
            Value::Array(_) | Value::Object(_) => {
                let values = self.iterate_to_vec(value.clone())?;
                let mut words = Vec::with_capacity(values.len());
                for item in values {
                    words.push(match expected {
                        ElementKind::Float32 => (self.coerce_number(item)? as f32).to_bits(),
                        ElementKind::Int32 => self.coerce_int32(item)? as u32,
                        _ => self.coerce_uint32(item)?,
                    });
                }
                words
            }
            _ => return Err(self.type_error(format!("Failed to execute '{method}' on '{interface}': The provided value cannot be converted to a sequence."))),
        };
        if offset > words.len() || (length > 0 && offset + length > words.len()) {
            return Ok(None);
        }
        let end = if length == 0 {
            words.len()
        } else {
            offset + length
        };
        Ok(Some(words[offset..end].to_vec()))
    }

    fn gl_shader_precision(kind: u32) -> Option<[i32; 3]> {
        Some(match kind {
            LOW_FLOAT | MEDIUM_FLOAT | HIGH_FLOAT => [127, 127, 23],
            LOW_INT | MEDIUM_INT | HIGH_INT => [31, 30, 0],
            _ => return None,
        })
    }

    fn lost_answer(name: &str) -> Value {
        if name.starts_with("is") {
            Value::Bool(name == "isContextLost")
        } else if name == "getAttribLocation" || name == "getFragDataLocation" {
            number(-1)
        } else if name == "checkFramebufferStatus" || name == "getUniformBlockIndex" {
            number(0)
        } else if name.starts_with("get")
            || name.starts_with("create")
            || name == "fenceSync"
            || name == "clientWaitSync"
        {
            if name == "clientWaitSync" {
                number(WAIT_FAILED)
            } else {
                Value::Null
            }
        } else {
            Value::Undefined
        }
    }

    pub(super) fn call_gl_method(
        &mut self,
        context: &Rc<RefCell<GlContext>>,
        name: &str,
        interface: &str,
        arguments: &[Value],
    ) -> Result<Value, ScriptError> {
        let lost = context.borrow().gl.lost;
        if lost
            && !matches!(
                name,
                "getError"
                    | "isContextLost"
                    | "getExtension"
                    | "getSupportedExtensions"
                    | "getContextAttributes"
            )
        {
            return Ok(Self::lost_answer(name));
        }
        macro_rules! gl {
            () => {
                context.borrow_mut().gl
            };
        }
        macro_rules! object {
            ($index:expr, $kind:expr, $nullable:expr) => {
                match self.gl_arg(
                    context, arguments, $index, $kind, $nullable, name, interface,
                )? {
                    Arg::Foreign => {
                        gl!().error(INVALID_OPERATION);
                        return Ok(Self::lost_answer(name));
                    }
                    Arg::Null => 0,
                    Arg::Id(id) => id,
                }
            };
        }
        let undefined = Ok(Value::Undefined);
        match name {
            "getError" => Ok(number(gl!().take_error())),
            "isContextLost" => Ok(Value::Bool(lost)),
            "getContextAttributes" => {
                if lost {
                    return Ok(Value::Null);
                }
                let state = context.borrow();
                let options = &state.options;
                Ok(self.object_from_entries(vec![
                    ("alpha".into(), Value::Bool(options.alpha)),
                    ("antialias".into(), Value::Bool(false)),
                    ("depth".into(), Value::Bool(options.depth)),
                    ("desynchronized".into(), Value::Bool(options.desynchronized)),
                    (
                        "failIfMajorPerformanceCaveat".into(),
                        Value::Bool(options.fail_if_caveat),
                    ),
                    (
                        "powerPreference".into(),
                        Value::String(options.power.as_str().into()),
                    ),
                    (
                        "premultipliedAlpha".into(),
                        Value::Bool(options.premultiplied),
                    ),
                    (
                        "preserveDrawingBuffer".into(),
                        Value::Bool(options.preserve),
                    ),
                    ("stencil".into(), Value::Bool(options.stencil)),
                    ("xrCompatible".into(), Value::Bool(options.xr)),
                ]))
            }
            "getSupportedExtensions" => {
                if lost {
                    return Ok(Value::Null);
                }
                let webgl2 = context.borrow().gl.webgl2;
                let names = EXTENSIONS
                    .iter()
                    .filter(|row| if webgl2 { row.2 } else { row.1 })
                    .map(|row| Value::String(row.0.into()))
                    .collect();
                Ok(Self::new_array(names))
            }
            "getExtension" => {
                let requested = self.coerce_string(arguments[0].clone())?;
                if lost {
                    return Ok(Value::Null);
                }
                let webgl2 = context.borrow().gl.webgl2;
                let Some(&(found, ..)) = EXTENSIONS.iter().find(|row| {
                    row.0.eq_ignore_ascii_case(&requested) && if webgl2 { row.2 } else { row.1 }
                }) else {
                    return Ok(Value::Null);
                };
                if let Some(object) = context.borrow().extensions.get(found) {
                    return Ok(Value::Object(object.clone()));
                }
                let object = self.webgl_object(found, GlObject::Extension(Rc::downgrade(context)));
                let mut state = context.borrow_mut();
                state.extensions.insert(found, object.clone());
                state.gl.extensions.push(found);
                Ok(Value::Object(object))
            }
            "makeXRCompatible" => Ok(Value::Promise(self.fulfilled_promise(Value::Undefined))),
            "activeTexture" => {
                let unit = self.gl_u32(arguments, 0)?;
                let mut state = context.borrow_mut();
                if !(TEXTURE0..TEXTURE0 + context::TEXTURE_UNITS as u32).contains(&unit) {
                    state.gl.error(INVALID_ENUM);
                } else {
                    state.gl.active_texture = unit - TEXTURE0;
                }
                undefined
            }
            "createBuffer"
            | "createTexture"
            | "createFramebuffer"
            | "createRenderbuffer"
            | "createProgram"
            | "createVertexArray"
            | "createQuery"
            | "createSampler"
            | "createTransformFeedback" => {
                let kind = match name {
                    "createBuffer" => Kind::Buffer,
                    "createTexture" => Kind::Texture,
                    "createFramebuffer" => Kind::Framebuffer,
                    "createRenderbuffer" => Kind::Renderbuffer,
                    "createProgram" => Kind::Program,
                    "createVertexArray" => Kind::VertexArray,
                    "createQuery" => Kind::Query,
                    "createSampler" => Kind::Sampler,
                    _ => Kind::TransformFeedback,
                };
                let id = {
                    let gl = &mut context.borrow_mut().gl;
                    let id = gl.create();
                    match kind {
                        Kind::Buffer => drop(gl.buffers.insert(
                            id,
                            context::Buffer {
                                data: Vec::new(),
                                usage: STATIC_DRAW,
                                element: None,
                            },
                        )),
                        Kind::Texture => drop(gl.textures.insert(id, Texture::new())),
                        Kind::Framebuffer => {
                            drop(gl.framebuffers.insert(id, context::Framebuffer::default()))
                        }
                        Kind::Renderbuffer => drop(gl.renderbuffers.insert(
                            id,
                            context::Renderbuffer {
                                level: None,
                                internal: RGBA4,
                                samples: 0,
                                bound: false,
                            },
                        )),
                        Kind::Program => drop(gl.programs.insert(
                            id,
                            context::Program {
                                feedback_mode: INTERLEAVED_ATTRIBS,
                                ..context::Program::default()
                            },
                        )),
                        Kind::VertexArray => {
                            drop(gl.vertex_arrays.insert(id, context::VertexArray::default()))
                        }
                        Kind::Query => drop(gl.queries.insert(
                            id,
                            context::Query {
                                target: 0,
                                result: 0,
                                active: false,
                            },
                        )),
                        Kind::Sampler => drop(gl.samplers.insert(
                            id,
                            context::SamplerObject {
                                params: super::texture::Params::default(),
                            },
                        )),
                        _ => drop(gl.feedbacks.insert(id, context::Feedback::default())),
                    }
                    id
                };
                Ok(self.wrap_gl(context, id, kind))
            }
            "createShader" => {
                let kind = self.gl_u32(arguments, 0)?;
                let id = {
                    let gl = &mut context.borrow_mut().gl;
                    if !matches!(kind, VERTEX_SHADER | FRAGMENT_SHADER) {
                        gl.error(INVALID_ENUM);
                        return Ok(Value::Null);
                    }
                    let id = gl.create();
                    gl.shaders.insert(
                        id,
                        context::Shader {
                            kind,
                            source: String::new(),
                            module: None,
                            compiled: false,
                            log: String::new(),
                            delete_pending: false,
                            attached: 0,
                        },
                    );
                    id
                };
                Ok(self.wrap_gl(context, id, Kind::Shader))
            }
            "fenceSync" => {
                let condition = self.gl_u32(arguments, 0)?;
                let flags = self.gl_u32(arguments, 1)?;
                let id = {
                    let gl = &mut context.borrow_mut().gl;
                    if condition != SYNC_GPU_COMMANDS_COMPLETE {
                        gl.error(INVALID_ENUM);
                        return Ok(Value::Null);
                    }
                    if flags != 0 {
                        gl.error(INVALID_VALUE);
                        return Ok(Value::Null);
                    }
                    let id = gl.create();
                    gl.syncs.insert(id, ());
                    id
                };
                Ok(self.wrap_gl(context, id, Kind::Sync))
            }
            "deleteBuffer"
            | "deleteTexture"
            | "deleteFramebuffer"
            | "deleteRenderbuffer"
            | "deleteShader"
            | "deleteProgram"
            | "deleteVertexArray"
            | "deleteQuery"
            | "deleteSampler"
            | "deleteSync"
            | "deleteTransformFeedback" => {
                let kind = match name {
                    "deleteBuffer" => Kind::Buffer,
                    "deleteTexture" => Kind::Texture,
                    "deleteFramebuffer" => Kind::Framebuffer,
                    "deleteRenderbuffer" => Kind::Renderbuffer,
                    "deleteShader" => Kind::Shader,
                    "deleteProgram" => Kind::Program,
                    "deleteVertexArray" => Kind::VertexArray,
                    "deleteQuery" => Kind::Query,
                    "deleteSampler" => Kind::Sampler,
                    "deleteSync" => Kind::Sync,
                    _ => Kind::TransformFeedback,
                };
                let id = object!(0, kind, true);
                if id == 0 {
                    return undefined;
                }
                let mut state = context.borrow_mut();
                let gl = &mut state.gl;
                match kind {
                    Kind::Buffer => gl.delete_buffer(id),
                    Kind::Texture => gl.delete_texture(id),
                    Kind::Framebuffer => gl.delete_framebuffer(id),
                    Kind::Renderbuffer => gl.delete_renderbuffer(id),
                    Kind::Shader => {
                        let attached = gl.shaders.get(&id).map(|shader| shader.attached);
                        match attached {
                            Some(0) => drop(gl.shaders.remove(&id)),
                            Some(_) => {
                                gl.shaders
                                    .get_mut(&id)
                                    .expect("the shader exists")
                                    .delete_pending = true
                            }
                            None => {}
                        }
                    }
                    Kind::Program => {
                        if gl.program == id {
                            if let Some(program) = gl.programs.get_mut(&id) {
                                program.delete_pending = true;
                            }
                        } else if let Some(program) = gl.programs.remove(&id) {
                            for shader in program.shaders {
                                detach_shader(gl, shader);
                            }
                        }
                    }
                    Kind::VertexArray => {
                        if gl.vertex_array == id {
                            gl.vertex_array = 0;
                        }
                        gl.vertex_arrays.remove(&id);
                    }
                    Kind::Query => {
                        gl.active_queries.retain(|_, active| *active != id);
                        gl.queries.remove(&id);
                    }
                    Kind::Sampler => {
                        for unit in &mut gl.unit_samplers {
                            if *unit == id {
                                *unit = 0;
                            }
                        }
                        gl.samplers.remove(&id);
                    }
                    Kind::Sync => drop(gl.syncs.remove(&id)),
                    Kind::TransformFeedback => {
                        if gl
                            .feedbacks
                            .get(&id)
                            .is_some_and(|feedback| feedback.active)
                        {
                            gl.error(INVALID_OPERATION);
                            return undefined;
                        }
                        if gl.feedback == id {
                            gl.feedback = 0;
                        }
                        gl.feedbacks.remove(&id);
                    }
                }
                if !matches!(kind, Kind::Shader | Kind::Program)
                    || !state.gl.shaders.contains_key(&id) && !state.gl.programs.contains_key(&id)
                {
                    state.wrappers.remove(&id);
                }
                undefined
            }
            "isBuffer"
            | "isTexture"
            | "isFramebuffer"
            | "isRenderbuffer"
            | "isShader"
            | "isProgram"
            | "isVertexArray"
            | "isQuery"
            | "isSampler"
            | "isSync"
            | "isTransformFeedback" => {
                let kind = match name {
                    "isBuffer" => Kind::Buffer,
                    "isTexture" => Kind::Texture,
                    "isFramebuffer" => Kind::Framebuffer,
                    "isRenderbuffer" => Kind::Renderbuffer,
                    "isShader" => Kind::Shader,
                    "isProgram" => Kind::Program,
                    "isVertexArray" => Kind::VertexArray,
                    "isQuery" => Kind::Query,
                    "isSampler" => Kind::Sampler,
                    "isSync" => Kind::Sync,
                    _ => Kind::TransformFeedback,
                };
                let id = match self.gl_arg(context, arguments, 0, kind, true, name, interface)? {
                    Arg::Id(id) => id,
                    _ => return Ok(Value::Bool(false)),
                };
                let gl = &context.borrow().gl;
                Ok(Value::Bool(match kind {
                    Kind::Buffer => gl
                        .buffers
                        .get(&id)
                        .is_some_and(|buffer| buffer.element.is_some()),
                    Kind::Texture => gl
                        .textures
                        .get(&id)
                        .is_some_and(|texture| texture.target != 0),
                    Kind::Framebuffer => gl
                        .framebuffers
                        .get(&id)
                        .is_some_and(|framebuffer| framebuffer.bound),
                    Kind::Renderbuffer => gl
                        .renderbuffers
                        .get(&id)
                        .is_some_and(|renderbuffer| renderbuffer.bound),
                    Kind::Shader => gl
                        .shaders
                        .get(&id)
                        .is_some_and(|shader| !shader.delete_pending),
                    Kind::Program => gl
                        .programs
                        .get(&id)
                        .is_some_and(|program| !program.delete_pending),
                    Kind::VertexArray => gl.vertex_arrays.get(&id).is_some_and(|array| array.bound),
                    Kind::Query => gl.queries.get(&id).is_some_and(|query| query.target != 0),
                    Kind::Sampler => gl.samplers.contains_key(&id),
                    Kind::Sync => gl.syncs.contains_key(&id),
                    Kind::TransformFeedback => {
                        gl.feedbacks.get(&id).is_some_and(|feedback| feedback.bound)
                    }
                }))
            }
            "bindBuffer" => {
                let target = self.gl_u32(arguments, 0)?;
                let id = object!(1, Kind::Buffer, true);
                gl!().bind_buffer(target, id);
                undefined
            }
            "bindTexture" => {
                let target = self.gl_u32(arguments, 0)?;
                let id = object!(1, Kind::Texture, true);
                gl!().bind_texture(target, id);
                undefined
            }
            "bindFramebuffer" => {
                let target = self.gl_u32(arguments, 0)?;
                let id = object!(1, Kind::Framebuffer, true);
                gl!().bind_framebuffer(target, id);
                undefined
            }
            "bindRenderbuffer" => {
                let target = self.gl_u32(arguments, 0)?;
                let id = object!(1, Kind::Renderbuffer, true);
                let gl = &mut context.borrow_mut().gl;
                if target != RENDERBUFFER {
                    gl.error(INVALID_ENUM);
                } else if id != 0 && !gl.renderbuffers.contains_key(&id) {
                    gl.error(INVALID_OPERATION);
                } else {
                    if let Some(renderbuffer) = gl.renderbuffers.get_mut(&id) {
                        renderbuffer.bound = true;
                    }
                    gl.renderbuffer = id;
                }
                undefined
            }
            "bindVertexArray" => {
                let id = object!(0, Kind::VertexArray, true);
                let gl = &mut context.borrow_mut().gl;
                match gl.vertex_arrays.get_mut(&id) {
                    Some(array) => {
                        array.bound = true;
                        gl.vertex_array = id;
                    }
                    None => gl.error(INVALID_OPERATION),
                }
                undefined
            }
            "bindSampler" => {
                let unit = self.gl_u32(arguments, 0)?;
                let id = object!(1, Kind::Sampler, true);
                let gl = &mut context.borrow_mut().gl;
                if unit as usize >= context::TEXTURE_UNITS {
                    gl.error(INVALID_VALUE);
                } else if id != 0 && !gl.samplers.contains_key(&id) {
                    gl.error(INVALID_OPERATION);
                } else {
                    gl.unit_samplers[unit as usize] = id;
                }
                undefined
            }
            "bindTransformFeedback" => {
                let target = self.gl_u32(arguments, 0)?;
                let id = object!(1, Kind::TransformFeedback, true);
                let gl = &mut context.borrow_mut().gl;
                if target != TRANSFORM_FEEDBACK {
                    gl.error(INVALID_ENUM);
                } else if gl.feedbacks[&gl.feedback].active && !gl.feedbacks[&gl.feedback].paused {
                    gl.error(INVALID_OPERATION);
                } else {
                    match gl.feedbacks.get_mut(&id) {
                        Some(feedback) => {
                            feedback.bound = true;
                            gl.feedback = id;
                        }
                        None => gl.error(INVALID_OPERATION),
                    }
                }
                undefined
            }
            "bindBufferBase" | "bindBufferRange" => {
                let target = self.gl_u32(arguments, 0)?;
                let index = self.gl_u32(arguments, 1)? as usize;
                let id = object!(2, Kind::Buffer, true);
                let (offset, size) = if name == "bindBufferRange" {
                    (self.gl_i64(arguments, 3)?, self.gl_i64(arguments, 4)?)
                } else {
                    (0, 0)
                };
                let gl = &mut context.borrow_mut().gl;
                if name == "bindBufferRange" && id != 0 && (offset < 0 || size <= 0) {
                    gl.error(INVALID_VALUE);
                    return undefined;
                }
                let range = context::Range {
                    buffer: id,
                    offset: offset as u64,
                    size: size as u64,
                };
                match target {
                    UNIFORM_BUFFER => {
                        if index >= context::UNIFORM_BUFFER_BINDINGS || offset % 256 != 0 {
                            gl.error(INVALID_VALUE);
                            return undefined;
                        }
                        gl.bind_buffer(UNIFORM_BUFFER, id);
                        gl.uniform_ranges[index] = range;
                    }
                    TRANSFORM_FEEDBACK_BUFFER => {
                        if index >= context::FEEDBACK_BUFFERS || offset % 4 != 0 {
                            gl.error(INVALID_VALUE);
                            return undefined;
                        }
                        gl.bind_buffer(TRANSFORM_FEEDBACK_BUFFER, id);
                        let current = gl.feedback;
                        gl.feedbacks
                            .get_mut(&current)
                            .expect("the bound transform feedback exists")
                            .buffers[index] = range;
                    }
                    _ => gl.error(INVALID_ENUM),
                }
                undefined
            }
            "bufferData" => self.gl_buffer_data(context, arguments, name, interface),
            "bufferSubData" => {
                let target = self.gl_u32(arguments, 0)?;
                let offset = self.gl_i64(arguments, 1)?;
                let Some((bytes, element, _)) = Self::view_bytes(&arguments[2]) else {
                    return Err(self.type_error(format!("Failed to execute '{name}' on '{interface}': parameter 3 is not of type 'ArrayBuffer'.")));
                };
                let source_offset = if arguments.len() > 3 {
                    self.gl_u32(arguments, 3)? as usize * element
                } else {
                    0
                };
                let length = if arguments.len() > 4 {
                    self.gl_u32(arguments, 4)? as usize * element
                } else {
                    0
                };
                let gl = &mut context.borrow_mut().gl;
                if source_offset > bytes.len() || source_offset + length > bytes.len() {
                    gl.error(INVALID_VALUE);
                    return undefined;
                }
                let end = if length == 0 {
                    bytes.len()
                } else {
                    source_offset + length
                };
                gl.buffer_sub_data(target, offset, &bytes[source_offset..end]);
                undefined
            }
            "copyBufferSubData" => {
                let (read, write) = (self.gl_u32(arguments, 0)?, self.gl_u32(arguments, 1)?);
                let (read_offset, write_offset, size) = (
                    self.gl_i64(arguments, 2)?,
                    self.gl_i64(arguments, 3)?,
                    self.gl_i64(arguments, 4)?,
                );
                let gl = &mut context.borrow_mut().gl;
                let (Some(from), Some(to)) = (gl.data_target(read), gl.data_target(write)) else {
                    return undefined;
                };
                if read_offset < 0 || write_offset < 0 || size < 0 {
                    gl.error(INVALID_VALUE);
                    return undefined;
                }
                let (read_offset, write_offset, size) =
                    (read_offset as usize, write_offset as usize, size as usize);
                let source = gl.buffers[&from].data.clone();
                let destination_length = gl.buffers[&to].data.len();
                if read_offset + size > source.len()
                    || write_offset + size > destination_length
                    || (from == to && read_offset.abs_diff(write_offset) < size)
                {
                    gl.error(INVALID_VALUE);
                    return undefined;
                }
                gl.buffers.get_mut(&to).expect("bound").data[write_offset..write_offset + size]
                    .copy_from_slice(&source[read_offset..read_offset + size]);
                undefined
            }
            "getBufferSubData" => {
                let target = self.gl_u32(arguments, 0)?;
                let offset = self.gl_i64(arguments, 1)?;
                let Value::TypedArray(view) = arguments[2].clone() else {
                    return Err(self.type_error(format!("Failed to execute '{name}' on '{interface}': parameter 3 is not of type 'ArrayBufferView'.")));
                };
                let element = view.borrow().kind.size();
                let destination_offset = if arguments.len() > 3 {
                    self.gl_u32(arguments, 3)? as usize * element
                } else {
                    0
                };
                let length = if arguments.len() > 4 {
                    self.gl_u32(arguments, 4)? as usize * element
                } else {
                    0
                };
                let gl = &mut context.borrow_mut().gl;
                let Some(id) = gl.data_target(target) else {
                    return undefined;
                };
                let view = view.borrow();
                let range = byte_range(&view);
                if offset < 0 || destination_offset > range.len() {
                    gl.error(INVALID_VALUE);
                    return undefined;
                }
                let length = if length == 0 {
                    range.len() - destination_offset
                } else {
                    length
                };
                let source = &gl.buffers[&id].data;
                if destination_offset + length > range.len()
                    || offset as usize + length > source.len()
                {
                    gl.error(INVALID_VALUE);
                    return undefined;
                }
                let start = range.start + destination_offset;
                view.buffer.borrow_mut()[start..start + length]
                    .copy_from_slice(&source[offset as usize..offset as usize + length]);
                undefined
            }
            "blendColor" => {
                let color = [
                    self.gl_f32(arguments, 0)?,
                    self.gl_f32(arguments, 1)?,
                    self.gl_f32(arguments, 2)?,
                    self.gl_f32(arguments, 3)?,
                ];
                gl!().blend.color = color;
                undefined
            }
            "blendEquation" | "blendEquationSeparate" => {
                let rgb = self.gl_u32(arguments, 0)?;
                let alpha = if name == "blendEquation" {
                    rgb
                } else {
                    self.gl_u32(arguments, 1)?
                };
                let gl = &mut context.borrow_mut().gl;
                let minmax = gl.webgl2 || gl.extensions.contains(&"EXT_blend_minmax");
                let valid = |mode: u32| {
                    matches!(mode, FUNC_ADD | FUNC_SUBTRACT | FUNC_REVERSE_SUBTRACT)
                        || minmax && matches!(mode, MIN | MAX)
                };
                if valid(rgb) && valid(alpha) {
                    gl.blend.equation = [rgb, alpha];
                } else {
                    gl.error(INVALID_ENUM);
                }
                undefined
            }
            "blendFunc" | "blendFuncSeparate" => {
                let values: Vec<u32> = if name == "blendFunc" {
                    let (source, destination) =
                        (self.gl_u32(arguments, 0)?, self.gl_u32(arguments, 1)?);
                    vec![source, destination, source, destination]
                } else {
                    vec![
                        self.gl_u32(arguments, 0)?,
                        self.gl_u32(arguments, 1)?,
                        self.gl_u32(arguments, 2)?,
                        self.gl_u32(arguments, 3)?,
                    ]
                };
                let gl = &mut context.borrow_mut().gl;
                let valid = |factor: u32, destination: bool| {
                    matches!(
                        factor,
                        ZERO | ONE
                            | SRC_COLOR
                            | ONE_MINUS_SRC_COLOR
                            | DST_COLOR
                            | ONE_MINUS_DST_COLOR
                            | SRC_ALPHA
                            | ONE_MINUS_SRC_ALPHA
                            | DST_ALPHA
                            | ONE_MINUS_DST_ALPHA
                            | CONSTANT_COLOR
                            | ONE_MINUS_CONSTANT_COLOR
                            | CONSTANT_ALPHA
                            | ONE_MINUS_CONSTANT_ALPHA
                    ) || factor == SRC_ALPHA_SATURATE && (!destination || gl.webgl2)
                };
                if !(valid(values[0], false)
                    && valid(values[1], true)
                    && valid(values[2], false)
                    && valid(values[3], true))
                {
                    gl.error(INVALID_ENUM);
                    return undefined;
                }
                // WebGL forbids pairing constant color with constant alpha
                let constant_color =
                    |factor: u32| matches!(factor, CONSTANT_COLOR | ONE_MINUS_CONSTANT_COLOR);
                let constant_alpha =
                    |factor: u32| matches!(factor, CONSTANT_ALPHA | ONE_MINUS_CONSTANT_ALPHA);
                if (constant_color(values[0]) && constant_alpha(values[1]))
                    || (constant_alpha(values[0]) && constant_color(values[1]))
                {
                    gl.error(INVALID_OPERATION);
                    return undefined;
                }
                gl.blend.source = [values[0], values[2]];
                gl.blend.destination = [values[1], values[3]];
                undefined
            }
            "checkFramebufferStatus" => {
                let target = self.gl_u32(arguments, 0)?;
                let gl = &mut context.borrow_mut().gl;
                let Some(id) = gl.framebuffer_binding(target) else {
                    return Ok(number(0));
                };
                Ok(number(gl.status(id)))
            }
            "clear" => {
                let mask = self.gl_u32(arguments, 0)?;
                gl!().clear(mask);
                undefined
            }
            "clearColor" => {
                let color = [
                    self.gl_f32(arguments, 0)?,
                    self.gl_f32(arguments, 1)?,
                    self.gl_f32(arguments, 2)?,
                    self.gl_f32(arguments, 3)?,
                ];
                gl!().clear_color = color;
                undefined
            }
            "clearDepth" => {
                let depth = self.gl_f32(arguments, 0)?;
                gl!().clear_depth = depth.clamp(0.0, 1.0);
                undefined
            }
            "clearStencil" => {
                let stencil = self.gl_i32(arguments, 0)?;
                gl!().clear_stencil = stencil;
                undefined
            }
            "clearBufferfv" | "clearBufferiv" | "clearBufferuiv" => {
                let buffer = self.gl_u32(arguments, 0)?;
                let draw_buffer = self.gl_i32(arguments, 1)?;
                let (kind, base) = match name {
                    "clearBufferfv" => (ElementKind::Float32, glsl::Base::Float),
                    "clearBufferiv" => (ElementKind::Int32, glsl::Base::Int),
                    _ => (ElementKind::Uint32, glsl::Base::Uint),
                };
                let offset = if arguments.len() > 3 {
                    self.gl_u32(arguments, 3)? as usize
                } else {
                    0
                };
                let Some(words) = self.gl_list(&arguments[2], kind, offset, 0, name, interface)?
                else {
                    gl!().error(INVALID_VALUE);
                    return undefined;
                };
                let needed = if buffer == COLOR { 4 } else { 1 };
                let gl = &mut context.borrow_mut().gl;
                if words.len() < needed {
                    gl.error(INVALID_VALUE);
                    return undefined;
                }
                let valid = match buffer {
                    COLOR => true,
                    DEPTH => base == glsl::Base::Float,
                    STENCIL => base == glsl::Base::Int,
                    _ => false,
                };
                if !valid {
                    gl.error(INVALID_ENUM);
                    return undefined;
                }
                let values: [f32; 4] = std::array::from_fn(|index| {
                    let word = words.get(index).copied().unwrap_or(0);
                    match base {
                        glsl::Base::Float => f32::from_bits(word),
                        glsl::Base::Int => word as i32 as f32,
                        _ => word as f32,
                    }
                });
                gl.clear_buffer(buffer, draw_buffer, values, base, None);
                undefined
            }
            "clearBufferfi" => {
                let buffer = self.gl_u32(arguments, 0)?;
                let draw_buffer = self.gl_i32(arguments, 1)?;
                let depth = self.gl_f32(arguments, 2)?;
                let stencil = self.gl_i32(arguments, 3)?;
                let gl = &mut context.borrow_mut().gl;
                if buffer != DEPTH_STENCIL {
                    gl.error(INVALID_ENUM);
                    return undefined;
                }
                gl.clear_buffer(
                    buffer,
                    draw_buffer,
                    [depth, 0.0, 0.0, 0.0],
                    glsl::Base::Float,
                    Some((depth, stencil)),
                );
                undefined
            }
            "colorMask" => {
                let mask = [
                    Self::gl_bool(arguments, 0),
                    Self::gl_bool(arguments, 1),
                    Self::gl_bool(arguments, 2),
                    Self::gl_bool(arguments, 3),
                ];
                gl!().color_mask = mask;
                undefined
            }
            "cullFace" => {
                let mode = self.gl_u32(arguments, 0)?;
                let gl = &mut context.borrow_mut().gl;
                if matches!(mode, FRONT | BACK | FRONT_AND_BACK) {
                    gl.cull_face = mode;
                } else {
                    gl.error(INVALID_ENUM);
                }
                undefined
            }
            "depthFunc" => {
                let func = self.gl_u32(arguments, 0)?;
                let gl = &mut context.borrow_mut().gl;
                if (NEVER..=ALWAYS).contains(&func) {
                    gl.depth_func = func;
                } else {
                    gl.error(INVALID_ENUM);
                }
                undefined
            }
            "depthMask" => {
                gl!().depth_mask = Self::gl_bool(arguments, 0);
                undefined
            }
            "depthRange" => {
                let (near, far) = (self.gl_f32(arguments, 0)?, self.gl_f32(arguments, 1)?);
                let gl = &mut context.borrow_mut().gl;
                if near > far {
                    gl.error(INVALID_OPERATION);
                } else {
                    gl.depth_range = [near.clamp(0.0, 1.0), far.clamp(0.0, 1.0)];
                }
                undefined
            }
            "enable" | "disable" | "isEnabled" => {
                let capability = self.gl_u32(arguments, 0)?;
                let gl = &mut context.borrow_mut().gl;
                let Some(found) = context::capability(capability, gl.webgl2) else {
                    gl.error(INVALID_ENUM);
                    return Ok(if name == "isEnabled" {
                        Value::Bool(false)
                    } else {
                        Value::Undefined
                    });
                };
                if name == "isEnabled" {
                    return Ok(Value::Bool(gl.enabled(found)));
                }
                gl.set_enabled(found, name == "enable");
                undefined
            }
            "frontFace" => {
                let mode = self.gl_u32(arguments, 0)?;
                let gl = &mut context.borrow_mut().gl;
                if matches!(mode, CW | CCW) {
                    gl.front_face = mode;
                } else {
                    gl.error(INVALID_ENUM);
                }
                undefined
            }
            "hint" => {
                let (target, mode) = (self.gl_u32(arguments, 0)?, self.gl_u32(arguments, 1)?);
                let gl = &mut context.borrow_mut().gl;
                let index = match target {
                    GENERATE_MIPMAP_HINT => 0,
                    FRAGMENT_SHADER_DERIVATIVE_HINT
                        if gl.webgl2 || gl.extensions.contains(&"OES_standard_derivatives") =>
                    {
                        1
                    }
                    _ => {
                        gl.error(INVALID_ENUM);
                        return undefined;
                    }
                };
                if matches!(mode, FASTEST | NICEST | DONT_CARE) {
                    gl.hints[index] = mode;
                } else {
                    gl.error(INVALID_ENUM);
                }
                undefined
            }
            "lineWidth" => {
                let width = self.gl_f32(arguments, 0)?;
                let gl = &mut context.borrow_mut().gl;
                if width <= 0.0 || width.is_nan() {
                    gl.error(INVALID_VALUE);
                } else {
                    gl.line_width = width;
                }
                undefined
            }
            "pixelStorei" => {
                let (parameter, value) = (self.gl_u32(arguments, 0)?, self.gl_i32(arguments, 1)?);
                let gl = &mut context.borrow_mut().gl;
                let webgl2 = gl.webgl2;
                let non_negative = |gl: &mut Context| {
                    if value < 0 {
                        gl.error(INVALID_VALUE);
                        None
                    } else {
                        Some(value as u32)
                    }
                };
                match parameter {
                    PACK_ALIGNMENT | UNPACK_ALIGNMENT => {
                        if !matches!(value, 1 | 2 | 4 | 8) {
                            gl.error(INVALID_VALUE);
                        } else if parameter == PACK_ALIGNMENT {
                            gl.pack.alignment = value as u32;
                        } else {
                            gl.unpack.alignment = value as u32;
                        }
                    }
                    UNPACK_FLIP_Y_WEBGL => gl.unpack_flip = value != 0,
                    UNPACK_PREMULTIPLY_ALPHA_WEBGL => gl.unpack_premultiply = value != 0,
                    UNPACK_COLORSPACE_CONVERSION_WEBGL => {
                        if matches!(value as u32, NONE | BROWSER_DEFAULT_WEBGL) {
                            gl.unpack_colorspace = value as u32;
                        } else {
                            gl.error(INVALID_ENUM);
                        }
                    }
                    PACK_ROW_LENGTH if webgl2 => {
                        gl.pack.row_length = non_negative(gl).unwrap_or(gl.pack.row_length)
                    }
                    PACK_SKIP_PIXELS if webgl2 => {
                        gl.pack.skip_pixels = non_negative(gl).unwrap_or(gl.pack.skip_pixels)
                    }
                    PACK_SKIP_ROWS if webgl2 => {
                        gl.pack.skip_rows = non_negative(gl).unwrap_or(gl.pack.skip_rows)
                    }
                    UNPACK_ROW_LENGTH if webgl2 => {
                        gl.unpack.row_length = non_negative(gl).unwrap_or(gl.unpack.row_length)
                    }
                    UNPACK_IMAGE_HEIGHT if webgl2 => {
                        gl.unpack.image_height = non_negative(gl).unwrap_or(gl.unpack.image_height)
                    }
                    UNPACK_SKIP_PIXELS if webgl2 => {
                        gl.unpack.skip_pixels = non_negative(gl).unwrap_or(gl.unpack.skip_pixels)
                    }
                    UNPACK_SKIP_ROWS if webgl2 => {
                        gl.unpack.skip_rows = non_negative(gl).unwrap_or(gl.unpack.skip_rows)
                    }
                    UNPACK_SKIP_IMAGES if webgl2 => {
                        gl.unpack.skip_images = non_negative(gl).unwrap_or(gl.unpack.skip_images)
                    }
                    _ => gl.error(INVALID_ENUM),
                }
                undefined
            }
            "polygonOffset" => {
                let offset = [self.gl_f32(arguments, 0)?, self.gl_f32(arguments, 1)?];
                gl!().polygon_offset = offset;
                undefined
            }
            "sampleCoverage" => {
                let value = self.gl_f32(arguments, 0)?;
                gl!().sample_coverage = (value.clamp(0.0, 1.0), Self::gl_bool(arguments, 1));
                undefined
            }
            "scissor" | "viewport" => {
                let rect = [
                    self.gl_i32(arguments, 0)?,
                    self.gl_i32(arguments, 1)?,
                    self.gl_i32(arguments, 2)?,
                    self.gl_i32(arguments, 3)?,
                ];
                let gl = &mut context.borrow_mut().gl;
                if rect[2] < 0 || rect[3] < 0 {
                    gl.error(INVALID_VALUE);
                } else if name == "scissor" {
                    gl.scissor = rect;
                } else {
                    let limit = context::MAX_VIEWPORT as i32;
                    gl.viewport = [rect[0], rect[1], rect[2].min(limit), rect[3].min(limit)];
                }
                undefined
            }
            "stencilFunc" | "stencilFuncSeparate" => {
                let separate = name == "stencilFuncSeparate";
                let face = if separate {
                    self.gl_u32(arguments, 0)?
                } else {
                    FRONT_AND_BACK
                };
                let start = usize::from(separate);
                let func = self.gl_u32(arguments, start)?;
                let reference = self.gl_i32(arguments, start + 1)?;
                let mask = self.gl_u32(arguments, start + 2)?;
                let gl = &mut context.borrow_mut().gl;
                if !matches!(face, FRONT | BACK | FRONT_AND_BACK)
                    || !(NEVER..=ALWAYS).contains(&func)
                {
                    gl.error(INVALID_ENUM);
                    return undefined;
                }
                for (index, side) in gl.stencil.iter_mut().enumerate() {
                    if face == FRONT_AND_BACK || (face == FRONT) == (index == 0) {
                        side.func = func;
                        side.reference = reference;
                        side.value_mask = mask;
                    }
                }
                undefined
            }
            "stencilMask" | "stencilMaskSeparate" => {
                let separate = name == "stencilMaskSeparate";
                let face = if separate {
                    self.gl_u32(arguments, 0)?
                } else {
                    FRONT_AND_BACK
                };
                let mask = self.gl_u32(arguments, usize::from(separate))?;
                let gl = &mut context.borrow_mut().gl;
                if !matches!(face, FRONT | BACK | FRONT_AND_BACK) {
                    gl.error(INVALID_ENUM);
                    return undefined;
                }
                for (index, side) in gl.stencil.iter_mut().enumerate() {
                    if face == FRONT_AND_BACK || (face == FRONT) == (index == 0) {
                        side.write_mask = mask;
                    }
                }
                undefined
            }
            "stencilOp" | "stencilOpSeparate" => {
                let separate = name == "stencilOpSeparate";
                let face = if separate {
                    self.gl_u32(arguments, 0)?
                } else {
                    FRONT_AND_BACK
                };
                let start = usize::from(separate);
                let ops = [
                    self.gl_u32(arguments, start)?,
                    self.gl_u32(arguments, start + 1)?,
                    self.gl_u32(arguments, start + 2)?,
                ];
                let gl = &mut context.borrow_mut().gl;
                let valid = |op: u32| {
                    matches!(
                        op,
                        KEEP | ZERO | REPLACE | INCR | DECR | INVERT | INCR_WRAP | DECR_WRAP
                    )
                };
                if !matches!(face, FRONT | BACK | FRONT_AND_BACK)
                    || !ops.iter().all(|&op| valid(op))
                {
                    gl.error(INVALID_ENUM);
                    return undefined;
                }
                for (index, side) in gl.stencil.iter_mut().enumerate() {
                    if face == FRONT_AND_BACK || (face == FRONT) == (index == 0) {
                        side.fail = ops[0];
                        side.depth_fail = ops[1];
                        side.pass = ops[2];
                    }
                }
                undefined
            }
            "finish"
            | "flush"
            | "invalidateFramebuffer"
            | "invalidateSubFramebuffer"
            | "waitSync" => undefined,
            "clientWaitSync" => {
                let id = object!(0, Kind::Sync, false);
                let flags = self.gl_u32(arguments, 1)?;
                let gl = &mut context.borrow_mut().gl;
                if flags & !SYNC_FLUSH_COMMANDS_BIT != 0 || !gl.syncs.contains_key(&id) {
                    gl.error(INVALID_VALUE);
                    return Ok(number(WAIT_FAILED));
                }
                Ok(number(ALREADY_SIGNALED))
            }
            "getSyncParameter" => {
                let id = object!(0, Kind::Sync, false);
                let parameter = self.gl_u32(arguments, 1)?;
                let gl = &mut context.borrow_mut().gl;
                if !gl.syncs.contains_key(&id) {
                    gl.error(INVALID_VALUE);
                    return Ok(Value::Null);
                }
                Ok(match parameter {
                    OBJECT_TYPE => number(SYNC_FENCE),
                    SYNC_STATUS => number(SIGNALED),
                    SYNC_CONDITION => number(SYNC_GPU_COMMANDS_COMPLETE),
                    SYNC_FLAGS => number(0),
                    _ => {
                        gl.error(INVALID_ENUM);
                        Value::Null
                    }
                })
            }
            "shaderSource" => {
                let id = object!(0, Kind::Shader, false);
                let source = self.coerce_string(arguments[1].clone())?;
                if let Some(shader) = gl!().shaders.get_mut(&id) {
                    shader.source = source;
                }
                undefined
            }
            "getShaderSource" => {
                let id = object!(0, Kind::Shader, false);
                Ok(context
                    .borrow()
                    .gl
                    .shaders
                    .get(&id)
                    .map_or(Value::Null, |shader| {
                        Value::String(shader.source.as_str().into())
                    }))
            }
            "compileShader" => {
                let id = object!(0, Kind::Shader, false);
                gl!().compile_shader(id);
                undefined
            }
            "getShaderParameter" => {
                let id = object!(0, Kind::Shader, false);
                let parameter = self.gl_u32(arguments, 1)?;
                let gl = &mut context.borrow_mut().gl;
                let Some(shader) = gl.shaders.get(&id) else {
                    gl.error(INVALID_VALUE);
                    return Ok(Value::Null);
                };
                Ok(match parameter {
                    SHADER_TYPE => number(shader.kind),
                    DELETE_STATUS => Value::Bool(shader.delete_pending),
                    COMPILE_STATUS => Value::Bool(shader.compiled),
                    COMPLETION_STATUS_KHR
                        if gl.extensions.contains(&"KHR_parallel_shader_compile") =>
                    {
                        Value::Bool(true)
                    }
                    _ => {
                        gl.error(INVALID_ENUM);
                        Value::Null
                    }
                })
            }
            "getShaderInfoLog" => {
                let id = object!(0, Kind::Shader, false);
                Ok(context
                    .borrow()
                    .gl
                    .shaders
                    .get(&id)
                    .map_or(Value::Null, |shader| {
                        Value::String(shader.log.as_str().into())
                    }))
            }
            "getShaderPrecisionFormat" => {
                let (shader, precision) = (self.gl_u32(arguments, 0)?, self.gl_u32(arguments, 1)?);
                let found = Self::gl_shader_precision(precision)
                    .filter(|_| matches!(shader, VERTEX_SHADER | FRAGMENT_SHADER));
                let Some(values) = found else {
                    gl!().error(INVALID_ENUM);
                    return Ok(Value::Null);
                };
                Ok(Value::Object(self.webgl_object(
                    "WebGLShaderPrecisionFormat",
                    GlObject::Precision(values),
                )))
            }
            "attachShader" | "detachShader" => {
                let program = object!(0, Kind::Program, false);
                let shader = object!(1, Kind::Shader, false);
                let gl = &mut context.borrow_mut().gl;
                let Some(kind) = gl.shaders.get(&shader).map(|found| found.kind) else {
                    gl.error(INVALID_OPERATION);
                    return undefined;
                };
                let Some(found) = gl.programs.get(&program) else {
                    gl.error(INVALID_OPERATION);
                    return undefined;
                };
                let attached = found.shaders.contains(&shader);
                if name == "attachShader" {
                    let same_kind = found.shaders.iter().any(|other| {
                        gl.shaders
                            .get(other)
                            .is_some_and(|other| other.kind == kind)
                    });
                    if attached || same_kind {
                        gl.error(INVALID_OPERATION);
                        return undefined;
                    }
                    gl.programs
                        .get_mut(&program)
                        .expect("the program exists")
                        .shaders
                        .push(shader);
                    gl.shaders
                        .get_mut(&shader)
                        .expect("the shader exists")
                        .attached += 1;
                } else {
                    if !attached {
                        gl.error(INVALID_OPERATION);
                        return undefined;
                    }
                    gl.programs
                        .get_mut(&program)
                        .expect("the program exists")
                        .shaders
                        .retain(|found| *found != shader);
                    detach_shader(gl, shader);
                }
                undefined
            }
            "getAttachedShaders" => {
                let program = object!(0, Kind::Program, false);
                let shaders = context
                    .borrow()
                    .gl
                    .programs
                    .get(&program)
                    .map(|found| found.shaders.clone());
                let Some(shaders) = shaders else {
                    return Ok(Value::Null);
                };
                let values = shaders
                    .into_iter()
                    .map(|id| self.wrap_gl(context, id, Kind::Shader))
                    .collect();
                Ok(Self::new_array(values))
            }
            "bindAttribLocation" => {
                let program = object!(0, Kind::Program, false);
                let index = self.gl_u32(arguments, 1)?;
                let attribute = self.coerce_string(arguments[2].clone())?;
                let gl = &mut context.borrow_mut().gl;
                if index as usize >= context::VERTEX_ATTRIBS {
                    gl.error(INVALID_VALUE);
                } else if attribute.starts_with("gl_")
                    || attribute.starts_with("webgl_")
                    || attribute.starts_with("_webgl_")
                {
                    gl.error(INVALID_OPERATION);
                } else if let Some(found) = gl.programs.get_mut(&program) {
                    found.bindings.insert(attribute, index);
                }
                undefined
            }
            "linkProgram" => {
                let program = object!(0, Kind::Program, false);
                let gl = &mut context.borrow_mut().gl;
                if gl.feedbacks[&gl.feedback].active
                    && gl.feedbacks[&gl.feedback].program == program
                {
                    gl.error(INVALID_OPERATION);
                    return undefined;
                }
                gl.link_program(program);
                undefined
            }
            "useProgram" => {
                let program = object!(0, Kind::Program, true);
                let gl = &mut context.borrow_mut().gl;
                if program != 0
                    && !gl
                        .programs
                        .get(&program)
                        .is_some_and(|found| found.link_status)
                {
                    gl.error(INVALID_OPERATION);
                    return undefined;
                }
                if gl.feedbacks[&gl.feedback].active && !gl.feedbacks[&gl.feedback].paused {
                    gl.error(INVALID_OPERATION);
                    return undefined;
                }
                let previous = gl.program;
                gl.program = program;
                if previous != program
                    && gl
                        .programs
                        .get(&previous)
                        .is_some_and(|found| found.delete_pending)
                {
                    let removed = gl.programs.remove(&previous).expect("the program exists");
                    for shader in removed.shaders {
                        detach_shader(gl, shader);
                    }
                }
                undefined
            }
            "validateProgram" => {
                let program = object!(0, Kind::Program, false);
                if let Some(found) = gl!().programs.get_mut(&program) {
                    found.validate_status = found.link_status;
                }
                undefined
            }
            "getProgramParameter" => {
                let program = object!(0, Kind::Program, false);
                let parameter = self.gl_u32(arguments, 1)?;
                let gl = &mut context.borrow_mut().gl;
                let webgl2 = gl.webgl2;
                let Some(found) = gl.programs.get(&program) else {
                    gl.error(INVALID_VALUE);
                    return Ok(Value::Null);
                };
                let linked = found.linked.as_ref().filter(|_| found.link_status);
                Ok(match parameter {
                    DELETE_STATUS => Value::Bool(found.delete_pending),
                    LINK_STATUS => Value::Bool(found.link_status),
                    VALIDATE_STATUS => Value::Bool(found.validate_status),
                    ATTACHED_SHADERS => number(found.shaders.len() as u32),
                    ACTIVE_ATTRIBUTES => {
                        number(linked.map_or(0, |linked| linked.attributes.len() as u32))
                    }
                    ACTIVE_UNIFORMS => {
                        number(linked.map_or(0, |linked| linked.uniforms.len() as u32))
                    }
                    ACTIVE_UNIFORM_BLOCKS if webgl2 => {
                        number(linked.map_or(0, |linked| linked.blocks.len() as u32))
                    }
                    TRANSFORM_FEEDBACK_BUFFER_MODE if webgl2 => number(found.feedback_mode),
                    TRANSFORM_FEEDBACK_VARYINGS if webgl2 => {
                        number(linked.map_or(0, |linked| linked.feedback.len() as u32))
                    }
                    COMPLETION_STATUS_KHR
                        if gl.extensions.contains(&"KHR_parallel_shader_compile") =>
                    {
                        Value::Bool(true)
                    }
                    _ => {
                        gl.error(INVALID_ENUM);
                        Value::Null
                    }
                })
            }
            "getProgramInfoLog" => {
                let program = object!(0, Kind::Program, false);
                Ok(context
                    .borrow()
                    .gl
                    .programs
                    .get(&program)
                    .map_or(Value::Null, |found| {
                        Value::String(found.log.as_str().into())
                    }))
            }
            "getActiveAttrib" | "getActiveUniform" | "getTransformFeedbackVarying" => {
                let program = object!(0, Kind::Program, false);
                let index = self.gl_u32(arguments, 1)? as usize;
                let info = {
                    let gl = &mut context.borrow_mut().gl;
                    let linked = gl
                        .programs
                        .get(&program)
                        .and_then(|found| found.linked.as_ref());
                    let info = linked.and_then(|linked| match name {
                        "getActiveAttrib" => linked.attributes.get(index).map(|attribute| {
                            (attribute.name.clone(), 1, context::type_enum(&attribute.ty))
                        }),
                        "getActiveUniform" => linked.uniforms.get(index).map(|uniform| {
                            (
                                uniform.name.clone(),
                                uniform.size as i32,
                                context::type_enum(&uniform.ty),
                            )
                        }),
                        _ => linked
                            .feedback
                            .get(index)
                            .map(|(varying, ty, ..)| match ty {
                                glsl::Ty::Array(element, length) => {
                                    (varying.clone(), *length as i32, context::type_enum(element))
                                }
                                _ => (varying.clone(), 1, context::type_enum(ty)),
                            }),
                    });
                    if info.is_none() {
                        gl.error(INVALID_VALUE);
                    }
                    info
                };
                Ok(match info {
                    Some((info_name, size, kind)) => Value::Object(self.webgl_object(
                        "WebGLActiveInfo",
                        GlObject::Info {
                            name: info_name,
                            size,
                            kind,
                        },
                    )),
                    None => Value::Null,
                })
            }
            "getAttribLocation" => {
                let program = object!(0, Kind::Program, false);
                let attribute = self.coerce_string(arguments[1].clone())?;
                let gl = &mut context.borrow_mut().gl;
                let Some(found) = gl.programs.get(&program) else {
                    gl.error(INVALID_OPERATION);
                    return Ok(number(-1));
                };
                let Some(linked) = found.linked.as_ref().filter(|_| found.link_status) else {
                    gl.error(INVALID_OPERATION);
                    return Ok(number(-1));
                };
                Ok(number(
                    linked
                        .attributes
                        .iter()
                        .find(|found| found.name == attribute)
                        .map_or(-1, |found| found.location as i32),
                ))
            }
            "getFragDataLocation" => {
                let program = object!(0, Kind::Program, false);
                let output = self.coerce_string(arguments[1].clone())?;
                let gl = &mut context.borrow_mut().gl;
                let Some(linked) = gl
                    .programs
                    .get(&program)
                    .and_then(|found| found.linked.as_ref().filter(|_| found.link_status))
                else {
                    gl.error(INVALID_OPERATION);
                    return Ok(number(-1));
                };
                Ok(number(
                    linked
                        .fragment_locations
                        .get(&output)
                        .copied()
                        .unwrap_or(-1),
                ))
            }
            "getUniformLocation" => {
                let program = object!(0, Kind::Program, false);
                let uniform = self.coerce_string(arguments[1].clone())?;
                let found = {
                    let gl = &mut context.borrow_mut().gl;
                    let Some(found) = gl.programs.get(&program) else {
                        gl.error(INVALID_OPERATION);
                        return Ok(Value::Null);
                    };
                    let generation = found.generation;
                    let Some(linked) = found.linked.as_ref().filter(|_| found.link_status) else {
                        gl.error(INVALID_OPERATION);
                        return Ok(Value::Null);
                    };
                    // "name", "name[0]" or "name[3]" for an array
                    let (base, element) = match uniform
                        .strip_suffix(']')
                        .and_then(|rest| rest.rsplit_once('['))
                    {
                        Some((base, digits))
                            if !digits.is_empty()
                                && digits.bytes().all(|byte| byte.is_ascii_digit()) =>
                        {
                            (base.to_string(), digits.parse::<usize>().ok())
                        }
                        _ => (uniform.clone(), Some(0)),
                    };
                    let index = linked.uniforms.iter().position(|found| {
                        found.base_name == base
                            && found.block.is_none()
                            && (found.array || uniform == base)
                    });
                    index.and_then(|index| {
                        let element = element?;
                        (element < linked.uniforms[index].size).then_some(())?;
                        linked
                            .locations
                            .iter()
                            .position(|&location| location == (index, element))
                            .map(|location| (generation, location))
                    })
                };
                Ok(match found {
                    Some((generation, index)) => Value::Object(self.webgl_object(
                        "WebGLUniformLocation",
                        GlObject::Location {
                            context: Rc::downgrade(context),
                            program,
                            generation,
                            index: index as u32,
                        },
                    )),
                    None => Value::Null,
                })
            }
            "getUniform" => {
                let program = object!(0, Kind::Program, false);
                let location = self.gl_location(context, &arguments[1], name, interface)?;
                let Some(Some((owner, generation, index))) = location else {
                    gl!().error(INVALID_OPERATION);
                    return Ok(Value::Null);
                };
                let found = context
                    .borrow()
                    .gl
                    .uniform_words(owner, generation, index)
                    .filter(|_| owner == program);
                let Some((ty, words)) = found else {
                    gl!().error(INVALID_OPERATION);
                    return Ok(Value::Null);
                };
                Ok(match ty {
                    glsl::Ty::Vec(glsl::Base::Float, 1) => number(f32::from_bits(words[0])),
                    glsl::Ty::Vec(glsl::Base::Int, 1) | glsl::Ty::Sampler(..) => {
                        number(words[0] as i32)
                    }
                    glsl::Ty::Vec(glsl::Base::Uint, 1) => number(words[0]),
                    glsl::Ty::Vec(glsl::Base::Bool, 1) => Value::Bool(words[0] != 0),
                    glsl::Ty::Vec(glsl::Base::Float, _) | glsl::Ty::Mat(..) => float_array(
                        &words
                            .iter()
                            .map(|word| f32::from_bits(*word))
                            .collect::<Vec<_>>(),
                    ),
                    glsl::Ty::Vec(glsl::Base::Int, _) => {
                        int_array(&words.iter().map(|word| *word as i32).collect::<Vec<_>>())
                    }
                    glsl::Ty::Vec(glsl::Base::Uint, _) => uint_array(&words),
                    glsl::Ty::Vec(glsl::Base::Bool, _) => {
                        Self::new_array(words.iter().map(|word| Value::Bool(*word != 0)).collect())
                    }
                    _ => Value::Null,
                })
            }
            _ if name.starts_with("uniform") && name != "uniformBlockBinding" => {
                self.gl_uniform(context, name, interface, arguments)
            }
            "vertexAttrib1f" | "vertexAttrib2f" | "vertexAttrib3f" | "vertexAttrib4f"
            | "vertexAttrib1fv" | "vertexAttrib2fv" | "vertexAttrib3fv" | "vertexAttrib4fv"
            | "vertexAttribI4i" | "vertexAttribI4ui" | "vertexAttribI4iv" | "vertexAttribI4uiv" => {
                let index = self.gl_u32(arguments, 0)?;
                let (kind, base) = if name.contains("I4ui") {
                    (ElementKind::Uint32, glsl::Base::Uint)
                } else if name.contains("I4i") {
                    (ElementKind::Int32, glsl::Base::Int)
                } else {
                    (ElementKind::Float32, glsl::Base::Float)
                };
                let count = if name.contains("I4") {
                    4
                } else {
                    usize::from(name.as_bytes()[12] - b'0')
                };
                let words = if name.ends_with('v') {
                    let Some(words) = self.gl_list(&arguments[1], kind, 0, 0, name, interface)?
                    else {
                        return undefined;
                    };
                    if words.len() < count {
                        gl!().error(INVALID_VALUE);
                        return undefined;
                    }
                    words[..count].to_vec()
                } else {
                    let mut words = Vec::with_capacity(count);
                    for position in 0..count {
                        words.push(match base {
                            glsl::Base::Float => self.gl_f32(arguments, position + 1)?.to_bits(),
                            glsl::Base::Int => self.gl_i32(arguments, position + 1)? as u32,
                            _ => self.gl_u32(arguments, position + 1)?,
                        });
                    }
                    words
                };
                let gl = &mut context.borrow_mut().gl;
                if index as usize >= context::VERTEX_ATTRIBS {
                    gl.error(INVALID_VALUE);
                    return undefined;
                }
                let mut value = [
                    0,
                    0,
                    0,
                    if base == glsl::Base::Float {
                        1.0_f32.to_bits()
                    } else {
                        1
                    },
                ];
                value[..count].copy_from_slice(&words);
                gl.generic[index as usize] = (value, base);
                undefined
            }
            "enableVertexAttribArray" | "disableVertexAttribArray" => {
                let index = self.gl_u32(arguments, 0)?;
                let gl = &mut context.borrow_mut().gl;
                if index as usize >= context::VERTEX_ATTRIBS {
                    gl.error(INVALID_VALUE);
                } else {
                    gl.current_vertex_array_mut().attribs[index as usize].enabled =
                        name == "enableVertexAttribArray";
                }
                undefined
            }
            "vertexAttribPointer" | "vertexAttribIPointer" => {
                let integer = name == "vertexAttribIPointer";
                let index = self.gl_u32(arguments, 0)?;
                let size = self.gl_i32(arguments, 1)?;
                let kind = self.gl_u32(arguments, 2)?;
                let (normalized, rest) = if integer {
                    (false, 3)
                } else {
                    (Self::gl_bool(arguments, 3), 4)
                };
                let stride = self.gl_i32(arguments, rest)?;
                let offset = self.gl_i64(arguments, rest + 1)?;
                let gl = &mut context.borrow_mut().gl;
                let float_kinds = matches!(kind, FLOAT)
                    || gl.webgl2
                        && matches!(
                            kind,
                            HALF_FLOAT | INT_2_10_10_10_REV | UNSIGNED_INT_2_10_10_10_REV
                        );
                let integer_kinds = matches!(kind, BYTE | UNSIGNED_BYTE | SHORT | UNSIGNED_SHORT)
                    || gl.webgl2 && matches!(kind, INT | UNSIGNED_INT);
                if !(integer_kinds || float_kinds && !integer) {
                    gl.error(INVALID_ENUM);
                    return undefined;
                }
                if index as usize >= context::VERTEX_ATTRIBS
                    || !(1..=4).contains(&size)
                    || !(0..=255).contains(&stride)
                    || offset < 0
                {
                    gl.error(INVALID_VALUE);
                    return undefined;
                }
                if matches!(kind, INT_2_10_10_10_REV | UNSIGNED_INT_2_10_10_10_REV) && size != 4 {
                    gl.error(INVALID_OPERATION);
                    return undefined;
                }
                let attrib = AttribArray {
                    enabled: false,
                    size,
                    kind,
                    normalized,
                    integer,
                    stride,
                    offset: offset as u32,
                    buffer: gl.array_buffer,
                    divisor: 0,
                };
                let component = attrib.element_size() / size as usize;
                let component = if matches!(kind, INT_2_10_10_10_REV | UNSIGNED_INT_2_10_10_10_REV)
                {
                    4
                } else {
                    component
                };
                if !(offset as usize).is_multiple_of(component)
                    || !(stride as usize).is_multiple_of(component)
                {
                    gl.error(INVALID_OPERATION);
                    return undefined;
                }
                if gl.array_buffer == 0 && offset != 0 {
                    gl.error(INVALID_OPERATION);
                    return undefined;
                }
                let slot = &mut gl.current_vertex_array_mut().attribs[index as usize];
                *slot = AttribArray {
                    enabled: slot.enabled,
                    divisor: slot.divisor,
                    ..attrib
                };
                undefined
            }
            "vertexAttribDivisor" => {
                let index = self.gl_u32(arguments, 0)?;
                let divisor = self.gl_u32(arguments, 1)?;
                let gl = &mut context.borrow_mut().gl;
                if index as usize >= context::VERTEX_ATTRIBS {
                    gl.error(INVALID_VALUE);
                } else {
                    gl.current_vertex_array_mut().attribs[index as usize].divisor = divisor;
                }
                undefined
            }
            "getVertexAttrib" => {
                let index = self.gl_u32(arguments, 0)?;
                let parameter = self.gl_u32(arguments, 1)?;
                let (attrib, generic, webgl2) = {
                    let gl = &mut context.borrow_mut().gl;
                    if index as usize >= context::VERTEX_ATTRIBS {
                        gl.error(INVALID_VALUE);
                        return Ok(Value::Null);
                    }
                    (
                        gl.current_vertex_array().attribs[index as usize],
                        gl.generic[index as usize],
                        gl.webgl2,
                    )
                };
                let instanced = webgl2
                    || context
                        .borrow()
                        .gl
                        .extensions
                        .contains(&"ANGLE_instanced_arrays");
                Ok(match parameter {
                    VERTEX_ATTRIB_ARRAY_BUFFER_BINDING => {
                        self.wrap_gl(context, attrib.buffer, Kind::Buffer)
                    }
                    VERTEX_ATTRIB_ARRAY_ENABLED => Value::Bool(attrib.enabled),
                    VERTEX_ATTRIB_ARRAY_SIZE => number(attrib.size),
                    VERTEX_ATTRIB_ARRAY_STRIDE => number(attrib.stride),
                    VERTEX_ATTRIB_ARRAY_TYPE => number(attrib.kind),
                    VERTEX_ATTRIB_ARRAY_NORMALIZED => Value::Bool(attrib.normalized),
                    VERTEX_ATTRIB_ARRAY_INTEGER if webgl2 => Value::Bool(attrib.integer),
                    VERTEX_ATTRIB_ARRAY_DIVISOR if instanced => number(attrib.divisor),
                    CURRENT_VERTEX_ATTRIB => match generic.1 {
                        glsl::Base::Int => int_array(&generic.0.map(|word| word as i32)),
                        glsl::Base::Uint => uint_array(&generic.0),
                        _ => float_array(&generic.0.map(f32::from_bits)),
                    },
                    _ => {
                        gl!().error(INVALID_ENUM);
                        Value::Null
                    }
                })
            }
            "getVertexAttribOffset" => {
                let index = self.gl_u32(arguments, 0)?;
                let parameter = self.gl_u32(arguments, 1)?;
                let gl = &mut context.borrow_mut().gl;
                if index as usize >= context::VERTEX_ATTRIBS {
                    gl.error(INVALID_VALUE);
                    return Ok(number(0));
                }
                if parameter != VERTEX_ATTRIB_ARRAY_POINTER {
                    gl.error(INVALID_ENUM);
                    return Ok(number(0));
                }
                Ok(number(
                    gl.current_vertex_array().attribs[index as usize].offset,
                ))
            }
            "drawArrays" | "drawArraysInstanced" => {
                let mode = self.gl_u32(arguments, 0)?;
                let first = self.gl_i32(arguments, 1)?;
                let count = self.gl_i32(arguments, 2)?;
                let instances = if name == "drawArraysInstanced" {
                    self.gl_i32(arguments, 3)?
                } else {
                    1
                };
                gl!().draw(
                    mode,
                    i64::from(first),
                    i64::from(count),
                    None,
                    i64::from(instances),
                );
                undefined
            }
            "drawElements" | "drawElementsInstanced" | "drawRangeElements" => {
                let mode = self.gl_u32(arguments, 0)?;
                let shift = if name == "drawRangeElements" { 2 } else { 0 };
                if name == "drawRangeElements" {
                    let (start, end) = (self.gl_u32(arguments, 1)?, self.gl_u32(arguments, 2)?);
                    if end < start {
                        gl!().error(INVALID_VALUE);
                        return undefined;
                    }
                }
                let count = self.gl_i32(arguments, 1 + shift)?;
                let kind = self.gl_u32(arguments, 2 + shift)?;
                let offset = self.gl_i64(arguments, 3 + shift)?;
                let instances = if name == "drawElementsInstanced" {
                    self.gl_i32(arguments, 4)?
                } else {
                    1
                };
                gl!().draw(
                    mode,
                    0,
                    i64::from(count),
                    Some((kind, offset)),
                    i64::from(instances),
                );
                undefined
            }
            "drawBuffers" => {
                let values = self.iterate_to_vec(arguments[0].clone())?;
                let mut buffers = Vec::with_capacity(values.len());
                for value in values {
                    buffers.push(self.coerce_uint32(value)?);
                }
                let gl = &mut context.borrow_mut().gl;
                if gl.draw_framebuffer == 0 {
                    if buffers.len() != 1 || !matches!(buffers[0], BACK | NONE) {
                        gl.error(INVALID_OPERATION);
                    } else {
                        gl.default_draw_buffer = buffers[0];
                    }
                    return undefined;
                }
                if buffers.len() > context::COLOR_ATTACHMENTS {
                    gl.error(INVALID_VALUE);
                    return undefined;
                }
                let valid = buffers.iter().enumerate().all(|(index, &buffer)| {
                    buffer == NONE || buffer == COLOR_ATTACHMENT0 + index as u32
                });
                if !valid {
                    gl.error(INVALID_OPERATION);
                    return undefined;
                }
                let id = gl.draw_framebuffer;
                gl.framebuffers.get_mut(&id).expect("bound").draw_buffers = buffers;
                undefined
            }
            "readBuffer" => {
                let source = self.gl_u32(arguments, 0)?;
                let gl = &mut context.borrow_mut().gl;
                let id = gl.read_framebuffer;
                if id == 0 {
                    if matches!(source, BACK | NONE) {
                        gl.default_read_buffer = source;
                    } else {
                        gl.error(INVALID_OPERATION);
                    }
                } else if source == NONE
                    || (COLOR_ATTACHMENT0..COLOR_ATTACHMENT0 + context::COLOR_ATTACHMENTS as u32)
                        .contains(&source)
                {
                    gl.framebuffers.get_mut(&id).expect("bound").read_buffer = source;
                } else if source == BACK {
                    gl.error(INVALID_OPERATION);
                } else {
                    gl.error(INVALID_ENUM);
                }
                undefined
            }
            "framebufferRenderbuffer" => {
                let target = self.gl_u32(arguments, 0)?;
                let point = self.gl_u32(arguments, 1)?;
                let renderbuffer_target = self.gl_u32(arguments, 2)?;
                let id = object!(3, Kind::Renderbuffer, true);
                let gl = &mut context.borrow_mut().gl;
                if renderbuffer_target != RENDERBUFFER {
                    gl.error(INVALID_ENUM);
                    return undefined;
                }
                gl.attach(
                    target,
                    point,
                    (id != 0).then_some(Attachment::Renderbuffer(id)),
                );
                undefined
            }
            "framebufferTexture2D" | "framebufferTextureLayer" => {
                let target = self.gl_u32(arguments, 0)?;
                let point = self.gl_u32(arguments, 1)?;
                let layered = name == "framebufferTextureLayer";
                let texture_target = if layered {
                    0
                } else {
                    self.gl_u32(arguments, 2)?
                };
                let id = object!(if layered { 2 } else { 3 }, Kind::Texture, true);
                let level = self.gl_i32(arguments, if layered { 3 } else { 4 })?;
                let layer = if layered {
                    self.gl_i32(arguments, 4)?
                } else {
                    0
                };
                let gl = &mut context.borrow_mut().gl;
                let face = match texture_target {
                    _ if layered => 0,
                    TEXTURE_2D => 0,
                    TEXTURE_CUBE_MAP_POSITIVE_X..=TEXTURE_CUBE_MAP_NEGATIVE_Z => {
                        (texture_target - TEXTURE_CUBE_MAP_POSITIVE_X) as usize
                    }
                    _ => {
                        gl.error(INVALID_ENUM);
                        return undefined;
                    }
                };
                if id != 0 {
                    let Some(texture) = gl.textures.get(&id) else {
                        gl.error(INVALID_OPERATION);
                        return undefined;
                    };
                    let fits = if layered {
                        matches!(texture.target, TEXTURE_3D | TEXTURE_2D_ARRAY)
                    } else {
                        texture.target
                            == if face > 0 || texture_target != TEXTURE_2D {
                                TEXTURE_CUBE_MAP
                            } else {
                                TEXTURE_2D
                            }
                    };
                    if !fits {
                        gl.error(INVALID_OPERATION);
                        return undefined;
                    }
                    if level < 0 || layer < 0 || (!gl.webgl2 && level != 0) {
                        gl.error(INVALID_VALUE);
                        return undefined;
                    }
                }
                gl.attach(
                    target,
                    point,
                    (id != 0).then_some(Attachment::Texture {
                        id,
                        face,
                        level: level as u32,
                        layer: layer as u32,
                    }),
                );
                undefined
            }
            "renderbufferStorage" | "renderbufferStorageMultisample" => {
                let target = self.gl_u32(arguments, 0)?;
                let multisample = name == "renderbufferStorageMultisample";
                let samples = if multisample {
                    self.gl_i32(arguments, 1)?
                } else {
                    0
                };
                let shift = usize::from(multisample);
                let internal = self.gl_u32(arguments, 1 + shift)?;
                let width = self.gl_i32(arguments, 2 + shift)?;
                let height = self.gl_i32(arguments, 3 + shift)?;
                let gl = &mut context.borrow_mut().gl;
                if target != RENDERBUFFER {
                    gl.error(INVALID_ENUM);
                    return undefined;
                }
                let id = gl.renderbuffer;
                if id == 0 {
                    gl.error(INVALID_OPERATION);
                    return undefined;
                }
                let Some(format) = Format::renderbuffer(internal, gl.webgl2).filter(|format| {
                    !format.is_color()
                        || gl.color_renderable(*format)
                        || matches!(internal, RGBA4 | RGB565 | RGB5_A1)
                }) else {
                    gl.error(INVALID_ENUM);
                    return undefined;
                };
                if width < 0
                    || height < 0
                    || width as u32 > context::MAX_RENDERBUFFER
                    || height as u32 > context::MAX_RENDERBUFFER
                    || !(0..=4).contains(&samples)
                {
                    gl.error(if samples > 4 {
                        INVALID_OPERATION
                    } else {
                        INVALID_VALUE
                    });
                    return undefined;
                }
                if samples > 0 && format.is_integer() {
                    gl.error(INVALID_OPERATION);
                    return undefined;
                }
                let mut level = Level::new(format, width as u32, height as u32, 1);
                if format.has_depth() || format.has_stencil() {
                    level.texels.fill([1.0, 0.0, 0.0, 1.0]);
                }
                let renderbuffer = gl.renderbuffers.get_mut(&id).expect("bound");
                renderbuffer.level = Some(level);
                renderbuffer.internal = internal;
                renderbuffer.samples = samples as u32;
                undefined
            }
            "getRenderbufferParameter" => {
                let target = self.gl_u32(arguments, 0)?;
                let parameter = self.gl_u32(arguments, 1)?;
                let gl = &mut context.borrow_mut().gl;
                if target != RENDERBUFFER {
                    gl.error(INVALID_ENUM);
                    return Ok(Value::Null);
                }
                let Some(renderbuffer) = gl.renderbuffers.get(&gl.renderbuffer) else {
                    gl.error(INVALID_OPERATION);
                    return Ok(Value::Null);
                };
                let format = renderbuffer
                    .level
                    .as_ref()
                    .map(|level| (level.format, level.width, level.height));
                let bits =
                    |channel: usize| format.map_or(0, |(format, ..)| format.channel_bits(channel));
                Ok(match parameter {
                    RENDERBUFFER_WIDTH => number(format.map_or(0, |found| found.1)),
                    RENDERBUFFER_HEIGHT => number(format.map_or(0, |found| found.2)),
                    RENDERBUFFER_INTERNAL_FORMAT => {
                        number(if renderbuffer.internal == DEPTH_STENCIL && gl.webgl2 {
                            DEPTH24_STENCIL8
                        } else {
                            renderbuffer.internal
                        })
                    }
                    RENDERBUFFER_RED_SIZE => number(bits(0)),
                    RENDERBUFFER_GREEN_SIZE => number(bits(1)),
                    RENDERBUFFER_BLUE_SIZE => number(bits(2)),
                    RENDERBUFFER_ALPHA_SIZE => number(bits(3)),
                    RENDERBUFFER_DEPTH_SIZE => number(
                        format
                            .filter(|found| found.0.has_depth())
                            .map_or(0, |found| u32::from(found.0.bits[0])),
                    ),
                    RENDERBUFFER_STENCIL_SIZE => number(
                        format
                            .filter(|found| found.0.has_stencil())
                            .map_or(0, |_| 8),
                    ),
                    RENDERBUFFER_SAMPLES if gl.webgl2 => number(renderbuffer.samples),
                    _ => {
                        gl.error(INVALID_ENUM);
                        Value::Null
                    }
                })
            }
            "getFramebufferAttachmentParameter" => self.gl_attachment_parameter(context, arguments),
            "texParameterf" | "texParameteri" | "samplerParameterf" | "samplerParameteri" => {
                let sampler = name.starts_with("sampler");
                let target = if sampler {
                    object!(0, Kind::Sampler, false)
                } else {
                    self.gl_u32(arguments, 0)?
                };
                let parameter = self.gl_u32(arguments, 1)?;
                let value = if name.ends_with('f') {
                    self.gl_f32(arguments, 2)?
                } else {
                    self.gl_i32(arguments, 2)? as f32
                };
                let gl = &mut context.borrow_mut().gl;
                if sampler {
                    let Some(mut params) = gl.samplers.get(&target).map(|found| found.params)
                    else {
                        gl.error(INVALID_OPERATION);
                        return undefined;
                    };
                    if gl.set_param(&mut params, parameter, value, true) {
                        gl.samplers
                            .get_mut(&target)
                            .expect("the sampler exists")
                            .params = params;
                    }
                    return undefined;
                }
                let Some(id) = gl.texture_params_target(target) else {
                    return undefined;
                };
                let webgl2 = gl.webgl2;
                let anisotropic = gl.extensions.contains(&"EXT_texture_filter_anisotropic");
                let texture = gl.textures.get_mut(&id).expect("bound");
                match parameter {
                    TEXTURE_BASE_LEVEL | TEXTURE_MAX_LEVEL if webgl2 => {
                        if value < 0.0 {
                            gl.error(INVALID_VALUE);
                        } else if parameter == TEXTURE_BASE_LEVEL {
                            texture.base_level = value as u32;
                        } else {
                            texture.max_level = value as u32;
                        }
                    }
                    TEXTURE_MAX_ANISOTROPY_EXT if anisotropic => {
                        if value < 1.0 {
                            gl.error(INVALID_VALUE);
                        } else {
                            texture.anisotropy = value.min(16.0);
                        }
                    }
                    _ => {
                        let mut params = texture.params;
                        if gl.set_param(&mut params, parameter, value, false) {
                            gl.textures.get_mut(&id).expect("bound").params = params;
                        }
                    }
                }
                undefined
            }
            "getTexParameter" | "getSamplerParameter" => {
                let sampler = name == "getSamplerParameter";
                let target = if sampler {
                    object!(0, Kind::Sampler, false)
                } else {
                    self.gl_u32(arguments, 0)?
                };
                let parameter = self.gl_u32(arguments, 1)?;
                let gl = &mut context.borrow_mut().gl;
                let webgl2 = gl.webgl2;
                let (params, texture) = if sampler {
                    match gl.samplers.get(&target) {
                        Some(found) => (found.params, None),
                        None => {
                            gl.error(INVALID_OPERATION);
                            return Ok(Value::Null);
                        }
                    }
                } else {
                    let Some(id) = gl.texture_params_target(target) else {
                        return Ok(Value::Null);
                    };
                    let texture = &gl.textures[&id];
                    (
                        texture.params,
                        Some((
                            texture.base_level,
                            texture.max_level,
                            texture.immutable_levels,
                            texture.anisotropy,
                        )),
                    )
                };
                let anisotropic = gl.extensions.contains(&"EXT_texture_filter_anisotropic");
                Ok(match (parameter, texture) {
                    (TEXTURE_MIN_FILTER, _) => number(params.min_filter),
                    (TEXTURE_MAG_FILTER, _) => number(params.mag_filter),
                    (TEXTURE_WRAP_S, _) => number(params.wrap[0]),
                    (TEXTURE_WRAP_T, _) => number(params.wrap[1]),
                    (TEXTURE_WRAP_R, _) if webgl2 => number(params.wrap[2]),
                    (TEXTURE_MIN_LOD, _) if webgl2 => number(params.min_lod),
                    (TEXTURE_MAX_LOD, _) if webgl2 => number(params.max_lod),
                    (TEXTURE_COMPARE_MODE, _) if webgl2 => number(params.compare_mode),
                    (TEXTURE_COMPARE_FUNC, _) if webgl2 => number(params.compare_func),
                    (TEXTURE_BASE_LEVEL, Some((base, ..))) if webgl2 => number(base),
                    (TEXTURE_MAX_LEVEL, Some((_, max, ..))) if webgl2 => number(max),
                    (TEXTURE_IMMUTABLE_FORMAT, Some((_, _, immutable, _))) if webgl2 => {
                        Value::Bool(immutable.is_some())
                    }
                    (TEXTURE_IMMUTABLE_LEVELS, Some((_, _, immutable, _))) if webgl2 => {
                        number(immutable.unwrap_or(0))
                    }
                    (TEXTURE_MAX_ANISOTROPY_EXT, Some((.., anisotropy))) if anisotropic => {
                        number(anisotropy)
                    }
                    _ => {
                        gl.error(INVALID_ENUM);
                        Value::Null
                    }
                })
            }
            "generateMipmap" => {
                let target = self.gl_u32(arguments, 0)?;
                let gl = &mut context.borrow_mut().gl;
                let Some(id) = gl.texture_params_target(target) else {
                    return undefined;
                };
                let webgl2 = gl.webgl2;
                let texture = gl.textures.get_mut(&id).expect("bound");
                let Some(base) = texture.base() else {
                    gl.error(INVALID_OPERATION);
                    return undefined;
                };
                let format = base.format;
                let power = base.width.is_power_of_two() && base.height.is_power_of_two();
                let filterable = !format.is_integer()
                    && !format.has_depth()
                    && (format.kind != TexelKind::Float
                        || format.float_bits() == 16
                        || gl.extensions.contains(&"OES_texture_float_linear"));
                if (!webgl2 && !power)
                    || !filterable && webgl2
                    || (texture.target == TEXTURE_CUBE_MAP
                        && !texture.complete(
                            &super::texture::Params {
                                min_filter: NEAREST,
                                ..texture.params
                            },
                            true,
                            true,
                            true,
                        ))
                {
                    gl.error(INVALID_OPERATION);
                    return undefined;
                }
                gl.textures.get_mut(&id).expect("bound").generate_mipmap();
                undefined
            }
            "texImage2D" | "texImage3D" | "texSubImage2D" | "texSubImage3D" => {
                self.gl_tex_image(context, name, interface, arguments)
            }
            "texStorage2D" | "texStorage3D" => {
                let three = name == "texStorage3D";
                let target = self.gl_u32(arguments, 0)?;
                let levels = self.gl_i32(arguments, 1)?;
                let internal = self.gl_u32(arguments, 2)?;
                let width = self.gl_i32(arguments, 3)?;
                let height = self.gl_i32(arguments, 4)?;
                let depth = if three { self.gl_i32(arguments, 5)? } else { 1 };
                let gl = &mut context.borrow_mut().gl;
                let valid_target = if three {
                    matches!(target, TEXTURE_3D | TEXTURE_2D_ARRAY)
                } else {
                    matches!(target, TEXTURE_2D | TEXTURE_CUBE_MAP)
                };
                if !valid_target {
                    gl.error(INVALID_ENUM);
                    return undefined;
                }
                let Some(id) = gl.texture_binding(target).filter(|&id| id != 0) else {
                    gl.error(INVALID_OPERATION);
                    return undefined;
                };
                let Some(format) = Format::sized(internal) else {
                    gl.error(INVALID_ENUM);
                    return undefined;
                };
                if levels < 1 || width < 1 || height < 1 || depth < 1 {
                    gl.error(INVALID_VALUE);
                    return undefined;
                }
                let largest = width
                    .max(height)
                    .max(if target == TEXTURE_3D { depth } else { 1 })
                    as u32;
                if levels as u32 > 32 - largest.leading_zeros()
                    || (target == TEXTURE_CUBE_MAP && width != height)
                {
                    gl.error(INVALID_OPERATION);
                    return undefined;
                }
                let texture = gl.textures.get_mut(&id).expect("bound");
                if texture.immutable_levels.is_some() {
                    gl.error(INVALID_OPERATION);
                    return undefined;
                }
                let faces = if target == TEXTURE_CUBE_MAP { 6 } else { 1 };
                let (mut w, mut h, mut d) = (width as u32, height as u32, depth as u32);
                for level in 0..levels as usize {
                    for face in 0..faces {
                        texture.set_level(face, level, Level::new(format, w, h, d));
                    }
                    w = (w / 2).max(1);
                    h = (h / 2).max(1);
                    if target == TEXTURE_3D {
                        d = (d / 2).max(1);
                    }
                }
                texture.immutable_levels = Some(levels as u32);
                undefined
            }
            "copyTexImage2D" => {
                let target = self.gl_u32(arguments, 0)?;
                let level = self.gl_i32(arguments, 1)?;
                let internal = self.gl_u32(arguments, 2)?;
                let (x, y, width, height) = (
                    self.gl_i32(arguments, 3)?,
                    self.gl_i32(arguments, 4)?,
                    self.gl_i32(arguments, 5)?,
                    self.gl_i32(arguments, 6)?,
                );
                let border = self.gl_i32(arguments, 7)?;
                let gl = &mut context.borrow_mut().gl;
                let kind = UNSIGNED_BYTE;
                let format_name = match internal {
                    RGBA | RGB | ALPHA | LUMINANCE | LUMINANCE_ALPHA => internal,
                    _ if gl.webgl2 => Format::sized(internal).map_or(internal, |found| found.base),
                    _ => {
                        gl.error(INVALID_ENUM);
                        return undefined;
                    }
                };
                let sized_type = Format::sized(internal)
                    .map(|found| {
                        if found.kind == TexelKind::Float {
                            FLOAT
                        } else {
                            kind
                        }
                    })
                    .unwrap_or(kind);
                let Some((id, face, format)) = gl.define_level(
                    target,
                    level,
                    internal,
                    width,
                    height,
                    1,
                    border,
                    format_name,
                    sized_type,
                    false,
                ) else {
                    return undefined;
                };
                let Some(texels) = gl.copy_region(x, y, width, height, format) else {
                    return undefined;
                };
                gl.store_level(
                    id,
                    face,
                    level,
                    Level {
                        width: width as u32,
                        height: height as u32,
                        depth: 1,
                        format,
                        texels,
                    },
                );
                undefined
            }
            "copyTexSubImage2D" | "copyTexSubImage3D" => {
                let three = name == "copyTexSubImage3D";
                let target = self.gl_u32(arguments, 0)?;
                let level = self.gl_i32(arguments, 1)?;
                let (xoffset, yoffset) = (self.gl_i32(arguments, 2)?, self.gl_i32(arguments, 3)?);
                let shift = usize::from(three);
                let zoffset = if three { self.gl_i32(arguments, 4)? } else { 0 };
                let (x, y, width, height) = (
                    self.gl_i32(arguments, 4 + shift)?,
                    self.gl_i32(arguments, 5 + shift)?,
                    self.gl_i32(arguments, 6 + shift)?,
                    self.gl_i32(arguments, 7 + shift)?,
                );
                let gl = &mut context.borrow_mut().gl;
                let Some((id, face)) = gl.image_target(target, three) else {
                    return undefined;
                };
                let Some(existing) = gl.textures[&id]
                    .level(face, level.max(0) as usize)
                    .map(|found| (found.format, found.width, found.height, found.depth))
                else {
                    gl.error(INVALID_OPERATION);
                    return undefined;
                };
                if xoffset < 0
                    || yoffset < 0
                    || zoffset < 0
                    || width < 0
                    || height < 0
                    || (xoffset + width) as u32 > existing.1
                    || (yoffset + height) as u32 > existing.2
                    || zoffset as u32 >= existing.3
                {
                    gl.error(INVALID_VALUE);
                    return undefined;
                }
                let Some(texels) = gl.copy_region(x, y, width, height, existing.0) else {
                    return undefined;
                };
                gl.write_sub_level(
                    id, face, level, xoffset, yoffset, zoffset, width, height, 1, &texels,
                );
                undefined
            }
            "compressedTexImage2D"
            | "compressedTexSubImage2D"
            | "compressedTexImage3D"
            | "compressedTexSubImage3D" => {
                gl!().error(INVALID_ENUM);
                undefined
            }
            "readPixels" => self.gl_read_pixels(context, name, interface, arguments),
            "blitFramebuffer" => {
                let mut values = [0; 8];
                for (index, slot) in values.iter_mut().enumerate() {
                    *slot = self.gl_i32(arguments, index)?;
                }
                let mask = self.gl_u32(arguments, 8)?;
                let filter = self.gl_u32(arguments, 9)?;
                gl!().blit(
                    [values[0], values[1], values[2], values[3]],
                    [values[4], values[5], values[6], values[7]],
                    mask,
                    filter,
                );
                undefined
            }
            "getBufferParameter" => {
                let target = self.gl_u32(arguments, 0)?;
                let parameter = self.gl_u32(arguments, 1)?;
                let gl = &mut context.borrow_mut().gl;
                let Some(id) = gl.buffer_binding(target) else {
                    return Ok(Value::Null);
                };
                let Some(buffer) = gl.buffers.get(&id) else {
                    gl.error(INVALID_OPERATION);
                    return Ok(Value::Null);
                };
                Ok(match parameter {
                    BUFFER_SIZE => number(buffer.data.len() as f64),
                    BUFFER_USAGE => number(buffer.usage),
                    _ => {
                        gl.error(INVALID_ENUM);
                        Value::Null
                    }
                })
            }
            "getParameter" => {
                let parameter = self.gl_u32(arguments, 0)?;
                self.gl_parameter(context, parameter)
            }
            "getIndexedParameter" => {
                let target = self.gl_u32(arguments, 0)?;
                let index = self.gl_u32(arguments, 1)? as usize;
                let range = {
                    let gl = &mut context.borrow_mut().gl;
                    let ranges = match target {
                        UNIFORM_BUFFER_BINDING | UNIFORM_BUFFER_START | UNIFORM_BUFFER_SIZE => {
                            gl.uniform_ranges.clone()
                        }
                        TRANSFORM_FEEDBACK_BUFFER_BINDING
                        | TRANSFORM_FEEDBACK_BUFFER_START
                        | TRANSFORM_FEEDBACK_BUFFER_SIZE => {
                            gl.feedbacks[&gl.feedback].buffers.clone()
                        }
                        _ => {
                            gl.error(INVALID_ENUM);
                            return Ok(Value::Null);
                        }
                    };
                    let Some(range) = ranges.get(index).copied() else {
                        gl.error(INVALID_VALUE);
                        return Ok(Value::Null);
                    };
                    range
                };
                Ok(match target {
                    UNIFORM_BUFFER_BINDING | TRANSFORM_FEEDBACK_BUFFER_BINDING => {
                        self.wrap_gl(context, range.buffer, Kind::Buffer)
                    }
                    UNIFORM_BUFFER_START | TRANSFORM_FEEDBACK_BUFFER_START => {
                        number(range.offset as f64)
                    }
                    _ => number(range.size as f64),
                })
            }
            "getInternalformatParameter" => {
                let target = self.gl_u32(arguments, 0)?;
                let internal = self.gl_u32(arguments, 1)?;
                let parameter = self.gl_u32(arguments, 2)?;
                let gl = &mut context.borrow_mut().gl;
                if target != RENDERBUFFER || parameter != SAMPLES {
                    gl.error(INVALID_ENUM);
                    return Ok(Value::Null);
                }
                let Some(format) = Format::sized(internal) else {
                    gl.error(INVALID_ENUM);
                    return Ok(Value::Null);
                };
                Ok(int_array(if format.is_integer() { &[] } else { &[4] }))
            }
            "beginQuery" | "endQuery" | "getQuery" => {
                let target = self.gl_u32(arguments, 0)?;
                let id = if name == "beginQuery" {
                    object!(1, Kind::Query, false)
                } else {
                    0
                };
                let gl = &mut context.borrow_mut().gl;
                if !matches!(
                    target,
                    ANY_SAMPLES_PASSED
                        | ANY_SAMPLES_PASSED_CONSERVATIVE
                        | TRANSFORM_FEEDBACK_PRIMITIVES_WRITTEN
                ) {
                    gl.error(INVALID_ENUM);
                    return Ok(if name == "getQuery" {
                        Value::Null
                    } else {
                        Value::Undefined
                    });
                }
                let samples = |found: u32| {
                    if found == TRANSFORM_FEEDBACK_PRIMITIVES_WRITTEN {
                        found
                    } else {
                        ANY_SAMPLES_PASSED
                    }
                };
                match name {
                    "beginQuery" => {
                        let busy = gl
                            .active_queries
                            .keys()
                            .any(|&active| samples(active) == samples(target));
                        let Some(query) = gl.queries.get_mut(&id) else {
                            gl.error(INVALID_OPERATION);
                            return undefined;
                        };
                        if busy || query.active || (query.target != 0 && query.target != target) {
                            gl.error(INVALID_OPERATION);
                            return undefined;
                        }
                        *query = context::Query {
                            target,
                            result: 0,
                            active: true,
                        };
                        gl.active_queries.insert(target, id);
                        undefined
                    }
                    "endQuery" => {
                        match gl.active_queries.remove(&target) {
                            Some(id) => {
                                if let Some(query) = gl.queries.get_mut(&id) {
                                    query.active = false;
                                }
                            }
                            None => gl.error(INVALID_OPERATION),
                        }
                        undefined
                    }
                    _ => {
                        let parameter = self.gl_u32(arguments, 1)?;
                        if parameter != CURRENT_QUERY {
                            context.borrow_mut().gl.error(INVALID_ENUM);
                            return Ok(Value::Null);
                        }
                        let id = context
                            .borrow()
                            .gl
                            .active_queries
                            .get(&target)
                            .copied()
                            .unwrap_or(0);
                        Ok(self.wrap_gl(context, id, Kind::Query))
                    }
                }
            }
            "getQueryParameter" => {
                let id = object!(0, Kind::Query, false);
                let parameter = self.gl_u32(arguments, 1)?;
                let gl = &mut context.borrow_mut().gl;
                let Some(query) = gl
                    .queries
                    .get(&id)
                    .filter(|query| !query.active && query.target != 0)
                else {
                    gl.error(INVALID_OPERATION);
                    return Ok(Value::Null);
                };
                Ok(match parameter {
                    QUERY_RESULT => {
                        if query.target == TRANSFORM_FEEDBACK_PRIMITIVES_WRITTEN {
                            number(query.result as f64)
                        } else {
                            Value::Bool(query.result != 0)
                        }
                    }
                    QUERY_RESULT_AVAILABLE => Value::Bool(true),
                    _ => {
                        gl.error(INVALID_ENUM);
                        Value::Null
                    }
                })
            }
            "beginTransformFeedback" => {
                let mode = self.gl_u32(arguments, 0)?;
                let gl = &mut context.borrow_mut().gl;
                if !matches!(mode, POINTS | LINES | TRIANGLES) {
                    gl.error(INVALID_ENUM);
                    return undefined;
                }
                let program = gl.program;
                let varyings = gl
                    .programs
                    .get(&program)
                    .and_then(|found| found.linked.as_ref())
                    .map_or(0, |linked| linked.feedback.len());
                let current = gl.feedback;
                let feedback = gl.feedbacks.get_mut(&current).expect("bound");
                if feedback.active || varyings == 0 {
                    gl.error(INVALID_OPERATION);
                    return undefined;
                }
                feedback.active = true;
                feedback.paused = false;
                feedback.mode = mode;
                feedback.program = program;
                feedback.written = vec![0; context::FEEDBACK_BUFFERS];
                undefined
            }
            "endTransformFeedback" | "pauseTransformFeedback" | "resumeTransformFeedback" => {
                let gl = &mut context.borrow_mut().gl;
                let current = gl.feedback;
                let feedback = gl.feedbacks.get_mut(&current).expect("bound");
                let allowed = match name {
                    "endTransformFeedback" => feedback.active,
                    "pauseTransformFeedback" => feedback.active && !feedback.paused,
                    _ => feedback.active && feedback.paused,
                };
                if !allowed {
                    gl.error(INVALID_OPERATION);
                    return undefined;
                }
                match name {
                    "endTransformFeedback" => {
                        feedback.active = false;
                        feedback.paused = false;
                    }
                    "pauseTransformFeedback" => feedback.paused = true,
                    _ => feedback.paused = false,
                }
                undefined
            }
            "transformFeedbackVaryings" => {
                let program = object!(0, Kind::Program, false);
                let values = self.iterate_to_vec(arguments[1].clone())?;
                let mut varyings = Vec::with_capacity(values.len());
                for value in values {
                    varyings.push(self.coerce_string(value)?);
                }
                let mode = self.gl_u32(arguments, 2)?;
                let gl = &mut context.borrow_mut().gl;
                if !matches!(mode, INTERLEAVED_ATTRIBS | SEPARATE_ATTRIBS) {
                    gl.error(INVALID_ENUM);
                    return undefined;
                }
                if mode == SEPARATE_ATTRIBS && varyings.len() > context::FEEDBACK_BUFFERS {
                    gl.error(INVALID_VALUE);
                    return undefined;
                }
                if let Some(found) = gl.programs.get_mut(&program) {
                    found.feedback_varyings = varyings;
                    found.feedback_mode = mode;
                }
                undefined
            }
            "getUniformBlockIndex" => {
                let program = object!(0, Kind::Program, false);
                let block = self.coerce_string(arguments[1].clone())?;
                let gl = &mut context.borrow_mut().gl;
                let Some(linked) = gl
                    .programs
                    .get(&program)
                    .and_then(|found| found.linked.as_ref())
                else {
                    gl.error(INVALID_OPERATION);
                    return Ok(number(INVALID_INDEX));
                };
                Ok(number(
                    linked
                        .blocks
                        .iter()
                        .position(|found| found.name == block)
                        .map_or(INVALID_INDEX, |index| index as u32),
                ))
            }
            "getActiveUniformBlockName" => {
                let program = object!(0, Kind::Program, false);
                let index = self.gl_u32(arguments, 1)? as usize;
                let gl = &mut context.borrow_mut().gl;
                let found = gl
                    .programs
                    .get(&program)
                    .and_then(|found| found.linked.as_ref())
                    .and_then(|linked| linked.blocks.get(index))
                    .map(|block| block.name.clone());
                match found {
                    Some(block) => Ok(Value::String(block.as_str().into())),
                    None => {
                        gl.error(INVALID_VALUE);
                        Ok(Value::Null)
                    }
                }
            }
            "getActiveUniformBlockParameter" => {
                let program = object!(0, Kind::Program, false);
                let index = self.gl_u32(arguments, 1)? as usize;
                let parameter = self.gl_u32(arguments, 2)?;
                let gl = &mut context.borrow_mut().gl;
                let Some(block) = gl
                    .programs
                    .get(&program)
                    .and_then(|found| found.linked.as_ref())
                    .and_then(|linked| linked.blocks.get(index))
                else {
                    gl.error(INVALID_VALUE);
                    return Ok(Value::Null);
                };
                Ok(match parameter {
                    UNIFORM_BLOCK_BINDING => number(block.binding),
                    UNIFORM_BLOCK_DATA_SIZE => number(block.size as u32),
                    UNIFORM_BLOCK_ACTIVE_UNIFORMS => number(block.members.len() as u32),
                    UNIFORM_BLOCK_ACTIVE_UNIFORM_INDICES => uint_array(
                        &block
                            .members
                            .iter()
                            .map(|&member| member as u32)
                            .collect::<Vec<_>>(),
                    ),
                    UNIFORM_BLOCK_REFERENCED_BY_VERTEX_SHADER => Value::Bool(block.stages[0]),
                    UNIFORM_BLOCK_REFERENCED_BY_FRAGMENT_SHADER => Value::Bool(block.stages[1]),
                    _ => {
                        gl.error(INVALID_ENUM);
                        Value::Null
                    }
                })
            }
            "uniformBlockBinding" => {
                let program = object!(0, Kind::Program, false);
                let index = self.gl_u32(arguments, 1)? as usize;
                let binding = self.gl_u32(arguments, 2)?;
                let gl = &mut context.borrow_mut().gl;
                let Some(block) = gl
                    .programs
                    .get_mut(&program)
                    .and_then(|found| found.linked.as_mut())
                    .and_then(|linked| linked.blocks.get_mut(index))
                else {
                    gl.error(INVALID_VALUE);
                    return undefined;
                };
                if binding as usize >= context::UNIFORM_BUFFER_BINDINGS {
                    gl.error(INVALID_VALUE);
                    return undefined;
                }
                block.binding = binding;
                undefined
            }
            "getUniformIndices" => {
                let program = object!(0, Kind::Program, false);
                let values = self.iterate_to_vec(arguments[1].clone())?;
                let mut names = Vec::with_capacity(values.len());
                for value in values {
                    names.push(self.coerce_string(value)?);
                }
                let gl = &mut context.borrow_mut().gl;
                let Some(linked) = gl
                    .programs
                    .get(&program)
                    .and_then(|found| found.linked.as_ref())
                else {
                    gl.error(INVALID_OPERATION);
                    return Ok(Value::Null);
                };
                let indices: Vec<Value> = names
                    .iter()
                    .map(|wanted| {
                        number(
                            linked
                                .uniforms
                                .iter()
                                .position(|uniform| {
                                    &uniform.name == wanted || &uniform.base_name == wanted
                                })
                                .map_or(INVALID_INDEX, |index| index as u32),
                        )
                    })
                    .collect();
                Ok(Self::new_array(indices))
            }
            "getActiveUniforms" => {
                let program = object!(0, Kind::Program, false);
                let values = self.iterate_to_vec(arguments[1].clone())?;
                let mut indices = Vec::with_capacity(values.len());
                for value in values {
                    indices.push(self.coerce_uint32(value)? as usize);
                }
                let parameter = self.gl_u32(arguments, 2)?;
                let gl = &mut context.borrow_mut().gl;
                let Some(linked) = gl
                    .programs
                    .get(&program)
                    .and_then(|found| found.linked.as_ref())
                else {
                    gl.error(INVALID_OPERATION);
                    return Ok(Value::Null);
                };
                if indices.iter().any(|&index| index >= linked.uniforms.len()) {
                    gl.error(INVALID_VALUE);
                    return Ok(Value::Null);
                }
                let mut answers = Vec::with_capacity(indices.len());
                for index in indices {
                    let uniform = &linked.uniforms[index];
                    let in_block = uniform.block.is_some();
                    answers.push(match parameter {
                        UNIFORM_TYPE => number(context::type_enum(&uniform.ty)),
                        UNIFORM_SIZE => number(uniform.size as u32),
                        UNIFORM_BLOCK_INDEX => {
                            number(uniform.block.map_or(-1, |block| block as i32))
                        }
                        UNIFORM_OFFSET => number(if in_block { uniform.offset as i32 } else { -1 }),
                        UNIFORM_ARRAY_STRIDE => number(if in_block {
                            uniform.array_stride as i32
                        } else {
                            -1
                        }),
                        UNIFORM_MATRIX_STRIDE => number(if in_block {
                            uniform.matrix_stride as i32
                        } else {
                            -1
                        }),
                        UNIFORM_IS_ROW_MAJOR => Value::Bool(false),
                        _ => {
                            gl.error(INVALID_ENUM);
                            return Ok(Value::Null);
                        }
                    });
                }
                Ok(Self::new_array(answers))
            }
            _ => undefined,
        }
    }

    fn gl_uniform(
        &mut self,
        context: &Rc<RefCell<GlContext>>,
        name: &str,
        interface: &str,
        arguments: &[Value],
    ) -> Result<Value, ScriptError> {
        let location = self.gl_location(context, &arguments[0], name, interface)?;
        let rest = &name["uniform".len()..];
        let (setter, components, vector) = if let Some(shape) = rest.strip_prefix("Matrix") {
            let shape = shape.trim_end_matches("fv");
            let (columns, rows) = match shape.split_once('x') {
                Some((columns, rows)) => (
                    columns.parse::<u8>().unwrap_or(0),
                    rows.parse::<u8>().unwrap_or(0),
                ),
                None => {
                    let size = shape.parse::<u8>().unwrap_or(0);
                    (size, size)
                }
            };
            (
                Setter::Matrix(columns, rows),
                usize::from(columns) * usize::from(rows),
                true,
            )
        } else {
            let count = usize::from(rest.as_bytes()[0] - b'0');
            let suffix = &rest[1..];
            let setter = match suffix.trim_end_matches('v') {
                "f" => Setter::Float,
                "i" => Setter::Int,
                _ => Setter::Uint,
            };
            (setter, count, suffix.ends_with('v'))
        };
        let words: Vec<u32> = if let Setter::Matrix(..) = setter {
            let transpose = Self::gl_bool(arguments, 1);
            let offset = if arguments.len() > 3 {
                self.gl_u32(arguments, 3)? as usize
            } else {
                0
            };
            let length = if arguments.len() > 4 {
                self.gl_u32(arguments, 4)? as usize
            } else {
                0
            };
            let Some(words) = self.gl_list(
                &arguments[2],
                ElementKind::Float32,
                offset,
                length,
                name,
                interface,
            )?
            else {
                context.borrow_mut().gl.error(INVALID_VALUE);
                return Ok(Value::Undefined);
            };
            let Some(Some((program, generation, index))) = location else {
                if location.is_none() {
                    context.borrow_mut().gl.error(INVALID_OPERATION);
                }
                return Ok(Value::Undefined);
            };
            context.borrow_mut().gl.set_uniform(
                program, generation, index, setter, components, &words, transpose,
            );
            return Ok(Value::Undefined);
        } else if vector {
            let kind = match setter {
                Setter::Float => ElementKind::Float32,
                Setter::Int => ElementKind::Int32,
                _ => ElementKind::Uint32,
            };
            let offset = if arguments.len() > 2 {
                self.gl_u32(arguments, 2)? as usize
            } else {
                0
            };
            let length = if arguments.len() > 3 {
                self.gl_u32(arguments, 3)? as usize
            } else {
                0
            };
            let Some(words) = self.gl_list(&arguments[1], kind, offset, length, name, interface)?
            else {
                context.borrow_mut().gl.error(INVALID_VALUE);
                return Ok(Value::Undefined);
            };
            words
        } else {
            let mut words = Vec::with_capacity(components);
            for position in 0..components {
                words.push(match setter {
                    Setter::Float => self.gl_f32(arguments, position + 1)?.to_bits(),
                    Setter::Int => self.gl_i32(arguments, position + 1)? as u32,
                    _ => self.gl_u32(arguments, position + 1)?,
                });
            }
            words
        };
        match location {
            None => context.borrow_mut().gl.error(INVALID_OPERATION),
            Some(None) => {}
            Some(Some((program, generation, index))) => context.borrow_mut().gl.set_uniform(
                program, generation, index, setter, components, &words, false,
            ),
        }
        Ok(Value::Undefined)
    }

    fn gl_buffer_data(
        &mut self,
        context: &Rc<RefCell<GlContext>>,
        arguments: &[Value],
        name: &str,
        interface: &str,
    ) -> Result<Value, ScriptError> {
        let target = self.gl_u32(arguments, 0)?;
        let usage = self.gl_u32(arguments, 2)?;
        let data = match &arguments[1] {
            Value::Null => {
                context.borrow_mut().gl.error(INVALID_VALUE);
                return Ok(Value::Undefined);
            }
            value @ (Value::Bytes(_) | Value::TypedArray(_) | Value::Object(_)) => {
                let Some((bytes, element, _)) = Self::view_bytes(value) else {
                    return Err(self.type_error(format!("Failed to execute '{name}' on '{interface}': parameter 2 is not of type 'ArrayBuffer'.")));
                };
                let offset = if arguments.len() > 3 {
                    self.gl_u32(arguments, 3)? as usize * element
                } else {
                    0
                };
                let length = if arguments.len() > 4 {
                    self.gl_u32(arguments, 4)? as usize * element
                } else {
                    0
                };
                if offset > bytes.len() || offset + length > bytes.len() {
                    context.borrow_mut().gl.error(INVALID_VALUE);
                    return Ok(Value::Undefined);
                }
                let end = if length == 0 {
                    bytes.len()
                } else {
                    offset + length
                };
                bytes[offset..end].to_vec()
            }
            _ => {
                let size = self.gl_i64(arguments, 1)?;
                if size < 0 {
                    context.borrow_mut().gl.error(INVALID_VALUE);
                    return Ok(Value::Undefined);
                }
                if size > 1 << 31 {
                    context.borrow_mut().gl.error(OUT_OF_MEMORY);
                    return Ok(Value::Undefined);
                }
                vec![0; size as usize]
            }
        };
        context.borrow_mut().gl.buffer_data(target, data, usage);
        Ok(Value::Undefined)
    }

    // texImage2D, texImage3D, texSubImage2D and texSubImage3D in all their overloads
    fn gl_tex_image(
        &mut self,
        context: &Rc<RefCell<GlContext>>,
        name: &str,
        interface: &str,
        arguments: &[Value],
    ) -> Result<Value, ScriptError> {
        let three = name.ends_with("3D");
        let sub = name.starts_with("texSub");
        let count = arguments.len();
        // the source form of the 2D calls in WebGL 1 has no size
        let short = !three && count < 9;
        let short_arity = if sub { 7 } else { 6 };
        if !three && short && count != short_arity {
            return Err(self.type_error(format!("Failed to execute '{name}' on '{interface}': Valid arities are: [{short_arity}, 9], but {count} arguments provided.")));
        }
        let target = self.gl_u32(arguments, 0)?;
        let level = self.gl_i32(arguments, 1)?;
        let mut index = 2;
        let (internal, offsets) = if sub {
            let mut offsets = [0; 3];
            for slot in offsets.iter_mut().take(if three { 3 } else { 2 }) {
                *slot = self.gl_i32(arguments, index)?;
                index += 1;
            }
            (0, offsets)
        } else {
            let internal = self.gl_u32(arguments, index)?;
            index += 1;
            (internal, [0; 3])
        };
        let (mut width, mut height, mut depth, border) = (0, 0, 1, 0);
        if !short {
            width = self.gl_i32(arguments, index)?;
            height = self.gl_i32(arguments, index + 1)?;
            index += 2;
            if three {
                depth = self.gl_i32(arguments, index)?;
                index += 1;
            }
            if !sub {
                let found = self.gl_i32(arguments, index)?;
                index += 1;
                if found != 0 {
                    context.borrow_mut().gl.error(INVALID_VALUE);
                    return Ok(Value::Undefined);
                }
            }
        }
        let _ = border;
        let format = self.gl_u32(arguments, index)?;
        let kind = self.gl_u32(arguments, index + 1)?;
        let source = arguments.get(index + 2).cloned().unwrap_or(Value::Null);
        let source_offset = arguments.get(index + 3).cloned();
        // the pixels: bytes from a view, texels from an image source, or nothing
        let pixels = match &source {
            Value::Null | Value::Undefined if !short => Pixels::None,
            Value::Number(_) if !short => {
                // an offset into the bound PIXEL_UNPACK_BUFFER
                let offset = self.gl_i64(arguments, index + 2)?;
                let gl = &mut context.borrow_mut().gl;
                let Some(buffer) = gl
                    .buffers
                    .get(&gl.pixel_unpack)
                    .filter(|_| gl.pixel_unpack != 0)
                else {
                    gl.error(INVALID_OPERATION);
                    return Ok(Value::Undefined);
                };
                if offset < 0 || offset as usize > buffer.data.len() {
                    gl.error(INVALID_VALUE);
                    return Ok(Value::Undefined);
                }
                Pixels::Bytes(buffer.data[offset as usize..].to_vec())
            }
            Value::TypedArray(view) if !short => {
                let view_kind = view.borrow().kind;
                if !view_fits(view_kind, kind) {
                    context.borrow_mut().gl.error(INVALID_OPERATION);
                    return Ok(Value::Undefined);
                }
                let (bytes, element, _) =
                    Self::view_bytes(&source).expect("a typed array has bytes");
                let offset = match source_offset {
                    Some(value) => self.coerce_uint32(value)? as usize * element,
                    None => 0,
                };
                if offset > bytes.len() {
                    context.borrow_mut().gl.error(INVALID_VALUE);
                    return Ok(Value::Undefined);
                }
                Pixels::Bytes(bytes[offset..].to_vec())
            }
            Value::Object(_) if !short && Self::view_bytes(&source).is_some() => {
                context.borrow_mut().gl.error(INVALID_OPERATION);
                return Ok(Value::Undefined);
            }
            _ => {
                let Some((source_width, source_height, rows)) = self.texture_source(&source)?
                else {
                    return Ok(Value::Undefined);
                };
                if short {
                    width = source_width as i32;
                    height = source_height as i32;
                }
                let (flip, premultiply) = {
                    let gl = &context.borrow().gl;
                    (gl.unpack_flip, gl.unpack_premultiply)
                };
                let layers = if three { depth.max(1) as u32 } else { 1 };
                let slice_height = if three {
                    source_height / layers
                } else {
                    source_height
                };
                let mut texels =
                    Vec::with_capacity((width.max(0) * height.max(0)) as usize * layers as usize);
                for layer in 0..layers {
                    for y in 0..height.max(0) as u32 {
                        let row = if flip {
                            slice_height.saturating_sub(1 + y)
                        } else {
                            y
                        };
                        for x in 0..width.max(0) as u32 {
                            let at = (layer * slice_height + row) * source_width + x;
                            let [r, g, b, a] = if x < source_width && row < slice_height {
                                rows.get(at as usize).copied().unwrap_or([0; 4])
                            } else {
                                [0; 4]
                            };
                            let alpha = f32::from(a) / 255.0;
                            let scale = if premultiply { alpha } else { 1.0 };
                            let channel = |value: u8| f32::from(value) / 255.0 * scale;
                            texels.push(match format {
                                ALPHA => [alpha, 0.0, 0.0, 0.0],
                                LUMINANCE_ALPHA => [channel(r), alpha, 0.0, 0.0],
                                _ => [channel(r), channel(g), channel(b), alpha],
                            });
                        }
                    }
                }
                Pixels::Texels(texels)
            }
        };
        let gl = &mut context.borrow_mut().gl;
        let (id, face, target_format) = if sub {
            match gl.sub_level_target(
                target, level, offsets[0], offsets[1], offsets[2], width, height, depth, format,
                kind, three,
            ) {
                Some(found) => found,
                None => return Ok(Value::Undefined),
            }
        } else {
            match gl.define_level(
                target, level, internal, width, height, depth, 0, format, kind, three,
            ) {
                Some(found) => found,
                None => return Ok(Value::Undefined),
            }
        };
        let texels = match pixels {
            Pixels::None => {
                if sub {
                    gl.error(INVALID_VALUE);
                    return Ok(Value::Undefined);
                }
                Level::new(target_format, width as u32, height as u32, depth as u32).texels
            }
            Pixels::Bytes(bytes) => {
                let Some((pixel, _)) = super::texture::pixel_size(format, kind) else {
                    gl.error(INVALID_ENUM);
                    return Ok(Value::Undefined);
                };
                if gl
                    .unpack
                    .needed(width as u32, height as u32, depth as u32, pixel)
                    > bytes.len()
                {
                    gl.error(INVALID_OPERATION);
                    return Ok(Value::Undefined);
                }
                gl.decode(
                    &bytes,
                    width as u32,
                    height as u32,
                    depth as u32,
                    format,
                    kind,
                    target_format,
                )
            }
            Pixels::Texels(values) => values
                .into_iter()
                .map(|value| target_format.store(value))
                .collect(),
        };
        if sub {
            gl.write_sub_level(
                id, face, level, offsets[0], offsets[1], offsets[2], width, height, depth, &texels,
            );
        } else {
            gl.store_level(
                id,
                face,
                level,
                Level {
                    width: width as u32,
                    height: height as u32,
                    depth: depth as u32,
                    format: target_format,
                    texels,
                },
            );
        }
        Ok(Value::Undefined)
    }

    fn gl_read_pixels(
        &mut self,
        context: &Rc<RefCell<GlContext>>,
        name: &str,
        interface: &str,
        arguments: &[Value],
    ) -> Result<Value, ScriptError> {
        let (x, y, width, height) = (
            self.gl_i32(arguments, 0)?,
            self.gl_i32(arguments, 1)?,
            self.gl_i32(arguments, 2)?,
            self.gl_i32(arguments, 3)?,
        );
        let format = self.gl_u32(arguments, 4)?;
        let kind = self.gl_u32(arguments, 5)?;
        match &arguments[6] {
            Value::TypedArray(view) => {
                let element = view.borrow().kind;
                let offset = if arguments.len() > 7 { self.gl_u32(arguments, 7)? as usize * element.size() } else { 0 };
                if !view_fits(element, kind) {
                    context.borrow_mut().gl.error(INVALID_OPERATION);
                    return Ok(Value::Undefined);
                }
                let (mut bytes, _, _) = Self::view_bytes(&arguments[6]).expect("a typed array has bytes");
                if offset > bytes.len() {
                    context.borrow_mut().gl.error(INVALID_VALUE);
                    return Ok(Value::Undefined);
                }
                context.borrow_mut().gl.read_pixels(x, y, width, height, format, kind, &mut bytes[offset..]);
                let view = view.borrow();
                let range = byte_range(&view);
                view.buffer.borrow_mut()[range].copy_from_slice(&bytes);
                Ok(Value::Undefined)
            }
            Value::Number(_) => {
                let offset = self.gl_i64(arguments, 6)?;
                let gl = &mut context.borrow_mut().gl;
                let pack = gl.pixel_pack;
                if pack == 0 || offset < 0 {
                    gl.error(INVALID_OPERATION);
                    return Ok(Value::Undefined);
                }
                let mut bytes = gl.buffers[&pack].data.clone();
                if offset as usize > bytes.len() {
                    gl.error(INVALID_OPERATION);
                    return Ok(Value::Undefined);
                }
                gl.read_pixels(x, y, width, height, format, kind, &mut bytes[offset as usize..]);
                gl.buffers.get_mut(&pack).expect("bound").data = bytes;
                Ok(Value::Undefined)
            }
            Value::Null => {
                context.borrow_mut().gl.error(INVALID_VALUE);
                Ok(Value::Undefined)
            }
            _ => Err(self.type_error(format!("Failed to execute '{name}' on '{interface}': parameter 7 is not of type 'ArrayBufferView'."))),
        }
    }

    fn gl_attachment_parameter(
        &mut self,
        context: &Rc<RefCell<GlContext>>,
        arguments: &[Value],
    ) -> Result<Value, ScriptError> {
        let target = self.gl_u32(arguments, 0)?;
        let point = self.gl_u32(arguments, 1)?;
        let parameter = self.gl_u32(arguments, 2)?;
        let (attachment, format, webgl2) = {
            let gl = &mut context.borrow_mut().gl;
            let Some(id) = gl.framebuffer_binding(target) else {
                return Ok(Value::Null);
            };
            let webgl2 = gl.webgl2;
            if id == 0 {
                if !webgl2 {
                    gl.error(INVALID_OPERATION);
                    return Ok(Value::Null);
                }
                let format = match point {
                    BACK => Some(gl.color.format),
                    DEPTH | STENCIL => gl.depth_stencil.as_ref().map(|level| level.format),
                    _ => {
                        gl.error(INVALID_ENUM);
                        return Ok(Value::Null);
                    }
                };
                return Ok(match parameter {
                    FRAMEBUFFER_ATTACHMENT_OBJECT_TYPE => number(if format.is_some() {
                        FRAMEBUFFER_DEFAULT
                    } else {
                        NONE
                    }),
                    FRAMEBUFFER_ATTACHMENT_RED_SIZE
                    | FRAMEBUFFER_ATTACHMENT_GREEN_SIZE
                    | FRAMEBUFFER_ATTACHMENT_BLUE_SIZE
                    | FRAMEBUFFER_ATTACHMENT_ALPHA_SIZE => number(format.map_or(0, |format| {
                        format.channel_bits((parameter - FRAMEBUFFER_ATTACHMENT_RED_SIZE) as usize)
                    })),
                    FRAMEBUFFER_ATTACHMENT_DEPTH_SIZE => number(
                        format
                            .filter(|format| format.has_depth() && point == DEPTH)
                            .map_or(0, |format| u32::from(format.bits[0])),
                    ),
                    FRAMEBUFFER_ATTACHMENT_STENCIL_SIZE => number(
                        format
                            .filter(|format| format.has_stencil() && point == STENCIL)
                            .map_or(0, |_| 8),
                    ),
                    FRAMEBUFFER_ATTACHMENT_COMPONENT_TYPE => number(UNSIGNED_NORMALIZED),
                    FRAMEBUFFER_ATTACHMENT_COLOR_ENCODING => number(LINEAR),
                    _ => {
                        gl.error(INVALID_ENUM);
                        Value::Null
                    }
                });
            }
            if !gl.valid_attachment(point) {
                gl.error(INVALID_ENUM);
                return Ok(Value::Null);
            }
            let framebuffer = &gl.framebuffers[&id];
            let attachment = if point == DEPTH_STENCIL_ATTACHMENT && webgl2 {
                let (depth, stencil) = (
                    framebuffer.attachments.get(&DEPTH_ATTACHMENT),
                    framebuffer.attachments.get(&STENCIL_ATTACHMENT),
                );
                if depth != stencil {
                    gl.error(INVALID_OPERATION);
                    return Ok(Value::Null);
                }
                depth.copied()
            } else {
                framebuffer.attachments.get(&point).copied()
            };
            let format = attachment
                .and_then(|attachment| gl.attachment_format(&attachment))
                .map(|found| found.0);
            (attachment, format, webgl2)
        };
        let Some(attachment) = attachment else {
            return Ok(match parameter {
                FRAMEBUFFER_ATTACHMENT_OBJECT_TYPE => number(NONE),
                FRAMEBUFFER_ATTACHMENT_OBJECT_NAME => Value::Null,
                _ => {
                    context.borrow_mut().gl.error(if webgl2 {
                        INVALID_OPERATION
                    } else {
                        INVALID_ENUM
                    });
                    Value::Null
                }
            });
        };
        let bits = |channel: usize| format.map_or(0, |format| format.channel_bits(channel));
        Ok(match (parameter, attachment) {
            (FRAMEBUFFER_ATTACHMENT_OBJECT_TYPE, Attachment::Texture { .. }) => number(TEXTURE),
            (FRAMEBUFFER_ATTACHMENT_OBJECT_TYPE, Attachment::Renderbuffer(_)) => {
                number(RENDERBUFFER)
            }
            (FRAMEBUFFER_ATTACHMENT_OBJECT_NAME, Attachment::Texture { id, .. }) => {
                self.wrap_gl(context, id, Kind::Texture)
            }
            (FRAMEBUFFER_ATTACHMENT_OBJECT_NAME, Attachment::Renderbuffer(id)) => {
                self.wrap_gl(context, id, Kind::Renderbuffer)
            }
            (FRAMEBUFFER_ATTACHMENT_TEXTURE_LEVEL, Attachment::Texture { level, .. }) => {
                number(level)
            }
            (FRAMEBUFFER_ATTACHMENT_TEXTURE_CUBE_MAP_FACE, Attachment::Texture { face, .. }) => {
                let cube = context
                    .borrow()
                    .gl
                    .textures
                    .values()
                    .any(|texture| texture.target == TEXTURE_CUBE_MAP)
                    && face > 0;
                number(if cube || face > 0 {
                    TEXTURE_CUBE_MAP_POSITIVE_X + face as u32
                } else {
                    0
                })
            }
            (FRAMEBUFFER_ATTACHMENT_TEXTURE_LAYER, Attachment::Texture { layer, .. }) if webgl2 => {
                number(layer)
            }
            (FRAMEBUFFER_ATTACHMENT_RED_SIZE, _) if webgl2 => number(bits(0)),
            (FRAMEBUFFER_ATTACHMENT_GREEN_SIZE, _) if webgl2 => number(bits(1)),
            (FRAMEBUFFER_ATTACHMENT_BLUE_SIZE, _) if webgl2 => number(bits(2)),
            (FRAMEBUFFER_ATTACHMENT_ALPHA_SIZE, _) if webgl2 => number(bits(3)),
            (FRAMEBUFFER_ATTACHMENT_DEPTH_SIZE, _) if webgl2 => number(
                format
                    .filter(|format| format.has_depth())
                    .map_or(0, |format| u32::from(format.bits[0])),
            ),
            (FRAMEBUFFER_ATTACHMENT_STENCIL_SIZE, _) if webgl2 => number(
                format
                    .filter(|format| format.has_stencil())
                    .map_or(0, |_| 8),
            ),
            (FRAMEBUFFER_ATTACHMENT_COMPONENT_TYPE, _) => {
                number(match format.map(|format| format.kind) {
                    Some(TexelKind::Float) => FLOAT,
                    Some(TexelKind::Int) => INT,
                    Some(TexelKind::Uint) => UNSIGNED_INT,
                    Some(TexelKind::Snorm) => SIGNED_NORMALIZED,
                    _ => UNSIGNED_NORMALIZED,
                })
            }
            (FRAMEBUFFER_ATTACHMENT_COLOR_ENCODING, _) => number(
                if format.is_some_and(|format| format.kind == TexelKind::Srgb) {
                    SRGB
                } else {
                    LINEAR
                },
            ),
            _ => {
                context.borrow_mut().gl.error(INVALID_ENUM);
                Value::Null
            }
        })
    }

    fn gl_parameter(
        &mut self,
        context: &Rc<RefCell<GlContext>>,
        parameter: u32,
    ) -> Result<Value, ScriptError> {
        let state = context.borrow();
        let gl = &state.gl;
        let webgl2 = gl.webgl2;
        let has = |name: &str| gl.extensions.contains(&name);
        let flag = |capability: Capability| Value::Bool(gl.enabled(capability));
        // the bits of what the draw framebuffer draws into
        let (color_format, depth_format) = if gl.draw_framebuffer == 0 {
            (
                Some(gl.color.format),
                gl.depth_stencil.as_ref().map(|level| level.format),
            )
        } else {
            let framebuffer = &gl.framebuffers[&gl.draw_framebuffer];
            let color = framebuffer
                .attachments
                .get(&COLOR_ATTACHMENT0)
                .and_then(|attachment| gl.attachment_format(attachment))
                .map(|found| found.0);
            let depth = [
                DEPTH_STENCIL_ATTACHMENT,
                DEPTH_ATTACHMENT,
                STENCIL_ATTACHMENT,
            ]
            .iter()
            .find_map(|point| framebuffer.attachments.get(point))
            .and_then(|attachment| gl.attachment_format(attachment))
            .map(|found| found.0);
            (color, depth)
        };
        let binding = |kind: Kind, id: u32| (kind, id);
        let object = match parameter {
            ARRAY_BUFFER_BINDING => Some(binding(Kind::Buffer, gl.array_buffer)),
            ELEMENT_ARRAY_BUFFER_BINDING => {
                Some(binding(Kind::Buffer, gl.current_vertex_array().element))
            }
            CURRENT_PROGRAM => Some(binding(Kind::Program, gl.program)),
            FRAMEBUFFER_BINDING => Some(binding(Kind::Framebuffer, gl.draw_framebuffer)),
            RENDERBUFFER_BINDING => Some(binding(Kind::Renderbuffer, gl.renderbuffer)),
            TEXTURE_BINDING_2D => Some(binding(
                Kind::Texture,
                gl.units[gl.active_texture as usize][0],
            )),
            TEXTURE_BINDING_CUBE_MAP => Some(binding(
                Kind::Texture,
                gl.units[gl.active_texture as usize][1],
            )),
            VERTEX_ARRAY_BINDING if webgl2 || has("OES_vertex_array_object") => {
                Some(binding(Kind::VertexArray, gl.vertex_array))
            }
            TEXTURE_BINDING_3D if webgl2 => Some(binding(
                Kind::Texture,
                gl.units[gl.active_texture as usize][2],
            )),
            TEXTURE_BINDING_2D_ARRAY if webgl2 => Some(binding(
                Kind::Texture,
                gl.units[gl.active_texture as usize][3],
            )),
            COPY_READ_BUFFER_BINDING if webgl2 => Some(binding(Kind::Buffer, gl.copy_read)),
            COPY_WRITE_BUFFER_BINDING if webgl2 => Some(binding(Kind::Buffer, gl.copy_write)),
            PIXEL_PACK_BUFFER_BINDING if webgl2 => Some(binding(Kind::Buffer, gl.pixel_pack)),
            PIXEL_UNPACK_BUFFER_BINDING if webgl2 => Some(binding(Kind::Buffer, gl.pixel_unpack)),
            UNIFORM_BUFFER_BINDING if webgl2 => Some(binding(Kind::Buffer, gl.uniform_buffer)),
            TRANSFORM_FEEDBACK_BUFFER_BINDING if webgl2 => {
                Some(binding(Kind::Buffer, gl.feedback_buffer))
            }
            TRANSFORM_FEEDBACK_BINDING if webgl2 => {
                Some(binding(Kind::TransformFeedback, gl.feedback))
            }
            READ_FRAMEBUFFER_BINDING if webgl2 => {
                Some(binding(Kind::Framebuffer, gl.read_framebuffer))
            }
            SAMPLER_BINDING if webgl2 => Some(binding(
                Kind::Sampler,
                gl.unit_samplers[gl.active_texture as usize],
            )),
            _ => None,
        };
        if let Some((kind, id)) = object {
            drop(state);
            return Ok(self.wrap_gl(context, id, kind));
        }
        let bits = |format: Option<Format>, channel: usize| {
            number(format.map_or(0, |format| format.channel_bits(channel)))
        };
        let stencil = &gl.stencil;
        let draw_buffer = |index: u32| -> u32 {
            if gl.draw_framebuffer == 0 {
                return if index == 0 {
                    gl.default_draw_buffer
                } else {
                    NONE
                };
            }
            gl.framebuffers[&gl.draw_framebuffer]
                .draw_buffers
                .get(index as usize)
                .copied()
                .unwrap_or(NONE)
        };
        let value = match parameter {
            ACTIVE_TEXTURE => number(TEXTURE0 + gl.active_texture),
            ALIASED_LINE_WIDTH_RANGE => float_array(&[1.0, 1.0]),
            ALIASED_POINT_SIZE_RANGE => float_array(&[1.0, 1024.0]),
            RED_BITS => bits(color_format, 0),
            GREEN_BITS => bits(color_format, 1),
            BLUE_BITS => bits(color_format, 2),
            ALPHA_BITS => bits(color_format, 3),
            DEPTH_BITS => number(
                depth_format
                    .filter(|format| format.has_depth())
                    .map_or(0, |format| u32::from(format.bits[0])),
            ),
            STENCIL_BITS => number(
                depth_format
                    .filter(|format| format.has_stencil())
                    .map_or(0, |_| 8),
            ),
            BLEND => flag(Capability::Blend),
            BLEND_COLOR => float_array(&gl.blend.color),
            BLEND_DST_ALPHA => number(gl.blend.destination[1]),
            BLEND_DST_RGB => number(gl.blend.destination[0]),
            BLEND_EQUATION => number(gl.blend.equation[0]),
            BLEND_EQUATION_ALPHA => number(gl.blend.equation[1]),
            BLEND_SRC_ALPHA => number(gl.blend.source[1]),
            BLEND_SRC_RGB => number(gl.blend.source[0]),
            COLOR_CLEAR_VALUE => float_array(&gl.clear_color),
            COLOR_WRITEMASK => {
                Self::new_array(gl.color_mask.iter().map(|&on| Value::Bool(on)).collect())
            }
            COMPRESSED_TEXTURE_FORMATS => uint_array(&[]),
            CULL_FACE => flag(Capability::CullFace),
            CULL_FACE_MODE => number(gl.cull_face),
            DEPTH_CLEAR_VALUE => number(gl.clear_depth),
            DEPTH_FUNC => number(gl.depth_func),
            DEPTH_RANGE => float_array(&gl.depth_range),
            DEPTH_TEST => flag(Capability::DepthTest),
            DEPTH_WRITEMASK => Value::Bool(gl.depth_mask),
            DITHER => flag(Capability::Dither),
            FRONT_FACE => number(gl.front_face),
            GENERATE_MIPMAP_HINT => number(gl.hints[0]),
            FRAGMENT_SHADER_DERIVATIVE_HINT if webgl2 || has("OES_standard_derivatives") => {
                number(gl.hints[1])
            }
            IMPLEMENTATION_COLOR_READ_FORMAT | IMPLEMENTATION_COLOR_READ_TYPE => {
                let read = if gl.read_framebuffer == 0 {
                    Some(gl.color.format)
                } else {
                    let framebuffer = &gl.framebuffers[&gl.read_framebuffer];
                    framebuffer
                        .attachments
                        .get(&framebuffer.read_buffer)
                        .and_then(|attachment| gl.attachment_format(attachment))
                        .map(|found| found.0)
                };
                match read {
                    Some(format) => {
                        let (format_name, kind) = gl.implementation_read(format);
                        number(if parameter == IMPLEMENTATION_COLOR_READ_FORMAT {
                            format_name
                        } else {
                            kind
                        })
                    }
                    None => {
                        drop(state);
                        context.borrow_mut().gl.error(INVALID_OPERATION);
                        return Ok(Value::Null);
                    }
                }
            }
            LINE_WIDTH => number(gl.line_width),
            MAX_COMBINED_TEXTURE_IMAGE_UNITS => number(context::TEXTURE_UNITS as u32),
            MAX_CUBE_MAP_TEXTURE_SIZE => number(context::MAX_CUBE_SIZE),
            MAX_FRAGMENT_UNIFORM_VECTORS => number(1024),
            MAX_RENDERBUFFER_SIZE => number(context::MAX_RENDERBUFFER),
            MAX_TEXTURE_IMAGE_UNITS | MAX_VERTEX_TEXTURE_IMAGE_UNITS => number(16),
            MAX_TEXTURE_SIZE => number(context::MAX_TEXTURE_SIZE_VALUE),
            MAX_VARYING_VECTORS => number(30),
            MAX_VERTEX_ATTRIBS => number(context::VERTEX_ATTRIBS as u32),
            MAX_VERTEX_UNIFORM_VECTORS => number(4096),
            MAX_VIEWPORT_DIMS => int_array(&[context::MAX_VIEWPORT as i32; 2]),
            PACK_ALIGNMENT => number(gl.pack.alignment),
            UNPACK_ALIGNMENT => number(gl.unpack.alignment),
            POLYGON_OFFSET_FACTOR => number(gl.polygon_offset[0]),
            POLYGON_OFFSET_FILL => flag(Capability::PolygonOffsetFill),
            POLYGON_OFFSET_UNITS => number(gl.polygon_offset[1]),
            RENDERER => Value::String("WebKit WebGL".into()),
            VENDOR => Value::String("WebKit".into()),
            VERSION => Value::String(
                if webgl2 {
                    "WebGL 2.0 (OpenGL ES 3.0 ceri)"
                } else {
                    "WebGL 1.0 (OpenGL ES 2.0 ceri)"
                }
                .into(),
            ),
            SHADING_LANGUAGE_VERSION => Value::String(
                if webgl2 {
                    "WebGL GLSL ES 3.00 (OpenGL ES GLSL ES 3.0 ceri)"
                } else {
                    "WebGL GLSL ES 1.0 (OpenGL ES GLSL ES 1.0 ceri)"
                }
                .into(),
            ),
            UNMASKED_VENDOR_WEBGL if has("WEBGL_debug_renderer_info") => {
                Value::String("ceri".into())
            }
            UNMASKED_RENDERER_WEBGL if has("WEBGL_debug_renderer_info") => {
                Value::String("ceri software rasterizer".into())
            }
            MAX_TEXTURE_MAX_ANISOTROPY_EXT if has("EXT_texture_filter_anisotropic") => number(16),
            SAMPLE_BUFFERS => number(0),
            SAMPLES => number(0),
            SAMPLE_ALPHA_TO_COVERAGE => flag(Capability::SampleAlphaToCoverage),
            SAMPLE_COVERAGE => flag(Capability::SampleCoverage),
            SAMPLE_COVERAGE_VALUE => number(gl.sample_coverage.0),
            SAMPLE_COVERAGE_INVERT => Value::Bool(gl.sample_coverage.1),
            SCISSOR_BOX => int_array(&gl.scissor),
            SCISSOR_TEST => flag(Capability::ScissorTest),
            STENCIL_TEST => flag(Capability::StencilTest),
            STENCIL_CLEAR_VALUE => number(gl.clear_stencil),
            STENCIL_FUNC => number(stencil[0].func),
            STENCIL_FAIL => number(stencil[0].fail),
            STENCIL_PASS_DEPTH_FAIL => number(stencil[0].depth_fail),
            STENCIL_PASS_DEPTH_PASS => number(stencil[0].pass),
            STENCIL_REF => number(stencil[0].reference),
            STENCIL_VALUE_MASK => number(stencil[0].value_mask),
            STENCIL_WRITEMASK => number(stencil[0].write_mask),
            STENCIL_BACK_FUNC => number(stencil[1].func),
            STENCIL_BACK_FAIL => number(stencil[1].fail),
            STENCIL_BACK_PASS_DEPTH_FAIL => number(stencil[1].depth_fail),
            STENCIL_BACK_PASS_DEPTH_PASS => number(stencil[1].pass),
            STENCIL_BACK_REF => number(stencil[1].reference),
            STENCIL_BACK_VALUE_MASK => number(stencil[1].value_mask),
            STENCIL_BACK_WRITEMASK => number(stencil[1].write_mask),
            SUBPIXEL_BITS => number(8),
            UNPACK_COLORSPACE_CONVERSION_WEBGL => number(gl.unpack_colorspace),
            UNPACK_FLIP_Y_WEBGL => Value::Bool(gl.unpack_flip),
            UNPACK_PREMULTIPLY_ALPHA_WEBGL => Value::Bool(gl.unpack_premultiply),
            VIEWPORT => int_array(&gl.viewport),
            MAX_COLOR_ATTACHMENTS | MAX_DRAW_BUFFERS if webgl2 || has("WEBGL_draw_buffers") => {
                number(context::COLOR_ATTACHMENTS as u32)
            }
            DRAW_BUFFER0..=DRAW_BUFFER15 if webgl2 || has("WEBGL_draw_buffers") => {
                if parameter - DRAW_BUFFER0 >= context::COLOR_ATTACHMENTS as u32 {
                    drop(state);
                    context.borrow_mut().gl.error(INVALID_ENUM);
                    return Ok(Value::Null);
                }
                number(draw_buffer(parameter - DRAW_BUFFER0))
            }
            MAX_3D_TEXTURE_SIZE if webgl2 => number(context::MAX_3D_SIZE),
            MAX_ARRAY_TEXTURE_LAYERS if webgl2 => number(context::MAX_LAYERS),
            MAX_CLIENT_WAIT_TIMEOUT_WEBGL if webgl2 => number(0),
            MAX_COMBINED_FRAGMENT_UNIFORM_COMPONENTS if webgl2 => number(4096 + 12 * 16384),
            MAX_COMBINED_UNIFORM_BLOCKS if webgl2 => number(24),
            MAX_COMBINED_VERTEX_UNIFORM_COMPONENTS if webgl2 => number(16384 + 12 * 16384),
            MAX_ELEMENT_INDEX if webgl2 => number(u32::MAX),
            MAX_ELEMENTS_INDICES | MAX_ELEMENTS_VERTICES if webgl2 => number(i32::MAX),
            MAX_FRAGMENT_INPUT_COMPONENTS
            | MAX_VARYING_COMPONENTS
            | MAX_VERTEX_OUTPUT_COMPONENTS
                if webgl2 =>
            {
                number(120)
            }
            MAX_FRAGMENT_UNIFORM_BLOCKS | MAX_VERTEX_UNIFORM_BLOCKS if webgl2 => number(12),
            MAX_FRAGMENT_UNIFORM_COMPONENTS if webgl2 => number(4096),
            MAX_VERTEX_UNIFORM_COMPONENTS if webgl2 => number(16384),
            MAX_PROGRAM_TEXEL_OFFSET if webgl2 => number(7),
            MIN_PROGRAM_TEXEL_OFFSET if webgl2 => number(-8),
            MAX_SAMPLES if webgl2 => number(4),
            MAX_SERVER_WAIT_TIMEOUT if webgl2 => number(0),
            MAX_TEXTURE_LOD_BIAS if webgl2 => number(2),
            MAX_TRANSFORM_FEEDBACK_INTERLEAVED_COMPONENTS if webgl2 => number(64),
            MAX_TRANSFORM_FEEDBACK_SEPARATE_ATTRIBS
            | MAX_TRANSFORM_FEEDBACK_SEPARATE_COMPONENTS
                if webgl2 =>
            {
                number(4)
            }
            MAX_UNIFORM_BLOCK_SIZE if webgl2 => number(65536),
            MAX_UNIFORM_BUFFER_BINDINGS if webgl2 => {
                number(context::UNIFORM_BUFFER_BINDINGS as u32)
            }
            UNIFORM_BUFFER_OFFSET_ALIGNMENT if webgl2 => number(256),
            PACK_ROW_LENGTH if webgl2 => number(gl.pack.row_length),
            PACK_SKIP_PIXELS if webgl2 => number(gl.pack.skip_pixels),
            PACK_SKIP_ROWS if webgl2 => number(gl.pack.skip_rows),
            UNPACK_ROW_LENGTH if webgl2 => number(gl.unpack.row_length),
            UNPACK_IMAGE_HEIGHT if webgl2 => number(gl.unpack.image_height),
            UNPACK_SKIP_PIXELS if webgl2 => number(gl.unpack.skip_pixels),
            UNPACK_SKIP_ROWS if webgl2 => number(gl.unpack.skip_rows),
            UNPACK_SKIP_IMAGES if webgl2 => number(gl.unpack.skip_images),
            RASTERIZER_DISCARD if webgl2 => flag(Capability::RasterizerDiscard),
            READ_BUFFER if webgl2 => number(if gl.read_framebuffer == 0 {
                gl.default_read_buffer
            } else {
                gl.framebuffers[&gl.read_framebuffer].read_buffer
            }),
            TRANSFORM_FEEDBACK_ACTIVE if webgl2 => Value::Bool(gl.feedbacks[&gl.feedback].active),
            TRANSFORM_FEEDBACK_PAUSED if webgl2 => Value::Bool(gl.feedbacks[&gl.feedback].paused),
            _ => {
                drop(state);
                context.borrow_mut().gl.error(INVALID_ENUM);
                return Ok(Value::Null);
            }
        };
        Ok(value)
    }
}

// a shader leaves a program; one waiting on deletion goes once nothing holds it
fn detach_shader(gl: &mut Context, shader: u32) {
    if let Some(found) = gl.shaders.get_mut(&shader) {
        found.attached = found.attached.saturating_sub(1);
        if found.attached == 0 && found.delete_pending {
            gl.shaders.remove(&shader);
        }
    }
}

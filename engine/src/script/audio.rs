// web audio over the engine mixer: contexts, gain and buffer source nodes, and buffers decoded
// off the script thread

use std::sync::mpsc::{Receiver, Sender, channel};

use super::object::{JsObject, ObjectClass, ObjectHandle, Property, Slot, new_object};
use super::realm::{Builder, Native};
use super::typed::ElementKind;
use super::*;
use crate::audio::{AudioDevice, AudioOutput, AudioRender, Clock, Voice, decode};
use crate::bridge::Waker;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum AudioClass {
    BaseContext,
    Context,
    Node,
    Destination,
    Gain,
    ScheduledSource,
    BufferSource,
    Param,
    Buffer,
}

// in order, so a parent's prototype exists before its children
const INTERFACES: &[(AudioClass, &str, Option<AudioClass>, u32)] = &[
    (AudioClass::BaseContext, "BaseAudioContext", None, 0),
    (
        AudioClass::Context,
        "AudioContext",
        Some(AudioClass::BaseContext),
        0,
    ),
    (AudioClass::Node, "AudioNode", None, 0),
    (
        AudioClass::Destination,
        "AudioDestinationNode",
        Some(AudioClass::Node),
        0,
    ),
    (AudioClass::Gain, "GainNode", Some(AudioClass::Node), 1),
    (
        AudioClass::ScheduledSource,
        "AudioScheduledSourceNode",
        Some(AudioClass::Node),
        0,
    ),
    (
        AudioClass::BufferSource,
        "AudioBufferSourceNode",
        Some(AudioClass::ScheduledSource),
        1,
    ),
    (AudioClass::Param, "AudioParam", None, 0),
    (AudioClass::Buffer, "AudioBuffer", None, 1),
];

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum AudioMember {
    SampleRate,
    CurrentTime,
    State,
    Destination,
    CreateGain,
    CreateBufferSource,
    CreateBuffer,
    DecodeAudioData,
    Resume,
    Suspend,
    Close,
    Connect,
    Disconnect,
    Context,
    NumberOfInputs,
    NumberOfOutputs,
    ChannelCount,
    MaxChannelCount,
    Gain,
    Start,
    Stop,
    Buffer,
    SetBuffer,
    PlaybackRate,
    Loop,
    SetLoop,
    LoopStart,
    SetLoopStart,
    LoopEnd,
    SetLoopEnd,
    ParamValue,
    SetParamValue,
    DefaultValue,
    MinValue,
    MaxValue,
    BufferRate,
    Length,
    Duration,
    NumberOfChannels,
    GetChannelData,
    CopyFromChannel,
    CopyToChannel,
}

#[derive(Clone, Copy)]
enum Member {
    Method(u32),
    Accessor(Option<AudioMember>),
}

const MEMBERS: &[(AudioClass, &str, AudioMember, Member)] = {
    use AudioClass as C;
    use AudioMember as M;
    use Member::{Accessor, Method};
    &[
        (C::BaseContext, "sampleRate", M::SampleRate, Accessor(None)),
        (
            C::BaseContext,
            "currentTime",
            M::CurrentTime,
            Accessor(None),
        ),
        (C::BaseContext, "state", M::State, Accessor(None)),
        (
            C::BaseContext,
            "destination",
            M::Destination,
            Accessor(None),
        ),
        (C::BaseContext, "createGain", M::CreateGain, Method(0)),
        (
            C::BaseContext,
            "createBufferSource",
            M::CreateBufferSource,
            Method(0),
        ),
        (C::BaseContext, "createBuffer", M::CreateBuffer, Method(3)),
        (
            C::BaseContext,
            "decodeAudioData",
            M::DecodeAudioData,
            Method(1),
        ),
        (C::Context, "resume", M::Resume, Method(0)),
        (C::Context, "suspend", M::Suspend, Method(0)),
        (C::Context, "close", M::Close, Method(0)),
        (C::Node, "connect", M::Connect, Method(1)),
        (C::Node, "disconnect", M::Disconnect, Method(0)),
        (C::Node, "context", M::Context, Accessor(None)),
        (C::Node, "numberOfInputs", M::NumberOfInputs, Accessor(None)),
        (
            C::Node,
            "numberOfOutputs",
            M::NumberOfOutputs,
            Accessor(None),
        ),
        (C::Node, "channelCount", M::ChannelCount, Accessor(None)),
        (
            C::Destination,
            "maxChannelCount",
            M::MaxChannelCount,
            Accessor(None),
        ),
        (C::Gain, "gain", M::Gain, Accessor(None)),
        (C::ScheduledSource, "start", M::Start, Method(0)),
        (C::ScheduledSource, "stop", M::Stop, Method(0)),
        (
            C::BufferSource,
            "buffer",
            M::Buffer,
            Accessor(Some(M::SetBuffer)),
        ),
        (
            C::BufferSource,
            "playbackRate",
            M::PlaybackRate,
            Accessor(None),
        ),
        (C::BufferSource, "loop", M::Loop, Accessor(Some(M::SetLoop))),
        (
            C::BufferSource,
            "loopStart",
            M::LoopStart,
            Accessor(Some(M::SetLoopStart)),
        ),
        (
            C::BufferSource,
            "loopEnd",
            M::LoopEnd,
            Accessor(Some(M::SetLoopEnd)),
        ),
        (
            C::Param,
            "value",
            M::ParamValue,
            Accessor(Some(M::SetParamValue)),
        ),
        (C::Param, "defaultValue", M::DefaultValue, Accessor(None)),
        (C::Param, "minValue", M::MinValue, Accessor(None)),
        (C::Param, "maxValue", M::MaxValue, Accessor(None)),
        (C::Buffer, "sampleRate", M::BufferRate, Accessor(None)),
        (C::Buffer, "length", M::Length, Accessor(None)),
        (C::Buffer, "duration", M::Duration, Accessor(None)),
        (
            C::Buffer,
            "numberOfChannels",
            M::NumberOfChannels,
            Accessor(None),
        ),
        (C::Buffer, "getChannelData", M::GetChannelData, Method(1)),
        (C::Buffer, "copyFromChannel", M::CopyFromChannel, Method(2)),
        (C::Buffer, "copyToChannel", M::CopyToChannel, Method(2)),
    ]
};

pub(super) struct AudioInterfaces(Vec<(AudioClass, ObjectHandle)>);

impl AudioInterfaces {
    fn prototype(&self, class: AudioClass) -> ObjectHandle {
        self.0
            .iter()
            .find(|(each, _)| *each == class)
            .map(|(_, prototype)| prototype.clone())
            .expect("every audio interface has a prototype")
    }
}

pub(super) fn create(
    build: &Builder,
    event_target: &ObjectHandle,
    object_prototype: &ObjectHandle,
    globals: &mut Vec<(&'static str, Value)>,
) -> AudioInterfaces {
    let mut prototypes: Vec<(AudioClass, ObjectHandle)> = Vec::new();
    for &(class, name, parent, length) in INTERFACES {
        let base = match parent {
            Some(parent) => AudioInterfaces(prototypes.clone()).prototype(parent),
            None if matches!(class, AudioClass::Param | AudioClass::Buffer) => {
                object_prototype.clone()
            }
            None => event_target.clone(),
        };
        let prototype = new_object(JsObject::with_prototype(Some(base), ObjectClass::Ordinary));
        for &(_, key, member, kind) in MEMBERS.iter().filter(|entry| entry.0 == class) {
            let function = |native: AudioMember, name: &str, length: u32| {
                Value::Object(build.function(Native::Audio(native), name, length))
            };
            let property = match kind {
                Member::Method(length) => Property {
                    enumerable: true,
                    ..Property::data(function(member, key, length))
                },
                Member::Accessor(setter) => Property {
                    slot: Slot::Accessor {
                        get: Some(function(member, &format!("get {key}"), 0)),
                        set: setter.map(|setter| function(setter, &format!("set {key}"), 1)),
                    },
                    enumerable: true,
                    writable: false,
                    configurable: true,
                },
            };
            prototype.borrow_mut().define(key.to_string(), property);
        }
        prototype.borrow_mut().define(
            "@@toStringTag".to_string(),
            Property::hidden(Value::String(name.into())),
        );
        let constructor =
            build.constructor(Native::AudioConstructor(class), name, length, &prototype);
        globals.push((name, Value::Object(constructor)));
        prototypes.push((class, prototype));
    }
    AudioInterfaces(prototypes)
}

pub(super) fn class_name(class: AudioClass) -> &'static str {
    INTERFACES
        .iter()
        .find(|entry| entry.0 == class)
        .map_or("AudioNode", |entry| entry.1)
}

fn inherits(class: AudioClass, ancestor: AudioClass) -> bool {
    let mut current = Some(class);
    while let Some(each) = current {
        if each == ancestor {
            return true;
        }
        current = INTERFACES
            .iter()
            .find(|entry| entry.0 == each)
            .and_then(|entry| entry.2);
    }
    false
}

fn owner(member: AudioMember) -> AudioClass {
    MEMBERS
        .iter()
        .find(|entry| {
            entry.2 == member
                || matches!(entry.3, Member::Accessor(Some(setter)) if setter == member)
        })
        .map(|entry| entry.0)
        .expect("every audio member is in the table")
}

fn object_id(object: &ObjectHandle) -> Option<u32> {
    match object.borrow().class {
        ObjectClass::EventTarget(id) => Some(id),
        _ => None,
    }
}

fn audio_id(value: &Value) -> Option<u32> {
    match value {
        Value::Object(object) => object_id(object),
        _ => None,
    }
}

fn to_u32(number: f64) -> u32 {
    if number.is_finite() {
        number.trunc().rem_euclid(4_294_967_296.0) as u32
    } else {
        0
    }
}

fn read_floats(bytes: &[u8]) -> Vec<f32> {
    bytes
        .as_chunks::<4>()
        .0
        .iter()
        .map(|bytes| f32::from_le_bytes(*bytes))
        .collect()
}

fn write_floats(bytes: &mut [u8], samples: &[f32]) {
    for (slot, sample) in bytes.as_chunks_mut::<4>().0.iter_mut().zip(samples) {
        *slot = sample.to_le_bytes();
    }
}

type Decoded = (u64, Option<Vec<Vec<f32>>>);

#[derive(Default)]
pub(super) struct AudioState {
    device: Option<Rc<dyn AudioDevice>>,
    waker: Option<Waker>,
    objects: HashMap<u32, AudioObject>,
    decodes: Vec<Decoding>,
    next_decode: u64,
    finished: Option<(Sender<Decoded>, Receiver<Decoded>)>,
}

struct Decoding {
    key: u64,
    promise: PromiseHandle,
    success: Value,
    failure: Value,
    rate: u32,
}

enum AudioObject {
    Context(ContextState),
    Node(NodeState),
    Param(ParamState),
    Buffer(BufferState),
}

struct ContextState {
    render: AudioRender,
    output: Option<Box<dyn AudioOutput>>,
    rate: u32,
    state: &'static str,
    destination: ObjectHandle,
    playing: Vec<u32>,
}

struct NodeState {
    context: u32,
    kind: NodeKind,
    // the nodes this one feeds, kept alive by the connection as in a browser
    outputs: Vec<(u32, ObjectHandle)>,
}

enum NodeKind {
    Destination,
    Gain(ObjectHandle),
    Source(SourceState),
}

struct SourceState {
    buffer: Option<ObjectHandle>,
    // once set to a buffer, a source never takes another
    had_buffer: bool,
    rate: ObjectHandle,
    looped: bool,
    loop_start: f64,
    loop_end: f64,
    // when, offset and duration from start(), in seconds
    start: Option<(f64, f64, Option<f64>)>,
    stop: Option<f64>,
    playing: bool,
}

struct ParamState {
    value: f32,
    default: f32,
    context: u32,
}

// the samples are the buffer's own; a channel's Float32Array exists only while script holds it
struct BufferState {
    rate: f32,
    length: usize,
    channels: Vec<Arc<[f32]>>,
    views: Vec<Option<TypedArrayHandle>>,
}

impl AudioObject {
    fn class(&self) -> AudioClass {
        match self {
            Self::Context(_) => AudioClass::Context,
            Self::Node(node) => match node.kind {
                NodeKind::Destination => AudioClass::Destination,
                NodeKind::Gain(_) => AudioClass::Gain,
                NodeKind::Source(_) => AudioClass::BufferSource,
            },
            Self::Param(_) => AudioClass::Param,
            Self::Buffer(_) => AudioClass::Buffer,
        }
    }
}

impl BufferState {
    fn new(rate: f32, channels: Vec<Vec<f32>>) -> Self {
        Self {
            rate,
            length: channels.first().map_or(0, Vec::len),
            views: vec![None; channels.len()],
            channels: channels.into_iter().map(Arc::from).collect(),
        }
    }

    // takes what script wrote through the channel's view
    fn sync(&mut self, channel: usize) {
        let Some(view) = &self.views[channel] else {
            return;
        };
        let view = view.borrow();
        let bytes = view.buffer.borrow();
        let Some(bytes) = bytes.get(view.start..view.end) else {
            return;
        };
        let samples = &self.channels[channel];
        if bytes.len() != samples.len() * 4
            || samples
                .iter()
                .zip(bytes.as_chunks::<4>().0)
                .all(|(sample, bytes)| sample.to_le_bytes() == *bytes)
        {
            return;
        }
        self.channels[channel] = read_floats(bytes).into();
    }

    fn view(&mut self, channel: usize) -> TypedArrayHandle {
        if let Some(view) = &self.views[channel] {
            return view.clone();
        }
        let bytes: Vec<u8> = self.channels[channel]
            .iter()
            .flat_map(|sample| sample.to_le_bytes())
            .collect();
        let end = bytes.len();
        let Value::TypedArray(view) =
            typed::new_typed(ElementKind::Float32, Rc::new(RefCell::new(bytes)), 0, end)
        else {
            unreachable!("new_typed makes typed arrays")
        };
        self.views[channel] = Some(view.clone());
        view
    }
}

impl AudioState {
    fn get(&self, id: u32) -> Option<&AudioObject> {
        self.objects.get(&id)
    }

    fn context(&mut self, id: u32) -> &mut ContextState {
        match self.objects.get_mut(&id) {
            Some(AudioObject::Context(context)) => context,
            _ => unreachable!("checked against the member's interface"),
        }
    }

    fn node(&mut self, id: u32) -> &mut NodeState {
        match self.objects.get_mut(&id) {
            Some(AudioObject::Node(node)) => node,
            _ => unreachable!("checked against the member's interface"),
        }
    }

    fn source(&mut self, id: u32) -> &mut SourceState {
        match &mut self.node(id).kind {
            NodeKind::Source(source) => source,
            _ => unreachable!("checked against the member's interface"),
        }
    }

    fn param(&mut self, id: u32) -> &mut ParamState {
        match self.objects.get_mut(&id) {
            Some(AudioObject::Param(param)) => param,
            _ => unreachable!("checked against the member's interface"),
        }
    }

    fn buffer(&mut self, id: u32) -> &mut BufferState {
        match self.objects.get_mut(&id) {
            Some(AudioObject::Buffer(buffer)) => buffer,
            _ => unreachable!("checked against the member's interface"),
        }
    }

    fn param_value(&self, param: &ObjectHandle) -> f32 {
        match object_id(param).and_then(|id| self.get(id)) {
            Some(AudioObject::Param(param)) => param.value,
            _ => 0.0,
        }
    }

    // how loud a node reaches the speakers: every path's gains multiplied, the paths summed
    fn route_gain(&self, node: u32, depth: usize) -> f32 {
        let Some(AudioObject::Node(state)) = self.get(node) else {
            return 0.0;
        };
        let own = match &state.kind {
            NodeKind::Destination => return 1.0,
            NodeKind::Gain(param) => self.param_value(param),
            NodeKind::Source(_) => 1.0,
        };
        if depth == 64 {
            return 0.0;
        }
        own * state
            .outputs
            .iter()
            .map(|(output, _)| self.route_gain(*output, depth + 1))
            .sum::<f32>()
    }

    // a playing source's speed in buffer frames per context frame and its looped stretch
    fn voice_shape(&self, source: u32, rate: u32) -> Option<(f64, Option<(f64, f64)>)> {
        let Some(AudioObject::Node(NodeState {
            kind: NodeKind::Source(source),
            ..
        })) = self.get(source)
        else {
            return None;
        };
        let buffer = object_id(source.buffer.as_ref()?)?;
        let Some(AudioObject::Buffer(buffer)) = self.get(buffer) else {
            return None;
        };
        let buffer_rate = f64::from(buffer.rate);
        let step = f64::from(self.param_value(&source.rate)) * buffer_rate / f64::from(rate);
        let duration = buffer.length as f64 / buffer_rate;
        let looped = source.looped.then(|| {
            let start = if (0.0..duration).contains(&source.loop_start) {
                source.loop_start
            } else {
                0.0
            };
            let end = if source.loop_end > 0.0 && source.loop_end <= duration {
                source.loop_end
            } else {
                duration
            };
            if start < end {
                (start * buffer_rate, end * buffer_rate)
            } else {
                (0.0, buffer.length as f64)
            }
        });
        Some((step, looped))
    }

    fn open(&self, rate: Option<u32>, render: &AudioRender) -> Box<dyn AudioOutput> {
        self.device
            .as_ref()
            .and_then(|device| device.open(rate, render.clone()).ok())
            .unwrap_or_else(|| Box::new(Clock::start(rate.unwrap_or(48_000), render.clone())))
    }
}

impl ScriptRuntime {
    pub(crate) fn set_audio(&mut self, device: Option<Rc<dyn AudioDevice>>, waker: Option<Waker>) {
        self.audio.device = device;
        self.audio.waker = waker;
    }

    pub(super) fn prune_audio(&mut self) {
        let live = &self.event_targets;
        self.audio.objects.retain(|id, _| live.contains_key(id));
    }

    fn audio_error(&mut self, name: &str, message: &str) -> ScriptError {
        self.throw_value(dom_exception(name, message))
    }

    fn audio_prototype(
        &mut self,
        class: AudioClass,
        new_target: &Value,
    ) -> Result<ObjectHandle, ScriptError> {
        match new_target {
            Value::Object(_) => self.prototype_from_constructor(new_target),
            _ => Ok(self.intrinsics.audio.prototype(class)),
        }
    }

    fn finite(&mut self, value: Value, kind: &str) -> Result<f64, ScriptError> {
        let number = self.coerce_number(value)?;
        if number.is_finite() {
            Ok(number)
        } else {
            Err(self.type_error(format!("The provided {kind} value is non-finite.")))
        }
    }

    fn optional_finite(&mut self, value: Value, default: f64) -> Result<f64, ScriptError> {
        match value {
            Value::Undefined => Ok(default),
            value => self.finite(value, "double"),
        }
    }

    fn new_audio_object(&mut self, class: AudioClass, state: AudioObject) -> ObjectHandle {
        let prototype = self.intrinsics.audio.prototype(class);
        self.new_audio_object_with(prototype, state)
    }

    fn new_audio_object_with(
        &mut self,
        prototype: ObjectHandle,
        state: AudioObject,
    ) -> ObjectHandle {
        let object = self.new_event_target(prototype);
        let id = object_id(&object).expect("event targets have ids");
        self.audio.objects.insert(id, state);
        object
    }

    fn new_param(&mut self, context: u32) -> ObjectHandle {
        self.new_audio_object(
            AudioClass::Param,
            AudioObject::Param(ParamState {
                value: 1.0,
                default: 1.0,
                context,
            }),
        )
    }

    fn new_node(
        &mut self,
        context: u32,
        class: AudioClass,
        prototype: ObjectHandle,
    ) -> ObjectHandle {
        let param = self.new_param(context);
        let kind = match class {
            AudioClass::Gain => NodeKind::Gain(param),
            _ => NodeKind::Source(SourceState {
                buffer: None,
                had_buffer: false,
                rate: param,
                looped: false,
                loop_start: 0.0,
                loop_end: 0.0,
                start: None,
                stop: None,
                playing: false,
            }),
        };
        self.new_audio_object_with(
            prototype,
            AudioObject::Node(NodeState {
                context,
                kind,
                outputs: Vec::new(),
            }),
        )
    }

    fn new_buffer(
        &mut self,
        prototype: ObjectHandle,
        channels: u32,
        length: u32,
        rate: f64,
    ) -> Result<Value, ScriptError> {
        if !(1..=32).contains(&channels) {
            return Err(self.audio_error(
                "NotSupportedError",
                &format!(
                    "The number of channels provided ({channels}) is outside the range [1, 32]."
                ),
            ));
        }
        if length == 0 {
            return Err(self.audio_error(
                "NotSupportedError",
                "The number of frames provided (0) is less than or equal to the minimum bound (0).",
            ));
        }
        self.check_rate(rate)?;
        let channels = vec![vec![0.0; length as usize]; channels as usize];
        let state = AudioObject::Buffer(BufferState::new(rate as f32, channels));
        Ok(Value::Object(self.new_audio_object_with(prototype, state)))
    }

    fn check_rate(&mut self, rate: f64) -> Result<(), ScriptError> {
        if (3000.0..=768_000.0).contains(&rate) {
            return Ok(());
        }
        Err(self.audio_error(
            "NotSupportedError",
            &format!("The sample rate provided ({rate}) is outside the range [3000, 768000]."),
        ))
    }

    fn option(&mut self, options: &Value, key: &str) -> Result<Value, ScriptError> {
        match options {
            Value::Undefined | Value::Null => Ok(Value::Undefined),
            options => self.get_property(options, key),
        }
    }

    pub(super) fn construct_audio(
        &mut self,
        class: AudioClass,
        arguments: Vec<Value>,
        new_target: Value,
    ) -> Result<Value, ScriptError> {
        let argument = |index: usize| arguments.get(index).cloned().unwrap_or(Value::Undefined);
        let prototype = self.audio_prototype(class, &new_target)?;
        match class {
            AudioClass::Context => self.construct_context(prototype, argument(0)),
            AudioClass::Buffer => {
                let options = argument(0);
                let channels = match self.option(&options, "numberOfChannels")? {
                    Value::Undefined => 1,
                    value => to_u32(self.coerce_number(value)?),
                };
                let required =
                    |runtime: &mut Self, key: &str| match runtime.option(&options, key)? {
                        Value::Undefined => Err(runtime.type_error(format!(
                            "Failed to construct 'AudioBuffer': required member {key} is undefined."
                        ))),
                        value => runtime.coerce_number(value),
                    };
                let length = to_u32(required(self, "length")?);
                let rate = required(self, "sampleRate")?;
                self.new_buffer(prototype, channels, length, rate)
            }
            AudioClass::Gain | AudioClass::BufferSource => {
                let context = audio_id(&argument(0)).filter(|id| {
                    self.audio
                        .get(*id)
                        .is_some_and(|object| inherits(object.class(), AudioClass::BaseContext))
                });
                let Some(context) = context else {
                    return Err(self.type_error(format!(
                        "Failed to construct '{}': parameter 1 is not of type 'BaseAudioContext'.",
                        class_name(class)
                    )));
                };
                let node = Value::Object(self.new_node(context, class, prototype));
                let options = argument(1);
                let settings: &[(&str, AudioMember)] = match class {
                    AudioClass::Gain => &[("gain", AudioMember::Gain)],
                    _ => &[
                        ("playbackRate", AudioMember::PlaybackRate),
                        ("buffer", AudioMember::SetBuffer),
                        ("loop", AudioMember::SetLoop),
                        ("loopStart", AudioMember::SetLoopStart),
                        ("loopEnd", AudioMember::SetLoopEnd),
                    ],
                };
                for &(key, member) in settings {
                    let value = self.option(&options, key)?;
                    if matches!(value, Value::Undefined) {
                        continue;
                    }
                    if matches!(member, AudioMember::Gain | AudioMember::PlaybackRate) {
                        let param = self.call_audio(member, node.clone(), Vec::new())?;
                        self.call_audio(AudioMember::SetParamValue, param, vec![value])?;
                    } else {
                        self.call_audio(member, node.clone(), vec![value])?;
                    }
                }
                Ok(node)
            }
            _ => Err(self.type_error("Illegal constructor")),
        }
    }

    fn construct_context(
        &mut self,
        prototype: ObjectHandle,
        options: Value,
    ) -> Result<Value, ScriptError> {
        let rate = match self.option(&options, "sampleRate")? {
            Value::Undefined => None,
            value => Some(self.finite(value, "float")?),
        };
        if let Some(rate) = rate {
            self.check_rate(rate)?;
        }
        let object = self.new_event_target(prototype);
        let id = object_id(&object).expect("event targets have ids");
        let destination = self.new_audio_object(
            AudioClass::Destination,
            AudioObject::Node(NodeState {
                context: id,
                kind: NodeKind::Destination,
                outputs: Vec::new(),
            }),
        );
        let render = AudioRender::default();
        render.set_waker(self.audio.waker.clone());
        let output = self.audio.open(rate.map(|rate| rate as u32), &render);
        render.set_running(true);
        self.audio.objects.insert(
            id,
            AudioObject::Context(ContextState {
                rate: output.sample_rate(),
                render,
                output: Some(output),
                state: "running",
                destination,
                playing: Vec::new(),
            }),
        );
        // a context plays until it is closed, whether or not script still holds it
        self.held_targets.insert(id, object.clone());
        Ok(Value::Object(object))
    }

    pub(super) fn deliver_audio_state(&mut self, id: u32) -> Result<Value, ScriptError> {
        let event = self.new_event(
            "Event",
            "statechange".to_string(),
            false,
            false,
            false,
            true,
        );
        self.dispatch(&event, events::ListenerTarget::Object(id))?;
        Ok(Value::Undefined)
    }

    fn change_context(&mut self, id: u32, member: AudioMember) -> Value {
        let (verb, next) = match member {
            AudioMember::Resume => ("resume", "running"),
            AudioMember::Suspend => ("suspend", "suspended"),
            _ => ("close", "closed"),
        };
        let current = self.audio.context(id).state;
        if current == "closed" {
            let error = dom_exception(
                "InvalidStateError",
                &format!("Cannot {verb} a closed AudioContext."),
            );
            return Value::Promise(self.rejected_promise_value(error));
        }
        if current != next {
            let rate = self.audio.context(id).rate;
            let render = self.audio.context(id).render.clone();
            let output = match next {
                "running" => Some(self.audio.open(Some(rate), &render)),
                _ => None,
            };
            let context = self.audio.context(id);
            context.output = output;
            context.state = next;
            render.set_running(next == "running");
            if next == "closed" {
                render.silence();
                for source in std::mem::take(&mut context.playing) {
                    if let Some(AudioObject::Node(NodeState {
                        kind: NodeKind::Source(source),
                        ..
                    })) = self.audio.objects.get_mut(&source)
                    {
                        source.playing = false;
                    }
                    self.held_targets.remove(&source);
                }
                self.held_targets.remove(&id);
            }
            self.schedule_native_task(Native::AudioStateChange(id));
        }
        Value::Promise(self.fulfilled_promise(Value::Undefined))
    }

    fn refresh_voices(&mut self, context: u32) {
        let Some(AudioObject::Context(state)) = self.audio.get(context) else {
            return;
        };
        let (render, rate) = (state.render.clone(), state.rate);
        for source in state.playing.clone() {
            let gain = self.audio.route_gain(source, 0);
            let Some((step, looped)) = self.audio.voice_shape(source, rate) else {
                continue;
            };
            render.update(source, |voice| {
                voice.gain = gain;
                voice.step = step;
                voice.looped = looped;
            });
        }
    }

    // hands a started source to the mixer once it has a buffer to play
    fn launch(&mut self, id: u32) {
        let context = self.audio.node(id).context;
        let source = self.audio.source(id);
        let (Some((when, offset, duration)), false, Some(buffer)) =
            (source.start, source.playing, source.buffer.clone())
        else {
            return;
        };
        let stop = source.stop;
        let buffer = object_id(&buffer).expect("buffers are event targets");
        let state = self.audio.buffer(buffer);
        for channel in 0..state.channels.len() {
            state.sync(channel);
        }
        let (channels, buffer_rate, length) =
            (state.channels.clone(), f64::from(state.rate), state.length);
        let context_state = self.audio.context(context);
        if context_state.state == "closed" {
            return;
        }
        let (render, context_rate) = (context_state.render.clone(), context_state.rate);
        let rate = f64::from(context_rate);
        let Some((step, looped)) = self.audio.voice_shape(id, context_rate) else {
            return;
        };
        let start = (when * rate).round() as u64;
        let stop = duration
            .map(|duration| {
                start.saturating_add((duration * buffer_rate / step.abs()).round() as u64)
            })
            .into_iter()
            .chain(stop.map(|stop| (stop * rate).round() as u64))
            .min();
        render.play(Voice {
            id,
            channels,
            start,
            stop,
            position: offset * buffer_rate,
            end: length as f64,
            looped,
            step,
            gain: self.audio.route_gain(id, 0),
        });
        self.audio.source(id).playing = true;
        self.audio.context(context).playing.push(id);
        // a playing source sounds to its end even when script lets go of it
        if let Some(object) = self.event_targets.get(&id).and_then(Weak::upgrade) {
            self.held_targets.insert(id, object);
        }
    }

    pub(super) fn pump_audio(&mut self) -> Result<usize, ScriptError> {
        let mut processed = 0;
        let finished: Vec<Decoded> = self
            .audio
            .finished
            .as_ref()
            .map(|(_, receiver)| receiver.try_iter().collect())
            .unwrap_or_default();
        for (key, channels) in finished {
            let Some(index) = self.audio.decodes.iter().position(|each| each.key == key) else {
                continue;
            };
            let decoding = self.audio.decodes.remove(index);
            processed += 1;
            let (callback, outcome) = match channels {
                Some(channels) => {
                    let state =
                        AudioObject::Buffer(BufferState::new(decoding.rate as f32, channels));
                    let buffer = Value::Object(self.new_audio_object(AudioClass::Buffer, state));
                    (decoding.success, PromiseOutcome::Fulfilled(buffer))
                }
                None => {
                    let error = dom_exception("EncodingError", "Unable to decode audio data");
                    (decoding.failure, PromiseOutcome::Rejected(error))
                }
            };
            if is_callable(&callback) {
                let value = match &outcome {
                    PromiseOutcome::Fulfilled(value) | PromiseOutcome::Rejected(value) => {
                        value.clone()
                    }
                    PromiseOutcome::Pending => Value::Undefined,
                };
                self.schedule_callback(callback, vec![value]);
            }
            self.settle_promise(&decoding.promise, outcome);
        }
        let ended: Vec<u32> = self
            .audio
            .objects
            .values()
            .filter_map(|object| match object {
                AudioObject::Context(context) => Some(context.render.take_ended()),
                _ => None,
            })
            .flatten()
            .collect();
        for id in ended {
            processed += 1;
            let Some(AudioObject::Node(NodeState {
                context,
                kind: NodeKind::Source(source),
                ..
            })) = self.audio.objects.get_mut(&id)
            else {
                continue;
            };
            source.playing = false;
            let context = *context;
            if let Some(AudioObject::Context(context)) = self.audio.objects.get_mut(&context) {
                context.playing.retain(|playing| *playing != id);
            }
            let event = self.new_event("Event", "ended".to_string(), false, false, false, true);
            self.dispatch(&event, events::ListenerTarget::Object(id))?;
            self.held_targets.remove(&id);
        }
        // a channel's view goes once script lets go of it, so samples are not held twice
        for object in self.audio.objects.values_mut() {
            let AudioObject::Buffer(buffer) = object else {
                continue;
            };
            for channel in 0..buffer.views.len() {
                let unused = buffer.views[channel].as_ref().is_some_and(|view| {
                    Rc::strong_count(view) == 1 && Rc::strong_count(&view.borrow().buffer) == 1
                });
                if unused {
                    buffer.sync(channel);
                    buffer.views[channel] = None;
                }
            }
        }
        Ok(processed)
    }

    fn decode_audio(&mut self, id: u32, arguments: &[Value]) -> Value {
        let argument = |index: usize| arguments.get(index).cloned().unwrap_or(Value::Undefined);
        let Value::Bytes(bytes) = argument(0) else {
            let error = self.make_error(
                "TypeError",
                "Failed to execute 'decodeAudioData' on 'BaseAudioContext': parameter 1 is not of type 'ArrayBuffer'.",
            );
            return Value::Promise(self.rejected_promise_value(error));
        };
        let bytes = bytes.borrow().clone();
        let rate = self.audio.context(id).rate;
        self.audio.next_decode += 1;
        let key = self.audio.next_decode;
        let sender = self.audio.finished.get_or_insert_with(channel).0.clone();
        let waker = self.audio.waker.clone();
        let promise = Self::pending_promise();
        let spawned = std::thread::Builder::new()
            .name("ceri-audio-decode".to_string())
            .spawn(move || {
                let _ = sender.send((key, decode(&bytes, rate)));
                if let Some(waker) = waker {
                    waker();
                }
            });
        if spawned.is_err() {
            let error = dom_exception("EncodingError", "Unable to decode audio data");
            self.settle_promise(&promise, PromiseOutcome::Rejected(error));
        } else {
            self.audio.decodes.push(Decoding {
                key,
                promise: promise.clone(),
                success: argument(1),
                failure: argument(2),
                rate,
            });
        }
        Value::Promise(promise)
    }

    fn connect_audio(&mut self, id: u32, arguments: &[Value]) -> Result<Value, ScriptError> {
        let target = arguments.first().cloned().unwrap_or(Value::Undefined);
        let target_id = audio_id(&target).filter(|target| {
            matches!(
                self.audio.get(*target),
                Some(AudioObject::Node(_) | AudioObject::Param(_))
            )
        });
        let Some(target_id) = target_id else {
            return Err(self.type_error(
                "Failed to execute 'connect' on 'AudioNode': parameter 1 is not of type 'AudioNode'.",
            ));
        };
        let Some(AudioObject::Node(target_node)) = self.audio.get(target_id) else {
            return Err(self.audio_error(
                "NotSupportedError",
                "Connecting a node to an AudioParam is not supported.",
            ));
        };
        let target_context = target_node.context;
        let inputs = if matches!(target_node.kind, NodeKind::Source(_)) {
            0
        } else {
            1
        };
        let context = self.audio.node(id).context;
        let outputs = self.node_outputs(id);
        let index = |runtime: &mut Self, at: usize| match arguments.get(at) {
            None | Some(Value::Undefined) => Ok(0),
            Some(value) => Ok(to_u32(runtime.coerce_number(value.clone())?)),
        };
        let output = index(self, 1)?;
        if output >= outputs {
            return Err(self.audio_error(
                "IndexSizeError",
                &format!("output index ({output}) exceeds number of outputs ({outputs})."),
            ));
        }
        let input = index(self, 2)?;
        if input >= inputs {
            return Err(self.audio_error(
                "IndexSizeError",
                &format!("input index ({input}) exceeds number of inputs ({inputs})."),
            ));
        }
        if target_context != context {
            return Err(self.audio_error(
                "InvalidAccessError",
                "cannot connect to an AudioNode belonging to a different audio context.",
            ));
        }
        let Value::Object(handle) = &target else {
            unreachable!("audio ids come from objects")
        };
        let node = self.audio.node(id);
        if !node.outputs.iter().any(|(output, _)| *output == target_id) {
            node.outputs.push((target_id, handle.clone()));
        }
        self.refresh_voices(context);
        Ok(target)
    }

    fn node_outputs(&mut self, id: u32) -> u32 {
        match self.audio.node(id).kind {
            NodeKind::Destination => 0,
            _ => 1,
        }
    }

    fn disconnect_audio(&mut self, id: u32, arguments: &[Value]) -> Result<(), ScriptError> {
        let argument = arguments.first().cloned().unwrap_or(Value::Undefined);
        let context = self.audio.node(id).context;
        let target = audio_id(&argument).filter(|target| {
            matches!(
                self.audio.get(*target),
                Some(AudioObject::Node(_) | AudioObject::Param(_))
            )
        });
        match (target, argument) {
            (Some(target), _) => {
                let outputs = &mut self.audio.node(id).outputs;
                let before = outputs.len();
                outputs.retain(|(output, _)| *output != target);
                if outputs.len() == before {
                    return Err(self.audio_error(
                        "InvalidAccessError",
                        "the given destination is not connected.",
                    ));
                }
            }
            (None, Value::Undefined) => self.audio.node(id).outputs.clear(),
            (None, value) => {
                let output = to_u32(self.coerce_number(value)?);
                let outputs = self.node_outputs(id);
                if output >= outputs {
                    return Err(self.audio_error(
                        "IndexSizeError",
                        &format!("output index ({output}) exceeds number of outputs ({outputs})."),
                    ));
                }
                self.audio.node(id).outputs.clear();
            }
        }
        self.refresh_voices(context);
        Ok(())
    }

    fn float32_argument(
        &mut self,
        value: Value,
        method: &str,
    ) -> Result<TypedArrayHandle, ScriptError> {
        match value {
            Value::TypedArray(array) if array.borrow().kind == ElementKind::Float32 => Ok(array),
            _ => Err(self.type_error(format!(
                "Failed to execute '{method}' on 'AudioBuffer': parameter 1 is not of type 'Float32Array'."
            ))),
        }
    }

    fn channel_index(&mut self, id: u32, value: Value) -> Result<usize, ScriptError> {
        let channel = to_u32(self.coerce_number(value)?);
        let channels = self.audio.buffer(id).channels.len();
        if channel as usize >= channels {
            return Err(self.audio_error(
                "IndexSizeError",
                &format!(
                    "The channel index provided ({channel}) is outside the range [0, {channels})."
                ),
            ));
        }
        Ok(channel as usize)
    }

    fn start_index(&mut self, id: u32, value: Value) -> Result<usize, ScriptError> {
        let start = match value {
            Value::Undefined => 0,
            value => to_u32(self.coerce_number(value)?),
        };
        let length = self.audio.buffer(id).length;
        if start as usize > length {
            return Err(self.audio_error(
                "IndexSizeError",
                &format!("The bufferOffset provided ({start}) is greater than the buffer length ({length})."),
            ));
        }
        Ok(start as usize)
    }

    pub(super) fn call_audio(
        &mut self,
        member: AudioMember,
        this: Value,
        arguments: Vec<Value>,
    ) -> Result<Value, ScriptError> {
        let id = audio_id(&this).filter(|id| {
            self.audio
                .get(*id)
                .is_some_and(|object| inherits(object.class(), owner(member)))
        });
        let Some(id) = id else {
            return Err(self.type_error("Illegal invocation"));
        };
        let argument = |index: usize| arguments.get(index).cloned().unwrap_or(Value::Undefined);
        let number = |value: f64| -> Result<Value, ScriptError> { Ok(Value::Number(value)) };
        match member {
            AudioMember::SampleRate => number(f64::from(self.audio.context(id).rate)),
            AudioMember::CurrentTime => {
                let context = self.audio.context(id);
                number(context.render.frames() as f64 / f64::from(context.rate))
            }
            AudioMember::State => Ok(Value::String(self.audio.context(id).state.into())),
            AudioMember::Destination => {
                Ok(Value::Object(self.audio.context(id).destination.clone()))
            }
            AudioMember::CreateGain | AudioMember::CreateBufferSource => {
                let class = match member {
                    AudioMember::CreateGain => AudioClass::Gain,
                    _ => AudioClass::BufferSource,
                };
                let prototype = self.intrinsics.audio.prototype(class);
                Ok(Value::Object(self.new_node(id, class, prototype)))
            }
            AudioMember::CreateBuffer => {
                let channels = to_u32(self.coerce_number(argument(0))?);
                let length = to_u32(self.coerce_number(argument(1))?);
                let rate = self.coerce_number(argument(2))?;
                let prototype = self.intrinsics.audio.prototype(AudioClass::Buffer);
                self.new_buffer(prototype, channels, length, rate)
            }
            AudioMember::DecodeAudioData => Ok(self.decode_audio(id, &arguments)),
            AudioMember::Resume | AudioMember::Suspend | AudioMember::Close => {
                Ok(self.change_context(id, member))
            }
            AudioMember::Connect => self.connect_audio(id, &arguments),
            AudioMember::Disconnect => {
                self.disconnect_audio(id, &arguments)?;
                Ok(Value::Undefined)
            }
            AudioMember::Context => {
                let context = self.audio.node(id).context;
                Ok(self
                    .event_targets
                    .get(&context)
                    .and_then(Weak::upgrade)
                    .map_or(Value::Null, Value::Object))
            }
            AudioMember::NumberOfInputs => number(match self.audio.node(id).kind {
                NodeKind::Source(_) => 0.0,
                _ => 1.0,
            }),
            AudioMember::NumberOfOutputs => number(f64::from(self.node_outputs(id))),
            AudioMember::ChannelCount | AudioMember::MaxChannelCount => number(2.0),
            AudioMember::Gain => match &self.audio.node(id).kind {
                NodeKind::Gain(param) => Ok(Value::Object(param.clone())),
                _ => unreachable!("checked against the member's interface"),
            },
            AudioMember::PlaybackRate => Ok(Value::Object(self.audio.source(id).rate.clone())),
            AudioMember::Start => {
                let when = self.optional_finite(argument(0), 0.0)?;
                let offset = self.optional_finite(argument(1), 0.0)?;
                let duration = match argument(2) {
                    Value::Undefined => None,
                    value => Some(self.finite(value, "double")?),
                };
                if self.audio.source(id).start.is_some() {
                    return Err(self.audio_error(
                        "InvalidStateError",
                        "Failed to execute 'start' on 'AudioBufferSourceNode': cannot call start more than once.",
                    ));
                }
                for (name, value) in [("start time", when), ("offset", offset)]
                    .into_iter()
                    .chain(duration.map(|duration| ("duration", duration)))
                {
                    if value < 0.0 {
                        return Err(self.range_error(format!(
                            "The {name} provided ({value}) is less than the minimum bound (0)."
                        )));
                    }
                }
                self.audio.source(id).start = Some((when, offset, duration));
                self.launch(id);
                Ok(Value::Undefined)
            }
            AudioMember::Stop => {
                let when = self.optional_finite(argument(0), 0.0)?;
                if self.audio.source(id).start.is_none() {
                    return Err(self.audio_error(
                        "InvalidStateError",
                        "Failed to execute 'stop' on 'AudioBufferSourceNode': cannot call stop without calling start first.",
                    ));
                }
                if when < 0.0 {
                    return Err(self.range_error(format!(
                        "The stop time provided ({when}) is less than the minimum bound (0)."
                    )));
                }
                self.audio.source(id).stop = Some(when);
                let context = self.audio.node(id).context;
                let context = self.audio.context(context);
                let frame = (when * f64::from(context.rate)).round() as u64;
                context.render.update(id, |voice| voice.stop = Some(frame));
                Ok(Value::Undefined)
            }
            AudioMember::Buffer => Ok(self
                .audio
                .source(id)
                .buffer
                .clone()
                .map_or(Value::Null, Value::Object)),
            AudioMember::SetBuffer => {
                let buffer = match argument(0) {
                    Value::Null => None,
                    Value::Object(object)
                        if object_id(&object).is_some_and(|buffer| {
                            matches!(self.audio.get(buffer), Some(AudioObject::Buffer(_)))
                        }) =>
                    {
                        Some(object)
                    }
                    _ => {
                        return Err(self.type_error(
                            "Failed to set the 'buffer' property on 'AudioBufferSourceNode': The provided value is not of type 'AudioBuffer'.",
                        ));
                    }
                };
                let source = self.audio.source(id);
                if buffer.is_some() && source.had_buffer {
                    return Err(self.audio_error(
                        "InvalidStateError",
                        "Failed to set the 'buffer' property on 'AudioBufferSourceNode': Cannot set buffer to non-null after it has been already been set to a non-null buffer",
                    ));
                }
                source.had_buffer |= buffer.is_some();
                source.buffer = buffer;
                self.launch(id);
                Ok(Value::Undefined)
            }
            AudioMember::Loop => Ok(Value::Bool(self.audio.source(id).looped)),
            AudioMember::LoopStart => number(self.audio.source(id).loop_start),
            AudioMember::LoopEnd => number(self.audio.source(id).loop_end),
            AudioMember::SetLoop | AudioMember::SetLoopStart | AudioMember::SetLoopEnd => {
                let value = argument(0);
                let looped = value.truthy();
                let seconds = match member {
                    AudioMember::SetLoop => 0.0,
                    _ => self.finite(value, "double")?,
                };
                let source = self.audio.source(id);
                match member {
                    AudioMember::SetLoop => source.looped = looped,
                    AudioMember::SetLoopStart => source.loop_start = seconds,
                    _ => source.loop_end = seconds,
                }
                let context = self.audio.node(id).context;
                self.refresh_voices(context);
                Ok(Value::Undefined)
            }
            AudioMember::ParamValue => number(f64::from(self.audio.param(id).value)),
            AudioMember::SetParamValue => {
                let value = self.finite(argument(0), "float")? as f32;
                let param = self.audio.param(id);
                param.value = value;
                let context = param.context;
                self.refresh_voices(context);
                Ok(Value::Undefined)
            }
            AudioMember::DefaultValue => number(f64::from(self.audio.param(id).default)),
            AudioMember::MinValue => number(f64::from(f32::MIN)),
            AudioMember::MaxValue => number(f64::from(f32::MAX)),
            AudioMember::BufferRate => number(f64::from(self.audio.buffer(id).rate)),
            AudioMember::Length => number(self.audio.buffer(id).length as f64),
            AudioMember::Duration => {
                let buffer = self.audio.buffer(id);
                number(buffer.length as f64 / f64::from(buffer.rate))
            }
            AudioMember::NumberOfChannels => number(self.audio.buffer(id).channels.len() as f64),
            AudioMember::GetChannelData => {
                let channel = self.channel_index(id, argument(0))?;
                Ok(Value::TypedArray(self.audio.buffer(id).view(channel)))
            }
            AudioMember::CopyFromChannel => {
                let target = self.float32_argument(argument(0), "copyFromChannel")?;
                let channel = self.channel_index(id, argument(1))?;
                let start = self.start_index(id, argument(2))?;
                let buffer = self.audio.buffer(id);
                buffer.sync(channel);
                let samples = buffer.channels[channel].clone();
                let target = target.borrow();
                let mut bytes = target.buffer.borrow_mut();
                if let Some(window) = bytes.get_mut(target.start..target.end) {
                    write_floats(window, &samples[start..]);
                }
                Ok(Value::Undefined)
            }
            AudioMember::CopyToChannel => {
                let source = self.float32_argument(argument(0), "copyToChannel")?;
                let channel = self.channel_index(id, argument(1))?;
                let start = self.start_index(id, argument(2))?;
                let samples: Vec<f32> = {
                    let source = source.borrow();
                    let bytes = source.buffer.borrow();
                    read_floats(bytes.get(source.start..source.end).unwrap_or_default())
                };
                let buffer = self.audio.buffer(id);
                buffer.sync(channel);
                let mut data = buffer.channels[channel].to_vec();
                for (slot, sample) in data[start..].iter_mut().zip(&samples) {
                    *slot = *sample;
                }
                if let Some(view) = &buffer.views[channel] {
                    let view = view.borrow();
                    let mut bytes = view.buffer.borrow_mut();
                    if let Some(window) = bytes.get_mut(view.start..view.end) {
                        write_floats(window, &data);
                    }
                }
                buffer.channels[channel] = data.into();
                Ok(Value::Undefined)
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::dom::Document;

    // speakers that play only when the test pulls their frames
    #[derive(Default)]
    struct Pulled(RefCell<Option<AudioRender>>);

    struct Opened(u32);

    impl AudioOutput for Opened {
        fn sample_rate(&self) -> u32 {
            self.0
        }
    }

    impl AudioDevice for Pulled {
        fn open(
            &self,
            sample_rate: Option<u32>,
            render: AudioRender,
        ) -> Result<Box<dyn AudioOutput>, String> {
            *self.0.borrow_mut() = Some(render);
            Ok(Box::new(Opened(sample_rate.unwrap_or(48_000))))
        }
    }

    #[test]
    fn decoded_buffers_play_through_gain_to_the_device_and_end() {
        let mut wav = b"RIFF\0\0\0\0WAVEfmt \x10\0\0\0\x01\0\x02\0\x80\xbb\0\0\0\xee\x02\0\x04\0\x10\0data\x10\0\0\0".to_vec();
        for _ in 0..4 {
            wav.extend(16384i16.to_le_bytes());
            wav.extend((-16384i16).to_le_bytes());
        }
        let bytes = wav.iter().map(u8::to_string).collect::<Vec<_>>().join(",");
        let device = Rc::new(Pulled::default());
        let mut runtime = ScriptRuntime::new(Rc::new(RefCell::new(
            Document::parse_markup("<p>audio</p>").expect("document should parse"),
        )));
        runtime.set_audio(Some(device.clone()), None);
        runtime
            .execute(&format!(
                r#"
                const context = new AudioContext();
                const master = context.createGain();
                console.log(master.connect(context.destination) === context.destination);
                master.gain.value = 0.5;
                context.onstatechange = () => console.log("statechange " + context.state);
                console.log([context.state, context.sampleRate, context.destination.numberOfInputs,
                    master.gain.defaultValue, master.context === context, String(context),
                    context instanceof BaseAudioContext, master instanceof AudioNode].join());
                context.decodeAudioData(new Uint8Array([{bytes}]).buffer).then(buffer => {{
                    (() => {{
                        const data = buffer.getChannelData(0);
                        console.log(["decoded", buffer.numberOfChannels, buffer.length, buffer.sampleRate,
                            buffer.duration, data[1], data === buffer.getChannelData(0)].join());
                        data[0] = 1;
                    }})();
                    const source = context.createBufferSource();
                    source.buffer = buffer;
                    source.connect(master);
                    source.onended = () => {{
                        console.log("ended " + Math.round(context.currentTime * 48000));
                        context.close().then(() => console.log("closed " + context.state));
                    }};
                    source.start(0, 0);
                    try {{ source.start(); }} catch (error) {{ console.log(error.name); }}
                    try {{ source.buffer = buffer; }} catch (error) {{ console.log(error.name); }}
                }});
                context.decodeAudioData(new ArrayBuffer(4)).catch(error => console.log("failed " + error.name));
                try {{ context.createBuffer(0, 1, 48000); }} catch (error) {{ console.log(error.name); }}
                try {{ new GainNode({{}}); }} catch (error) {{ console.log(error.name); }}
                "#
            ))
            .expect("audio script should run");
        for _ in 0..5000 {
            runtime.pump().expect("audio pump should run");
            if runtime.logs().len() >= 8 {
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(1));
        }
        // the two decodes settle in whichever order their threads finish
        let failed = runtime
            .logs()
            .iter()
            .position(|line| line == "failed EncodingError")
            .expect("the bad buffer should fail to decode");
        let mut logs = runtime.logs()[..8].to_vec();
        logs.remove(failed);
        assert_eq!(
            logs,
            [
                "true",
                "running,48000,1,1,true,[object AudioContext],true,true",
                "NotSupportedError",
                "TypeError",
                "decoded,2,4,48000,0.00008333333333333333,0.5,true",
                "InvalidStateError",
                "InvalidStateError",
            ]
        );
        let render = device
            .0
            .borrow()
            .clone()
            .expect("the context should open the device");
        let mut out = vec![0.0; 12];
        render.render(&mut out, 2);
        assert_eq!(
            out,
            [
                0.5, -0.25, 0.25, -0.25, 0.25, -0.25, 0.25, -0.25, 0.0, 0.0, 0.0, 0.0
            ]
        );
        runtime.pump().expect("audio pump should run");
        runtime.advance_time(0).expect("tasks should run");
        assert_eq!(
            runtime.logs()[8..],
            ["ended 6", "closed closed", "statechange closed"]
        );
        let views = runtime
            .audio
            .objects
            .values()
            .filter_map(|object| match object {
                AudioObject::Buffer(buffer) => Some(buffer.views.iter().flatten().count()),
                _ => None,
            });
        assert_eq!(
            views.sum::<usize>(),
            0,
            "channel views go once script lets go"
        );
    }
}

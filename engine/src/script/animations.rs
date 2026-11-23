// Element.animate and the Animation it returns: keyframes from script, sampled with the page's
// style like a css animation and drawn above it, with the timing controls, events and promises
// of the web animations api

use super::object::{JsObject, ObjectClass, ObjectHandle, Property, Slot, new_object};
use super::realm::{Builder, Native};
use super::*;
use crate::css::{ScriptAnimation, ScriptClock, ScriptDirection, ScriptTiming, Timing};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum AnimMember {
    CurrentTime,
    SetCurrentTime,
    StartTime,
    PlayState,
    Pending,
    Id,
    Effect,
    Finished,
    Ready,
    PlaybackRate,
    Play,
    Pause,
    Cancel,
    Finish,
    Reverse,
    Persist,
    CommitStyles,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Phase {
    Running,
    Paused,
    Finished,
    Idle,
}

pub(super) struct AnimInterfaces {
    prototype: ObjectHandle,
}

pub(super) struct AnimState {
    node: NodeId,
    record: ScriptAnimation,
    phase: Phase,
    // bumped whenever the clock is moved, so a finish task set before does nothing
    generation: u64,
    finished: PromiseHandle,
    ready: PromiseHandle,
    id: String,
    order: u64,
}

#[derive(Default)]
pub(super) struct Animations {
    states: HashMap<u32, AnimState>,
    next_order: u64,
}

fn define(target: &ObjectHandle, name: &str, get: ObjectHandle, set: Option<ObjectHandle>) {
    target.borrow_mut().define(
        name.to_string(),
        Property {
            slot: Slot::accessor(Some(Value::Object(get)), set.map(Value::Object)),
            enumerable: true,
            writable: false,
            configurable: true,
        },
    );
}

pub(super) fn create(
    build: &Builder,
    event_target: &ObjectHandle,
    globals: &mut Vec<(&'static str, Value)>,
) -> AnimInterfaces {
    let prototype = new_object(JsObject::with_prototype(
        Some(event_target.clone()),
        ObjectClass::Ordinary,
    ));
    let getter = |name: &str, member: AnimMember| {
        build.function(Native::Anim(member), &format!("get {name}"), 0)
    };
    for (name, member) in [
        ("startTime", AnimMember::StartTime),
        ("playState", AnimMember::PlayState),
        ("pending", AnimMember::Pending),
        ("id", AnimMember::Id),
        ("effect", AnimMember::Effect),
        ("finished", AnimMember::Finished),
        ("ready", AnimMember::Ready),
        ("playbackRate", AnimMember::PlaybackRate),
    ] {
        define(&prototype, name, getter(name, member), None);
    }
    define(
        &prototype,
        "currentTime",
        getter("currentTime", AnimMember::CurrentTime),
        Some(build.function(
            Native::Anim(AnimMember::SetCurrentTime),
            "set currentTime",
            1,
        )),
    );
    for (name, member) in [
        ("play", AnimMember::Play),
        ("pause", AnimMember::Pause),
        ("cancel", AnimMember::Cancel),
        ("finish", AnimMember::Finish),
        ("reverse", AnimMember::Reverse),
        ("persist", AnimMember::Persist),
        ("commitStyles", AnimMember::CommitStyles),
    ] {
        let function = build.function(Native::Anim(member), name, 0);
        prototype.borrow_mut().define(
            name.to_string(),
            Property {
                enumerable: true,
                ..Property::data(Value::Object(function))
            },
        );
    }
    prototype.borrow_mut().define(
        "@@toStringTag".to_string(),
        Property::hidden(Value::String("Animation".into())),
    );
    let constructor = build.constructor(Native::AnimConstructor, "Animation", 0, &prototype);
    globals.push(("Animation", Value::Object(constructor)));
    AnimInterfaces { prototype }
}

// a keyframe member's name as css spells it
fn css_name(name: &str) -> Option<String> {
    match name {
        "offset" | "easing" | "composite" => None,
        "cssFloat" => Some("float".to_string()),
        "cssOffset" => Some("offset".to_string()),
        custom if custom.starts_with("--") => Some(custom.to_string()),
        camel => {
            let mut css = String::with_capacity(camel.len() + 4);
            for character in camel.chars() {
                if character.is_ascii_uppercase() {
                    css.push('-');
                    css.push(character.to_ascii_lowercase());
                } else {
                    css.push(character);
                }
            }
            Some(css)
        }
    }
}

// keyframes as offsets from 0 to 1, each with the css properties and values it sets
type Keyframes = Vec<(f32, Vec<(String, String)>)>;

// offsets a keyframe list leaves out sit evenly between the ones it gives, from 0 to 1
fn spread(offsets: &mut [Option<f32>]) -> Vec<f32> {
    let count = offsets.len();
    if count == 0 {
        return Vec::new();
    }
    if offsets[0].is_none() {
        offsets[0] = Some(if count == 1 { 1.0 } else { 0.0 });
    }
    if offsets[count - 1].is_none() {
        offsets[count - 1] = Some(1.0);
    }
    let mut index = 0;
    while index < count {
        if offsets[index].is_some() {
            index += 1;
            continue;
        }
        let start = index - 1;
        let end = (index..count)
            .find(|at| offsets[*at].is_some())
            .unwrap_or(count - 1);
        let (from, to) = (offsets[start].unwrap_or(0.0), offsets[end].unwrap_or(1.0));
        for (at, slot) in offsets.iter_mut().enumerate().take(end).skip(index) {
            let share = (at - start) as f32 / (end - start) as f32;
            *slot = Some(from + (to - from) * share);
        }
        index = end;
    }
    offsets.iter().map(|offset| offset.unwrap_or(0.0)).collect()
}

impl ScriptRuntime {
    // what style sampling needs of the animations in play, by element and in creation order
    pub(super) fn animation_snapshot(&self) -> BTreeMap<NodeId, Vec<ScriptAnimation>> {
        let mut live: Vec<&AnimState> = self
            .animations
            .states
            .values()
            .filter(|state| match state.phase {
                Phase::Idle => false,
                Phase::Finished => state.record.timing.fill_forwards,
                _ => true,
            })
            .collect();
        live.sort_by_key(|state| state.order);
        let mut snapshot: BTreeMap<NodeId, Vec<ScriptAnimation>> = BTreeMap::new();
        for state in live {
            snapshot
                .entry(state.node)
                .or_default()
                .push(state.record.clone());
        }
        snapshot
    }

    fn read_keyframes(&mut self, source: &Value) -> Result<Keyframes, ScriptError> {
        let text = |runtime: &mut Self, value: Value| -> Result<String, ScriptError> {
            Ok(runtime.coerce_string(value)?.to_string())
        };
        match source.resolve() {
            Value::Undefined | Value::Null => Ok(Vec::new()),
            Value::Array(items) => {
                let items = items.borrow().clone();
                let mut offsets = Vec::new();
                let mut frames = Vec::new();
                for item in items {
                    let offset = match self.get_property(&item, "offset")? {
                        Value::Undefined | Value::Null => None,
                        value => Some(self.coerce_number(value)? as f32),
                    };
                    let mut values = Vec::new();
                    for key in self.own_enumerable_keys(&item)? {
                        if let Some(name) = css_name(&key) {
                            let value = self.get_property(&item, &key)?;
                            values.push((name, text(self, value)?));
                        }
                    }
                    offsets.push(offset);
                    frames.push(values);
                }
                Ok(spread(&mut offsets).into_iter().zip(frames).collect())
            }
            // property-indexed: each property's values spread evenly on their own
            object @ Value::Object(_) => {
                let mut frames: Keyframes = Vec::new();
                for key in self.own_enumerable_keys(&object)? {
                    let Some(name) = css_name(&key) else {
                        continue;
                    };
                    let values = match self.get_property(&object, &key)?.resolve() {
                        Value::Array(values) => values.borrow().to_vec(),
                        single => vec![single],
                    };
                    let mut offsets = vec![None; values.len()];
                    for (offset, value) in spread(&mut offsets).into_iter().zip(values) {
                        let value = text(self, value)?;
                        match frames.iter_mut().find(|(at, _)| (*at - offset).abs() < 1e-6) {
                            Some((_, list)) => list.push((name.clone(), value)),
                            None => frames.push((offset, vec![(name.clone(), value)])),
                        }
                    }
                }
                frames.sort_by(|left, right| left.0.total_cmp(&right.0));
                Ok(frames)
            }
            _ => Err(self.type_error(
                "Failed to execute 'animate' on 'Element': The provided value cannot be converted to a sequence.",
            )),
        }
    }

    fn read_timing(&mut self, source: &Value) -> Result<(ScriptTiming, String), ScriptError> {
        let mut timing = ScriptTiming {
            delay: 0.0,
            duration: 0.0,
            iterations: 1.0,
            direction: ScriptDirection::Normal,
            easing: Timing::Linear,
            fill_backwards: false,
            fill_forwards: false,
        };
        let mut id = String::new();
        let options = source.resolve();
        if let Value::Number(duration) = options {
            timing.duration = duration.max(0.0);
            return Ok((timing, id));
        }
        if !matches!(options, Value::Object(_)) {
            return Ok((timing, id));
        }
        let number = |runtime: &mut Self, name: &str| -> Result<Option<f64>, ScriptError> {
            match runtime.get_property(&options, name)? {
                Value::Undefined => Ok(None),
                Value::String(text) if &*text == "auto" => Ok(Some(0.0)),
                value => runtime.coerce_number(value).map(Some),
            }
        };
        let word = |runtime: &mut Self, name: &str| -> Result<Option<String>, ScriptError> {
            match runtime.get_property(&options, name)? {
                Value::Undefined => Ok(None),
                value => Ok(Some(runtime.coerce_string(value)?.to_string())),
            }
        };
        if let Some(duration) = number(self, "duration")? {
            if duration.is_nan() || duration < 0.0 {
                return Err(self.type_error(
                    "Failed to execute 'animate' on 'Element': duration must be non-negative or auto.",
                ));
            }
            timing.duration = duration;
        }
        timing.delay = number(self, "delay")?.unwrap_or(0.0);
        if let Some(iterations) = number(self, "iterations")? {
            if iterations.is_nan() || iterations < 0.0 {
                return Err(self.type_error(
                    "Failed to execute 'animate' on 'Element': iterations must be non-negative.",
                ));
            }
            timing.iterations = iterations;
        }
        timing.direction = match word(self, "direction")?.as_deref() {
            Some("reverse") => ScriptDirection::Reverse,
            Some("alternate") => ScriptDirection::Alternate,
            Some("alternate-reverse") => ScriptDirection::AlternateReverse,
            _ => ScriptDirection::Normal,
        };
        if let Some(easing) = word(self, "easing")? {
            timing.easing = crate::css::parse_timing(&easing).ok_or_else(|| {
                self.type_error(format!(
                    "Failed to execute 'animate' on 'Element': '{easing}' is not a valid value for easing"
                ))
            })?;
        }
        let fill = word(self, "fill")?;
        timing.fill_backwards = matches!(fill.as_deref(), Some("backwards" | "both"));
        timing.fill_forwards = matches!(fill.as_deref(), Some("forwards" | "both"));
        id = word(self, "id")?.unwrap_or_default();
        Ok((timing, id))
    }

    pub(super) fn animate_element(
        &mut self,
        node: NodeId,
        arguments: &[Value],
    ) -> Result<Value, ScriptError> {
        let keyframes = self.read_keyframes(arguments.first().unwrap_or(&Value::Undefined))?;
        let (timing, id) = self.read_timing(arguments.get(1).unwrap_or(&Value::Undefined))?;
        let object = self.new_event_target(self.intrinsics.animations.prototype.clone());
        let ObjectClass::EventTarget(handle) = object.borrow().class else {
            unreachable!("new_event_target makes event targets")
        };
        let ready = Self::pending_promise();
        self.settle_promise(
            &ready,
            PromiseOutcome::Fulfilled(Value::Object(object.clone())),
        );
        self.animations.next_order += 1;
        self.animations.states.insert(
            handle,
            AnimState {
                node,
                record: ScriptAnimation {
                    keyframes,
                    timing,
                    clock: ScriptClock::Running(self.clock),
                },
                phase: Phase::Running,
                generation: 0,
                finished: Self::pending_promise(),
                ready,
                id,
                order: self.animations.next_order,
            },
        );
        // a running animation stays alive for its finish listeners
        self.held_targets.insert(handle, object.clone());
        self.schedule_finish(handle);
        Ok(Value::Object(object))
    }

    // the animations on an element, or on every element of the page, oldest first
    pub(super) fn animations_of(&self, node: Option<NodeId>) -> Value {
        let mut found: Vec<(u64, u32)> = self
            .animations
            .states
            .iter()
            .filter(|(_, state)| node.is_none_or(|node| state.node == node))
            .filter(|(_, state)| match state.phase {
                Phase::Idle => false,
                Phase::Finished => state.record.timing.fill_forwards,
                _ => true,
            })
            .map(|(id, state)| (state.order, *id))
            .collect();
        found.sort_unstable();
        let objects = found
            .into_iter()
            .filter_map(|(_, id)| self.event_targets.get(&id).and_then(Weak::upgrade))
            .map(Value::Object)
            .collect();
        Self::new_array(objects)
    }

    fn end_time(state: &AnimState) -> f64 {
        let timing = &state.record.timing;
        timing.delay + timing.active_duration()
    }

    // the animation's local time as currentTime reports it
    fn local_time(&self, state: &AnimState) -> f64 {
        match state.phase {
            Phase::Finished => Self::end_time(state),
            _ => state.record.local_time(self.clock),
        }
    }

    // a task at the moment a running animation reaches its end
    fn schedule_finish(&mut self, handle: u32) {
        let Some(state) = self.animations.states.get_mut(&handle) else {
            return;
        };
        state.generation += 1;
        let ScriptClock::Running(start) = state.record.clock else {
            return;
        };
        let end = Self::end_time(state);
        if state.phase != Phase::Running || !end.is_finite() {
            return;
        }
        let due = start
            .saturating_add(end.max(0.0).ceil() as u64)
            .max(self.clock);
        let generation = state.generation;
        let id = self.next_task_id;
        self.next_task_id = self.next_task_id.saturating_add(1);
        self.tasks.push(ScheduledTask {
            id,
            due,
            callback: Value::Builtin(Builtin::Native(Native::AnimFinish(handle, generation))),
            arguments: Vec::new(),
            kind: TaskKind::Timeout,
        });
    }

    pub(super) fn deliver_animation_finish(
        &mut self,
        handle: u32,
        generation: u64,
    ) -> Result<Value, ScriptError> {
        let Some(state) = self.animations.states.get_mut(&handle) else {
            return Ok(Value::Undefined);
        };
        if state.generation != generation || state.phase != Phase::Running {
            return Ok(Value::Undefined);
        }
        state.phase = Phase::Finished;
        let finished = state.finished.clone();
        let object = self
            .event_targets
            .get(&handle)
            .and_then(Weak::upgrade)
            .map_or(Value::Undefined, Value::Object);
        self.settle_promise(&finished, PromiseOutcome::Fulfilled(object));
        self.fire(
            events::ListenerTarget::Object(handle),
            "finish",
            events::HostEventFields::default(),
        )?;
        let _held = self.held_targets.remove(&handle);
        self.drain_microtasks()?;
        Ok(Value::Undefined)
    }

    fn restart_finished_promise(&mut self, handle: u32) {
        if let Some(state) = self.animations.states.get_mut(&handle)
            && !matches!(state.finished.borrow().outcome, PromiseOutcome::Pending)
        {
            state.finished = Self::pending_promise();
        }
    }

    fn hold_animation(&mut self, handle: u32) {
        if let Some(object) = self.event_targets.get(&handle).and_then(Weak::upgrade) {
            self.held_targets.insert(handle, object);
        }
    }

    pub(super) fn call_animation(
        &mut self,
        member: AnimMember,
        this: Value,
        arguments: Vec<Value>,
    ) -> Result<Value, ScriptError> {
        let handle = match &this {
            Value::Object(object) => match object.borrow().class {
                ObjectClass::EventTarget(id) if self.animations.states.contains_key(&id) => {
                    Some(id)
                }
                _ => None,
            },
            _ => None,
        };
        let Some(handle) = handle else {
            return Err(self.type_error("Illegal invocation"));
        };
        let state = &self.animations.states[&handle];
        let phase = state.phase;
        let end = Self::end_time(state);
        let local = self.local_time(state);
        Ok(match member {
            AnimMember::CurrentTime => match phase {
                Phase::Idle => Value::Null,
                _ => Value::Number(local),
            },
            AnimMember::StartTime => match (phase, state.record.clock) {
                (Phase::Running | Phase::Finished, ScriptClock::Running(start)) => {
                    Value::Number(start as f64)
                }
                _ => Value::Null,
            },
            AnimMember::PlayState => Value::String(
                match phase {
                    Phase::Running => "running",
                    Phase::Paused => "paused",
                    Phase::Finished => "finished",
                    Phase::Idle => "idle",
                }
                .into(),
            ),
            AnimMember::Pending => Value::Bool(false),
            AnimMember::Id => Value::String(state.id.clone().into()),
            AnimMember::PlaybackRate => Value::Number(1.0),
            AnimMember::Finished => Value::Promise(state.finished.clone()),
            AnimMember::Ready => Value::Promise(state.ready.clone()),
            AnimMember::Effect => {
                let timing = state.record.timing;
                let target = self.node_value(state.node);
                let progress = timing.progress(local);
                let direction = match timing.direction {
                    ScriptDirection::Normal => "normal",
                    ScriptDirection::Reverse => "reverse",
                    ScriptDirection::Alternate => "alternate",
                    ScriptDirection::AlternateReverse => "alternate-reverse",
                };
                let fill = match (timing.fill_backwards, timing.fill_forwards) {
                    (true, true) => "both",
                    (true, false) => "backwards",
                    (false, true) => "forwards",
                    (false, false) => "none",
                };
                let number = |value: f64| Value::Number(value);
                let timing_object = plain_object([
                    ("delay".to_string(), number(timing.delay)),
                    ("duration".to_string(), number(timing.duration)),
                    ("iterations".to_string(), number(timing.iterations)),
                    ("direction".to_string(), Value::String(direction.into())),
                    ("fill".to_string(), Value::String(fill.into())),
                ]);
                let computed = plain_object([
                    ("delay".to_string(), number(timing.delay)),
                    ("duration".to_string(), number(timing.duration)),
                    (
                        "activeDuration".to_string(),
                        number(timing.active_duration()),
                    ),
                    ("endTime".to_string(), number(end)),
                    ("localTime".to_string(), number(local)),
                    (
                        "progress".to_string(),
                        progress.map_or(Value::Null, |progress| number(f64::from(progress))),
                    ),
                    (
                        "currentIteration".to_string(),
                        if local < timing.delay || timing.duration <= 0.0 {
                            Value::Null
                        } else {
                            number(
                                ((local - timing.delay) / timing.duration)
                                    .floor()
                                    .min((timing.iterations - 1.0).max(0.0)),
                            )
                        },
                    ),
                ]);
                plain_object([
                    ("target".to_string(), target),
                    (
                        "getTiming".to_string(),
                        Value::Builtin(Builtin::ReturnValue(Rc::new(timing_object))),
                    ),
                    (
                        "getComputedTiming".to_string(),
                        Value::Builtin(Builtin::ReturnValue(Rc::new(computed))),
                    ),
                ])
            }
            AnimMember::SetCurrentTime => {
                let time = self.coerce_number(arguments.first().cloned().unwrap_or(Value::Null))?;
                if !time.is_finite() {
                    return Err(self.type_error("currentTime must be a finite number"));
                }
                let clock = self.clock;
                let state = self
                    .animations
                    .states
                    .get_mut(&handle)
                    .expect("checked above");
                state.record.clock = match phase {
                    Phase::Paused => ScriptClock::Held(time),
                    _ => ScriptClock::Running(clock.saturating_sub(time.max(0.0) as u64)),
                };
                if matches!(phase, Phase::Finished | Phase::Idle) {
                    state.phase = if time >= end {
                        Phase::Finished
                    } else {
                        Phase::Running
                    };
                }
                if state.phase == Phase::Running {
                    self.restart_finished_promise(handle);
                    self.hold_animation(handle);
                }
                self.schedule_finish(handle);
                Value::Undefined
            }
            AnimMember::Play => {
                let clock = self.clock;
                let state = self
                    .animations
                    .states
                    .get_mut(&handle)
                    .expect("checked above");
                let from = match phase {
                    Phase::Paused if local < end => local,
                    Phase::Running if local < end => return Ok(Value::Undefined),
                    _ => 0.0,
                };
                state.record.clock = ScriptClock::Running(clock.saturating_sub(from as u64));
                state.phase = Phase::Running;
                self.restart_finished_promise(handle);
                self.hold_animation(handle);
                self.schedule_finish(handle);
                Value::Undefined
            }
            AnimMember::Pause => {
                if matches!(phase, Phase::Running | Phase::Finished | Phase::Idle) {
                    let state = self
                        .animations
                        .states
                        .get_mut(&handle)
                        .expect("checked above");
                    state.record.clock = ScriptClock::Held(match phase {
                        Phase::Idle => 0.0,
                        _ => local,
                    });
                    state.phase = Phase::Paused;
                    state.generation += 1;
                }
                Value::Undefined
            }
            AnimMember::Reverse => {
                // a reversed animation runs back from where it is, as its keyframes flipped
                if !end.is_finite() {
                    return Err(self.throw_value(dom_exception(
                        "InvalidStateError",
                        "Cannot play reversed Animation with infinite target effect end.",
                    )));
                }
                let clock = self.clock;
                let state = self
                    .animations
                    .states
                    .get_mut(&handle)
                    .expect("checked above");
                let timing = &mut state.record.timing;
                timing.direction = match timing.direction {
                    ScriptDirection::Normal => ScriptDirection::Reverse,
                    ScriptDirection::Reverse => ScriptDirection::Normal,
                    ScriptDirection::Alternate => ScriptDirection::AlternateReverse,
                    ScriptDirection::AlternateReverse => ScriptDirection::Alternate,
                };
                std::mem::swap(&mut timing.fill_backwards, &mut timing.fill_forwards);
                let from = match phase {
                    Phase::Idle => 0.0,
                    _ => (end - local).max(0.0),
                };
                state.record.clock = ScriptClock::Running(clock.saturating_sub(from as u64));
                state.phase = Phase::Running;
                self.restart_finished_promise(handle);
                self.hold_animation(handle);
                self.schedule_finish(handle);
                Value::Undefined
            }
            AnimMember::Finish => {
                if !end.is_finite() {
                    return Err(self.throw_value(dom_exception(
                        "InvalidStateError",
                        "Failed to execute 'finish' on 'Animation': Cannot finish Animation with an infinite target effect end.",
                    )));
                }
                let clock = self.clock;
                let state = self
                    .animations
                    .states
                    .get_mut(&handle)
                    .expect("checked above");
                state.record.clock = ScriptClock::Running(clock.saturating_sub(end as u64));
                state.phase = Phase::Running;
                self.restart_finished_promise(handle);
                self.hold_animation(handle);
                self.schedule_finish(handle);
                Value::Undefined
            }
            AnimMember::Cancel => {
                if phase == Phase::Idle {
                    return Ok(Value::Undefined);
                }
                let state = self
                    .animations
                    .states
                    .get_mut(&handle)
                    .expect("checked above");
                state.phase = Phase::Idle;
                state.generation += 1;
                let finished = std::mem::replace(&mut state.finished, Self::pending_promise());
                // nobody need listen for the rejection a cancel brings
                finished.borrow_mut().handled = true;
                self.settle_promise(
                    &finished,
                    PromiseOutcome::Rejected(dom_exception(
                        "AbortError",
                        "The user aborted a request.",
                    )),
                );
                self.fire(
                    events::ListenerTarget::Object(handle),
                    "cancel",
                    events::HostEventFields::default(),
                )?;
                let _held = self.held_targets.remove(&handle);
                Value::Undefined
            }
            AnimMember::Persist => Value::Undefined,
            // the values it sets now become the element's own inline style
            AnimMember::CommitStyles => {
                let state = &self.animations.states[&handle];
                let node = state.node;
                let mut record = state.record.clone();
                record.timing.fill_backwards = true;
                record.timing.fill_forwards = true;
                let progress = record.timing.progress(local);
                let values = progress.map_or_else(Vec::new, |_| {
                    crate::css::sample_script_values(&record, self.clock)
                });
                let mut document = self.document.borrow_mut();
                for (property, value) in values {
                    document.set_style_property(node, &property, value);
                }
                Value::Undefined
            }
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn keyframe_offsets_fill_in_evenly_between_the_given_ones() {
        assert_eq!(spread(&mut [None, None, None]), [0.0, 0.5, 1.0]);
        assert_eq!(spread(&mut [None, Some(0.8), None]), [0.0, 0.8, 1.0]);
        assert_eq!(
            spread(&mut [None, None, Some(0.6), None, None]),
            [0.0, 0.3, 0.6, 0.8, 1.0]
        );
        assert_eq!(spread(&mut [None]), [1.0]);
        assert_eq!(
            css_name("backgroundColor").as_deref(),
            Some("background-color")
        );
        assert_eq!(css_name("cssFloat").as_deref(), Some("float"));
        assert_eq!(css_name("offset"), None);
    }
}

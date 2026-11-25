// audio and video elements: a source opens on decoder threads, one for pictures and one for
// sound, and the element keeps the playback clock, ready state, promises and events of
// HTMLMediaElement around what those threads hand back

use super::realm::Native;
use super::*;
use crate::audio::{AudioOutput, AudioRender};
use crate::dom::Namespace;
use crate::image::Image;
use crate::layout::ImageLayer;
use crate::sound::Sound;
use crate::video::{
    MediaBytes, MediaDecoder, MediaInfo, MediaSource, Picture, Track, VideoSurface,
};

const NETWORK_EMPTY: u8 = 0;
const NETWORK_IDLE: u8 = 1;
const NETWORK_LOADING: u8 = 2;
const NETWORK_NO_SOURCE: u8 = 3;
const HAVE_NOTHING: u8 = 0;
const HAVE_METADATA: u8 = 1;
const HAVE_FUTURE_DATA: u8 = 3;
const HAVE_ENOUGH_DATA: u8 = 4;
const MEDIA_ERR_SRC_NOT_SUPPORTED: u16 = 4;
// how often a playing element reports its time, as Chromium does
const TIME_UPDATE_INTERVAL: u64 = 250;

#[derive(Default)]
pub(super) struct MediaState {
    decoder: Option<Arc<dyn MediaDecoder>>,
    waker: Option<crate::bridge::Waker>,
    elements: HashMap<NodeId, MediaElement>,
}

impl MediaState {
    pub(super) fn trace(&self, tracer: &mut super::heap::Tracer) {
        for element in self.elements.values() {
            for waiter in &element.waiters {
                tracer.trace_promise(waiter);
            }
            for (_, callback) in &element.frame_callbacks {
                tracer.trace_value(callback);
            }
        }
    }
}

struct MediaElement {
    // runs of the load algorithm; a task queued for an older one does nothing
    generation: u64,
    url: Option<String>,
    track: Option<Track>,
    sound: Option<Sound>,
    info: Option<MediaInfo>,
    network: u8,
    ready: u8,
    error: Option<u16>,
    paused: bool,
    ended: bool,
    seeking: bool,
    // the last seek asked of the track, which the frames on screen must answer
    seek: u64,
    // a position set before metadata, taken up once it arrives
    start_at: Option<f64>,
    // the playback position when the clock last stopped, and the clock time it started again
    position: f64,
    anchor: Option<u64>,
    rate: f64,
    default_rate: f64,
    volume: f64,
    muted: Option<bool>,
    preserves_pitch: bool,
    frame: Option<Picture>,
    frame_time: f64,
    // a frame came since the last load or seek
    shown: bool,
    // autoplay starts it unless script played or paused it first
    can_autoplay: bool,
    // in a document at the last look, so leaving one pauses it while new Audio() plays unplaced
    connected: bool,
    reported: u64,
    waiters: Vec<PromiseHandle>,
    // frames shown since the element was made, and the callbacks waiting on the next one
    presented: u64,
    frame_callbacks: Vec<(u64, Value)>,
}

impl MediaElement {
    fn new() -> Self {
        Self {
            generation: 0,
            url: None,
            track: None,
            sound: None,
            info: None,
            network: NETWORK_EMPTY,
            ready: HAVE_NOTHING,
            error: None,
            paused: true,
            ended: false,
            seeking: false,
            seek: 0,
            start_at: None,
            position: 0.0,
            anchor: None,
            rate: 1.0,
            default_rate: 1.0,
            volume: 1.0,
            muted: None,
            preserves_pitch: true,
            frame: None,
            frame_time: 0.0,
            shown: false,
            can_autoplay: true,
            connected: false,
            reported: 0,
            waiters: Vec::new(),
            presented: 0,
            frame_callbacks: Vec::new(),
        }
    }

    fn duration(&self) -> f64 {
        self.info.map_or(f64::NAN, |info| info.duration)
    }

    fn current_time(&self, now: u64) -> f64 {
        let running = self.anchor.map_or(0.0, |anchor| {
            now.saturating_sub(anchor) as f64 / 1000.0 * self.rate
        });
        let time = self.position + running;
        match self.duration() {
            duration if duration.is_finite() => time.min(duration),
            _ => time,
        }
    }

    fn should_run(&self) -> bool {
        !self.paused && !self.seeking && !self.ended && self.ready >= HAVE_FUTURE_DATA
    }

    // starts or stops the clock to match the state; every change to that state ends here
    fn settle_clock(&mut self, now: u64) {
        match (self.should_run(), self.anchor) {
            (true, None) => self.anchor = Some(now),
            (false, Some(_)) => {
                self.position = self.current_time(now);
                self.anchor = None;
            }
            _ => {}
        }
    }

    fn begin_seek(&mut self, now: u64, target: f64) -> Vec<Cue> {
        let duration = self.duration();
        let target = match duration.is_finite() {
            true => target.min(duration),
            false => target,
        };
        self.anchor = None;
        self.position = target.max(0.0);
        self.seeking = true;
        self.ended = false;
        self.shown = false;
        self.seek += 1;
        if let Some(track) = &self.track {
            track.seek(self.seek, self.position);
        }
        if let Some(sound) = &self.sound {
            sound.seek(self.seek, self.position);
        }
        self.settle_clock(now);
        vec![Cue::Event("seeking")]
    }

    // what the decoder thread brought since the last look, and where the clock now stands
    fn advance(&mut self, now: u64, connected: bool, autoplay: bool, looped: bool) -> Advance {
        let mut step = Advance::default();
        if self.track.is_none() && self.sound.is_none() {
            return step;
        }
        if self.info.is_none() {
            let pictures = self.track.as_ref().map(|track| track.lock().opened.clone());
            let sound = self.sound.as_ref().map(|sound| sound.lock().opened.clone());
            if matches!(pictures, Some(None)) || matches!(sound, Some(None)) {
                return step;
            }
            // either may fail alone, for a video without sound or sound without pictures
            let pictures = pictures.flatten().and_then(Result::ok);
            let sound = sound.flatten().and_then(Result::ok);
            if pictures.is_none() {
                self.track = None;
            }
            if sound.is_none() {
                self.sound = None;
            }
            let info = match (pictures, sound) {
                (Some(info), sound) if info.duration.is_nan() => MediaInfo {
                    duration: sound.map_or(f64::NAN, |sound| sound.duration),
                    ..info
                },
                (Some(info), _) => info,
                (None, Some(sound)) => MediaInfo {
                    duration: sound.duration,
                    width: 0,
                    height: 0,
                },
                (None, None) => {
                    step.failed = true;
                    return step;
                }
            };
            self.info = Some(info);
            self.ready = HAVE_METADATA;
            self.network = NETWORK_IDLE;
            step.cues
                .extend([Cue::Event("durationchange"), Cue::Event("loadedmetadata")]);
        }
        let (mut exhausted, mut landed) = (true, true);
        if let Some(track) = &self.track {
            let mut state = track.lock();
            let mut done = false;
            if state.reached == self.seek {
                let time = self.current_time(now);
                if let Some((at, picture)) = state.take_frame(time, !self.shown) {
                    self.frame = Some(picture);
                    self.frame_time = at;
                    self.shown = true;
                    self.presented += 1;
                    step.frames += 1;
                }
                done = state.exhausted();
            }
            exhausted &= done;
            landed &= self.shown || done;
        }
        if let Some(sound) = &self.sound {
            let state = sound.lock();
            exhausted &= state.exhausted();
            landed &= state.ready(self.seek);
        }
        if let Some(start) = self.start_at.take() {
            step.cues.extend(self.begin_seek(now, start));
            return step;
        }
        if landed && self.ready < HAVE_ENOUGH_DATA {
            self.ready = HAVE_ENOUGH_DATA;
            step.cues
                .extend([Cue::Event("loadeddata"), Cue::Event("canplay")]);
            if !self.paused {
                step.cues.push(Cue::Playing);
            } else if autoplay && self.can_autoplay {
                self.paused = false;
                step.cues.extend([Cue::Event("play"), Cue::Playing]);
            }
            step.cues.push(Cue::Event("canplaythrough"));
        }
        if landed && self.seeking {
            self.seeking = false;
            step.cues
                .extend([Cue::Event("timeupdate"), Cue::Event("seeked")]);
        }
        self.settle_clock(now);
        if self.anchor.is_some() {
            let time = self.current_time(now);
            let duration = self.duration();
            let at_end = match duration.is_finite() && duration > 0.0 {
                true => time >= duration,
                false => exhausted && time >= self.frame_time,
            };
            if at_end && looped {
                step.cues.extend(self.begin_seek(now, 0.0));
            } else if at_end {
                self.position = time;
                self.anchor = None;
                self.paused = true;
                self.ended = true;
                step.cues.extend([
                    Cue::Event("timeupdate"),
                    Cue::Event("pause"),
                    Cue::Event("ended"),
                ]);
            } else if now.saturating_sub(self.reported) >= TIME_UPDATE_INTERVAL {
                self.reported = now;
                step.cues.push(Cue::Event("timeupdate"));
            }
        }
        // a media element taken out of the document pauses
        let removed = std::mem::replace(&mut self.connected, connected) && !connected;
        if removed && !self.paused {
            self.paused = true;
            self.settle_clock(now);
            step.cues
                .extend([Cue::Event("timeupdate"), Cue::Event("pause")]);
            step.aborted = true;
        }
        step
    }

    // the speakers open while it plays and follow its clock while that runs
    fn follow_sound(
        &mut self,
        now: u64,
        muted: bool,
        open: impl FnOnce(u32, &AudioRender) -> Box<dyn AudioOutput>,
    ) {
        let time = self.current_time(now);
        let clock = self.anchor.map(|anchor| (self.position, anchor, self.rate));
        let gain = if muted { 0.0 } else { self.volume as f32 };
        if let Some(sound) = &mut self.sound {
            sound.follow(!self.paused, clock, time, gain, self.preserves_pitch, open);
        }
    }
}

#[derive(Default)]
struct Advance {
    cues: Vec<Cue>,
    frames: usize,
    failed: bool,
    // pending play() promises are rejected
    aborted: bool,
}

#[derive(Clone, Debug)]
pub(super) enum Cue {
    Event(&'static str),
    // fires playing, then fulfills the play() promises waiting on it
    Playing,
    // fulfills the waiting play() promises without an event
    Resolve,
}

fn is_media(document: &Document, node: NodeId) -> bool {
    document.element(node).is_some_and(|element| {
        matches!(element.name.as_str(), "video" | "audio") && element.namespace == Namespace::Html
    })
}

// the src attribute, or the first source child with one
fn media_reference(document: &Document, node: NodeId) -> Option<String> {
    if let Some(source) = document.attribute(node, "src") {
        return Some(source.to_string());
    }
    document
        .children(node)
        .iter()
        .find(|&&child| {
            document
                .element(child)
                .is_some_and(|element| element.name == "source")
                && document.attribute(child, "src").is_some()
        })
        .and_then(|&child| document.attribute(child, "src").map(str::to_string))
}

fn abort_error(reason: &str) -> Value {
    dom_exception("AbortError", reason)
}

fn unsupported() -> Value {
    dom_exception(
        "NotSupportedError",
        "Failed to load because no supported source was found.",
    )
}

impl ScriptRuntime {
    pub(crate) fn set_media_decoder(
        &mut self,
        decoder: Option<Arc<dyn MediaDecoder>>,
        waker: Option<crate::bridge::Waker>,
    ) {
        self.media.decoder = decoder;
        self.media.waker = waker;
    }

    // new Audio(src): an audio element that preloads, loading src when one is given
    pub(super) fn construct_audio_element(
        &mut self,
        arguments: &[Value],
    ) -> Result<Value, ScriptError> {
        let node = self.document.borrow_mut().create_element("audio");
        self.document
            .borrow_mut()
            .set_attribute(node, "preload", "auto".to_string());
        if let Some(source) = arguments
            .first()
            .filter(|value| !matches!(value, Value::Undefined))
        {
            let source = self.coerce_string(source.clone())?;
            self.document
                .borrow_mut()
                .set_attribute(node, "src", source.to_string());
            self.load_media(node);
        }
        Ok(self.node_value(node))
    }

    pub(super) fn forget_media(&mut self, gone: impl Fn(NodeId) -> bool) {
        self.media.elements.retain(|node, _| !gone(*node));
    }

    pub(super) fn is_media_element(&self, node: NodeId) -> bool {
        is_media(&self.document.borrow(), node)
    }

    // whether some element's clock runs, which wants a frame at every interval
    pub(crate) fn media_playing(&self) -> bool {
        self.media
            .elements
            .values()
            .any(|element| element.anchor.is_some())
    }

    // the frames on screen, for paint
    pub(crate) fn video_layers(&self) -> Vec<ImageLayer> {
        let document = self.document.borrow();
        self.media
            .elements
            .iter()
            .filter(|(node, _)| document.is_connected(**node))
            .filter_map(|(node, element)| {
                Some(ImageLayer {
                    node: *node,
                    image: element.frame.as_ref()?.image(),
                    background: None,
                })
            })
            .collect()
    }

    // the size a video lays out at when CSS gives it none
    pub(crate) fn video_size(&self, node: NodeId) -> (u32, u32) {
        self.media
            .elements
            .get(&node)
            .and_then(|element| element.info)
            .filter(|info| info.width > 0 && info.height > 0)
            .map_or((300, 150), |info| (info.width, info.height))
    }

    // the frame drawImage reads, None before one arrives
    pub(crate) fn video_content(&self, node: NodeId) -> Option<Arc<Image>> {
        Some(self.media.elements.get(&node)?.frame.as_ref()?.pixels())
    }

    pub(crate) fn video_surface(&self, node: NodeId) -> Option<Arc<dyn VideoSurface>> {
        self.media.elements.get(&node)?.frame.as_ref()?.surface()
    }

    // videos the last paint showed as planes have their surfaces left unread
    pub(crate) fn mark_planes(&self, planes: &[crate::paint::Plane]) {
        for (node, element) in &self.media.elements {
            if let Some(track) = &element.track {
                track.lock().planed = planes.iter().any(|plane| plane.node == *node);
            }
        }
    }

    fn queue_cues(&mut self, node: NodeId, cues: Vec<Cue>) {
        let generation = self
            .media
            .elements
            .get(&node)
            .map_or(0, |element| element.generation);
        for cue in cues {
            self.schedule_native_task(Native::MediaCue(Box::new((node, generation, cue))));
        }
    }

    pub(super) fn deliver_media_cue(
        &mut self,
        node: NodeId,
        generation: u64,
        cue: Cue,
    ) -> Result<Value, ScriptError> {
        if self
            .media
            .elements
            .get(&node)
            .is_none_or(|element| element.generation != generation)
        {
            return Ok(Value::Undefined);
        }
        let name = match cue {
            Cue::Event(name) => Some(name),
            Cue::Playing => Some("playing"),
            Cue::Resolve => None,
        };
        if let Some(name) = name {
            let event = self.new_event("Event", name.to_string(), false, false, false, true);
            self.dispatch(&event, events::ListenerTarget::Node(node))?;
        }
        if matches!(cue, Cue::Playing | Cue::Resolve) {
            let waiters = self
                .media
                .elements
                .get_mut(&node)
                .map(|element| std::mem::take(&mut element.waiters))
                .unwrap_or_default();
            for waiter in waiters {
                self.settle_promise(&waiter, PromiseOutcome::Fulfilled(Value::Undefined));
            }
        }
        Ok(Value::Undefined)
    }

    fn reject_waiters(&mut self, node: NodeId, reason: impl Fn() -> Value) {
        let waiters = self
            .media
            .elements
            .get_mut(&node)
            .map(|element| std::mem::take(&mut element.waiters))
            .unwrap_or_default();
        for waiter in waiters {
            self.settle_promise(&waiter, PromiseOutcome::Rejected(reason()));
        }
    }

    pub(super) fn start_media_loads_in(&mut self, root: NodeId) {
        let nodes = {
            let document = self.document.borrow();
            std::iter::once(root)
                .chain(document.descendants(root))
                .filter(|&node| {
                    is_media(&document, node) && media_reference(&document, node).is_some()
                })
                .collect::<Vec<_>>()
        };
        for node in nodes {
            self.load_media(node);
        }
    }

    // the media element load algorithm, run by load(), a new src, or a parsed element with one
    pub(super) fn load_media(&mut self, node: NodeId) {
        if !self.is_media_element(node) {
            return;
        }
        let reference = media_reference(&self.document.borrow(), node);
        let url = reference.as_deref().and_then(|reference| {
            let base = self.base_url.as_ref()?;
            images::image_key(base, reference).filter(|_| !reference.trim().is_empty())
        });
        let element = self
            .media
            .elements
            .entry(node)
            .or_insert_with(MediaElement::new);
        let network = element.network;
        let generation = element.generation + 1;
        let waiters = std::mem::take(&mut element.waiters);
        // what script chose survives a new source; the rest starts over
        *element = MediaElement {
            generation,
            url,
            volume: element.volume,
            muted: element.muted,
            preserves_pitch: element.preserves_pitch,
            default_rate: element.default_rate,
            connected: element.connected,
            rate: element.default_rate,
            ..MediaElement::new()
        };
        for waiter in waiters {
            self.settle_promise(
                &waiter,
                PromiseOutcome::Rejected(abort_error(
                    "The play() request was interrupted by a new load request.",
                )),
            );
        }
        let mut cues = Vec::new();
        if matches!(network, NETWORK_LOADING | NETWORK_IDLE) {
            cues.push(Cue::Event("abort"));
        }
        if network != NETWORK_EMPTY {
            cues.push(Cue::Event("emptied"));
        }
        self.queue_cues(node, cues);
        if reference.is_none() {
            return;
        }
        let element = self.media.elements.get_mut(&node).expect("inserted above");
        element.network = NETWORK_LOADING;
        self.schedule_native_task(Native::MediaLoad(node, generation));
    }

    pub(super) fn deliver_media_load(
        &mut self,
        node: NodeId,
        generation: u64,
    ) -> Result<Value, ScriptError> {
        let Some(element) = self
            .media
            .elements
            .get(&node)
            .filter(|element| element.generation == generation)
        else {
            return Ok(Value::Undefined);
        };
        let url = element.url.clone();
        let pictures = self
            .document
            .borrow()
            .element(node)
            .is_some_and(|element| element.name == "video");
        let event = self.new_event("Event", "loadstart".into(), false, false, false, true);
        self.dispatch(&event, events::ListenerTarget::Node(node))?;
        let source = url.and_then(|url| self.media_source(&url));
        let decoder = self.media.decoder.clone();
        let waker = self.media.waker.clone();
        let Some(element) = self
            .media
            .elements
            .get_mut(&node)
            .filter(|element| element.generation == generation)
        else {
            return Ok(Value::Undefined);
        };
        let Some(source) = source else {
            self.fail_media(node);
            return Ok(Value::Undefined);
        };
        if let Some(decoder) = decoder.clone().filter(|_| pictures) {
            element.track = Some(Track::open(decoder, source.clone(), waker.clone()));
        }
        element.sound = Some(Sound::open(decoder, source, waker));
        Ok(Value::Undefined)
    }

    fn fail_media(&mut self, node: NodeId) {
        let Some(element) = self.media.elements.get_mut(&node) else {
            return;
        };
        element.track = None;
        element.sound = None;
        element.error = Some(MEDIA_ERR_SRC_NOT_SUPPORTED);
        element.network = NETWORK_NO_SOURCE;
        self.reject_waiters(node, unsupported);
        self.queue_cues(node, vec![Cue::Event("error")]);
    }

    // what the embedder holds is read where it is; anything else is fetched whole
    fn media_source(&mut self, url: &str) -> Option<MediaSource> {
        let check = self.content_security_policy().url(
            crate::csp::Directive::Media,
            url,
            crate::csp::Initiator::default(),
        );
        if !self.permits(check) {
            return None;
        }
        let parsed = AppUrl::parse(url).ok();
        if let (Some(parsed), Some(provider)) = (&parsed, &self.resource_provider)
            && let Some(source) = provider.media(parsed)
        {
            return Some(source);
        }
        let promise = self.fetch(
            &[Value::String(url.to_string().into())],
            crate::csp::Directive::Media,
            "Media",
        );
        let response = match &promise.borrow().outcome {
            PromiseOutcome::Fulfilled(Value::Response(response))
                if (200..300).contains(&response.status) =>
            {
                Some(response.clone())
            }
            _ => None,
        }?;
        let body = Arc::from(response.body.borrow().bytes.as_slice());
        Some(MediaSource::Bytes {
            body: MediaBytes::Shared(body),
            mime_type: response.mime_type.clone(),
        })
    }

    // takes up what the decoder threads did; the count is of frames that changed what shows
    pub(super) fn pump_media(&mut self) -> usize {
        let now = self.clock;
        let nodes = self
            .media
            .elements
            .iter()
            .filter(|(_, element)| element.track.is_some() || element.sound.is_some())
            .map(|(node, _)| *node)
            .collect::<Vec<_>>();
        let mut frames = 0;
        for node in nodes {
            let (connected, autoplay, looped, muted) = {
                let document = self.document.borrow();
                (
                    document.is_connected(node),
                    document.attribute(node, "autoplay").is_some(),
                    document.attribute(node, "loop").is_some(),
                    document.attribute(node, "muted").is_some(),
                )
            };
            let element = self.media.elements.get_mut(&node).expect("listed above");
            let step = element.advance(now, connected, autoplay, looped);
            let muted = element.muted.unwrap_or(muted);
            let audio = &self.audio;
            element.follow_sound(now, muted, |rate, render| audio.open(Some(rate), render));
            frames += step.frames;
            if step.frames > 0 && !element.frame_callbacks.is_empty() {
                self.run_video_frame_callbacks(node);
            }
            if step.failed {
                self.fail_media(node);
            }
            if step.aborted {
                self.reject_waiters(node, || {
                    abort_error("The play() request was interrupted because the media was removed from the document.")
                });
            }
            self.queue_cues(node, step.cues);
        }
        frames
    }

    // the HTMLMediaElement and HTMLVideoElement members, None for any other
    pub(super) fn media_member(
        &mut self,
        node: NodeId,
        name: &str,
        access: interfaces::Access,
        first: Value,
    ) -> Result<Option<Value>, ScriptError> {
        use interfaces::Access::{Call, Get, Set};
        if !self.is_media_element(node) {
            return Ok(None);
        }
        let now = self.clock;
        let muted_attribute = self.document.borrow().attribute(node, "muted").is_some();
        let element = self
            .media
            .elements
            .entry(node)
            .or_insert_with(MediaElement::new);
        let value = match (name, access) {
            ("currentSrc", Get) => Value::String(element.url.clone().unwrap_or_default().into()),
            ("networkState", Get) => Value::Number(f64::from(element.network)),
            ("readyState", Get) => Value::Number(f64::from(element.ready)),
            ("paused", Get) => Value::Bool(element.paused),
            ("ended", Get) => Value::Bool(element.ended),
            ("seeking", Get) => Value::Bool(element.seeking),
            ("duration", Get) => Value::Number(element.duration()),
            ("currentTime", Get) => Value::Number(match element.ready {
                HAVE_NOTHING => element.start_at.unwrap_or(0.0),
                _ => element.current_time(now),
            }),
            ("playbackRate", Get) => Value::Number(element.rate),
            ("defaultPlaybackRate", Get) => Value::Number(element.default_rate),
            ("volume", Get) => Value::Number(element.volume),
            ("muted", Get) => Value::Bool(element.muted.unwrap_or(muted_attribute)),
            ("preservesPitch", Get) => Value::Bool(element.preserves_pitch),
            ("videoWidth", Get) => {
                Value::Number(f64::from(element.info.map_or(0, |info| info.width)))
            }
            ("videoHeight", Get) => {
                Value::Number(f64::from(element.info.map_or(0, |info| info.height)))
            }
            ("error", Get) => match element.error {
                Some(code) => self.object_from_entries(vec![
                    ("code".into(), Value::Number(f64::from(code))),
                    (
                        "message".into(),
                        Value::String("MEDIA_ELEMENT_ERROR: Format error".into()),
                    ),
                ]),
                None => Value::Null,
            },
            ("currentTime", Set) => {
                let time = self.media_number(first, "currentTime")?;
                let element = self.media.elements.get_mut(&node).expect("inserted above");
                if element.ready == HAVE_NOTHING {
                    element.start_at = Some(time);
                } else {
                    let cues = element.begin_seek(now, time);
                    self.queue_cues(node, cues);
                }
                Value::Undefined
            }
            ("fastSeek", Call) => {
                let time = self.media_number(first, "fastSeek")?;
                return self.media_member(node, "currentTime", Set, Value::Number(time));
            }
            ("playbackRate" | "defaultPlaybackRate", Set) => {
                let rate = self.media_number(first, name)?;
                if rate != 0.0 && !(0.0625..=16.0).contains(&rate) {
                    return Err(self.throw_value(dom_exception(
                        "NotSupportedError",
                        &format!(
                            "Failed to set the '{name}' property on 'HTMLMediaElement': The provided playback rate ({}) is not in the supported playback range.",
                            number_to_string(rate)
                        ),
                    )));
                }
                let element = self.media.elements.get_mut(&node).expect("inserted above");
                let current = if name == "playbackRate" {
                    &mut element.rate
                } else {
                    &mut element.default_rate
                };
                if *current != rate {
                    if name == "playbackRate" {
                        element.position = element.current_time(now);
                        element.anchor = element.anchor.map(|_| now);
                        element.rate = rate;
                    } else {
                        element.default_rate = rate;
                    }
                    self.queue_cues(node, vec![Cue::Event("ratechange")]);
                }
                Value::Undefined
            }
            ("volume", Set) => {
                let volume = self.media_number(first, "volume")?;
                if !(0.0..=1.0).contains(&volume) {
                    return Err(self.throw_value(dom_exception(
                        "IndexSizeError",
                        &format!(
                            "Failed to set the 'volume' property on 'HTMLMediaElement': The volume provided ({}) is outside the range [0, 1].",
                            number_to_string(volume)
                        ),
                    )));
                }
                let element = self.media.elements.get_mut(&node).expect("inserted above");
                if element.volume != volume {
                    element.volume = volume;
                    self.queue_cues(node, vec![Cue::Event("volumechange")]);
                }
                Value::Undefined
            }
            ("muted", Set) => {
                let muted = first.truthy();
                let was = element.muted.replace(muted).unwrap_or(muted_attribute);
                if was != muted {
                    self.queue_cues(node, vec![Cue::Event("volumechange")]);
                }
                Value::Undefined
            }
            ("preservesPitch", Set) => {
                element.preserves_pitch = first.truthy();
                Value::Undefined
            }
            ("canPlayType", Call) => {
                let mime_type = self.coerce_string(first)?;
                let essence = mime_type.split(';').next().unwrap_or_default().trim();
                // the engine reads pcm wave files itself
                let wave = ["audio/wav", "audio/x-wav"]
                    .iter()
                    .any(|wave| wave.eq_ignore_ascii_case(essence));
                let answer = match &self.media.decoder {
                    _ if wave => "maybe",
                    Some(decoder) => decoder.can_play(mime_type.trim()),
                    None => "",
                };
                Value::String(answer.into())
            }
            ("load", Call) => {
                self.load_media(node);
                Value::Undefined
            }
            ("requestVideoFrameCallback", Call) => {
                if !is_callable(&first) {
                    return Err(self.type_error(
                        "Failed to execute 'requestVideoFrameCallback' on 'HTMLVideoElement': The callback provided as parameter 1 is not a function.",
                    ));
                }
                let id = self.next_task_id;
                self.next_task_id = id.saturating_add(1);
                let element = self.media.elements.get_mut(&node).expect("inserted above");
                element.frame_callbacks.push((id, first));
                Value::Number(id as f64)
            }
            ("cancelVideoFrameCallback", Call) => {
                let id = self.coerce_number(first)? as u64;
                let element = self.media.elements.get_mut(&node).expect("inserted above");
                element
                    .frame_callbacks
                    .retain(|(waiting, _)| *waiting != id);
                self.tasks.retain(|task| task.id != id);
                Value::Undefined
            }
            ("getVideoPlaybackQuality", Call) => {
                let presented = element.presented as f64;
                self.object_from_entries(vec![
                    ("creationTime".into(), Value::Number(now as f64)),
                    ("totalVideoFrames".into(), Value::Number(presented)),
                    ("droppedVideoFrames".into(), Value::Number(0.0)),
                    ("corruptedVideoFrames".into(), Value::Number(0.0)),
                ])
            }
            ("play", Call) => Value::Promise(self.play_media(node)),
            ("pause", Call) => {
                self.pause_media(node);
                Value::Undefined
            }
            _ => return Ok(None),
        };
        Ok(Some(value))
    }

    // the callbacks waiting on a video's next frame run once, with the frame it showed
    fn run_video_frame_callbacks(&mut self, node: NodeId) {
        let now = self.clock;
        let element = self.media.elements.get_mut(&node).expect("pumped above");
        let callbacks = std::mem::take(&mut element.frame_callbacks);
        let (width, height) = element
            .info
            .map_or((0, 0), |info| (info.width, info.height));
        let (media_time, presented) = (element.frame_time, element.presented);
        let metadata = self.object_from_entries(vec![
            ("presentationTime".into(), Value::Number(now as f64)),
            (
                "expectedDisplayTime".into(),
                Value::Number(now.saturating_add(self.frame_interval) as f64),
            ),
            ("width".into(), Value::Number(f64::from(width))),
            ("height".into(), Value::Number(f64::from(height))),
            ("mediaTime".into(), Value::Number(media_time)),
            ("presentedFrames".into(), Value::Number(presented as f64)),
        ]);
        for (id, callback) in callbacks {
            self.tasks.push(ScheduledTask {
                id,
                due: now,
                callback,
                arguments: vec![Value::Number(now as f64), metadata.clone()],
                kind: TaskKind::Timeout,
            });
        }
    }

    fn media_number(&mut self, value: Value, name: &str) -> Result<f64, ScriptError> {
        let number = self.coerce_number(value)?;
        if number.is_finite() {
            Ok(number)
        } else {
            Err(self.type_error(format!(
                "Failed to set the '{name}' property on 'HTMLMediaElement': The provided double value is non-finite."
            )))
        }
    }

    fn play_media(&mut self, node: NodeId) -> PromiseHandle {
        let now = self.clock;
        let errored = self
            .media
            .elements
            .get(&node)
            .is_some_and(|element| element.error == Some(MEDIA_ERR_SRC_NOT_SUPPORTED));
        if errored {
            return self.rejected_promise_value(dom_exception(
                "NotSupportedError",
                "The element has no supported sources.",
            ));
        }
        if self
            .media
            .elements
            .get(&node)
            .is_none_or(|element| element.network == NETWORK_EMPTY)
        {
            self.load_media(node);
        }
        let promise = Self::pending_promise();
        let element = self.media.elements.get_mut(&node).expect("loaded above");
        element.can_autoplay = false;
        element.waiters.push(promise.clone());
        let mut cues = Vec::new();
        if element.ended {
            cues.extend(element.begin_seek(now, 0.0));
        }
        if element.paused {
            element.paused = false;
            cues.push(Cue::Event("play"));
            cues.push(match element.ready >= HAVE_FUTURE_DATA {
                true => Cue::Playing,
                false => Cue::Event("waiting"),
            });
        } else if element.ready >= HAVE_FUTURE_DATA {
            cues.push(Cue::Resolve);
        }
        element.reported = now;
        element.settle_clock(now);
        self.queue_cues(node, cues);
        promise
    }

    fn pause_media(&mut self, node: NodeId) {
        let now = self.clock;
        if self
            .media
            .elements
            .get(&node)
            .is_none_or(|element| element.network == NETWORK_EMPTY)
        {
            self.load_media(node);
        }
        let element = self.media.elements.get_mut(&node).expect("loaded above");
        element.can_autoplay = false;
        if element.paused {
            return;
        }
        element.paused = true;
        element.settle_clock(now);
        self.reject_waiters(node, || {
            abort_error("The play() request was interrupted by a call to pause().")
        });
        self.queue_cues(node, vec![Cue::Event("timeupdate"), Cue::Event("pause")]);
    }
}

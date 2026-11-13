// the requests made on a window's engine thread, kept only while a devtools session records the
// network; the browser and its page script share that thread, so the log lives with it

use crate::navigation::{Resource, ResourceRequest, ResourceResponse};
use std::cell::RefCell;
use std::collections::BTreeMap;
use std::sync::OnceLock;
use std::time::{Instant, SystemTime, UNIX_EPOCH};

pub(crate) struct Exchange {
    pub(crate) loader: u64,
    pub(crate) document: String,
    pub(crate) url: String,
    pub(crate) method: String,
    pub(crate) headers: BTreeMap<String, String>,
    pub(crate) post_data: Vec<u8>,
    pub(crate) kind: &'static str,
    pub(crate) sent: f64,
    pub(crate) wall: f64,
    pub(crate) finished: f64,
    pub(crate) outcome: Result<Reply, String>,
}

pub(crate) struct Reply {
    pub(crate) url: String,
    pub(crate) status: u16,
    pub(crate) mime: String,
    pub(crate) headers: BTreeMap<String, String>,
    pub(crate) body: Vec<u8>,
}

impl Reply {
    pub(crate) fn resource(url: impl ToString, resource: &Resource) -> Self {
        Self {
            url: url.to_string(),
            status: 200,
            mime: resource.mime_type().to_string(),
            headers: BTreeMap::from([("content-type".into(), resource.mime_type().into())]),
            body: resource.body().to_vec(),
        }
    }

    // an answer with only a status, such as a 404 for what isn't there
    pub(crate) fn status(url: impl ToString, status: u16) -> Self {
        Self {
            url: url.to_string(),
            status,
            mime: String::new(),
            headers: BTreeMap::new(),
            body: Vec::new(),
        }
    }

    pub(crate) fn response(url: impl ToString, response: &ResourceResponse) -> Self {
        let mut headers = response.headers().clone();
        headers
            .entry("content-type".into())
            .or_insert_with(|| response.mime_type().into());
        Self {
            url: url.to_string(),
            status: response.status(),
            mime: response.mime_type().to_string(),
            headers,
            body: response.body().to_vec(),
        }
    }
}

// a request under way, when the network is being recorded
pub(crate) struct Sent {
    at: f64,
    wall: f64,
}

#[derive(Default)]
struct Log {
    recording: bool,
    loader: u64,
    document: String,
    pending: Vec<Exchange>,
}

thread_local! {
    static LOG: RefCell<Log> = RefCell::default();
}

// the protocol's monotonic seconds
pub(crate) fn now() -> f64 {
    static EPOCH: OnceLock<Instant> = OnceLock::new();
    EPOCH.get_or_init(Instant::now).elapsed().as_secs_f64()
}

// records from now on as part of the document with this loader id and URL, or stops with None
pub(crate) fn set_recording(document: Option<(u64, String)>) {
    LOG.with_borrow_mut(|log| match document {
        Some((loader, url)) => {
            log.recording = true;
            log.loader = loader;
            log.document = url;
        }
        None => *log = Log::default(),
    });
}

pub(crate) fn send() -> Option<Sent> {
    LOG.with_borrow(|log| log.recording).then(|| Sent {
        at: now(),
        wall: SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_or(0.0, |since| since.as_secs_f64()),
    })
}

// keeps what a request got; kind is the protocol's resource type, or None to tell it from the
// reply's MIME type
pub(crate) fn record(
    sent: Option<Sent>,
    request: &ResourceRequest,
    kind: Option<&'static str>,
    outcome: impl FnOnce() -> Result<Reply, String>,
) {
    record_to(sent, request.url().to_string(), request, kind, outcome);
}

// a navigation's request, which starts the document later requests belong to
pub(crate) fn record_navigation(
    sent: Option<Sent>,
    request: &ResourceRequest,
    loader: u64,
    outcome: impl FnOnce() -> Result<Reply, String>,
) {
    LOG.with_borrow_mut(|log| {
        if log.recording {
            log.loader = loader;
            log.document = request.url().to_string();
        }
    });
    record(sent, request, Some("Document"), outcome);
}

// the same for a request whose target is outside the app's origins
pub(crate) fn record_to(
    sent: Option<Sent>,
    target: String,
    request: &ResourceRequest,
    kind: Option<&'static str>,
    outcome: impl FnOnce() -> Result<Reply, String>,
) {
    let Some(sent) = sent else {
        return;
    };
    let outcome = outcome();
    let kind = kind.unwrap_or_else(|| match &outcome {
        Ok(reply) => kind_of(&reply.mime),
        Err(_) => "Other",
    });
    LOG.with_borrow_mut(|log| {
        if !log.recording {
            return;
        }
        log.pending.push(Exchange {
            loader: log.loader,
            document: log.document.clone(),
            url: target,
            method: request.method().to_string(),
            headers: request.headers().clone(),
            post_data: request.body().to_vec(),
            kind,
            sent: sent.at,
            wall: sent.wall,
            finished: now(),
            outcome,
        });
    });
}

pub(crate) fn take() -> Vec<Exchange> {
    LOG.with_borrow_mut(|log| std::mem::take(&mut log.pending))
}

fn kind_of(mime: &str) -> &'static str {
    let mime = mime.split(';').next().unwrap_or_default().trim();
    match mime {
        "text/css" => "Stylesheet",
        _ if mime.contains("javascript") || mime.contains("ecmascript") => "Script",
        _ if mime.starts_with("image/") => "Image",
        _ if mime.starts_with("font/") || mime.contains("font") => "Font",
        _ if mime.starts_with("audio/") || mime.starts_with("video/") => "Media",
        "text/vtt" => "TextTrack",
        _ => "Other",
    }
}

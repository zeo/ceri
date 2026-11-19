// the Network domain: each request the page's thread made while a session records, sent as its
// will-be-sent, response, data and finish events, with bodies kept for the frontend to ask for

use super::{Browser, Failure, frame_id, post};
use crate::netlog::{self, Exchange};
use crate::script::encoded_body;
use serde_json::{Value as Json, json};
use std::collections::VecDeque;

// what is kept of finished requests; the oldest go first past either bound
const KEPT_REQUESTS: usize = 1000;
const KEPT_BYTES: usize = 64 << 20;

#[derive(Default)]
pub(super) struct Kept {
    next: u64,
    exchanges: VecDeque<(String, Exchange)>,
    bytes: usize,
}

impl Kept {
    fn find(&self, id: &str) -> Result<&Exchange, Failure> {
        self.exchanges
            .iter()
            .find(|(held, _)| held == id)
            .map(|(_, exchange)| exchange)
            .ok_or_else(|| (-32000, "No resource with given identifier found".into()))
    }
}

impl Browser {
    pub(super) fn answer_network(
        &mut self,
        session: u64,
        method: &str,
        params: &Json,
    ) -> Result<Json, Failure> {
        let request_id = params["requestId"].as_str().unwrap_or_default();
        match method {
            "Network.enable" => {
                let document = (
                    self.current_document_id.unwrap_or_default(),
                    self.document_url(),
                );
                if let Some(held) = self.session(session) {
                    held.network = true;
                }
                netlog::set_recording(Some(document));
                Ok(json!({}))
            }
            "Network.disable" => {
                if let Some(held) = self.session(session) {
                    held.network = false;
                }
                self.stop_recording();
                Ok(json!({}))
            }
            "Network.getResponseBody" => {
                let exchange = self.inspection.network.find(request_id)?;
                let reply = exchange.outcome.as_ref().map_err(|_| {
                    (
                        -32000,
                        "No data found for resource with given identifier".into(),
                    )
                })?;
                let (body, encoded) = encoded_body(&reply.body, &reply.mime);
                Ok(json!({ "body": body, "base64Encoded": encoded }))
            }
            "Network.getRequestPostData" => {
                let exchange = self.inspection.network.find(request_id)?;
                if exchange.post_data.is_empty() {
                    return Err((-32000, "No post data available for the request".into()));
                }
                let (data, encoded) = encoded_body(&exchange.post_data, "text/plain");
                Ok(json!({ "postData": data, "base64Encoded": encoded }))
            }
            _ => Err((-32601, format!("'{method}' wasn't found"))),
        }
    }

    // the log records only while some session wants it
    pub(super) fn stop_recording(&mut self) {
        if !self.inspection.sessions.iter().any(|held| held.network) {
            netlog::set_recording(None);
            self.inspection.network = Kept::default();
        }
    }

    pub(super) fn flush_network(&mut self) {
        let inspection = &mut self.inspection;
        for exchange in netlog::take() {
            let kept = &mut inspection.network;
            kept.next += 1;
            let id = kept.next.to_string();
            for session in inspection.sessions.iter().filter(|held| held.network) {
                for (method, params) in events(&id, &exchange) {
                    post(session, method, params);
                }
            }
            kept.bytes += size(&exchange);
            kept.exchanges.push_back((id, exchange));
            while kept.exchanges.len() > KEPT_REQUESTS || kept.bytes > KEPT_BYTES {
                let Some((_, dropped)) = kept.exchanges.pop_front() else {
                    break;
                };
                kept.bytes -= size(&dropped);
            }
        }
    }
}

fn size(exchange: &Exchange) -> usize {
    exchange.post_data.len()
        + exchange
            .outcome
            .as_ref()
            .map_or(0, |reply| reply.body.len())
}

fn events(id: &str, exchange: &Exchange) -> Vec<(&'static str, Json)> {
    let loader = exchange.loader.to_string();
    let kind = exchange.kind;
    let mut request = json!({
        "url": exchange.url,
        "method": exchange.method,
        "headers": exchange.headers,
        "initialPriority": match kind {
            "Document" | "Stylesheet" => "VeryHigh",
            "Image" | "Media" | "Other" => "Low",
            _ => "High",
        },
        "referrerPolicy": "strict-origin-when-cross-origin",
    });
    if !exchange.post_data.is_empty() {
        request["hasPostData"] = true.into();
        if let Ok(text) = std::str::from_utf8(&exchange.post_data) {
            request["postData"] = text.into();
        }
    }
    let initiator = match kind {
        "Fetch" | "XHR" => "script",
        _ => "other",
    };
    let mut events = vec![(
        "Network.requestWillBeSent",
        json!({
            "requestId": id,
            "loaderId": loader,
            "documentURL": exchange.document,
            "request": request,
            "timestamp": exchange.sent,
            "wallTime": exchange.wall,
            "initiator": { "type": initiator },
            "redirectHasExtraInfo": false,
            "type": kind,
            "frameId": frame_id(),
        }),
    )];
    let finished = exchange.finished;
    match &exchange.outcome {
        Ok(reply) => {
            let mut parts = reply.mime.split(';');
            let mime = parts.next().unwrap_or_default().trim();
            let charset = parts
                .find_map(|part| part.trim().strip_prefix("charset="))
                .unwrap_or_default();
            let length = reply.body.len();
            events.push((
                "Network.responseReceived",
                json!({
                    "requestId": id,
                    "loaderId": loader,
                    "timestamp": finished,
                    "type": kind,
                    "response": {
                        "url": reply.url,
                        "status": reply.status,
                        "statusText": status_text(reply.status),
                        "headers": reply.headers,
                        "mimeType": mime,
                        "charset": charset.trim_matches('"'),
                        "connectionReused": false,
                        "connectionId": 0,
                        "encodedDataLength": 0,
                        "securityState": match reply.url.starts_with("https:") {
                            true => "secure",
                            false => "neutral",
                        },
                    },
                    "hasExtraInfo": false,
                    "frameId": frame_id(),
                }),
            ));
            if length > 0 {
                events.push((
                    "Network.dataReceived",
                    json!({
                        "requestId": id,
                        "timestamp": finished,
                        "dataLength": length,
                        "encodedDataLength": length,
                    }),
                ));
            }
            events.push((
                "Network.loadingFinished",
                json!({ "requestId": id, "timestamp": finished, "encodedDataLength": length }),
            ));
        }
        Err(text) => events.push((
            "Network.loadingFailed",
            json!({
                "requestId": id,
                "timestamp": finished,
                "type": kind,
                "errorText": text,
                "canceled": false,
            }),
        )),
    }
    events
}

fn status_text(status: u16) -> &'static str {
    match status {
        200 => "OK",
        201 => "Created",
        204 => "No Content",
        206 => "Partial Content",
        301 => "Moved Permanently",
        302 => "Found",
        303 => "See Other",
        304 => "Not Modified",
        307 => "Temporary Redirect",
        308 => "Permanent Redirect",
        400 => "Bad Request",
        401 => "Unauthorized",
        403 => "Forbidden",
        404 => "Not Found",
        405 => "Method Not Allowed",
        500 => "Internal Server Error",
        502 => "Bad Gateway",
        503 => "Service Unavailable",
        _ => "",
    }
}

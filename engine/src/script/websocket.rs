// the client side of the WebSocket protocol, RFC 6455: a thread makes the connection, runs the
// opening handshake and then reads frames, and a second thread writes them, so script never
// waits on the network

use crate::bridge::Waker;
use crate::navigation::{SocketConnector, SocketStream};
use std::collections::hash_map::RandomState;
use std::hash::{BuildHasher, Hasher};
use std::io::{self, Read, Write};
use std::net::{Shutdown, TcpStream};
use std::sync::mpsc::{Receiver, RecvTimeoutError, Sender, channel};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

const ACCEPT_GUID: &str = "258EAFA5-E914-47DA-95CA-C5AB0DC85B11";
const MAX_HEAD_BYTES: usize = 16 * 1024;
const MAX_MESSAGE_BYTES: usize = 64 * 1024 * 1024;
const HANDSHAKE_TIMEOUT: Duration = Duration::from_secs(30);
// how long a closing handshake waits for the server to answer and hang up
const CLOSE_TIMEOUT: Duration = Duration::from_secs(5);
const ABNORMAL: u16 = 1006;

#[derive(Debug, PartialEq)]
pub(super) enum SocketEvent {
    Opened {
        protocol: String,
        extensions: String,
    },
    Text(String),
    Binary(Vec<u8>),
    Sent(usize),
    Closed {
        code: u16,
        reason: String,
        clean: bool,
    },
}

pub(super) enum Outgoing {
    Text(String),
    Binary(Vec<u8>),
    Close(Option<u16>, String),
    Pong(Vec<u8>),
}

pub(super) struct Handshake {
    pub(super) target: String,
    pub(super) origin: String,
    pub(super) protocols: Vec<String>,
}

enum Link {
    Connecting,
    Open(TcpStream),
    Aborted,
}

// the page's side of one connection; dropping it hangs up, as leaving a page does
pub(super) struct Connection {
    outgoing: Sender<Outgoing>,
    link: Arc<Mutex<Link>>,
}

impl Connection {
    pub(super) fn send(&self, message: Outgoing) {
        let _ = self.outgoing.send(message);
    }

    pub(super) fn abort(&self) {
        let mut link = self
            .link
            .lock()
            .unwrap_or_else(|poison| poison.into_inner());
        if let Link::Open(stream) = &*link {
            let _ = stream.shutdown(Shutdown::Both);
        }
        *link = Link::Aborted;
    }
}

impl Drop for Connection {
    fn drop(&mut self) {
        self.abort();
    }
}

#[derive(Clone)]
struct Reporter {
    id: u32,
    events: Sender<(u32, SocketEvent)>,
    waker: Option<Waker>,
}

impl Reporter {
    fn send(&self, event: SocketEvent) {
        if self.events.send((self.id, event)).is_ok()
            && let Some(waker) = &self.waker
        {
            waker();
        }
    }

    fn closed(&self, code: u16, reason: String, clean: bool) {
        self.send(SocketEvent::Closed {
            code,
            reason,
            clean,
        });
    }
}

// what the page sees when a connection fails without a clean close
pub(super) fn failure() -> SocketEvent {
    SocketEvent::Closed {
        code: ABNORMAL,
        reason: String::new(),
        clean: false,
    }
}

pub(super) fn open(
    connector: SocketConnector,
    handshake: Handshake,
    id: u32,
    events: Sender<(u32, SocketEvent)>,
    waker: Option<Waker>,
) -> Connection {
    let (outgoing, queued) = channel();
    let link = Arc::new(Mutex::new(Link::Connecting));
    let reporter = Reporter { id, events, waker };
    let shared = link.clone();
    let pongs = outgoing.clone();
    let spawned = std::thread::Builder::new()
        .name("ceri-websocket".to_string())
        .spawn(move || run(connector, &handshake, &shared, queued, pongs, &reporter));
    if spawned.is_err() {
        *link.lock().unwrap_or_else(|poison| poison.into_inner()) = Link::Aborted;
    }
    Connection { outgoing, link }
}

fn run(
    connector: SocketConnector,
    handshake: &Handshake,
    link: &Mutex<Link>,
    queued: Receiver<Outgoing>,
    pongs: Sender<Outgoing>,
    reporter: &Reporter,
) {
    let mut masks = MaskKeys::new();
    let opened = connector().and_then(|socket| shake_hands(socket, handshake, &mut masks));
    let (stream, protocol, extensions) = match opened {
        Ok(opened) => opened,
        Err(_) => return reporter.send(failure()),
    };
    let clones = (stream.try_clone(), stream.try_clone());
    let (Ok(writer), Ok(kept)) = clones else {
        return reporter.send(failure());
    };
    {
        let mut link = link.lock().unwrap_or_else(|poison| poison.into_inner());
        if matches!(*link, Link::Aborted) {
            let _ = stream.shutdown(Shutdown::Both);
            return reporter.send(failure());
        }
        *link = Link::Open(kept);
    }
    reporter.send(SocketEvent::Opened {
        protocol,
        extensions,
    });
    let writing = reporter.clone();
    if std::thread::Builder::new()
        .name("ceri-websocket-writer".to_string())
        .spawn(move || write_frames(writer, queued, &writing, masks))
        .is_err()
    {
        let _ = stream.shutdown(Shutdown::Both);
        return reporter.send(failure());
    }
    let (code, reason, clean) = read_frames(&stream, &pongs, reporter);
    let _ = stream.shutdown(Shutdown::Both);
    reporter.closed(code, reason, clean);
}

// masking keys need to be unpredictable to the network, so they start from std's per-process
// random hash keys rather than the clock
struct MaskKeys(u64);

impl MaskKeys {
    fn new() -> Self {
        Self(RandomState::new().build_hasher().finish() | 1)
    }

    fn next(&mut self) -> [u8; 4] {
        let mut x = self.0;
        x ^= x << 13;
        x ^= x >> 7;
        x ^= x << 17;
        self.0 = x;
        (x >> 16).to_le_bytes()[..4].try_into().expect("four bytes")
    }
}

fn protocol_error(message: &str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, message.to_string())
}

pub(super) fn accept_key(key: &str) -> String {
    super::canvas::base64(&super::sha1_digest(
        format!("{key}{ACCEPT_GUID}").as_bytes(),
    ))
}

fn shake_hands(
    socket: SocketStream,
    handshake: &Handshake,
    masks: &mut MaskKeys,
) -> io::Result<(TcpStream, String, String)> {
    let SocketStream { stream, host } = socket;
    stream.set_read_timeout(Some(HANDSHAKE_TIMEOUT))?;
    stream.set_write_timeout(Some(HANDSHAKE_TIMEOUT))?;
    let nonce = [masks.next(), masks.next(), masks.next(), masks.next()].concat();
    let key = super::canvas::base64(&nonce);
    let mut request = format!(
        "GET {} HTTP/1.1\r\nHost: {host}\r\nUpgrade: websocket\r\nConnection: Upgrade\r\nSec-WebSocket-Key: {key}\r\nSec-WebSocket-Version: 13\r\nOrigin: {}\r\n",
        handshake.target, handshake.origin
    );
    if !handshake.protocols.is_empty() {
        request.push_str(&format!(
            "Sec-WebSocket-Protocol: {}\r\n",
            handshake.protocols.join(", ")
        ));
    }
    request.push_str("\r\n");
    (&stream).write_all(request.as_bytes())?;
    // the head is read a byte at a time so no frame bytes after it are consumed
    let mut head = Vec::new();
    let mut byte = [0];
    while !head.ends_with(b"\r\n\r\n") {
        if head.len() >= MAX_HEAD_BYTES {
            return Err(protocol_error("the handshake response is too long"));
        }
        (&stream).read_exact(&mut byte)?;
        head.push(byte[0]);
    }
    let head = std::str::from_utf8(&head).map_err(|_| protocol_error("a malformed handshake"))?;
    let (protocol, extensions) = check_response(head, &key, &handshake.protocols)?;
    stream.set_read_timeout(None)?;
    stream.set_write_timeout(None)?;
    Ok((stream, protocol, extensions))
}

// the subprotocol and extensions a 101 answer agreed to, once it proves it understood the key
fn check_response(head: &str, key: &str, offered: &[String]) -> io::Result<(String, String)> {
    let mut lines = head.split("\r\n");
    let status = lines.next().and_then(|line| line.split(' ').nth(1));
    if status != Some("101") {
        return Err(protocol_error("the server did not switch protocols"));
    }
    let headers = lines
        .filter_map(|line| line.split_once(':'))
        .map(|(name, value)| (name.trim().to_ascii_lowercase(), value.trim()))
        .collect::<Vec<_>>();
    let header = |name: &str| {
        headers
            .iter()
            .find(|(each, _)| each == name)
            .map(|(_, value)| *value)
    };
    let upgraded = header("upgrade").is_some_and(|value| value.eq_ignore_ascii_case("websocket"))
        && header("connection").is_some_and(|value| {
            value
                .split(',')
                .any(|token| token.trim().eq_ignore_ascii_case("upgrade"))
        });
    if !upgraded || header("sec-websocket-accept") != Some(accept_key(key).as_str()) {
        return Err(protocol_error("the server did not accept the handshake"));
    }
    let protocol = header("sec-websocket-protocol").unwrap_or_default();
    if !protocol.is_empty() && !offered.iter().any(|each| each == protocol) {
        return Err(protocol_error("the server chose a subprotocol not offered"));
    }
    // no extensions are offered, so a server that names one broke the handshake
    if header("sec-websocket-extensions").is_some_and(|value| !value.is_empty()) {
        return Err(protocol_error("the server named an extension not offered"));
    }
    Ok((protocol.to_string(), String::new()))
}

pub(super) fn frame(opcode: u8, payload: &[u8], mask: [u8; 4]) -> Vec<u8> {
    let mut frame = vec![0x80 | opcode];
    match payload.len() {
        length @ 0..=125 => frame.push(0x80 | length as u8),
        length @ 126..=0xffff => {
            frame.push(0x80 | 126);
            frame.extend_from_slice(&(length as u16).to_be_bytes());
        }
        length => {
            frame.push(0x80 | 127);
            frame.extend_from_slice(&(length as u64).to_be_bytes());
        }
    }
    frame.extend_from_slice(&mask);
    frame.extend(
        payload
            .iter()
            .zip(mask.iter().cycle())
            .map(|(byte, key)| byte ^ key),
    );
    frame
}

fn write_frames(
    mut stream: TcpStream,
    queued: Receiver<Outgoing>,
    reporter: &Reporter,
    mut masks: MaskKeys,
) {
    for message in &queued {
        let (opcode, payload, counted) = match message {
            Outgoing::Text(text) => {
                let length = text.len();
                (1, text.into_bytes(), Some(length))
            }
            Outgoing::Binary(bytes) => {
                let length = bytes.len();
                (2, bytes, Some(length))
            }
            Outgoing::Pong(payload) => (10, payload, None),
            Outgoing::Close(code, reason) => {
                let mut payload = code.map(u16::to_be_bytes).unwrap_or_default().to_vec();
                if code.is_some() {
                    payload.extend_from_slice(reason.as_bytes());
                }
                (8, payload, None)
            }
        };
        if stream
            .write_all(&frame(opcode, &payload, masks.next()))
            .is_err()
        {
            let _ = stream.shutdown(Shutdown::Both);
            return;
        }
        if let Some(length) = counted {
            reporter.send(SocketEvent::Sent(length));
        }
        if opcode == 8 {
            // the server has a while to hang up; once both sides drop their senders it has
            let deadline = Instant::now() + CLOSE_TIMEOUT;
            loop {
                match queued.recv_timeout(deadline.saturating_duration_since(Instant::now())) {
                    Ok(_) => continue,
                    Err(RecvTimeoutError::Timeout) => {
                        let _ = stream.shutdown(Shutdown::Both);
                        return;
                    }
                    Err(RecvTimeoutError::Disconnected) => return,
                }
            }
        }
    }
}

// reads until the connection ends, reporting messages; returns how it closed
fn read_frames(
    stream: &TcpStream,
    replies: &Sender<Outgoing>,
    reporter: &Reporter,
) -> (u16, String, bool) {
    let mut reader = stream;
    let mut message: Option<(u8, Vec<u8>)> = None;
    loop {
        let frame = match read_frame(&mut reader) {
            Ok(frame) => frame,
            Err(_) => return (ABNORMAL, String::new(), false),
        };
        let (fin, opcode, payload) = frame;
        match opcode {
            0 => match message.as_mut() {
                Some((_, data)) if data.len() + payload.len() <= MAX_MESSAGE_BYTES => {
                    data.extend_from_slice(&payload);
                }
                _ => return (ABNORMAL, String::new(), false),
            },
            1 | 2 if message.is_none() => message = Some((opcode, payload)),
            8 => return closing(&mut reader, &payload, replies),
            9 => {
                let _ = replies.send(Outgoing::Pong(payload));
            }
            10 => {}
            _ => return (ABNORMAL, String::new(), false),
        }
        if fin
            && opcode < 8
            && let Some((kind, data)) = message.take()
        {
            let event = if kind == 1 {
                match String::from_utf8(data) {
                    Ok(text) => SocketEvent::Text(text),
                    Err(_) => return (ABNORMAL, String::new(), false),
                }
            } else {
                SocketEvent::Binary(data)
            };
            reporter.send(event);
        }
    }
}

fn read_frame(reader: &mut impl Read) -> io::Result<(bool, u8, Vec<u8>)> {
    let mut head = [0; 2];
    reader.read_exact(&mut head)?;
    let (fin, opcode) = (head[0] & 0x80 != 0, head[0] & 0x0f);
    if head[0] & 0x70 != 0 || head[1] & 0x80 != 0 {
        return Err(protocol_error("reserved bits or a masked server frame"));
    }
    let length = match head[1] & 0x7f {
        126 => {
            let mut length = [0; 2];
            reader.read_exact(&mut length)?;
            u64::from(u16::from_be_bytes(length))
        }
        127 => {
            let mut length = [0; 8];
            reader.read_exact(&mut length)?;
            u64::from_be_bytes(length)
        }
        length => u64::from(length),
    };
    if opcode >= 8 && (!fin || length > 125) {
        return Err(protocol_error("a fragmented or oversized control frame"));
    }
    if length > MAX_MESSAGE_BYTES as u64 {
        return Err(protocol_error("a frame over the message limit"));
    }
    let mut payload = vec![0; length as usize];
    reader.read_exact(&mut payload)?;
    Ok((fin, opcode, payload))
}

// the server began or answered a closing handshake: echo it, then wait for the hang-up
fn closing(
    reader: &mut impl Read,
    payload: &[u8],
    replies: &Sender<Outgoing>,
) -> (u16, String, bool) {
    let (code, reason) = match payload {
        [] => (1005, String::new()),
        [high, low, reason @ ..] => match std::str::from_utf8(reason) {
            Ok(reason) => (u16::from_be_bytes([*high, *low]), reason.to_string()),
            Err(_) => return (ABNORMAL, String::new(), false),
        },
        [_] => return (ABNORMAL, String::new(), false),
    };
    let echoed = (code != 1005).then_some(code);
    let _ = replies.send(Outgoing::Close(echoed, String::new()));
    let mut rest = [0; 512];
    while matches!(reader.read(&mut rest), Ok(read) if read > 0) {}
    (code, reason, true)
}

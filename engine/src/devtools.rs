// a devtools protocol endpoint on loopback: /json lists each live window's page, and a
// websocket at /devtools/page/<id> carries one session's messages to that window's engine thread

use crate::host::EngineWorkerCommand;
use crate::script::accept_key;
use serde_json::json;
use std::io::{self, BufRead, BufReader, Read, Write};
use std::net::{Ipv4Addr, SocketAddr, TcpListener, TcpStream};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc::{self, SyncSender};
use std::sync::{Arc, Mutex, OnceLock};
use std::thread;

const MAX_HEAD: usize = 16 * 1024;
const MAX_MESSAGE: usize = 64 * 1024 * 1024;

static SERVER: OnceLock<SocketAddr> = OnceLock::new();
static TARGETS: Mutex<Vec<Target>> = Mutex::new(Vec::new());
static NEXT_ID: AtomicU64 = AtomicU64::new(1);

pub(crate) enum Inbound {
    Attach {
        session: u64,
        outgoing: mpsc::Sender<String>,
    },
    Detach {
        session: u64,
    },
    Message {
        session: u64,
        text: String,
    },
}

#[derive(Default)]
pub(crate) struct PageInfo {
    pub(crate) title: String,
    pub(crate) url: String,
}

struct Target {
    id: String,
    page: Arc<Mutex<PageInfo>>,
    commands: SyncSender<EngineWorkerCommand>,
}

// a window's place in the target list, for as long as the window lives
pub(crate) struct Registration {
    id: String,
    pub(crate) page: Arc<Mutex<PageInfo>>,
}

impl Drop for Registration {
    fn drop(&mut self) {
        lock(&TARGETS).retain(|target| target.id != self.id);
    }
}

pub(crate) fn register(commands: SyncSender<EngineWorkerCommand>) -> Option<Registration> {
    SERVER.get()?;
    let id = NEXT_ID.fetch_add(1, Ordering::Relaxed).to_string();
    let page = Arc::new(Mutex::new(PageInfo::default()));
    lock(&TARGETS).push(Target {
        id: id.clone(),
        page: page.clone(),
        commands,
    });
    Some(Registration { id, page })
}

pub fn devtools_serving() -> bool {
    SERVER.get().is_some()
}

// serves the protocol on 127.0.0.1 at the port, or at any free port for 0; once per process
pub fn serve_devtools(port: u16) -> io::Result<SocketAddr> {
    let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, port))?;
    let address = listener.local_addr()?;
    if SERVER.set(address).is_err() {
        return Err(io::Error::new(
            io::ErrorKind::AlreadyExists,
            "devtools are already being served",
        ));
    }
    thread::Builder::new()
        .name("ceri-devtools".to_string())
        .spawn(move || {
            for stream in listener.incoming().flatten() {
                let _ = thread::Builder::new()
                    .name("ceri-devtools-connection".to_string())
                    .spawn(move || serve_connection(stream));
            }
        })?;
    Ok(address)
}

fn lock<T>(mutex: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    mutex
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

struct Request {
    method: String,
    path: String,
    headers: Vec<(String, String)>,
}

impl Request {
    fn header(&self, name: &str) -> Option<&str> {
        self.headers
            .iter()
            .find(|(key, _)| key.eq_ignore_ascii_case(name))
            .map(|(_, value)| value.as_str())
    }
}

fn read_request(reader: &mut impl BufRead) -> io::Result<Request> {
    let invalid = || io::Error::new(io::ErrorKind::InvalidData, "malformed request");
    let mut lines = Vec::new();
    let mut total = 0;
    loop {
        let mut line = String::new();
        total += reader
            .take((MAX_HEAD - total) as u64)
            .read_line(&mut line)?;
        if !line.ends_with('\n') {
            return Err(invalid());
        }
        let line = line.trim_end().to_string();
        if line.is_empty() {
            break;
        }
        lines.push(line);
    }
    let mut first = lines.first().ok_or_else(invalid)?.split(' ');
    let method = first.next().ok_or_else(invalid)?.to_string();
    let path = first.next().ok_or_else(invalid)?.to_string();
    let headers = lines[1..]
        .iter()
        .filter_map(|line| line.split_once(':'))
        .map(|(key, value)| (key.trim().to_string(), value.trim().to_string()))
        .collect();
    Ok(Request {
        method,
        path,
        headers,
    })
}

// a page elsewhere could reach a loopback port by rebinding its own name to 127.0.0.1, so only
// loopback names are served, as chromium does
fn loopback_host(host: &str) -> bool {
    let name = match host.strip_prefix('[') {
        Some(rest) => rest.split(']').next().unwrap_or_default(),
        None => host.rsplit_once(':').map_or(host, |(name, _)| name),
    };
    matches!(name, "localhost" | "127.0.0.1" | "::1")
}

fn serve_connection(stream: TcpStream) -> io::Result<()> {
    let mut reader = BufReader::new(stream.try_clone()?);
    let request = read_request(&mut reader)?;
    let mut stream = stream;
    let host = request.header("Host").unwrap_or_default().to_string();
    if !loopback_host(&host) {
        return respond(
            &mut stream,
            "403 Forbidden",
            "text/plain",
            "Host header is not allowed",
        );
    }
    let path = request.path.split('?').next().unwrap_or_default();
    if let Some(id) = path.strip_prefix("/devtools/page/") {
        let upgrade = request
            .header("Upgrade")
            .is_some_and(|value| value.eq_ignore_ascii_case("websocket"));
        let key = request.header("Sec-WebSocket-Key").map(str::to_string);
        // pages could open a websocket to loopback too, so only the devtools frontend and clients
        // that send no origin get a session, as chromium's --remote-allow-origins default does
        let origin_allowed = request
            .header("Origin")
            .is_none_or(|origin| origin == "devtools://devtools");
        let commands = lock(&TARGETS)
            .iter()
            .find(|target| target.id == id)
            .map(|target| target.commands.clone());
        return match (upgrade, key, origin_allowed, commands) {
            (_, _, false, _) => respond(
                &mut stream,
                "403 Forbidden",
                "text/plain",
                "Rejected an incoming WebSocket connection from a disallowed origin",
            ),
            (true, Some(key), true, Some(commands)) => {
                serve_session(stream, reader, &key, commands)
            }
            (_, _, true, None) => respond(
                &mut stream,
                "404 Not Found",
                "text/plain",
                "No such target id",
            ),
            _ => respond(
                &mut stream,
                "400 Bad Request",
                "text/plain",
                "Not a WebSocket handshake",
            ),
        };
    }
    let body = match (request.method.as_str(), path) {
        ("GET", "/json" | "/json/list") => {
            let targets: Vec<_> = lock(&TARGETS)
                .iter()
                .map(|target| {
                    let page = lock(&target.page);
                    let socket = format!("{host}/devtools/page/{}", target.id);
                    json!({
                        "description": "",
                        "devtoolsFrontendUrl": format!("devtools://devtools/bundled/inspector.html?ws={socket}"),
                        "id": target.id,
                        "title": if page.title.is_empty() { &page.url } else { &page.title },
                        "type": "page",
                        "url": page.url,
                        "webSocketDebuggerUrl": format!("ws://{socket}"),
                    })
                })
                .collect();
            json!(targets)
        }
        ("GET", "/json/version") => json!({
            "Browser": format!("Ceri/{}", env!("CARGO_PKG_VERSION")),
            "Protocol-Version": "1.3",
        }),
        _ => return respond(&mut stream, "404 Not Found", "text/plain", "Unknown path"),
    };
    respond(
        &mut stream,
        "200 OK",
        "application/json; charset=UTF-8",
        &body.to_string(),
    )
}

fn respond(stream: &mut TcpStream, status: &str, kind: &str, body: &str) -> io::Result<()> {
    write!(
        stream,
        "HTTP/1.1 {status}\r\nContent-Type: {kind}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
        body.len()
    )?;
    stream.flush()
}

fn serve_session(
    mut stream: TcpStream,
    mut reader: BufReader<TcpStream>,
    key: &str,
    commands: SyncSender<EngineWorkerCommand>,
) -> io::Result<()> {
    let accept = accept_key(key);
    write!(
        stream,
        "HTTP/1.1 101 WebSocket Protocol Handshake\r\nUpgrade: WebSocket\r\nConnection: Upgrade\r\nSec-WebSocket-Accept: {accept}\r\n\r\n"
    )?;
    stream.flush()?;
    let session = NEXT_ID.fetch_add(1, Ordering::Relaxed);
    let (outgoing, messages) = mpsc::channel::<String>();
    let writer = Arc::new(Mutex::new(stream));
    let deliver = |inbound| {
        commands
            .send(EngineWorkerCommand::Inspector(inbound))
            .is_ok()
    };
    if !deliver(Inbound::Attach { session, outgoing }) {
        return Ok(());
    }
    let sender = writer.clone();
    thread::Builder::new()
        .name("ceri-devtools-writer".to_string())
        .spawn(move || {
            for message in messages {
                if write_frame(&mut lock(&sender), 0x1, message.as_bytes()).is_err() {
                    break;
                }
            }
        })?;
    let result = read_messages(&mut reader, &writer, |text| {
        deliver(Inbound::Message { session, text })
    });
    deliver(Inbound::Detach { session });
    let _ = lock(&writer).shutdown(std::net::Shutdown::Both);
    result
}

// reads client frames until a close, handing each whole text message on
fn read_messages(
    reader: &mut impl Read,
    writer: &Mutex<TcpStream>,
    mut deliver: impl FnMut(String) -> bool,
) -> io::Result<()> {
    let mut message = Vec::new();
    loop {
        let mut head = [0; 2];
        reader.read_exact(&mut head)?;
        let (fin, opcode) = (head[0] & 0x80 != 0, head[0] & 0x0f);
        let length = match head[1] & 0x7f {
            126 => {
                let mut bytes = [0; 2];
                reader.read_exact(&mut bytes)?;
                u64::from(u16::from_be_bytes(bytes))
            }
            127 => {
                let mut bytes = [0; 8];
                reader.read_exact(&mut bytes)?;
                u64::from_be_bytes(bytes)
            }
            length => u64::from(length),
        };
        // clients mask every frame
        if head[1] & 0x80 == 0 {
            return close(writer, 1002);
        }
        if length > (MAX_MESSAGE - message.len()) as u64 {
            return close(writer, 1009);
        }
        let mut mask = [0; 4];
        reader.read_exact(&mut mask)?;
        let mut payload = vec![0; length as usize];
        reader.read_exact(&mut payload)?;
        for (index, byte) in payload.iter_mut().enumerate() {
            *byte ^= mask[index % 4];
        }
        match opcode {
            0x0..=0x2 => {
                message.extend_from_slice(&payload);
                if fin {
                    let Ok(text) = String::from_utf8(std::mem::take(&mut message)) else {
                        return close(writer, 1007);
                    };
                    if !deliver(text) {
                        return close(writer, 1001);
                    }
                }
            }
            0x8 => return close(writer, 1000),
            0x9 => write_frame(&mut lock(writer), 0xA, &payload)?,
            _ => {}
        }
    }
}

fn close(writer: &Mutex<TcpStream>, code: u16) -> io::Result<()> {
    write_frame(&mut lock(writer), 0x8, &code.to_be_bytes())
}

fn write_frame(stream: &mut TcpStream, opcode: u8, payload: &[u8]) -> io::Result<()> {
    let mut frame = vec![0x80 | opcode];
    match payload.len() {
        length @ 0..=125 => frame.push(length as u8),
        length @ 126..=0xffff => {
            frame.push(126);
            frame.extend_from_slice(&(length as u16).to_be_bytes());
        }
        length => {
            frame.push(127);
            frame.extend_from_slice(&(length as u64).to_be_bytes());
        }
    }
    frame.extend_from_slice(payload);
    stream.write_all(&frame)?;
    stream.flush()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn exchange(address: SocketAddr, request: &str) -> (TcpStream, String) {
        let mut stream = TcpStream::connect(address).expect("the server should accept");
        stream
            .write_all(request.as_bytes())
            .expect("the request should send");
        let mut reader = BufReader::new(stream.try_clone().expect("the stream should clone"));
        let mut head = String::new();
        loop {
            let mut line = String::new();
            reader
                .read_line(&mut line)
                .expect("the response should read");
            head.push_str(&line);
            if line == "\r\n" || line.is_empty() {
                break;
            }
        }
        if let Some(length) = head
            .lines()
            .find_map(|line| line.strip_prefix("Content-Length: "))
        {
            let mut body = vec![0; length.parse().unwrap()];
            reader.read_exact(&mut body).unwrap();
            head.push_str(&String::from_utf8(body).unwrap());
        }
        (stream, head)
    }

    fn handshake(id: &str, origin: &str) -> String {
        format!(
            "GET /devtools/page/{id} HTTP/1.1\r\nHost: localhost\r\nUpgrade: websocket\r\nConnection: Upgrade\r\nSec-WebSocket-Key: dGhlIHNhbXBsZSBub25jZQ==\r\nSec-WebSocket-Version: 13\r\n{origin}\r\n"
        )
    }

    #[test]
    fn serves_targets_and_sessions_to_loopback_clients_only() {
        let address = serve_devtools(0).expect("devtools should serve");
        assert!(devtools_serving());
        let (commands, inbound) = mpsc::sync_channel(4);
        let registration = register(commands).expect("a target should register");
        lock(&registration.page).url = "https://app.ceri.invalid/".to_string();
        let id = registration.id.clone();

        let (_, list) = exchange(
            address,
            "GET /json/list HTTP/1.1\r\nHost: 127.0.0.1:9\r\n\r\n",
        );
        assert!(list.starts_with("HTTP/1.1 200 OK"), "{list}");
        let body: serde_json::Value =
            serde_json::from_str(list.split("\r\n\r\n").nth(1).unwrap()).unwrap();
        assert_eq!(body[0]["id"], id);
        assert_eq!(body[0]["title"], "https://app.ceri.invalid/");
        assert_eq!(
            body[0]["webSocketDebuggerUrl"],
            format!("ws://127.0.0.1:9/devtools/page/{id}")
        );

        let (_, rebound) = exchange(
            address,
            "GET /json HTTP/1.1\r\nHost: attacker.example:9\r\n\r\n",
        );
        assert!(rebound.starts_with("HTTP/1.1 403"), "{rebound}");
        let (_, web) = exchange(
            address,
            &handshake(&id, "Origin: https://attacker.example\r\n"),
        );
        assert!(web.starts_with("HTTP/1.1 403"), "{web}");
        let (_, missing) = exchange(address, &handshake("nope", ""));
        assert!(missing.starts_with("HTTP/1.1 404"), "{missing}");

        let (mut socket, accepted) =
            exchange(address, &handshake(&id, "Origin: devtools://devtools\r\n"));
        // the accept value from rfc 6455's own example
        assert!(
            accepted.contains("Sec-WebSocket-Accept: s3pPLMBiTxaQ9kYGzzhZRbK+xOo=\r\n"),
            "{accepted}"
        );
        let Ok(EngineWorkerCommand::Inspector(Inbound::Attach { session, outgoing })) =
            inbound.recv()
        else {
            panic!("the session should attach first");
        };
        let mask = [1, 2, 3, 4];
        let mut frame = vec![0x81, 0x80 | 5];
        frame.extend_from_slice(&mask);
        frame.extend(
            b"hello"
                .iter()
                .enumerate()
                .map(|(index, byte)| byte ^ mask[index % 4]),
        );
        socket.write_all(&frame).unwrap();
        let Ok(EngineWorkerCommand::Inspector(Inbound::Message {
            session: from,
            text,
        })) = inbound.recv()
        else {
            panic!("the message should arrive");
        };
        assert_eq!((from, text.as_str()), (session, "hello"));
        outgoing.send("world".to_string()).unwrap();
        let mut answer = [0; 7];
        socket.read_exact(&mut answer).unwrap();
        assert_eq!(&answer, b"\x81\x05world");
        socket.write_all(&[0x88, 0x80, 0, 0, 0, 0]).unwrap();
        let Ok(EngineWorkerCommand::Inspector(Inbound::Detach { session: closed })) =
            inbound.recv()
        else {
            panic!("a close should detach the session");
        };
        assert_eq!(closed, session);

        drop(registration);
        let (_, list) = exchange(address, "GET /json HTTP/1.1\r\nHost: localhost\r\n\r\n");
        assert!(list.ends_with("[]"), "{list}");
    }
}

// a small session bus client: the message format, external authentication and a reader thread,
// enough for the tray, notifications and desktop portals

use std::collections::HashMap;
use std::io::{BufReader, Read, Write};
use std::os::unix::ffi::OsStrExt;
use std::os::unix::net::UnixStream;
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::{Arc, Mutex, mpsc};
use std::time::Duration;

const CALL_TIMEOUT: Duration = Duration::from_secs(25);
// the specification's limit on a whole message
const MAX_MESSAGE: usize = 1 << 27;
const MAX_DEPTH: usize = 64;

pub(crate) const METHOD_CALL: u8 = 1;
pub(crate) const METHOD_RETURN: u8 = 2;
pub(crate) const ERROR: u8 = 3;
pub(crate) const SIGNAL: u8 = 4;
pub(crate) const NO_REPLY_EXPECTED: u8 = 1;

#[derive(Clone, Debug, PartialEq)]
pub(crate) enum Value {
    Byte(u8),
    Bool(bool),
    Int16(i16),
    Uint16(u16),
    Int32(i32),
    Uint32(u32),
    Int64(i64),
    Uint64(u64),
    Double(f64),
    Str(String),
    Path(String),
    Signature(String),
    Fd(u32),
    Variant(Box<Value>),
    // an array of bytes, kept whole rather than as one value per byte
    Bytes(Vec<u8>),
    // the element signature travels with the items so an empty array still has a type
    Array(String, Vec<Value>),
    Struct(Vec<Value>),
    Entry(Box<Value>, Box<Value>),
}

impl Value {
    pub(crate) fn signature(&self) -> String {
        match self {
            Self::Byte(_) => "y".into(),
            Self::Bool(_) => "b".into(),
            Self::Int16(_) => "n".into(),
            Self::Uint16(_) => "q".into(),
            Self::Int32(_) => "i".into(),
            Self::Uint32(_) => "u".into(),
            Self::Int64(_) => "x".into(),
            Self::Uint64(_) => "t".into(),
            Self::Double(_) => "d".into(),
            Self::Str(_) => "s".into(),
            Self::Path(_) => "o".into(),
            Self::Signature(_) => "g".into(),
            Self::Fd(_) => "h".into(),
            Self::Variant(_) => "v".into(),
            Self::Bytes(_) => "ay".into(),
            Self::Array(element, _) => format!("a{element}"),
            Self::Struct(fields) => format!(
                "({})",
                fields.iter().map(Self::signature).collect::<String>()
            ),
            Self::Entry(key, value) => format!("{{{}{}}}", key.signature(), value.signature()),
        }
    }

    pub(crate) fn str(text: impl Into<String>) -> Self {
        Self::Str(text.into())
    }

    pub(crate) fn variant(value: Value) -> Self {
        Self::Variant(Box::new(value))
    }

    // an a{sv} dictionary, the usual shape for properties and options
    pub(crate) fn dict(entries: impl IntoIterator<Item = (&'static str, Value)>) -> Self {
        Self::Array(
            "{sv}".into(),
            entries
                .into_iter()
                .map(|(key, value)| {
                    Self::Entry(Box::new(Self::str(key)), Box::new(Self::variant(value)))
                })
                .collect(),
        )
    }

    pub(crate) fn as_str(&self) -> Option<&str> {
        match self {
            Self::Str(text) | Self::Path(text) | Self::Signature(text) => Some(text),
            Self::Variant(inner) => inner.as_str(),
            _ => None,
        }
    }

    pub(crate) fn as_i64(&self) -> Option<i64> {
        Some(match self {
            Self::Byte(value) => (*value).into(),
            Self::Int16(value) => (*value).into(),
            Self::Uint16(value) => (*value).into(),
            Self::Int32(value) => (*value).into(),
            Self::Uint32(value) => (*value).into(),
            Self::Int64(value) => *value,
            Self::Variant(inner) => return inner.as_i64(),
            _ => return None,
        })
    }

    pub(crate) fn items(&self) -> &[Value] {
        match self {
            Self::Array(_, items) | Self::Struct(items) => items,
            Self::Variant(inner) => inner.items(),
            _ => &[],
        }
    }

    // the value under a key of an a{sv} dictionary, with its variant unwrapped
    pub(crate) fn get(&self, key: &str) -> Option<&Value> {
        self.items().iter().find_map(|entry| match entry {
            Self::Entry(name, value) if name.as_str() == Some(key) => match value.as_ref() {
                Self::Variant(inner) => Some(inner.as_ref()),
                other => Some(other),
            },
            _ => None,
        })
    }
}

fn alignment(signature: &str) -> usize {
    match signature.as_bytes().first() {
        Some(b'n' | b'q') => 2,
        Some(b'b' | b'i' | b'u' | b'h' | b's' | b'o' | b'a') => 4,
        Some(b'x' | b't' | b'd' | b'(' | b'{') => 8,
        _ => 1,
    }
}

fn pad(buffer: &mut Vec<u8>, to: usize) {
    buffer.resize(buffer.len().next_multiple_of(to), 0);
}

fn write_string(buffer: &mut Vec<u8>, text: &str) {
    pad(buffer, 4);
    buffer.extend_from_slice(&(text.len() as u32).to_le_bytes());
    buffer.extend_from_slice(text.as_bytes());
    buffer.push(0);
}

fn write(buffer: &mut Vec<u8>, value: &Value) {
    let fixed = |buffer: &mut Vec<u8>, bytes: &[u8]| {
        pad(buffer, bytes.len());
        buffer.extend_from_slice(bytes);
    };
    match value {
        Value::Byte(byte) => buffer.push(*byte),
        Value::Bool(flag) => fixed(buffer, &u32::from(*flag).to_le_bytes()),
        Value::Int16(number) => fixed(buffer, &number.to_le_bytes()),
        Value::Uint16(number) => fixed(buffer, &number.to_le_bytes()),
        Value::Int32(number) => fixed(buffer, &number.to_le_bytes()),
        Value::Uint32(number) | Value::Fd(number) => fixed(buffer, &number.to_le_bytes()),
        Value::Int64(number) => fixed(buffer, &number.to_le_bytes()),
        Value::Uint64(number) => fixed(buffer, &number.to_le_bytes()),
        Value::Double(number) => fixed(buffer, &number.to_le_bytes()),
        Value::Str(text) | Value::Path(text) => write_string(buffer, text),
        Value::Signature(text) => {
            buffer.push(text.len() as u8);
            buffer.extend_from_slice(text.as_bytes());
            buffer.push(0);
        }
        Value::Variant(inner) => {
            write(buffer, &Value::Signature(inner.signature()));
            write(buffer, inner);
        }
        Value::Bytes(bytes) => {
            fixed(buffer, &(bytes.len() as u32).to_le_bytes());
            buffer.extend_from_slice(bytes);
        }
        Value::Array(element, items) => {
            pad(buffer, 4);
            let length_at = buffer.len();
            buffer.extend_from_slice(&[0; 4]);
            // the padding before the first element is not part of the length
            pad(buffer, alignment(element));
            let start = buffer.len();
            for item in items {
                write(buffer, item);
            }
            let length = (buffer.len() - start) as u32;
            buffer[length_at..length_at + 4].copy_from_slice(&length.to_le_bytes());
        }
        Value::Struct(fields) => {
            pad(buffer, 8);
            for field in fields {
                write(buffer, field);
            }
        }
        Value::Entry(key, value) => {
            pad(buffer, 8);
            write(buffer, key);
            write(buffer, value);
        }
    }
}

// the first complete type of a signature and what follows it
fn split_type(signature: &str) -> Result<(&str, &str), String> {
    let bytes = signature.as_bytes();
    let mut end = 0;
    loop {
        match bytes.get(end) {
            Some(b'a') => end += 1,
            Some(b'(' | b'{') => {
                let mut depth = 0;
                for (index, byte) in bytes.iter().enumerate().skip(end) {
                    match byte {
                        b'(' | b'{' => depth += 1,
                        b')' | b'}' => depth -= 1,
                        _ => {}
                    }
                    if depth == 0 {
                        return Ok(signature.split_at(index + 1));
                    }
                }
                return Err(format!("the signature {signature:?} is unbalanced"));
            }
            Some(
                b'y' | b'b' | b'n' | b'q' | b'i' | b'u' | b'x' | b't' | b'd' | b's' | b'o' | b'g'
                | b'h' | b'v',
            ) => {
                return Ok(signature.split_at(end + 1));
            }
            _ => return Err(format!("the signature {signature:?} is not a type")),
        }
    }
}

struct Reader<'a> {
    data: &'a [u8],
    position: usize,
    big_endian: bool,
}

impl Reader<'_> {
    fn align(&mut self, to: usize) -> Result<(), String> {
        let next = self.position.next_multiple_of(to);
        if next > self.data.len() {
            return Err("the message ends early".into());
        }
        self.position = next;
        Ok(())
    }

    fn take(&mut self, length: usize) -> Result<&[u8], String> {
        let end = self
            .position
            .checked_add(length)
            .filter(|end| *end <= self.data.len());
        let end = end.ok_or("the message ends early")?;
        let bytes = &self.data[self.position..end];
        self.position = end;
        Ok(bytes)
    }

    fn fixed<const N: usize>(&mut self) -> Result<[u8; N], String> {
        self.align(N)?;
        let mut bytes: [u8; N] = self.take(N)?.try_into().map_err(|_| "short read")?;
        if self.big_endian {
            bytes.reverse();
        }
        Ok(bytes)
    }

    fn u32(&mut self) -> Result<u32, String> {
        self.fixed().map(u32::from_le_bytes)
    }

    fn text(&mut self, length: usize) -> Result<String, String> {
        let bytes = self.take(length + 1)?;
        if bytes[length] != 0 {
            return Err("a string is not terminated".into());
        }
        String::from_utf8(bytes[..length].to_vec()).map_err(|_| "a string is not UTF-8".into())
    }

    fn read(&mut self, signature: &str, depth: usize) -> Result<Value, String> {
        if depth > MAX_DEPTH {
            return Err("the message nests too deeply".into());
        }
        Ok(match signature.as_bytes().first() {
            Some(b'y') => Value::Byte(self.take(1)?[0]),
            Some(b'b') => Value::Bool(self.u32()? != 0),
            Some(b'n') => Value::Int16(self.fixed().map(i16::from_le_bytes)?),
            Some(b'q') => Value::Uint16(self.fixed().map(u16::from_le_bytes)?),
            Some(b'i') => Value::Int32(self.fixed().map(i32::from_le_bytes)?),
            Some(b'u') => Value::Uint32(self.u32()?),
            Some(b'h') => Value::Fd(self.u32()?),
            Some(b'x') => Value::Int64(self.fixed().map(i64::from_le_bytes)?),
            Some(b't') => Value::Uint64(self.fixed().map(u64::from_le_bytes)?),
            Some(b'd') => Value::Double(self.fixed().map(f64::from_le_bytes)?),
            Some(b's') => {
                let length = self.u32()? as usize;
                Value::Str(self.text(length)?)
            }
            Some(b'o') => {
                let length = self.u32()? as usize;
                Value::Path(self.text(length)?)
            }
            Some(b'g') => {
                let length = usize::from(self.take(1)?[0]);
                Value::Signature(self.text(length)?)
            }
            Some(b'v') => {
                let length = usize::from(self.take(1)?[0]);
                let inner = self.text(length)?;
                match split_type(&inner)? {
                    (single, "") => Value::Variant(Box::new(self.read(single, depth + 1)?)),
                    _ => return Err(format!("the variant signature {inner:?} is not one type")),
                }
            }
            Some(b'a') => {
                let length = self.u32()? as usize;
                let element = &signature[1..];
                self.align(alignment(element))?;
                if element == "y" {
                    return Ok(Value::Bytes(self.take(length)?.to_vec()));
                }
                let end = self
                    .position
                    .checked_add(length)
                    .filter(|end| *end <= self.data.len());
                let end = end.ok_or("an array runs past the message")?;
                let mut items = Vec::new();
                while self.position < end {
                    let before = self.position;
                    items.push(self.read(element, depth + 1)?);
                    // an empty struct takes no bytes and would never reach the end
                    if self.position == before {
                        return Err("an array element takes no space".into());
                    }
                }
                Value::Array(element.to_string(), items)
            }
            Some(b'(') => {
                self.align(8)?;
                let mut rest = &signature[1..signature.len() - 1];
                let mut fields = Vec::new();
                while !rest.is_empty() {
                    let (field, after) = split_type(rest)?;
                    fields.push(self.read(field, depth + 1)?);
                    rest = after;
                }
                Value::Struct(fields)
            }
            Some(b'{') => {
                self.align(8)?;
                let (key, value) = split_type(&signature[1..signature.len() - 1])?;
                Value::Entry(
                    Box::new(self.read(key, depth + 1)?),
                    Box::new(self.read(value, depth + 1)?),
                )
            }
            _ => return Err(format!("the signature {signature:?} is not a type")),
        })
    }

    fn read_all(&mut self, mut signature: &str) -> Result<Vec<Value>, String> {
        let mut values = Vec::new();
        while !signature.is_empty() {
            let (single, rest) = split_type(signature)?;
            values.push(self.read(single, 0)?);
            signature = rest;
        }
        Ok(values)
    }
}

#[derive(Clone, Debug, Default, PartialEq)]
pub(crate) struct Message {
    pub(crate) kind: u8,
    pub(crate) flags: u8,
    pub(crate) serial: u32,
    pub(crate) path: Option<String>,
    pub(crate) interface: Option<String>,
    pub(crate) member: Option<String>,
    pub(crate) error_name: Option<String>,
    pub(crate) reply_serial: Option<u32>,
    pub(crate) destination: Option<String>,
    pub(crate) sender: Option<String>,
    pub(crate) body: Vec<Value>,
}

impl Message {
    pub(crate) fn call(
        destination: &str,
        path: &str,
        interface: &str,
        member: &str,
        body: Vec<Value>,
    ) -> Self {
        Self {
            kind: METHOD_CALL,
            destination: Some(destination.into()),
            path: Some(path.into()),
            interface: Some(interface.into()),
            member: Some(member.into()),
            body,
            ..Self::default()
        }
    }

    pub(crate) fn signal(path: &str, interface: &str, member: &str, body: Vec<Value>) -> Self {
        Self {
            kind: SIGNAL,
            path: Some(path.into()),
            interface: Some(interface.into()),
            member: Some(member.into()),
            body,
            ..Self::default()
        }
    }

    pub(crate) fn reply(&self, body: Vec<Value>) -> Self {
        Self {
            kind: METHOD_RETURN,
            flags: NO_REPLY_EXPECTED,
            reply_serial: Some(self.serial),
            destination: self.sender.clone(),
            body,
            ..Self::default()
        }
    }

    pub(crate) fn error(&self, name: &str, text: &str) -> Self {
        Self {
            kind: ERROR,
            flags: NO_REPLY_EXPECTED,
            error_name: Some(name.into()),
            reply_serial: Some(self.serial),
            destination: self.sender.clone(),
            body: vec![Value::str(text)],
            ..Self::default()
        }
    }

    pub(crate) fn wants_reply(&self) -> bool {
        self.kind == METHOD_CALL && self.flags & NO_REPLY_EXPECTED == 0
    }

    fn encode(&self, serial: u32) -> Vec<u8> {
        let mut fields = Vec::new();
        let mut field = |code: u8, value: Value| {
            fields.push(Value::Struct(vec![
                Value::Byte(code),
                Value::variant(value),
            ]));
        };
        if let Some(path) = &self.path {
            field(1, Value::Path(path.clone()));
        }
        for (code, value) in [
            (2, &self.interface),
            (3, &self.member),
            (4, &self.error_name),
        ] {
            if let Some(value) = value {
                field(code, Value::Str(value.clone()));
            }
        }
        if let Some(reply_serial) = self.reply_serial {
            field(5, Value::Uint32(reply_serial));
        }
        if let Some(destination) = &self.destination {
            field(6, Value::Str(destination.clone()));
        }
        if !self.body.is_empty() {
            field(
                8,
                Value::Signature(self.body.iter().map(Value::signature).collect()),
            );
        }
        let mut body = Vec::new();
        for value in &self.body {
            write(&mut body, value);
        }
        let mut buffer = vec![b'l', self.kind, self.flags, 1];
        buffer.extend_from_slice(&(body.len() as u32).to_le_bytes());
        buffer.extend_from_slice(&serial.to_le_bytes());
        write(&mut buffer, &Value::Array("(yv)".into(), fields));
        pad(&mut buffer, 8);
        buffer.extend_from_slice(&body);
        buffer
    }

    fn decode(fixed: &[u8; 16], rest: &[u8]) -> Result<Self, String> {
        let big_endian = match fixed[0] {
            b'l' => false,
            b'B' => true,
            _ => return Err("the message has no byte order".into()),
        };
        let data = [fixed.as_slice(), rest].concat();
        let mut reader = Reader {
            data: &data,
            position: 4,
            big_endian,
        };
        let body_length = reader.u32()? as usize;
        let mut message = Self {
            kind: fixed[1],
            flags: fixed[2],
            serial: reader.u32()?,
            ..Self::default()
        };
        let mut signature = String::new();
        for field in reader.read("a(yv)", 0)?.items() {
            let [code, value] = field.items() else {
                continue;
            };
            let Value::Byte(code) = code else { continue };
            let text = || value.as_str().map(str::to_string);
            match code {
                1 => message.path = text(),
                2 => message.interface = text(),
                3 => message.member = text(),
                4 => message.error_name = text(),
                5 => message.reply_serial = value.as_i64().map(|serial| serial as u32),
                6 => message.destination = text(),
                7 => message.sender = text(),
                8 => signature = text().unwrap_or_default(),
                _ => {}
            }
        }
        reader.align(8)?;
        let body_start = reader.position;
        if data.len() - body_start != body_length {
            return Err("the message body has the wrong length".into());
        }
        // values align from the start of the body, which itself sits on an 8-byte boundary
        let mut body = Reader {
            data: &data[body_start..],
            position: 0,
            big_endian,
        };
        message.body = body.read_all(&signature)?;
        Ok(message)
    }
}

fn read_message(stream: &mut impl Read) -> Result<Message, String> {
    let mut fixed = [0u8; 16];
    stream
        .read_exact(&mut fixed)
        .map_err(|error| error.to_string())?;
    let number = |at: usize| {
        let bytes: [u8; 4] = fixed[at..at + 4].try_into().unwrap_or_default();
        match fixed[0] {
            b'B' => u32::from_be_bytes(bytes),
            _ => u32::from_le_bytes(bytes),
        }
    };
    let fields = number(12) as usize;
    let header = (16 + fields).next_multiple_of(8);
    let total = header + number(4) as usize;
    if total > MAX_MESSAGE {
        return Err("the bus sent a message larger than the protocol allows".into());
    }
    let mut rest = vec![0; total - 16];
    stream
        .read_exact(&mut rest)
        .map_err(|error| error.to_string())?;
    Message::decode(&fixed, &rest)
}

// percent escapes undone, as bus addresses and file uris carry them
pub(crate) fn unescape(value: &str) -> Vec<u8> {
    let bytes = value.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut index = 0;
    while index < bytes.len() {
        let hex = bytes
            .get(index + 1..index + 3)
            .and_then(|pair| std::str::from_utf8(pair).ok());
        match (
            bytes[index],
            hex.and_then(|pair| u8::from_str_radix(pair, 16).ok()),
        ) {
            (b'%', Some(byte)) => {
                out.push(byte);
                index += 3;
            }
            (byte, _) => {
                out.push(byte);
                index += 1;
            }
        }
    }
    out
}

fn connect(address: &str) -> Result<UnixStream, String> {
    let mut last = format!("the bus address {address:?} names no unix socket");
    for entry in address.split(';') {
        let Some(keys) = entry.strip_prefix("unix:") else {
            continue;
        };
        let keys: HashMap<_, _> = keys
            .split(',')
            .filter_map(|pair| pair.split_once('='))
            .collect();
        let attempt = if let Some(path) = keys.get("path") {
            UnixStream::connect(std::ffi::OsStr::from_bytes(&unescape(path)))
        } else if let Some(name) = keys.get("abstract") {
            abstract_socket(&unescape(name))
        } else {
            continue;
        };
        match attempt {
            Ok(stream) => return Ok(stream),
            Err(error) => last = format!("could not reach the session bus at {entry}: {error}"),
        }
    }
    Err(last)
}

#[cfg(target_os = "linux")]
fn abstract_socket(name: &[u8]) -> std::io::Result<UnixStream> {
    use std::os::linux::net::SocketAddrExt;
    UnixStream::connect_addr(&std::os::unix::net::SocketAddr::from_abstract_name(name)?)
}

#[cfg(not(target_os = "linux"))]
fn abstract_socket(_name: &[u8]) -> std::io::Result<UnixStream> {
    Err(std::io::Error::other("abstract sockets are Linux only"))
}

pub(crate) fn session_address() -> Result<String, String> {
    if let Ok(address) = std::env::var("DBUS_SESSION_BUS_ADDRESS")
        && !address.is_empty()
    {
        return Ok(address);
    }
    match std::env::var("XDG_RUNTIME_DIR") {
        Ok(runtime) if !runtime.is_empty() => Ok(format!("unix:path={runtime}/bus")),
        _ => Err(
            "there is no session bus: DBUS_SESSION_BUS_ADDRESS and XDG_RUNTIME_DIR are unset"
                .into(),
        ),
    }
}

// the EXTERNAL mechanism: the bus checks the uid against the socket's own credentials
fn authenticate(stream: &mut UnixStream) -> Result<(), String> {
    use std::os::unix::fs::MetadataExt;
    let uid = std::fs::metadata("/proc/self")
        .map(|metadata| metadata.uid())
        .map_err(|error| format!("could not read this process's uid: {error}"))?;
    let hex: String = uid
        .to_string()
        .bytes()
        .map(|byte| format!("{byte:02x}"))
        .collect();
    let fail =
        |error: std::io::Error| format!("could not authenticate with the session bus: {error}");
    stream
        .write_all(format!("\0AUTH EXTERNAL {hex}\r\n").as_bytes())
        .map_err(fail)?;
    let mut line = Vec::new();
    let mut byte = [0u8];
    while !line.ends_with(b"\r\n") && line.len() < 512 {
        stream.read_exact(&mut byte).map_err(fail)?;
        line.push(byte[0]);
    }
    if !line.starts_with(b"OK ") {
        return Err(format!(
            "the session bus refused this process: {}",
            String::from_utf8_lossy(&line).trim()
        ));
    }
    stream.write_all(b"BEGIN\r\n").map_err(fail)
}

type Handler = Box<dyn Fn(&Bus, Message) + Send + Sync>;

pub(crate) struct Bus {
    writer: Mutex<UnixStream>,
    serial: AtomicU32,
    pending: Mutex<HashMap<u32, mpsc::Sender<Message>>>,
    handler: Handler,
}

impl Bus {
    // the reader thread hands method calls and signals to the handler, which runs on that
    // thread and so may send but never wait on a call
    pub(crate) fn session(
        handler: impl Fn(&Bus, Message) + Send + Sync + 'static,
    ) -> Result<Arc<Self>, String> {
        Self::at(&session_address()?, handler)
    }

    pub(crate) fn at(
        address: &str,
        handler: impl Fn(&Bus, Message) + Send + Sync + 'static,
    ) -> Result<Arc<Self>, String> {
        let mut stream = connect(address)?;
        authenticate(&mut stream)?;
        let reader = stream.try_clone().map_err(|error| error.to_string())?;
        let bus = Arc::new(Self {
            writer: Mutex::new(stream),
            serial: AtomicU32::new(1),
            pending: Mutex::new(HashMap::new()),
            handler: Box::new(handler),
        });
        let listener = bus.clone();
        std::thread::Builder::new()
            .name("ceri-dbus".into())
            .spawn(move || listener.listen(reader))
            .map_err(|error| format!("could not start the bus reader: {error}"))?;
        // the bus answers nothing else until a connection says hello
        bus.call(Message::call(
            "org.freedesktop.DBus",
            "/org/freedesktop/DBus",
            "org.freedesktop.DBus",
            "Hello",
            Vec::new(),
        ))?;
        Ok(bus)
    }

    fn listen(&self, stream: UnixStream) {
        let mut stream = BufReader::new(stream);
        while let Ok(message) = read_message(&mut stream) {
            let waiting = match message.kind {
                METHOD_RETURN | ERROR => message.reply_serial.and_then(|serial| {
                    self.pending
                        .lock()
                        .expect("bus calls poisoned")
                        .remove(&serial)
                }),
                _ => None,
            };
            match waiting {
                Some(waiter) => {
                    let _ = waiter.send(message);
                }
                None if matches!(message.kind, METHOD_CALL | SIGNAL) => {
                    (self.handler)(self, message)
                }
                None => {}
            }
        }
        self.pending.lock().expect("bus calls poisoned").clear();
    }

    pub(crate) fn send(&self, message: &Message) -> Result<u32, String> {
        let serial = self.serial.fetch_add(1, Ordering::Relaxed);
        self.writer
            .lock()
            .expect("bus writer poisoned")
            .write_all(&message.encode(serial))
            .map_err(|error| format!("could not write to the session bus: {error}"))?;
        Ok(serial)
    }

    // waits for the reply, so it must not run on the reader thread
    pub(crate) fn call(&self, message: Message) -> Result<Vec<Value>, String> {
        let (sender, receiver) = mpsc::channel();
        let mut writer = self.writer.lock().expect("bus writer poisoned");
        let serial = self.serial.fetch_add(1, Ordering::Relaxed);
        self.pending
            .lock()
            .expect("bus calls poisoned")
            .insert(serial, sender);
        writer
            .write_all(&message.encode(serial))
            .map_err(|error| format!("could not write to the session bus: {error}"))?;
        drop(writer);
        let member = message.member.as_deref().unwrap_or_default();
        let reply = receiver
            .recv_timeout(CALL_TIMEOUT)
            .map_err(|_| format!("the session bus did not answer {member}"))?;
        self.pending
            .lock()
            .expect("bus calls poisoned")
            .remove(&serial);
        match reply.kind {
            ERROR => Err(format!(
                "{member} failed: {} {}",
                reply.error_name.unwrap_or_default(),
                reply
                    .body
                    .first()
                    .and_then(Value::as_str)
                    .unwrap_or_default()
            )),
            _ => Ok(reply.body),
        }
    }

    pub(crate) fn close(&self) {
        let _ = self
            .writer
            .lock()
            .expect("bus writer poisoned")
            .shutdown(std::net::Shutdown::Both);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn messages_survive_a_round_trip_through_the_wire_format() {
        let body = vec![
            Value::Int32(-7),
            Value::Struct(vec![
                Value::Uint32(3),
                Value::Struct(vec![
                    Value::Int32(0),
                    Value::dict([
                        ("label", Value::str("save")),
                        ("enabled", Value::Bool(true)),
                    ]),
                    Value::Array("v".into(), vec![Value::variant(Value::Byte(9))]),
                ]),
            ]),
            Value::Array(
                "(iiay)".into(),
                vec![Value::Struct(vec![
                    Value::Int32(1),
                    Value::Int32(1),
                    Value::Bytes(vec![255, 1, 2, 3]),
                ])],
            ),
            Value::Array("s".into(), Vec::new()),
            Value::Double(0.5),
        ];
        let mut message = Message::call(
            "org.example",
            "/MenuBar",
            "com.canonical.dbusmenu",
            "GetLayout",
            body,
        );
        message.sender = Some(":1.4".into());
        let encoded = message.encode(42);
        let decoded = read_message(&mut encoded.as_slice()).expect("the message should decode");
        assert_eq!(decoded.serial, 42);
        // the sender header is for the bus to set, so a client never writes one
        assert_eq!(
            Message {
                sender: Some(":1.4".into()),
                serial: 0,
                ..decoded
            },
            message
        );
        assert_eq!(message.body[1].signature(), "(u(ia{sv}av))");
        assert_eq!(
            message.body[1].items()[1].items()[1].get("label"),
            Some(&Value::str("save"))
        );
    }

    #[test]
    fn reads_big_endian_messages_and_rejects_broken_ones() {
        let mut message = vec![b'B', SIGNAL, 0, 1, 0, 0, 0, 4, 0, 0, 0, 13, 0, 0, 0, 7];
        // one header field, the body signature u, then the padding to the body
        message.extend_from_slice(&[8, 1, b'g', 0, 1, b'u', 0, 0, 0, 0, 1, 2]);
        let decoded =
            read_message(&mut message.as_slice()).expect("a big-endian message should decode");
        assert_eq!(
            (decoded.serial, decoded.body),
            (13, vec![Value::Uint32(258)])
        );
        let mut empty = vec![b'l', SIGNAL, 0, 1, 16, 0, 0, 0, 1, 0, 0, 0, 9, 0, 0, 0];
        empty.extend_from_slice(&[8, 1, b'g', 0, 3, b'a', b'(', b')', 0, 0, 0, 0, 0, 0, 0, 0]);
        empty.extend_from_slice(&[8, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0]);
        assert!(read_message(&mut empty.as_slice()).is_err());
        assert!(read_message(&mut &message[..20]).is_err());
        assert_eq!(split_type("a{sv}i"), Ok(("a{sv}", "i")));
        assert!(split_type("(ii").is_err());
        assert_eq!(unescape("/tmp/dbus%2dx"), b"/tmp/dbus-x");
    }
}

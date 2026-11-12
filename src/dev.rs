// the dev server a debug build serves its app origin from, such as vite's, reached over
// loopback http so pages keep the app origin, its capabilities and its storage

use ceri_engine::{AppUrl, NavigationError, Resource, mime_type_for};
use std::io::{self, Read, Write};
use std::net::TcpStream;
use std::path::Path;
use std::time::Duration;

const MAX_RESPONSE_BYTES: u64 = 64 * 1024 * 1024;
// a dev server compiles a module the first time it is asked for, which can take a while
const TIMEOUT: Duration = Duration::from_secs(60);

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct DevServer {
    // host and port, as the Host header and the connection take them
    authority: String,
}

impl DevServer {
    pub(crate) fn parse(url: &str) -> Result<Self, String> {
        let refuse = || format!("the dev URL {url} must be http on localhost, 127.0.0.1 or [::1]");
        let authority = url
            .strip_prefix("http://")
            .ok_or_else(refuse)?
            .trim_end_matches('/');
        let (host, port) = match authority.rsplit_once(':') {
            Some((host, port)) if !host.ends_with(':') => (host, port.parse::<u16>().ok()),
            _ => (authority, Some(80)),
        };
        match (host, port) {
            ("localhost" | "127.0.0.1" | "[::1]", Some(port)) if port > 0 => Ok(Self {
                authority: format!("{host}:{port}"),
            }),
            _ => Err(refuse()),
        }
    }

    pub(crate) fn connect(&self) -> io::Result<TcpStream> {
        let stream = TcpStream::connect(&self.authority)?;
        stream.set_read_timeout(Some(TIMEOUT))?;
        stream.set_write_timeout(Some(TIMEOUT))?;
        Ok(stream)
    }

    // what the dev server answers for an app URL; a 404 is a missing resource
    pub(crate) fn get(&self, url: &AppUrl) -> Result<Option<Resource>, NavigationError> {
        let failure = |error: io::Error| {
            NavigationError::ResourceProvider(format!(
                "the dev server at {} did not answer for {}: {error}",
                self.authority,
                url.path()
            ))
        };
        let mut stream = self.connect().map_err(failure)?;
        let request = format!(
            "GET {} HTTP/1.1\r\nHost: {}\r\nAccept: */*\r\nConnection: close\r\n\r\n",
            target(url),
            self.authority
        );
        stream.write_all(request.as_bytes()).map_err(failure)?;
        let mut raw = Vec::new();
        stream
            .take(MAX_RESPONSE_BYTES + 1)
            .read_to_end(&mut raw)
            .map_err(failure)?;
        if raw.len() as u64 > MAX_RESPONSE_BYTES {
            return Err(failure(io::Error::other("the response is over 64 MiB")));
        }
        let (status, content_type, body) = parse_response(&raw).map_err(failure)?;
        match status {
            200..=299 => {
                let mime_type = content_type
                    .as_deref()
                    .and_then(|value| value.split(';').next())
                    .map(str::trim)
                    .filter(|value| !value.is_empty())
                    .unwrap_or_else(|| mime_type_for(Path::new(url.path())));
                Ok(Some(Resource::new(mime_type, body)))
            }
            404 => Ok(None),
            _ => Err(NavigationError::ResourceProvider(format!(
                "the dev server answered {status} for {}: {}",
                url.path(),
                String::from_utf8_lossy(&body).trim()
            ))),
        }
    }
}

// the path and query of an app URL as a request line carries them
pub(crate) fn target(url: &AppUrl) -> String {
    let escape = |target: &mut String, text: &str, reserved: &[u8]| {
        for byte in text.bytes() {
            match byte {
                b'!'..=b'~' if !reserved.contains(&byte) => target.push(char::from(byte)),
                _ => target.push_str(&format!("%{byte:02X}")),
            }
        }
    };
    let mut target = String::new();
    escape(&mut target, url.path(), b"%?#");
    if !url.query().is_empty() {
        target.push('?');
        escape(&mut target, url.query(), b"#");
    }
    target
}

// the status, content type and body of an http/1.1 response that ends with its connection
fn parse_response(raw: &[u8]) -> io::Result<(u16, Option<String>, Vec<u8>)> {
    let malformed = || io::Error::new(io::ErrorKind::InvalidData, "a malformed response");
    let end = raw
        .windows(4)
        .position(|window| window == b"\r\n\r\n")
        .ok_or_else(malformed)?;
    let head = std::str::from_utf8(&raw[..end]).map_err(|_| malformed())?;
    let mut lines = head.split("\r\n");
    let status = lines
        .next()
        .and_then(|line| line.split(' ').nth(1))
        .and_then(|code| code.parse().ok())
        .ok_or_else(malformed)?;
    let mut content_type = None;
    let mut chunked = false;
    let mut length = None;
    for (name, value) in lines.filter_map(|line| line.split_once(':')) {
        let value = value.trim();
        match name.trim().to_ascii_lowercase().as_str() {
            "content-type" => content_type = Some(value.to_string()),
            "transfer-encoding" => chunked = value.eq_ignore_ascii_case("chunked"),
            "content-length" => length = value.parse::<usize>().ok(),
            _ => {}
        }
    }
    let body = &raw[end + 4..];
    let body = match (chunked, length) {
        (true, _) => dechunk(body).ok_or_else(malformed)?,
        (false, Some(length)) => body.get(..length).ok_or_else(malformed)?.to_vec(),
        (false, None) => body.to_vec(),
    };
    Ok((status, content_type, body))
}

fn dechunk(mut body: &[u8]) -> Option<Vec<u8>> {
    let mut out = Vec::new();
    loop {
        let line = body.windows(2).position(|window| window == b"\r\n")?;
        let size = std::str::from_utf8(&body[..line]).ok()?;
        let size = usize::from_str_radix(size.split(';').next()?.trim(), 16).ok()?;
        body = &body[line + 2..];
        if size == 0 {
            return Some(out);
        }
        out.extend_from_slice(body.get(..size)?);
        body = body.get(size + 2..)?;
    }
}

#[cfg(test)]
mod tests {
    use super::{DevServer, parse_response, target};
    use ceri_engine::AppUrl;
    use std::io::{Read, Write};
    use std::net::TcpListener;

    #[test]
    fn takes_only_loopback_http() {
        let server = DevServer::parse("http://localhost:1420/").expect("loopback");
        assert_eq!(server.authority, "localhost:1420");
        assert_eq!(
            DevServer::parse("http://[::1]").expect("v6").authority,
            "[::1]:80"
        );
        for refused in [
            "https://localhost:1420",
            "http://example.com:1420",
            "http://localhost:0",
            "http://localhost:99999",
            "http://192.168.1.4:1420",
        ] {
            assert!(DevServer::parse(refused).is_err(), "{refused}");
        }
    }

    #[test]
    fn request_targets_keep_the_query_and_escape_what_a_request_line_cannot_hold() {
        let url = AppUrl::parse("https://app.ceri.invalid/@fs/D/a%20b/c.js?v=1#top").expect("url");
        assert_eq!(target(&url), "/@fs/D/a%20b/c.js?v=1");
    }

    #[test]
    fn reads_chunked_sized_and_closing_responses() {
        let chunked = b"HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\nContent-Type: text/javascript\r\n\r\n4\r\nexpo\r\n6;x=1\r\nrt {};\r\n0\r\n\r\n";
        assert_eq!(
            parse_response(chunked).expect("chunked"),
            (
                200,
                Some("text/javascript".to_string()),
                b"export {};".to_vec()
            )
        );
        let sized = b"HTTP/1.1 404 Not Found\r\nContent-Length: 2\r\n\r\nnoextra";
        assert_eq!(parse_response(sized).expect("sized").2, b"no");
        assert_eq!(
            parse_response(b"HTTP/1.1 200 OK\r\n\r\nall")
                .expect("closing")
                .2,
            b"all"
        );
        assert!(
            parse_response(b"HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\n\r\nzz\r\n").is_err()
        );
    }

    #[test]
    fn serves_the_app_origin_from_the_dev_server() {
        let listener = TcpListener::bind("127.0.0.1:0").expect("listener");
        let port = listener.local_addr().expect("address").port();
        let server = std::thread::spawn(move || {
            let mut requests = Vec::new();
            for answer in [
                "HTTP/1.1 200 OK\r\nContent-Type: text/html; charset=utf-8\r\nContent-Length: 2\r\n\r\nhi",
                "HTTP/1.1 404 Not Found\r\nContent-Length: 0\r\n\r\n",
                "HTTP/1.1 500 Internal Server Error\r\nContent-Length: 6\r\n\r\nbroken",
            ] {
                let (mut stream, _) = listener.accept().expect("accept");
                let mut request = Vec::new();
                let mut byte = [0];
                while !request.ends_with(b"\r\n\r\n") {
                    stream.read_exact(&mut byte).expect("request");
                    request.push(byte[0]);
                }
                requests.push(String::from_utf8_lossy(&request).into_owned());
                stream.write_all(answer.as_bytes()).expect("answer");
            }
            requests
        });
        let dev = DevServer::parse(&format!("http://127.0.0.1:{port}")).expect("dev");
        let url =
            |path: &str| AppUrl::parse(&format!("https://app.ceri.invalid{path}")).expect("url");
        let page = dev
            .get(&url("/index.html?x=1"))
            .expect("page")
            .expect("found");
        assert_eq!((page.mime_type(), page.body()), ("text/html", &b"hi"[..]));
        assert_eq!(dev.get(&url("/gone.js")).expect("missing"), None);
        let error = dev.get(&url("/bad.js")).expect_err("a server error");
        assert!(error.to_string().contains("500"), "{error}");
        let requests = server.join().expect("server");
        assert!(
            requests[0].starts_with(&format!(
                "GET /index.html?x=1 HTTP/1.1\r\nHost: 127.0.0.1:{port}\r\n"
            )),
            "{}",
            requests[0]
        );
    }
}

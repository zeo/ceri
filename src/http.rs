use ceri_engine::{
    ExternalEvent, ExternalReport, ExternalRequest, NavigationError, ResourceResponse,
};
use std::collections::BTreeMap;

pub fn request(request: &ExternalRequest) -> Result<ResourceResponse, NavigationError> {
    platform::request(request)
}

// runs the request on a thread of its own and reports the head and each read of the body as
// they arrive, so a page reads a slow or endless answer as it comes
pub fn spawn(request: ExternalRequest, report: ExternalReport) {
    std::thread::spawn(move || {
        let outcome = platform::exchange(
            &request,
            true,
            |status, headers| {
                report(ExternalEvent::Head {
                    status,
                    mime_type: mime_type_of(&headers),
                    headers,
                })
            },
            |chunk| {
                report(ExternalEvent::Chunk(chunk.to_vec()));
                true
            },
        );
        report(match outcome {
            Ok(()) => ExternalEvent::Done,
            Err(error) => ExternalEvent::Failed(error.to_string()),
        });
    });
}

fn mime_type_of(headers: &BTreeMap<String, String>) -> String {
    headers
        .get("content-type")
        .and_then(|value| value.split(';').next())
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_string)
        .unwrap_or_else(|| "application/octet-stream".to_string())
}

#[cfg(target_os = "windows")]
mod platform {
    use ceri_engine::{ExternalRequest, NavigationError, ResourceResponse};
    use std::collections::BTreeMap;
    use std::ffi::c_void;
    use std::sync::OnceLock;
    use windows::Win32::Networking::WinHttp::{
        WINHTTP_ACCESS_TYPE, WINHTTP_ACCESS_TYPE_AUTOMATIC_PROXY, WINHTTP_ADDREQ_FLAG_ADD,
        WINHTTP_ADDREQ_FLAG_REPLACE, WINHTTP_FLAG_SECURE, WINHTTP_OPEN_REQUEST_FLAGS,
        WINHTTP_QUERY_FLAG_NUMBER, WINHTTP_QUERY_RAW_HEADERS_CRLF, WINHTTP_QUERY_STATUS_CODE,
    };
    use windows::Win32::System::LibraryLoader::{GetProcAddress, LoadLibraryW};
    use windows::core::{BOOL, PCWSTR, s, w};

    const MAX_RESPONSE_BYTES: usize = 32 * 1024 * 1024;
    const TIMEOUT_MILLISECONDS: i32 = 30_000;
    const READ_CHUNK: usize = 64 * 1024;

    type FarProc = unsafe extern "system" fn() -> isize;
    type OpenFn =
        unsafe extern "system" fn(PCWSTR, WINHTTP_ACCESS_TYPE, PCWSTR, PCWSTR, u32) -> *mut c_void;
    type CloseHandleFn = unsafe extern "system" fn(*mut c_void) -> BOOL;
    type SetTimeoutsFn = unsafe extern "system" fn(*mut c_void, i32, i32, i32, i32) -> BOOL;
    type ConnectFn = unsafe extern "system" fn(*mut c_void, PCWSTR, u16, u32) -> *mut c_void;
    type OpenRequestFn = unsafe extern "system" fn(
        *mut c_void,
        PCWSTR,
        PCWSTR,
        PCWSTR,
        PCWSTR,
        *const PCWSTR,
        WINHTTP_OPEN_REQUEST_FLAGS,
    ) -> *mut c_void;
    type AddRequestHeadersFn = unsafe extern "system" fn(*mut c_void, PCWSTR, u32, u32) -> BOOL;
    type SendRequestFn =
        unsafe extern "system" fn(*mut c_void, PCWSTR, u32, *const c_void, u32, u32, usize) -> BOOL;
    type ReceiveResponseFn = unsafe extern "system" fn(*mut c_void, *mut c_void) -> BOOL;
    type QueryHeadersFn = unsafe extern "system" fn(
        *mut c_void,
        u32,
        PCWSTR,
        *mut c_void,
        *mut u32,
        *mut u32,
    ) -> BOOL;
    type ReadDataFn = unsafe extern "system" fn(*mut c_void, *mut c_void, u32, *mut u32) -> BOOL;

    struct WinHttp {
        open: OpenFn,
        close: CloseHandleFn,
        set_timeouts: SetTimeoutsFn,
        connect: ConnectFn,
        open_request: OpenRequestFn,
        add_headers: AddRequestHeadersFn,
        send_request: SendRequestFn,
        receive_response: ReceiveResponseFn,
        query_headers: QueryHeadersFn,
        read_data: ReadDataFn,
    }

    fn winhttp() -> Option<&'static WinHttp> {
        static API: OnceLock<Option<WinHttp>> = OnceLock::new();
        API.get_or_init(|| {
            // safety: winhttp stays loaded for the process lifetime and exports match winhttp abi
            unsafe {
                let module = LoadLibraryW(w!("winhttp.dll")).ok()?;
                Some(WinHttp {
                    open: std::mem::transmute::<FarProc, OpenFn>(GetProcAddress(
                        module,
                        s!("WinHttpOpen"),
                    )?),
                    close: std::mem::transmute::<FarProc, CloseHandleFn>(GetProcAddress(
                        module,
                        s!("WinHttpCloseHandle"),
                    )?),
                    set_timeouts: std::mem::transmute::<FarProc, SetTimeoutsFn>(GetProcAddress(
                        module,
                        s!("WinHttpSetTimeouts"),
                    )?),
                    connect: std::mem::transmute::<FarProc, ConnectFn>(GetProcAddress(
                        module,
                        s!("WinHttpConnect"),
                    )?),
                    open_request: std::mem::transmute::<FarProc, OpenRequestFn>(GetProcAddress(
                        module,
                        s!("WinHttpOpenRequest"),
                    )?),
                    add_headers: std::mem::transmute::<FarProc, AddRequestHeadersFn>(
                        GetProcAddress(module, s!("WinHttpAddRequestHeaders"))?,
                    ),
                    send_request: std::mem::transmute::<FarProc, SendRequestFn>(GetProcAddress(
                        module,
                        s!("WinHttpSendRequest"),
                    )?),
                    receive_response: std::mem::transmute::<FarProc, ReceiveResponseFn>(
                        GetProcAddress(module, s!("WinHttpReceiveResponse"))?,
                    ),
                    query_headers: std::mem::transmute::<FarProc, QueryHeadersFn>(GetProcAddress(
                        module,
                        s!("WinHttpQueryHeaders"),
                    )?),
                    read_data: std::mem::transmute::<FarProc, ReadDataFn>(GetProcAddress(
                        module,
                        s!("WinHttpReadData"),
                    )?),
                })
            }
        })
        .as_ref()
    }

    struct Internet(&'static WinHttp, *mut c_void);

    impl Drop for Internet {
        fn drop(&mut self) {
            if !self.1.is_null() {
                // safety: the handle came from WinHTTP and is closed once
                let _ = unsafe { (self.0.close)(self.1) };
            }
        }
    }

    fn wide(value: &str) -> Vec<u16> {
        value.encode_utf16().chain(std::iter::once(0)).collect()
    }

    fn failure(detail: impl Into<String>) -> NavigationError {
        NavigationError::ResourceProvider(detail.into())
    }

    pub(super) fn request(request: &ExternalRequest) -> Result<ResourceResponse, NavigationError> {
        let mut head = (0, BTreeMap::new());
        let mut body = Vec::new();
        exchange(
            request,
            false,
            |status, headers| head = (status, headers),
            |chunk| {
                body.extend_from_slice(chunk);
                body.len() <= MAX_RESPONSE_BYTES
            },
        )?;
        let (status, headers) = head;
        ResourceResponse::new(status, super::mime_type_of(&headers), headers, body)
    }

    // the request travels over the platform TLS stack, so certificate policy stays with the OS;
    // a streamed exchange waits as long as the server takes between reads, as browsers do
    pub(super) fn exchange(
        request: &ExternalRequest,
        streamed: bool,
        mut on_head: impl FnMut(u16, BTreeMap<String, String>),
        mut on_chunk: impl FnMut(&[u8]) -> bool,
    ) -> Result<(), NavigationError> {
        let (host, port, target) = split_target(request.url())?;
        let api = winhttp().ok_or_else(|| failure("the platform HTTP stack is unavailable"))?;
        let agent = wide(concat!("Ceri/", env!("CARGO_PKG_VERSION")));
        // safety: every pointer below refers to a live null terminated buffer
        let session = Internet(api, unsafe {
            (api.open)(
                PCWSTR(agent.as_ptr()),
                WINHTTP_ACCESS_TYPE_AUTOMATIC_PROXY,
                PCWSTR::null(),
                PCWSTR::null(),
                0,
            )
        });
        if session.1.is_null() {
            return Err(failure("the platform HTTP stack is unavailable"));
        }
        // safety: the session handle is live
        let _ = unsafe {
            (api.set_timeouts)(
                session.1,
                TIMEOUT_MILLISECONDS,
                TIMEOUT_MILLISECONDS,
                TIMEOUT_MILLISECONDS,
                if streamed { 0 } else { TIMEOUT_MILLISECONDS },
            )
        };
        let host_wide = wide(&host);
        // safety: the session handle is live and the host buffer outlives the call
        let connection = Internet(api, unsafe {
            (api.connect)(session.1, PCWSTR(host_wide.as_ptr()), port, 0)
        });
        if connection.1.is_null() {
            return Err(failure(format!("could not connect to {host}")));
        }
        let verb = wide(request.method());
        let object = wide(&target);
        // safety: the connection handle is live and both buffers outlive the call
        let call = Internet(api, unsafe {
            (api.open_request)(
                connection.1,
                PCWSTR(verb.as_ptr()),
                PCWSTR(object.as_ptr()),
                PCWSTR::null(),
                PCWSTR::null(),
                std::ptr::null(),
                WINHTTP_FLAG_SECURE,
            )
        });
        if call.1.is_null() {
            return Err(failure(format!("could not open a request to {host}")));
        }
        if !request.headers().is_empty() {
            let headers = request
                .headers()
                .iter()
                .map(|(name, value)| format!("{name}: {value}"))
                .collect::<Vec<_>>()
                .join("\r\n");
            let headers = headers.encode_utf16().collect::<Vec<_>>();
            // safety: the request handle is live and the header buffer outlives the call
            let _ = unsafe {
                (api.add_headers)(
                    call.1,
                    PCWSTR(headers.as_ptr()),
                    headers.len() as u32,
                    WINHTTP_ADDREQ_FLAG_ADD | WINHTTP_ADDREQ_FLAG_REPLACE,
                )
            };
        }
        let body = request.body();
        // safety: the body slice outlives the call, and its length is passed unchanged
        let sent = unsafe {
            (api.send_request)(
                call.1,
                PCWSTR::null(),
                0,
                if body.is_empty() {
                    std::ptr::null()
                } else {
                    body.as_ptr().cast()
                },
                body.len() as u32,
                body.len() as u32,
                0,
            )
        }
        .ok();
        if let Err(error) = sent {
            return Err(failure(format!(
                "the request to {host} could not be sent: {}",
                error.message()
            )));
        }
        // safety: the request handle is live
        if unsafe { (api.receive_response)(call.1, std::ptr::null_mut()) }
            .ok()
            .is_err()
        {
            return Err(failure(format!("{host} did not answer the request")));
        }

        let mut status = 0_u32;
        let mut status_size = std::mem::size_of::<u32>() as u32;
        // safety: the buffer is a u32 and its size is passed as such
        if unsafe {
            (api.query_headers)(
                call.1,
                WINHTTP_QUERY_STATUS_CODE | WINHTTP_QUERY_FLAG_NUMBER,
                PCWSTR::null(),
                (&raw mut status).cast(),
                &mut status_size,
                std::ptr::null_mut(),
            )
        }
        .ok()
        .is_err()
        {
            return Err(failure("the response had no status code"));
        }
        on_head(status as u16, read_raw_headers(api, call.1));
        let mut chunk = vec![0_u8; READ_CHUNK];
        loop {
            let mut read = 0_u32;
            // safety: the chunk buffer is live and its capacity is passed as the read limit
            let ok = unsafe {
                (api.read_data)(
                    call.1,
                    chunk.as_mut_ptr().cast(),
                    chunk.len() as u32,
                    &mut read,
                )
            }
            .ok();
            if ok.is_err() {
                return Err(failure(format!(
                    "the response from {host} could not be read"
                )));
            }
            if read == 0 {
                return Ok(());
            }
            if !on_chunk(&chunk[..read as usize]) {
                return Err(failure(format!(
                    "the response from {host} is larger than {} MiB",
                    MAX_RESPONSE_BYTES / (1024 * 1024)
                )));
            }
        }
    }

    fn read_raw_headers(api: &WinHttp, call: *mut c_void) -> BTreeMap<String, String> {
        let mut length = 0_u32;
        // safety: no buffer asks WinHTTP for the size it needs, which it reports as a failure
        let _ = unsafe {
            (api.query_headers)(
                call,
                WINHTTP_QUERY_RAW_HEADERS_CRLF,
                PCWSTR::null(),
                std::ptr::null_mut(),
                &mut length,
                std::ptr::null_mut(),
            )
        };
        if length == 0 || length as usize > 1 << 20 {
            return BTreeMap::new();
        }
        let mut buffer = vec![0_u16; length as usize / 2 + 1];
        // safety: the buffer holds the byte length WinHTTP reported
        if unsafe {
            (api.query_headers)(
                call,
                WINHTTP_QUERY_RAW_HEADERS_CRLF,
                PCWSTR::null(),
                buffer.as_mut_ptr().cast(),
                &mut length,
                std::ptr::null_mut(),
            )
        }
        .ok()
        .is_err()
        {
            return BTreeMap::new();
        }
        let text = String::from_utf16_lossy(&buffer);
        parse_headers(text.trim_end_matches('\0'))
    }

    fn parse_headers(text: &str) -> BTreeMap<String, String> {
        let mut headers = BTreeMap::new();
        for line in text.split("\r\n").skip(1) {
            let Some((name, value)) = line.split_once(':') else {
                continue;
            };
            let name = name.trim().to_ascii_lowercase();
            if name.is_empty() {
                continue;
            }
            headers.insert(name, value.trim().to_string());
        }
        headers
    }

    // an https url splits into the host, its port and the origin relative target
    fn split_target(url: &str) -> Result<(String, u16, String), NavigationError> {
        let rest = url
            .strip_prefix("https://")
            .ok_or(NavigationError::InsecureScheme)?;
        let (authority, target) = match rest.find(['/', '?']) {
            Some(index) => (&rest[..index], &rest[index..]),
            None => (rest, "/"),
        };
        let (host, port) = match authority.rsplit_once(':') {
            Some((host, port)) => (
                host,
                port.parse::<u16>()
                    .map_err(|_| NavigationError::InvalidUrl(url.to_string()))?,
            ),
            None => (authority, 443),
        };
        if host.is_empty() {
            return Err(NavigationError::InvalidUrl(url.to_string()));
        }
        Ok((host.to_string(), port, target.to_string()))
    }

    #[cfg(test)]
    mod tests {
        use super::{parse_headers, split_target};

        #[test]
        fn splits_https_targets_into_host_port_and_path() {
            assert_eq!(
                split_target("https://api.example.test/v1/items?page=2").expect("url"),
                (
                    "api.example.test".to_string(),
                    443,
                    "/v1/items?page=2".to_string()
                )
            );
            assert_eq!(
                split_target("https://api.example.test:8443").expect("url"),
                ("api.example.test".to_string(), 8443, "/".to_string())
            );
            assert!(split_target("http://api.example.test").is_err());
        }

        #[test]
        fn reads_headers_after_the_status_line() {
            let headers = parse_headers(
                "HTTP/1.1 200 OK\r\nContent-Type: application/json; charset=utf-8\r\nETag: \"abc\"\r\n",
            );
            assert_eq!(
                headers.get("content-type").map(String::as_str),
                Some("application/json; charset=utf-8")
            );
            assert_eq!(headers.get("etag").map(String::as_str), Some("\"abc\""));
        }
    }
}

#[cfg(not(target_os = "windows"))]
mod platform {
    use ceri_engine::{ExternalRequest, NavigationError, ResourceResponse};
    use std::collections::BTreeMap;

    pub(super) fn request(request: &ExternalRequest) -> Result<ResourceResponse, NavigationError> {
        ceri_engine::request_external(request)
    }

    // the request answers whole here, so the body is reported as one piece
    pub(super) fn exchange(
        request: &ExternalRequest,
        _streamed: bool,
        mut on_head: impl FnMut(u16, BTreeMap<String, String>),
        mut on_chunk: impl FnMut(&[u8]) -> bool,
    ) -> Result<(), NavigationError> {
        let response = ceri_engine::request_external(request)?;
        on_head(response.status(), response.headers().clone());
        on_chunk(response.body());
        Ok(())
    }
}

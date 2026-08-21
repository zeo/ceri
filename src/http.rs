use ceri_engine::{ExternalRequest, NavigationError, ResourceResponse};

const MAX_RESPONSE_BYTES: usize = 32 * 1024 * 1024;

pub fn request(request: &ExternalRequest) -> Result<ResourceResponse, NavigationError> {
    platform::request(request)
}

#[cfg(target_os = "windows")]
mod platform {
    use super::MAX_RESPONSE_BYTES;
    use ceri_engine::{ExternalRequest, NavigationError, ResourceResponse};
    use std::collections::BTreeMap;
    use std::ffi::c_void;

    type Handle = *mut c_void;

    const ACCESS_TYPE_AUTOMATIC_PROXY: u32 = 4;
    const FLAG_SECURE: u32 = 0x0080_0000;
    const ADD_REQUEST_HEADERS_REPLACE: u32 = 0x8000_0000;
    const ADD_REQUEST_HEADERS_ADD: u32 = 0x2000_0000;
    const QUERY_STATUS_CODE: u32 = 19;
    const QUERY_RAW_HEADERS_CRLF: u32 = 22;
    const QUERY_FLAG_NUMBER: u32 = 0x2000_0000;
    const TIMEOUT_MILLISECONDS: i32 = 30_000;
    const READ_CHUNK: usize = 64 * 1024;

    #[link(name = "winhttp")]
    unsafe extern "system" {
        fn WinHttpOpen(
            agent: *const u16,
            access_type: u32,
            proxy: *const u16,
            proxy_bypass: *const u16,
            flags: u32,
        ) -> Handle;
        fn WinHttpSetTimeouts(
            session: Handle,
            resolve: i32,
            connect: i32,
            send: i32,
            receive: i32,
        ) -> i32;
        fn WinHttpConnect(session: Handle, host: *const u16, port: u16, reserved: u32) -> Handle;
        fn WinHttpOpenRequest(
            connection: Handle,
            verb: *const u16,
            object: *const u16,
            version: *const u16,
            referrer: *const u16,
            accept_types: *const *const u16,
            flags: u32,
        ) -> Handle;
        fn WinHttpAddRequestHeaders(
            request: Handle,
            headers: *const u16,
            length: u32,
            modifiers: u32,
        ) -> i32;
        fn WinHttpSendRequest(
            request: Handle,
            headers: *const u16,
            headers_length: u32,
            optional: *const c_void,
            optional_length: u32,
            total_length: u32,
            context: usize,
        ) -> i32;
        fn WinHttpReceiveResponse(request: Handle, reserved: *mut c_void) -> i32;
        fn WinHttpQueryHeaders(
            request: Handle,
            info_level: u32,
            name: *const u16,
            buffer: *mut c_void,
            buffer_length: *mut u32,
            index: *mut u32,
        ) -> i32;
        fn WinHttpReadData(
            request: Handle,
            buffer: *mut c_void,
            to_read: u32,
            read: *mut u32,
        ) -> i32;
        fn WinHttpCloseHandle(handle: Handle) -> i32;
    }

    struct Internet(Handle);

    impl Drop for Internet {
        fn drop(&mut self) {
            if !self.0.is_null() {
                // SAFETY: the handle came from WinHTTP and is closed once
                unsafe { WinHttpCloseHandle(self.0) };
            }
        }
    }

    fn wide(value: &str) -> Vec<u16> {
        value.encode_utf16().chain(std::iter::once(0)).collect()
    }

    fn failure(detail: impl Into<String>) -> NavigationError {
        NavigationError::ResourceProvider(detail.into())
    }

    // the request travels over the platform TLS stack, so certificate policy stays with the OS
    pub(super) fn request(request: &ExternalRequest) -> Result<ResourceResponse, NavigationError> {
        let (host, port, target) = split_target(request.url())?;
        let agent = wide(concat!("Ceri/", env!("CARGO_PKG_VERSION")));
        // SAFETY: every pointer below refers to a live null terminated buffer
        let session = Internet(unsafe {
            WinHttpOpen(
                agent.as_ptr(),
                ACCESS_TYPE_AUTOMATIC_PROXY,
                std::ptr::null(),
                std::ptr::null(),
                0,
            )
        });
        if session.0.is_null() {
            return Err(failure("the platform HTTP stack is unavailable"));
        }
        // SAFETY: the session handle is live
        unsafe {
            WinHttpSetTimeouts(
                session.0,
                TIMEOUT_MILLISECONDS,
                TIMEOUT_MILLISECONDS,
                TIMEOUT_MILLISECONDS,
                TIMEOUT_MILLISECONDS,
            )
        };
        let host_wide = wide(&host);
        // SAFETY: the session handle is live and the host buffer outlives the call
        let connection =
            Internet(unsafe { WinHttpConnect(session.0, host_wide.as_ptr(), port, 0) });
        if connection.0.is_null() {
            return Err(failure(format!("could not connect to {host}")));
        }
        let verb = wide(request.method());
        let object = wide(&target);
        // SAFETY: the connection handle is live and both buffers outlive the call
        let call = Internet(unsafe {
            WinHttpOpenRequest(
                connection.0,
                verb.as_ptr(),
                object.as_ptr(),
                std::ptr::null(),
                std::ptr::null(),
                std::ptr::null(),
                FLAG_SECURE,
            )
        });
        if call.0.is_null() {
            return Err(failure(format!("could not open a request to {host}")));
        }
        if !request.headers().is_empty() {
            let headers = request
                .headers()
                .iter()
                .map(|(name, value)| format!("{name}: {value}"))
                .collect::<Vec<_>>()
                .join("\r\n");
            let headers = wide(&headers);
            // SAFETY: the request handle is live and the header buffer outlives the call
            unsafe {
                WinHttpAddRequestHeaders(
                    call.0,
                    headers.as_ptr(),
                    u32::MAX,
                    ADD_REQUEST_HEADERS_ADD | ADD_REQUEST_HEADERS_REPLACE,
                )
            };
        }
        let body = request.body();
        // SAFETY: the body slice outlives the call, and its length is passed unchanged
        let sent = unsafe {
            WinHttpSendRequest(
                call.0,
                std::ptr::null(),
                0,
                body.as_ptr() as *const c_void,
                body.len() as u32,
                body.len() as u32,
                0,
            )
        };
        if sent == 0 {
            return Err(failure(format!("the request to {host} could not be sent")));
        }
        // SAFETY: the request handle is live
        if unsafe { WinHttpReceiveResponse(call.0, std::ptr::null_mut()) } == 0 {
            return Err(failure(format!("{host} did not answer the request")));
        }

        let mut status = 0_u32;
        let mut status_size = std::mem::size_of::<u32>() as u32;
        // SAFETY: the buffer is a u32 and its size is passed as such
        if unsafe {
            WinHttpQueryHeaders(
                call.0,
                QUERY_STATUS_CODE | QUERY_FLAG_NUMBER,
                std::ptr::null(),
                &mut status as *mut u32 as *mut c_void,
                &mut status_size,
                std::ptr::null_mut(),
            )
        } == 0
        {
            return Err(failure("the response had no status code"));
        }
        let headers = read_raw_headers(call.0);
        let mut body = Vec::new();
        let mut chunk = vec![0_u8; READ_CHUNK];
        loop {
            let mut read = 0_u32;
            // SAFETY: the chunk buffer is live and its capacity is passed as the read limit
            let ok = unsafe {
                WinHttpReadData(
                    call.0,
                    chunk.as_mut_ptr() as *mut c_void,
                    chunk.len() as u32,
                    &mut read,
                )
            };
            if ok == 0 {
                return Err(failure(format!(
                    "the response from {host} could not be read"
                )));
            }
            if read == 0 {
                break;
            }
            body.extend_from_slice(&chunk[..read as usize]);
            if body.len() > MAX_RESPONSE_BYTES {
                return Err(failure(format!(
                    "the response from {host} is larger than {} MiB",
                    MAX_RESPONSE_BYTES / (1024 * 1024)
                )));
            }
        }
        let mime_type = headers
            .get("content-type")
            .and_then(|value| value.split(';').next())
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .map(str::to_string)
            .unwrap_or_else(|| "application/octet-stream".to_string());
        ResourceResponse::new(status as u16, mime_type, headers, body)
    }

    fn read_raw_headers(call: Handle) -> BTreeMap<String, String> {
        let mut length = 0_u32;
        // SAFETY: a null buffer asks WinHTTP for the size it needs
        unsafe {
            WinHttpQueryHeaders(
                call,
                QUERY_RAW_HEADERS_CRLF,
                std::ptr::null(),
                std::ptr::null_mut(),
                &mut length,
                std::ptr::null_mut(),
            )
        };
        if length == 0 || length as usize > 1 << 20 {
            return BTreeMap::new();
        }
        let mut buffer = vec![0_u16; length as usize / 2 + 1];
        // SAFETY: the buffer holds the byte length WinHTTP reported
        if unsafe {
            WinHttpQueryHeaders(
                call,
                QUERY_RAW_HEADERS_CRLF,
                std::ptr::null(),
                buffer.as_mut_ptr() as *mut c_void,
                &mut length,
                std::ptr::null_mut(),
            )
        } == 0
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

    pub(super) fn request(request: &ExternalRequest) -> Result<ResourceResponse, NavigationError> {
        ceri_engine::request_external(request)
    }
}

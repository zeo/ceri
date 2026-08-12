use crate::navigation::{
    AppUrl, NavigationError, Resource, ResourceProvider, ResourceRequest, ResourceResponse,
};
use std::collections::BTreeMap;
use std::time::Duration;
use ureq::http::Request;
use ureq::tls::{RootCerts, TlsConfig};
use ureq::Agent;

const DEFAULT_TIMEOUT: Duration = Duration::from_secs(30);

pub struct NativeHttpProvider {
    agent: Agent,
    base_url: String,
}

impl NativeHttpProvider {
    pub fn new(base_url: impl Into<String>) -> Result<Self, NavigationError> {
        let base_url = validate_base_url(base_url.into())?;
        let config = Agent::config_builder()
            .https_only(true)
            .http_status_as_error(false)
            .max_redirects(0)
            .timeout_global(Some(DEFAULT_TIMEOUT))
            .tls_config(
                TlsConfig::builder()
                    .root_certs(RootCerts::PlatformVerifier)
                    .build(),
            )
            .build();
        Ok(Self {
            agent: config.new_agent(),
            base_url,
        })
    }

    pub fn base_url(&self) -> &str {
        &self.base_url
    }

    fn target_url(&self, url: &AppUrl) -> String {
        let path = encode_path(url.path());
        let mut target = format!("{}{}", self.base_url, path);
        if !url.query().is_empty() {
            target.push('?');
            target.push_str(url.query());
        }
        target
    }

    fn request_response(
        &self,
        request: &ResourceRequest,
    ) -> Result<ResourceResponse, NavigationError> {
        let target = self.target_url(request.url());
        let path = request.url().path().to_string();
        let mut builder = Request::builder()
            .method(request.method())
            .uri(&target);
        for (name, value) in request.headers() {
            builder = builder.header(name, value);
        }
        let request = builder.body(request.body().to_vec()).map_err(|error| {
            NavigationError::ResourceProvider(format!("could not build HTTP request: {error}"))
        })?;
        let mut response = self
            .agent
            .run(request)
            .map_err(|error| NavigationError::ResourceProvider(error.to_string()))?;
        let status = response.status().as_u16();
        let mut headers = BTreeMap::new();
        for (name, value) in response.headers() {
            let value = value.to_str().map_err(|error| {
                NavigationError::ResourceProvider(format!(
                    "response header {} is not visible text: {error}",
                    name.as_str()
                ))
            })?;
            headers.insert(name.as_str().to_string(), value.to_string());
        }
        let mime_type = headers
            .get("content-type")
            .and_then(|value| value.split(';').next())
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .map(str::to_string)
            .unwrap_or_else(|| mime_type_for_path(&path).to_string());
        let body = response
            .body_mut()
            .read_to_vec()
            .map_err(|error| NavigationError::ResourceProvider(error.to_string()))?;
        ResourceResponse::new(status, mime_type, headers, body)
    }
}

impl ResourceProvider for NativeHttpProvider {
    fn provide(&self, url: &AppUrl) -> Result<Option<Resource>, NavigationError> {
        let response = self.request_response(&ResourceRequest::get(url.clone()))?;
        if !(200..300).contains(&response.status()) {
            return Ok(None);
        }
        Ok(Some(Resource::new(response.mime_type(), response.body().to_vec())))
    }

    fn provide_response(
        &self,
        request: &ResourceRequest,
    ) -> Result<Option<ResourceResponse>, NavigationError> {
        self.request_response(request).map(Some)
    }
}

fn validate_base_url(base_url: String) -> Result<String, NavigationError> {
    let base_url = base_url.trim().trim_end_matches('/').to_string();
    let Some(authority_and_path) = base_url.strip_prefix("https://") else {
        return Err(NavigationError::InsecureScheme);
    };
    let authority = authority_and_path
        .split_once('/')
        .map_or(authority_and_path, |(authority, _)| authority);
    if authority.is_empty()
        || authority.contains('@')
        || base_url.chars().any(|character| character.is_whitespace())
        || base_url.contains(['?', '#', '\0'])
    {
        return Err(NavigationError::InvalidUrl(base_url));
    }
    Ok(base_url)
}

fn encode_path(path: &str) -> String {
    const HEX: &[u8; 16] = b"0123456789ABCDEF";
    let mut encoded = String::with_capacity(path.len());
    for byte in path.bytes() {
        if byte.is_ascii_alphanumeric() || matches!(byte, b'/' | b'-' | b'.' | b'_' | b'~') {
            encoded.push(byte as char);
        } else {
            encoded.push('%');
            encoded.push(HEX[(byte >> 4) as usize] as char);
            encoded.push(HEX[(byte & 0x0f) as usize] as char);
        }
    }
    encoded
}

fn mime_type_for_path(path: &str) -> &'static str {
    let extension = path.rsplit_once('.').map_or("", |(_, extension)| extension);
    match extension.to_ascii_lowercase().as_str() {
        "css" => "text/css",
        "htm" | "html" => "text/html",
        "js" | "mjs" => "text/javascript",
        "json" => "application/json",
        "svg" => "image/svg+xml",
        "wasm" => "application/wasm",
        "png" => "image/png",
        "jpg" | "jpeg" => "image/jpeg",
        "gif" => "image/gif",
        "woff" => "font/woff",
        "woff2" => "font/woff2",
        _ => "application/octet-stream",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn validates_https_base_urls_without_creating_a_listener() {
        let provider = NativeHttpProvider::new("https://example.test/app/")
            .expect("HTTPS provider base should be accepted");
        assert_eq!(provider.base_url(), "https://example.test/app");
        let url = AppUrl::parse("https://app.ceri.invalid/docs/hello world?q=rust")
            .expect("app URL should parse");
        assert_eq!(
            provider.target_url(&url),
            "https://example.test/app/docs/hello%20world?q=rust"
        );
    }

    #[test]
    fn rejects_insecure_or_credentialed_base_urls() {
        assert!(matches!(
            NativeHttpProvider::new("http://example.test"),
            Err(NavigationError::InsecureScheme)
        ));
        assert!(matches!(
            NativeHttpProvider::new("https://user@example.test"),
            Err(NavigationError::InvalidUrl(_))
        ));
    }

    #[test]
    fn encodes_paths_and_infers_common_mime_types() {
        assert_eq!(encode_path("/docs/café file.html"), "/docs/caf%C3%A9%20file.html");
        assert_eq!(mime_type_for_path("/data/config.json"), "application/json");
        assert_eq!(mime_type_for_path("/assets/unknown.bin"), "application/octet-stream");
    }
}

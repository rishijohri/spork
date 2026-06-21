//! [`HttpTransport`] — POST a request to a **plaintext `http://`** OpenAI-compatible
//! endpoint and parse the JSON response.
//!
//! This is a hand-rolled, dependency-free HTTP/1.1 client over
//! [`std::net::TcpStream`]. It exists to serve the **local** model servers P6
//! targets — Ollama, LM Studio, vLLM, a LiteLLM proxy — which all speak
//! OpenAI-compatible JSON over plaintext HTTP on localhost (the
//! [`OpenAiAdapter`](https://docs.rs/spork-provider) maps to/from that shape).
//! The daemon authorizes `model.invoke` and `net.connect` before it runs
//! (DESIGN.md §15.2).
//!
//! ## Scope: plaintext only (TLS cloud is an additive impl behind the seam)
//!
//! A first-party-cloud endpoint (`https://api.openai.com`,
//! `https://api.anthropic.com`) needs TLS, which needs a client dependency the
//! workspace has not adopted and which cannot be exercised offline. That is an
//! explicit out-of-scope for this slice: it lands as a *separate* `Transport`
//! impl (e.g. a `TlsHttpTransport`) behind the same trait, changing nothing here
//! (CLAUDE.md C1/C3). To keep the boundary honest this transport **refuses** an
//! `https://` endpoint with [`TransportError::UnsupportedEndpoint`] rather than
//! pretend to support it.

use std::io::{Read, Write};
use std::net::{TcpStream, ToSocketAddrs};
use std::time::Duration;

use serde_json::Value;

use crate::{Transport, TransportError};

/// The default per-request socket read timeout. A local model can take a while to
/// generate, but an unbounded read would hang the calling dispatch on a stuck
/// server; this bounds it (a configurable timeout is an additive follow-up).
const DEFAULT_READ_TIMEOUT: Duration = Duration::from_secs(120);

/// The connect timeout — a local server is either up or it is not, so a short
/// bound surfaces "server not running" quickly instead of a long TCP wait.
const DEFAULT_CONNECT_TIMEOUT: Duration = Duration::from_secs(10);

/// The default cap on a response's total bytes. The transport talks to a separate
/// local model-server process it cannot vouch for; an unbounded `read_to_end`
/// would let a malicious/buggy server stream gigabytes within the read timeout and
/// exhaust the single-threaded daemon's memory. A model's JSON response is small,
/// so a generous-but-finite 64 MiB cap is ample; exceeding it is a transport error.
const DEFAULT_MAX_RESPONSE_BYTES: u64 = 64 * 1024 * 1024;

/// A [`Transport`] that POSTs JSON to a plaintext `http://` endpoint.
///
/// Construct with [`HttpTransport::new`] giving the full endpoint URL (e.g.
/// `http://127.0.0.1:11434/v1/chat/completions`). Extra headers — for example an
/// `Authorization` a local proxy expects — are added with
/// [`HttpTransport::header`]; the daemon resolves any secret from the vault and
/// passes the literal value, so no secret is stored in this struct beyond the
/// life of the request the daemon builds (DESIGN.md §15.4).
#[derive(Debug, Clone)]
pub struct HttpTransport {
    endpoint: String,
    headers: Vec<(String, String)>,
    max_response_bytes: u64,
}

impl HttpTransport {
    /// A transport that POSTs to `endpoint` (which must be an `http://` URL).
    ///
    /// The scheme is validated lazily on [`invoke`](Transport::invoke) so
    /// construction never fails; an `https://` endpoint is refused there with
    /// [`TransportError::UnsupportedEndpoint`].
    #[must_use]
    pub fn new(endpoint: impl Into<String>) -> Self {
        HttpTransport {
            endpoint: endpoint.into(),
            headers: Vec::new(),
            max_response_bytes: DEFAULT_MAX_RESPONSE_BYTES,
        }
    }

    /// Add a request header (e.g. `Authorization: Bearer …` for a local proxy).
    #[must_use]
    pub fn header(mut self, name: impl Into<String>, value: impl Into<String>) -> Self {
        self.headers.push((name.into(), value.into()));
        self
    }

    /// Override the maximum response size (bytes) accepted before the transport
    /// rejects the response (default [`DEFAULT_MAX_RESPONSE_BYTES`]).
    #[must_use]
    pub fn with_max_response_bytes(mut self, max: u64) -> Self {
        self.max_response_bytes = max;
        self
    }

    /// The endpoint URL this transport POSTs to.
    #[must_use]
    pub fn endpoint(&self) -> &str {
        &self.endpoint
    }
}

impl Transport for HttpTransport {
    fn invoke(&self, request: &Value) -> Result<Value, TransportError> {
        let target = HttpTarget::parse(&self.endpoint)?;
        let body = serde_json::to_vec(request)
            .map_err(|e| TransportError::Io(format!("encode request: {e}")))?;

        // Resolve + connect with a bounded connect timeout so a down server fails
        // fast rather than blocking the dispatch on a long TCP retry.
        let addr = (target.host.as_str(), target.port)
            .to_socket_addrs()
            .map_err(|e| TransportError::Io(format!("resolve {}: {e}", target.host)))?
            .next()
            .ok_or_else(|| TransportError::Io(format!("no address for {}", target.host)))?;
        let mut stream = TcpStream::connect_timeout(&addr, DEFAULT_CONNECT_TIMEOUT)
            .map_err(|e| TransportError::Io(format!("connect {addr}: {e}")))?;
        stream
            .set_read_timeout(Some(DEFAULT_READ_TIMEOUT))
            .map_err(|e| TransportError::Io(format!("set read timeout: {e}")))?;

        let mut req = Vec::with_capacity(body.len() + 256);
        write!(
            &mut req,
            "POST {} HTTP/1.1\r\nHost: {}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\nAccept: application/json\r\n",
            target.path,
            target.host_header(),
            body.len()
        )
        .map_err(|e| TransportError::Io(format!("build request: {e}")))?;
        for (name, value) in &self.headers {
            write!(&mut req, "{name}: {value}\r\n")
                .map_err(|e| TransportError::Io(format!("build header {name}: {e}")))?;
        }
        req.extend_from_slice(b"\r\n");
        req.extend_from_slice(&body);

        stream
            .write_all(&req)
            .map_err(|e| TransportError::Io(format!("write request: {e}")))?;
        stream
            .flush()
            .map_err(|e| TransportError::Io(format!("flush request: {e}")))?;

        // We send `Connection: close`, so the server closes after the full
        // response — reading to EOF yields the complete message. Read at most
        // `max_response_bytes + 1` so an over-cap response is detected and
        // rejected rather than exhausting memory (the server is an untrusted
        // separate process).
        let mut raw = Vec::new();
        (&mut stream)
            .take(self.max_response_bytes.saturating_add(1))
            .read_to_end(&mut raw)
            .map_err(|e| TransportError::Io(format!("read response: {e}")))?;
        if raw.len() as u64 > self.max_response_bytes {
            return Err(TransportError::MalformedResponse(format!(
                "response exceeds the {}-byte cap",
                self.max_response_bytes
            )));
        }

        let (status, body_bytes) = parse_http_response(&raw)?;
        let body_str = String::from_utf8_lossy(&body_bytes);
        if !(200..300).contains(&status) {
            return Err(TransportError::Http {
                status,
                body: truncate(body_str.trim(), 512),
            });
        }
        serde_json::from_str::<Value>(body_str.trim()).map_err(|e| {
            TransportError::MalformedResponse(format!(
                "response body is not JSON ({e}): {}",
                truncate(body_str.trim(), 256)
            ))
        })
    }
}

/// A parsed plaintext-HTTP target: host, port, and request path.
struct HttpTarget {
    host: String,
    port: u16,
    path: String,
}

impl HttpTarget {
    /// Parse an `http://host[:port][/path]` endpoint. Refuses non-`http` schemes
    /// (notably `https://`, which needs the TLS transport this slice defers).
    fn parse(endpoint: &str) -> Result<Self, TransportError> {
        if let Some(rest) = endpoint.strip_prefix("https://") {
            let _ = rest;
            return Err(TransportError::UnsupportedEndpoint(format!(
                "{endpoint:?} is https; the plaintext HttpTransport serves local http only — \
                 a TLS transport for cloud endpoints is a separate impl behind the Transport seam"
            )));
        }
        let rest = endpoint.strip_prefix("http://").ok_or_else(|| {
            TransportError::UnsupportedEndpoint(format!("{endpoint:?} is not an http:// URL"))
        })?;
        let (authority, path) = match rest.find('/') {
            Some(i) => (&rest[..i], &rest[i..]),
            None => (rest, "/"),
        };
        if authority.is_empty() {
            return Err(TransportError::UnsupportedEndpoint(format!(
                "{endpoint:?} has no host"
            )));
        }
        let (host, port) = match authority.rsplit_once(':') {
            Some((h, p)) => {
                let port = p.parse::<u16>().map_err(|_| {
                    TransportError::UnsupportedEndpoint(format!("invalid port in {endpoint:?}"))
                })?;
                (h.to_string(), port)
            }
            None => (authority.to_string(), 80),
        };
        Ok(HttpTarget {
            host,
            port,
            path: path.to_string(),
        })
    }

    /// The `Host:` header value (includes the port when it is non-default).
    fn host_header(&self) -> String {
        if self.port == 80 {
            self.host.clone()
        } else {
            format!("{}:{}", self.host, self.port)
        }
    }
}

/// Parse a raw HTTP/1.1 response into `(status_code, body_bytes)`.
///
/// Handles the three body framings a local OpenAI-compatible server uses:
/// `Content-Length`, `Transfer-Encoding: chunked`, and (with our
/// `Connection: close`) read-to-EOF. The header scan is case-insensitive.
fn parse_http_response(raw: &[u8]) -> Result<(u16, Vec<u8>), TransportError> {
    // Split headers from body on the first CRLFCRLF.
    let split = find_subslice(raw, b"\r\n\r\n").ok_or_else(|| {
        TransportError::MalformedResponse("no header/body boundary in response".into())
    })?;
    let header_block = &raw[..split];
    let body = &raw[split + 4..];

    let header_text = String::from_utf8_lossy(header_block);
    let mut lines = header_text.split("\r\n");
    let status_line = lines
        .next()
        .ok_or_else(|| TransportError::MalformedResponse("empty response".into()))?;
    // `HTTP/1.1 200 OK` → the second whitespace-separated token is the code.
    let status = status_line
        .split_whitespace()
        .nth(1)
        .and_then(|c| c.parse::<u16>().ok())
        .ok_or_else(|| {
            TransportError::MalformedResponse(format!("bad status line {status_line:?}"))
        })?;

    let mut chunked = false;
    for line in lines {
        if let Some((name, value)) = line.split_once(':') {
            let name = name.trim().to_ascii_lowercase();
            let value = value.trim();
            if name == "transfer-encoding" && value.eq_ignore_ascii_case("chunked") {
                chunked = true;
            }
        }
    }

    let body = if chunked {
        dechunk(body)?
    } else {
        // Content-Length and read-to-EOF both leave the body as the bytes after
        // the boundary (we read to EOF, so a Content-Length body is complete).
        body.to_vec()
    };
    Ok((status, body))
}

/// Decode an HTTP/1.1 chunked-transfer body into its concatenated payload.
fn dechunk(mut body: &[u8]) -> Result<Vec<u8>, TransportError> {
    let mut out = Vec::with_capacity(body.len());
    loop {
        let line_end = find_subslice(body, b"\r\n").ok_or_else(|| {
            TransportError::MalformedResponse("chunk size line missing CRLF".into())
        })?;
        let size_line = String::from_utf8_lossy(&body[..line_end]);
        // A chunk size may carry extensions after a ';'; take the hex prefix only.
        let hex = size_line.split(';').next().unwrap_or("").trim();
        let size = usize::from_str_radix(hex, 16)
            .map_err(|_| TransportError::MalformedResponse(format!("bad chunk size {hex:?}")))?;
        body = &body[line_end + 2..];
        if size == 0 {
            break;
        }
        if body.len() < size {
            return Err(TransportError::MalformedResponse(
                "chunk shorter than its declared size".into(),
            ));
        }
        out.extend_from_slice(&body[..size]);
        body = &body[size..];
        // Each chunk's data is followed by a CRLF.
        if body.starts_with(b"\r\n") {
            body = &body[2..];
        }
    }
    Ok(out)
}

/// Find the first index of `needle` in `haystack`.
fn find_subslice(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    if needle.is_empty() || haystack.len() < needle.len() {
        return None;
    }
    haystack
        .windows(needle.len())
        .position(|window| window == needle)
}

/// Truncate a diagnostic string to `max` bytes on a char boundary.
fn truncate(s: &str, max: usize) -> String {
    if s.len() <= max {
        return s.to_string();
    }
    let mut end = max;
    while !s.is_char_boundary(end) {
        end -= 1;
    }
    format!("{}…", &s[..end])
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::net::TcpListener;
    use std::thread;

    /// Spawn a one-shot loopback HTTP server that reads one request and writes
    /// `response` verbatim, returning its `http://127.0.0.1:<port>` base URL.
    fn serve_once(response: &'static [u8]) -> String {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        thread::spawn(move || {
            if let Ok((mut socket, _)) = listener.accept() {
                // Drain the request headers+body enough to unblock the client.
                let mut buf = [0u8; 4096];
                let _ = socket.read(&mut buf);
                let _ = socket.write_all(response);
                let _ = socket.flush();
                // Dropping the socket closes the connection (Connection: close).
            }
        });
        format!("http://{addr}")
    }

    #[test]
    fn content_length_response_parses() {
        let body = b"{\"choices\":[{\"message\":{\"role\":\"assistant\",\"content\":\"hi\"}}]}";
        // Build a Content-Length response.
        let resp: &'static [u8] = Box::leak(
            format!(
                "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                body.len(),
                std::str::from_utf8(body).unwrap()
            )
            .into_bytes()
            .into_boxed_slice(),
        );
        let base = serve_once(resp);
        let t = HttpTransport::new(format!("{base}/v1/chat/completions"));
        let out = t
            .invoke(&serde_json::json!({ "model": "llama3.1", "messages": [] }))
            .unwrap();
        assert_eq!(out["choices"][0]["message"]["content"], "hi");
    }

    #[test]
    fn chunked_response_parses() {
        // Two chunks ("{\"ok\":" + "true}") then the terminating zero chunk.
        let resp: &'static [u8] = b"HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\nConnection: close\r\n\r\n6\r\n{\"ok\":\r\n5\r\ntrue}\r\n0\r\n\r\n";
        let base = serve_once(resp);
        let t = HttpTransport::new(format!("{base}/v1/chat/completions"));
        let out = t.invoke(&serde_json::json!({})).unwrap();
        assert_eq!(out["ok"], true);
    }

    #[test]
    fn non_2xx_status_is_an_http_error() {
        let resp: &'static [u8] = b"HTTP/1.1 429 Too Many Requests\r\nContent-Length: 13\r\nConnection: close\r\n\r\nrate limited!";
        let base = serve_once(resp);
        let t = HttpTransport::new(format!("{base}/v1/chat/completions"));
        let err = t.invoke(&serde_json::json!({})).unwrap_err();
        match err {
            TransportError::Http { status, body } => {
                assert_eq!(status, 429);
                assert!(body.contains("rate limited"), "{body}");
            }
            other => panic!("expected Http error, got {other:?}"),
        }
    }

    #[test]
    fn non_json_2xx_body_is_malformed() {
        let resp: &'static [u8] =
            b"HTTP/1.1 200 OK\r\nContent-Length: 11\r\nConnection: close\r\n\r\nnot json at";
        let base = serve_once(resp);
        let t = HttpTransport::new(format!("{base}/v1/chat/completions"));
        let err = t.invoke(&serde_json::json!({})).unwrap_err();
        assert!(matches!(err, TransportError::MalformedResponse(_)));
    }

    #[test]
    fn response_over_the_size_cap_is_rejected() {
        // A response larger than the configured cap is refused rather than read
        // unbounded into memory (an untrusted local server could stream forever).
        let body = "{\"choices\":[{\"message\":{\"content\":\"a fairly long body that exceeds the tiny cap\"}}]}";
        let resp: &'static [u8] = Box::leak(
            format!(
                "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                body.len(),
                body
            )
            .into_bytes()
            .into_boxed_slice(),
        );
        let base = serve_once(resp);
        let t =
            HttpTransport::new(format!("{base}/v1/chat/completions")).with_max_response_bytes(16);
        let err = t.invoke(&serde_json::json!({})).unwrap_err();
        assert!(
            matches!(err, TransportError::MalformedResponse(ref m) if m.contains("cap")),
            "expected a size-cap error, got {err:?}"
        );
    }

    #[test]
    fn https_endpoint_is_refused() {
        let t = HttpTransport::new("https://api.openai.com/v1/chat/completions");
        let err = t.invoke(&serde_json::json!({})).unwrap_err();
        assert!(matches!(err, TransportError::UnsupportedEndpoint(_)));
    }

    #[test]
    fn non_http_endpoint_is_refused() {
        let t = HttpTransport::new("ftp://localhost/x");
        assert!(matches!(
            t.invoke(&serde_json::json!({})).unwrap_err(),
            TransportError::UnsupportedEndpoint(_)
        ));
    }

    #[test]
    fn connection_refused_is_an_io_error() {
        // Port 1 is reserved and not listening — connect fails fast.
        let t = HttpTransport::new("http://127.0.0.1:1/v1/chat/completions");
        assert!(matches!(
            t.invoke(&serde_json::json!({})).unwrap_err(),
            TransportError::Io(_)
        ));
    }

    #[test]
    fn target_parse_extracts_host_port_path() {
        let target = HttpTarget::parse("http://127.0.0.1:11434/v1/chat/completions").unwrap();
        assert_eq!(target.host, "127.0.0.1");
        assert_eq!(target.port, 11434);
        assert_eq!(target.path, "/v1/chat/completions");
        assert_eq!(target.host_header(), "127.0.0.1:11434");

        let defaulted = HttpTarget::parse("http://localhost").unwrap();
        assert_eq!(defaulted.port, 80);
        assert_eq!(defaulted.path, "/");
        assert_eq!(defaulted.host_header(), "localhost");
    }
}

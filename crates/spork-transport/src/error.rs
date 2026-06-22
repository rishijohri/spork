//! The transport error taxonomy: [`TransportError`].

use thiserror::Error;

/// A failure delivering a request to a model or reading its response.
///
/// `#[non_exhaustive]`: P6 freezes the [`Transport`](crate::Transport) *seam*,
/// not the exhaustive list of every way a future transport (TLS cloud, gRPC, a
/// streaming channel) can fail, so variants may be added additively without a
/// breaking `match` downstream (mirrors `spork_provider::ProviderError`).
#[derive(Debug, Error)]
#[non_exhaustive]
pub enum TransportError {
    /// The request could not be serialized, written, or the response could not
    /// be read — a low-level I/O failure on the pipe or socket.
    #[error("transport io: {0}")]
    Io(String),

    /// A subprocess transport failed to spawn the program, or the program exited
    /// non-zero. Carries the program's diagnostics (e.g. captured stderr) so the
    /// failure is surfaced, never swallowed (DESIGN.md §12.6).
    #[error("subprocess transport: {0}")]
    Process(String),

    /// An HTTP transport reached the endpoint but it answered with a non-2xx
    /// status. Carries the status code and any body text for diagnosis.
    #[error("http {status}: {body}")]
    Http {
        /// The HTTP status code the endpoint returned.
        status: u16,
        /// The response body (truncated for diagnostics).
        body: String,
    },

    /// The endpoint scheme/shape is not one this transport supports — e.g. an
    /// `https://` URL handed to the plaintext [`HttpTransport`](crate::HttpTransport),
    /// which directs the caller to a TLS transport rather than silently failing.
    #[error("unsupported endpoint: {0}")]
    UnsupportedEndpoint(String),

    /// The model returned something that was not valid JSON (or not the JSON
    /// document shape the transport expects on its channel). The adapter's
    /// `from_wire` would otherwise reject it as malformed; the transport catches
    /// the decode failure first so the error names the transport, not the mapping.
    #[error("malformed transport response: {0}")]
    MalformedResponse(String),
}

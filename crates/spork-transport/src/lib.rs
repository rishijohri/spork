//! Spork P6 model-access **transport seam** — the `Transport` port that carries a
//! provider's wire request to a running model and returns its wire response.
//!
//! F4 froze the *mapping* half of model access: the
//! [`ProviderAdapter`](https://docs.rs/spork-provider) renders a canonical
//! transcript to a provider's request JSON and parses its response JSON back,
//! with **no network client** (DESIGN.md §12.1). This crate adds the missing
//! half — the I/O that actually delivers `to_wire(...)`'s payload to a model and
//! feeds the reply to `from_wire(...)` — behind one trait so the agent loop never
//! hard-codes a channel.
//!
//! Per the foundation discipline (CLAUDE.md C1/C3) the trait is the seam and this
//! crate ships **two real, fully-offline-testable implementations**, not a stub:
//!
//! - [`SubprocessTransport`] — drives a local agent CLI by writing the request as
//!   one JSON line to its stdin and reading one JSON document from its stdout
//!   (the JSONL convention the
//!   [`CliAdapter`](https://docs.rs/spork-provider) maps to). Pure `std::process`,
//!   tested against a shell-script fixture.
//! - [`HttpTransport`] — POSTs the request JSON to a **plaintext `http://`**
//!   OpenAI-compatible endpoint and parses the JSON body. A hand-rolled,
//!   dependency-free HTTP/1.1 client over [`std::net::TcpStream`] that handles
//!   `Content-Length`, chunked transfer-encoding, and read-to-EOF — enough for the
//!   local servers P6 targets (Ollama, LM Studio, vLLM, a LiteLLM proxy), tested
//!   against a loopback [`std::net::TcpListener`].
//!
//! ## What is deliberately **not** here (additive, behind the same seam)
//!
//! A **TLS** transport for first-party cloud (`https://api.openai.com`,
//! `https://api.anthropic.com`) is an explicit, documented out-of-scope for this
//! slice (CLAUDE.md C1's "documented out-of-scope, never a silent placeholder"):
//! it requires a TLS client dependency the workspace has not yet taken on, and it
//! cannot be exercised offline. It lands as a *new* `Transport` impl (e.g.
//! `TlsHttpTransport`) behind this exact trait — adding it is purely additive and
//! changes nothing here (CLAUDE.md C3). The plaintext [`HttpTransport`] therefore
//! refuses an `https://` endpoint loudly rather than pretend to support it.
//!
//! Design references: DESIGN.md §5.4 (model access), §12.1 (the provider port),
//! §12.6 (normalization drift rejected loudly), §15.1–§15.2 (the daemon gates
//! `model.invoke`/`net.connect`/`process.spawn` before any of these run).

#![forbid(unsafe_code)]
#![warn(missing_docs)]

mod error;
mod http;
mod subprocess;

pub use error::TransportError;
pub use http::{http_get_json, HttpTransport};
pub use subprocess::SubprocessTransport;

use serde_json::Value;

/// A channel that delivers one provider wire request to a model and returns its
/// wire response.
///
/// This is the hexagonal port the agent loop calls after
/// [`ProviderAdapter::to_wire`](https://docs.rs/spork-provider) and before
/// [`ProviderAdapter::from_wire`](https://docs.rs/spork-provider): the `request`
/// is a provider-native JSON request, the returned [`Value`] is the provider's
/// JSON response, and *neither shape is interpreted here* — the transport only
/// moves bytes, so it is provider-agnostic and a new provider needs no new
/// transport (CLAUDE.md C3).
///
/// `Send + Sync` so the daemon can hold a transport across its owner thread and
/// share it between dispatches.
pub trait Transport: Send + Sync {
    /// Send `request` to the model and return its raw JSON response.
    ///
    /// # Errors
    /// Returns a [`TransportError`] if the request cannot be delivered (process
    /// spawn / connect failure), the model returns a transport-level failure
    /// (non-zero exit / non-2xx status), or the response is not valid JSON.
    fn invoke(&self, request: &Value) -> Result<Value, TransportError>;
}

/// A blanket impl so a `Box<dyn Transport>` is itself a [`Transport`] — lets the
/// agent loop hold transports as trait objects in a registry and still call them
/// through the trait uniformly.
impl Transport for Box<dyn Transport> {
    fn invoke(&self, request: &Value) -> Result<Value, TransportError> {
        (**self).invoke(request)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A trivial in-process transport, proving the seam is object-safe and the
    /// `Box<dyn Transport>` blanket impl forwards correctly (the daemon holds
    /// transports as trait objects).
    struct EchoTransport;
    impl Transport for EchoTransport {
        fn invoke(&self, request: &Value) -> Result<Value, TransportError> {
            Ok(request.clone())
        }
    }

    #[test]
    fn boxed_transport_forwards_through_the_blanket_impl() {
        let boxed: Box<dyn Transport> = Box::new(EchoTransport);
        let req = serde_json::json!({ "model": "x", "lines": [] });
        assert_eq!(boxed.invoke(&req).unwrap(), req);
    }
}

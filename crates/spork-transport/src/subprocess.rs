//! [`SubprocessTransport`] — drive a local agent CLI over stdin/stdout JSONL.
//!
//! An agent CLI (e.g. a Copilot-style CLI) is invoked rather than called over
//! HTTP: it runs locally, reads a request from stdin, and writes its response to
//! stdout (DESIGN.md §12.1, §12.6). This transport realizes exactly that channel
//! for the [`CliAdapter`](https://docs.rs/spork-provider): it writes the request
//! JSON as one line to the child's stdin, closes stdin, waits for the child, and
//! parses one JSON document from its stdout.
//!
//! It is `Local` (the router binds the CLI provider to
//! [`Locality::Local`](https://docs.rs/spork-provider)) and the daemon authorizes
//! `process.spawn` before it runs (DESIGN.md §15.2).
//!
//! ## Blocking semantics (a documented limitation, not a stub)
//!
//! [`invoke`](Transport::invoke) blocks until the child exits (it reads stdout to
//! EOF via [`std::process::Child::wait_with_output`]). A misbehaving model CLI
//! that never exits would block the calling dispatch. A timeout/cancellation
//! wrapper is an additive decorator behind the [`Transport`] seam (it would run
//! the child under a deadline and kill it on expiry); it is intentionally out of
//! scope for this slice and called out here so the omission is a decision, not an
//! oversight.

use std::io::Write;
use std::process::{Command, Stdio};

use serde_json::Value;

use crate::{Transport, TransportError};

/// A [`Transport`] that pipes one request through a local CLI's stdin/stdout.
///
/// Construct with [`SubprocessTransport::new`] giving the program and its
/// arguments; each [`invoke`](Transport::invoke) spawns a fresh child (a clean,
/// stateless invocation per request — the conversation state lives in the
/// canonical transcript the request carries, not in a long-lived process).
#[derive(Debug, Clone)]
pub struct SubprocessTransport {
    program: String,
    args: Vec<String>,
}

impl SubprocessTransport {
    /// A transport that runs `program` with `args` for every request.
    #[must_use]
    pub fn new(
        program: impl Into<String>,
        args: impl IntoIterator<Item = impl Into<String>>,
    ) -> Self {
        SubprocessTransport {
            program: program.into(),
            args: args.into_iter().map(Into::into).collect(),
        }
    }

    /// The program this transport spawns.
    #[must_use]
    pub fn program(&self) -> &str {
        &self.program
    }
}

impl Transport for SubprocessTransport {
    fn invoke(&self, request: &Value) -> Result<Value, TransportError> {
        let body = serde_json::to_vec(request)
            .map_err(|e| TransportError::Io(format!("encode request: {e}")))?;

        let mut child = Command::new(&self.program)
            .args(&self.args)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .map_err(|e| TransportError::Process(format!("spawn {:?}: {e}", self.program)))?;

        // Write the request as one JSON line, then close stdin so the child sees
        // EOF and produces its response. Take the handle so it is dropped (closed)
        // before we wait, avoiding a deadlock where the child blocks on more input.
        {
            let mut stdin = child
                .stdin
                .take()
                .ok_or_else(|| TransportError::Io("child stdin unavailable".into()))?;
            stdin
                .write_all(&body)
                .and_then(|()| stdin.write_all(b"\n"))
                .map_err(|e| TransportError::Io(format!("write request to child stdin: {e}")))?;
            // `stdin` drops here, closing the pipe.
        }

        let output = child
            .wait_with_output()
            .map_err(|e| TransportError::Process(format!("wait for child: {e}")))?;

        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            return Err(TransportError::Process(format!(
                "{:?} exited with {}: {}",
                self.program,
                output.status,
                truncate(stderr.trim(), 512)
            )));
        }

        let stdout = String::from_utf8_lossy(&output.stdout);
        parse_response(&stdout)
    }
}

/// Parse the response JSON document from a child's stdout.
///
/// Accepts either the whole stdout as one JSON document (pretty or single-line)
/// or — for a CLI that interleaves log lines with a final JSON result — the last
/// non-empty line that parses as JSON. This is the lenient counterpart of the
/// strict `from_wire` mapping: a CLI that emits diagnostics before its result
/// still round-trips.
fn parse_response(stdout: &str) -> Result<Value, TransportError> {
    let trimmed = stdout.trim();
    if trimmed.is_empty() {
        return Err(TransportError::MalformedResponse(
            "child produced no output on stdout".into(),
        ));
    }
    // Fast path: the entire output is one JSON document.
    if let Ok(value) = serde_json::from_str::<Value>(trimmed) {
        return Ok(value);
    }
    // Fallback: the last non-empty line that parses as JSON (logs + final result).
    for line in trimmed.lines().rev() {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        if let Ok(value) = serde_json::from_str::<Value>(line) {
            return Ok(value);
        }
    }
    Err(TransportError::MalformedResponse(format!(
        "no JSON document found in child stdout: {}",
        truncate(trimmed, 256)
    )))
}

/// Truncate a diagnostic string to `max` bytes (on a char boundary), appending an
/// ellipsis marker if it was cut, so an error never carries an unbounded payload.
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

    /// Write an executable shell script to a tempdir and return its path. The
    /// scripts emulate an agent CLI: they read stdin and print to stdout.
    fn write_script(dir: &std::path::Path, name: &str, body: &str) -> std::path::PathBuf {
        let path = dir.join(name);
        let mut f = std::fs::File::create(&path).unwrap();
        writeln!(f, "#!/bin/sh").unwrap();
        write!(f, "{body}").unwrap();
        drop(f);
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
        }
        path
    }

    #[test]
    fn round_trips_a_single_json_line() {
        let dir = tempfile::tempdir().unwrap();
        // An echo agent: read the request, emit a fixed assistant response line.
        let script = write_script(
            dir.path(),
            "echo-agent.sh",
            "cat >/dev/null\n\
             printf '%s\\n' '{\"lines\":[{\"role\":\"assistant\",\"content\":[{\"type\":\"text\",\"text\":\"hi\"}]}]}'\n",
        );
        let t =
            SubprocessTransport::new(script.to_string_lossy().to_string(), Vec::<String>::new());
        let req = serde_json::json!({ "model": "cli/agent", "lines": [] });
        let resp = t.invoke(&req).unwrap();
        assert_eq!(resp["lines"][0]["role"], "assistant");
        assert_eq!(resp["lines"][0]["content"][0]["text"], "hi");
    }

    #[test]
    fn echoes_request_back_so_the_pipe_carries_the_payload() {
        let dir = tempfile::tempdir().unwrap();
        // A pure echo: whatever arrives on stdin is written to stdout — proves the
        // request bytes actually traverse the pipe.
        let script = write_script(dir.path(), "cat-agent.sh", "cat\n");
        let t =
            SubprocessTransport::new(script.to_string_lossy().to_string(), Vec::<String>::new());
        let req =
            serde_json::json!({ "model": "cli/agent", "lines": [{"role":"user","content":[]}] });
        let resp = t.invoke(&req).unwrap();
        assert_eq!(resp, req);
    }

    #[test]
    fn picks_the_final_json_line_after_log_noise() {
        let dir = tempfile::tempdir().unwrap();
        let script = write_script(
            dir.path(),
            "noisy-agent.sh",
            "cat >/dev/null\n\
             echo 'loading model...'\n\
             echo 'warming up'\n\
             printf '%s\\n' '{\"lines\":[{\"role\":\"assistant\",\"content\":[]}]}'\n",
        );
        let t =
            SubprocessTransport::new(script.to_string_lossy().to_string(), Vec::<String>::new());
        let resp = t.invoke(&serde_json::json!({})).unwrap();
        assert_eq!(resp["lines"][0]["role"], "assistant");
    }

    #[test]
    fn nonzero_exit_is_a_process_error_with_stderr() {
        let dir = tempfile::tempdir().unwrap();
        let script = write_script(
            dir.path(),
            "failing-agent.sh",
            "cat >/dev/null\n\
             echo 'boom: rate limited' 1>&2\n\
             exit 3\n",
        );
        let t =
            SubprocessTransport::new(script.to_string_lossy().to_string(), Vec::<String>::new());
        let err = t.invoke(&serde_json::json!({})).unwrap_err();
        match err {
            TransportError::Process(msg) => assert!(msg.contains("boom: rate limited"), "{msg}"),
            other => panic!("expected Process error, got {other:?}"),
        }
    }

    #[test]
    fn empty_output_is_a_malformed_response() {
        let dir = tempfile::tempdir().unwrap();
        let script = write_script(dir.path(), "silent-agent.sh", "cat >/dev/null\n");
        let t =
            SubprocessTransport::new(script.to_string_lossy().to_string(), Vec::<String>::new());
        let err = t.invoke(&serde_json::json!({})).unwrap_err();
        assert!(matches!(err, TransportError::MalformedResponse(_)));
    }

    #[test]
    fn missing_program_is_a_process_error() {
        let t = SubprocessTransport::new("/nonexistent/spork-no-such-agent", Vec::<String>::new());
        let err = t.invoke(&serde_json::json!({})).unwrap_err();
        assert!(matches!(err, TransportError::Process(_)));
    }

    #[test]
    fn parse_response_prefers_whole_document_over_last_line() {
        // A pretty-printed multi-line JSON document parses as a whole.
        let pretty = "{\n  \"lines\": [\n  ]\n}";
        assert_eq!(
            parse_response(pretty).unwrap(),
            serde_json::json!({"lines": []})
        );
    }
}

//! `spork-cas` command-line tool.
//!
//! An operator- and harness-facing front-end over the Spork content-addressed
//! store ([`spork_cas`]). It turns the byte-identity substrate into something you
//! can drive from a shell: capture a directory into a snapshot, read objects back
//! out, prove the dedup and roundtrip invariants, and generate the deterministic
//! fixtures the performance / definition-of-done harness measures against.
//!
//! # Subcommands
//! - **`put-tree <dir>`** — capture a directory into a [`spork_cas::Snapshot`]
//!   under the frozen default ignore profile (deps excluded by default,
//!   DESIGN.md §10.5) and report the snapshot id, [`spork_cas::PutStats`], and
//!   elapsed time. `--json` emits a machine-readable record for the harness.
//! - **`cat <hash>`** — write a stored object to stdout: a blob reassembled to
//!   its original bytes, or a tree/snapshot pretty-printed as JSON.
//! - **`chunk-stats <dir>`** — capture twice (cold then warm) and prove the warm
//!   re-capture writes **zero** new objects (the dedup DoD demo). Exits non-zero
//!   if dedup does not hold.
//! - **`verify-roundtrip <dir>`** — capture then materialize to a temp dir and
//!   assert every kept file is byte-identical. Exits 0/1 on the result.
//! - **`gen-fixture <dir> --files N [--seed n]`** — deterministically create a
//!   corpus (no wall-clock / no RNG nondeterminism) for the 50k-file harness.
//!
//! # Exit codes
//! `0` on success and on a *checking* command whose assertion held; `1` when a
//! checking command's assertion failed (e.g. a roundtrip mismatch); `2` (clap's
//! convention) for a usage error; and a non-zero code with a printed message for
//! any hard fault (I/O, decode, malformed input).
//!
//! Design references: DESIGN.md §10 (snapshot & state-tracking engine / object
//! model), §14.5 (selection / performance / definition-of-done harness).

#![forbid(unsafe_code)]
#![warn(missing_docs)]

mod cli;
mod commands;
mod fixture;
mod store;

use std::process::ExitCode;

use clap::Parser;

use crate::cli::Cli;

/// Parse the command line and run the selected subcommand.
///
/// Hard faults are printed (with their `anyhow` context chain) to stderr and
/// turned into exit code `1`; a checking command's failed assertion is returned
/// as its own non-zero code; success is `0`.
fn main() -> ExitCode {
    let cli = Cli::parse();
    match commands::run(cli) {
        Ok(code) => ExitCode::from(code as u8),
        Err(err) => {
            eprintln!("spork-cas: error: {err:#}");
            ExitCode::from(1)
        }
    }
}

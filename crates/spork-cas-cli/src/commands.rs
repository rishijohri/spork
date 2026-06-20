//! Subcommand implementations and dispatch for `spork-cas`.
//!
//! [`run`] takes the parsed [`crate::cli::Cli`] and routes it to the matching
//! command module. Each command is its own submodule so its logic, output
//! shaping, and tests live together. Commands that report numbers support a
//! `--json` mode (machine-readable for the perf / DoD harness) alongside the
//! default human-readable summary.
//!
//! The process exit code is carried by [`run`]'s return: it yields the integer
//! exit status (0 = success/assertion-held, non-zero = assertion-failed), with
//! hard errors surfaced as `anyhow::Error` for `main` to print. This lets
//! `verify-roundtrip` exit 0/1 on the *result of its check* while still using
//! `?` for genuine failures.
//!
//! Design references: DESIGN.md §10 (object model / capture), §14.5 (perf / DoD).

pub mod cat;
pub mod chunk_stats;
pub mod gen_fixture;
pub mod put_tree;
pub mod verify_roundtrip;

use anyhow::Result;

use crate::cli::{Cli, Command};

/// The process exit code a command run resolves to.
///
/// Distinct from `anyhow::Error`: a [`Command`] that performs a *check*
/// (`verify-roundtrip`, `chunk-stats`) returns success/failure as an exit code
/// rather than an error, so a failed assertion is a clean non-zero exit, not a
/// panic or backtrace. Genuine faults (I/O, decode) still propagate as errors.
pub type ExitCode = i32;

/// Dispatch a parsed CLI to the appropriate command.
///
/// # Errors
/// Propagates any hard failure (I/O, decode, malformed input) as an
/// `anyhow::Error`. A command's *logical* failure (e.g. a round-trip mismatch)
/// is reported through a non-zero [`ExitCode`], not an error.
pub fn run(cli: Cli) -> Result<ExitCode> {
    match cli.command {
        Command::PutTree(args) => put_tree::run(args),
        Command::Cat(args) => cat::run(args),
        Command::ChunkStats(args) => chunk_stats::run(args),
        Command::VerifyRoundtrip(args) => verify_roundtrip::run(args),
        Command::GenFixture(args) => gen_fixture::run(args),
    }
}

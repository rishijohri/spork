//! Command-line surface for `spork-cas` (clap derive).
//!
//! This module defines *only* the parsed shape of the command line — the
//! [`Cli`] root, the [`Command`] subcommand enum, and the per-command argument
//! structs. Dispatch and behavior live in [`crate::commands`]; keeping the
//! grammar separate makes the available surface easy to read at a glance and
//! keeps each command's logic self-contained.
//!
//! Design references: DESIGN.md §10 (snapshot & state-tracking engine), §14.5
//! (perf / definition-of-done harness this CLI feeds).

use std::path::PathBuf;

use clap::{Args, Parser, Subcommand};

use crate::store::DEFAULT_STORE;

/// `spork-cas` — exercise the content-addressed store from the command line.
///
/// A thin operator/harness front-end over [`spork_cas`]: capture a directory
/// into a snapshot, read objects back out, prove dedup, verify round-trip
/// fidelity, and generate deterministic fixtures for the perf harness.
#[derive(Debug, Parser)]
#[command(
    name = "spork-cas",
    version,
    about = "Content-addressed store CLI: put-tree / cat / chunk-stats / verify-roundtrip / gen-fixture",
    long_about = None,
)]
pub struct Cli {
    /// The subcommand to run.
    #[command(subcommand)]
    pub command: Command,
}

/// The `spork-cas` subcommands.
#[derive(Debug, Subcommand)]
pub enum Command {
    /// Capture a directory into a snapshot and report dedup statistics.
    PutTree(PutTreeArgs),
    /// Write a stored object to stdout (blob bytes, or tree/snapshot as JSON).
    Cat(CatArgs),
    /// Capture a directory twice (cold then warm) and prove warm writes nothing.
    ChunkStats(ChunkStatsArgs),
    /// Capture then materialize a directory and assert byte-identical round-trip.
    VerifyRoundtrip(VerifyRoundtripArgs),
    /// Deterministically create a fixture corpus for the perf harness.
    GenFixture(GenFixtureArgs),
}

/// Where the object store lives, shared by every store-touching command.
///
/// Flattened into each command's args so the `--store` flag reads identically
/// everywhere and has a single documented default.
#[derive(Debug, Args)]
pub struct StoreOpt {
    /// Path to the object store's loose-object directory.
    #[arg(long, default_value = DEFAULT_STORE, value_name = "PATH")]
    pub store: PathBuf,
}

/// Arguments for `put-tree`.
#[derive(Debug, Args)]
pub struct PutTreeArgs {
    /// The directory to capture.
    #[arg(value_name = "DIR")]
    pub dir: PathBuf,

    /// Object-store location.
    #[command(flatten)]
    pub store: StoreOpt,

    /// An imported Git parent commit SHA-1 to record on the snapshot.
    ///
    /// Optional; Spork never *computes* a SHA-1, this is purely an imported
    /// lineage pointer (DESIGN.md §10.4). Must be 40 lowercase hex characters.
    #[arg(long, value_name = "SHA1")]
    pub git_parent: Option<String>,

    /// Emit machine-readable JSON (for the perf / DoD harness) instead of a
    /// human-readable summary.
    #[arg(long)]
    pub json: bool,
}

/// Arguments for `cat`.
#[derive(Debug, Args)]
pub struct CatArgs {
    /// The object digest (64 lowercase hex chars) to read.
    #[arg(value_name = "HASH")]
    pub hash: String,

    /// Object-store location.
    #[command(flatten)]
    pub store: StoreOpt,

    /// Force interpreting the object as a specific kind rather than
    /// auto-detecting (blob → raw bytes; tree/snapshot → JSON).
    #[arg(long, value_enum, value_name = "KIND")]
    pub kind: Option<ObjectKindArg>,
}

/// The object kind a user may force `cat` to use.
///
/// `cat` auto-detects by default; this is the escape hatch for the rare case of
/// reading an object whose kind the caller already knows (or wants to assert).
#[derive(Debug, Clone, Copy, PartialEq, Eq, clap::ValueEnum)]
pub enum ObjectKindArg {
    /// Reassemble and emit the blob's original file bytes.
    Blob,
    /// Emit the tree as pretty JSON.
    Tree,
    /// Emit the snapshot as pretty JSON.
    Snapshot,
}

/// Arguments for `chunk-stats`.
#[derive(Debug, Args)]
pub struct ChunkStatsArgs {
    /// The directory to capture twice.
    #[arg(value_name = "DIR")]
    pub dir: PathBuf,

    /// Object-store location.
    #[command(flatten)]
    pub store: StoreOpt,

    /// Emit machine-readable JSON instead of a human-readable summary.
    #[arg(long)]
    pub json: bool,
}

/// Arguments for `verify-roundtrip`.
#[derive(Debug, Args)]
pub struct VerifyRoundtripArgs {
    /// The directory to capture and re-materialize.
    #[arg(value_name = "DIR")]
    pub dir: PathBuf,

    /// Object-store location.
    #[command(flatten)]
    pub store: StoreOpt,

    /// Emit machine-readable JSON instead of a human-readable summary.
    #[arg(long)]
    pub json: bool,
}

/// Arguments for `gen-fixture`.
#[derive(Debug, Args)]
pub struct GenFixtureArgs {
    /// The directory to create the fixture corpus in (created if absent).
    #[arg(value_name = "DIR")]
    pub dir: PathBuf,

    /// How many files to generate.
    #[arg(long, value_name = "N", default_value_t = crate::fixture::DEFAULT_FILE_COUNT)]
    pub files: usize,

    /// The PRNG seed (the corpus is a pure function of this; default 0).
    #[arg(long, value_name = "N", default_value_t = 0)]
    pub seed: u64,

    /// Emit machine-readable JSON instead of a human-readable summary.
    #[arg(long)]
    pub json: bool,
}

#[cfg(test)]
mod tests {
    use super::*;
    use clap::CommandFactory;

    #[test]
    fn cli_definition_is_valid() {
        // clap asserts the command tree is internally consistent (no duplicate
        // args, valid defaults, etc.) on demand.
        Cli::command().debug_assert();
    }

    #[test]
    fn put_tree_parses_with_defaults() {
        let cli = Cli::try_parse_from(["spork-cas", "put-tree", "some/dir"]).unwrap();
        match cli.command {
            Command::PutTree(args) => {
                assert_eq!(args.dir, PathBuf::from("some/dir"));
                assert_eq!(args.store.store, PathBuf::from(DEFAULT_STORE));
                assert!(!args.json);
                assert!(args.git_parent.is_none());
            }
            other => panic!("expected put-tree, got {other:?}"),
        }
    }

    #[test]
    fn put_tree_accepts_store_json_and_parent() {
        let cli = Cli::try_parse_from([
            "spork-cas",
            "put-tree",
            "d",
            "--store",
            "/tmp/objs",
            "--json",
            "--git-parent",
            "abc123",
        ])
        .unwrap();
        match cli.command {
            Command::PutTree(args) => {
                assert_eq!(args.store.store, PathBuf::from("/tmp/objs"));
                assert!(args.json);
                assert_eq!(args.git_parent.as_deref(), Some("abc123"));
            }
            other => panic!("expected put-tree, got {other:?}"),
        }
    }

    #[test]
    fn gen_fixture_parses_files_and_seed() {
        let cli = Cli::try_parse_from([
            "spork-cas",
            "gen-fixture",
            "d",
            "--files",
            "500",
            "--seed",
            "7",
        ])
        .unwrap();
        match cli.command {
            Command::GenFixture(args) => {
                assert_eq!(args.files, 500);
                assert_eq!(args.seed, 7);
            }
            other => panic!("expected gen-fixture, got {other:?}"),
        }
    }

    #[test]
    fn cat_parses_forced_kind() {
        let cli = Cli::try_parse_from(["spork-cas", "cat", "deadbeef", "--kind", "tree"]).unwrap();
        match cli.command {
            Command::Cat(args) => {
                assert_eq!(args.hash, "deadbeef");
                assert_eq!(args.kind, Some(ObjectKindArg::Tree));
            }
            other => panic!("expected cat, got {other:?}"),
        }
    }

    #[test]
    fn missing_required_dir_is_an_error() {
        assert!(Cli::try_parse_from(["spork-cas", "put-tree"]).is_err());
    }
}

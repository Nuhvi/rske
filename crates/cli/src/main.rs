//! rsk-cli — run the Rske node from the command line.
//!
//! A thin wrapper over the [`rsk`] library: each command parses arguments into
//! a [`Config`] and calls one library entry point. All the I/O lives in the
//! library, so the node is embeddable in any app.
//!
//! `sync` downloads real Bitcoin headers into the store, which PoW-,
//! continuity-, difficulty- and timestamp-validates every one and finalizes
//! difficulty periods as it goes; it then scans backwards for merge-mining tags
//! and runs the RSK header IBD pass. `export-checkpoints` writes the store's
//! period-boundary headers back out as a `checkpoints.txt`.
//!
//! Logs go to stderr (tracing); data (e.g. exported checkpoints) goes to
//! stdout, so `rsk-cli export-checkpoints > file` stays clean.

use std::io::Write;
use std::net::SocketAddr;
use std::path::PathBuf;

use anyhow::{Context, Result};
use clap::{Parser, Subcommand};
use rsk::{Config, DEFAULT_DATA_DIR, Node};
use tracing::info;

#[derive(Parser)]
#[command(
    name = "rsk-cli",
    version,
    about = "The Rske node: talk to real peers and sync headers"
)]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Sync Bitcoin headers, scan the tag window, and run the IBD
    Sync(SyncArgs),
    /// Export the store's period-boundary headers as a checkpoints.txt
    #[command(name = "export-checkpoints")]
    ExportCheckpoints(ExportArgs),
}

fn main() -> Result<()> {
    init_tracing();
    match Cli::parse().command {
        Command::Sync(args) => args.run(),
        Command::ExportCheckpoints(args) => args.run(),
    }
}

#[derive(clap::Args)]
struct SyncArgs {
    /// Directory for the chain store
    #[arg(long, value_name = "DIR", default_value = DEFAULT_DATA_DIR)]
    data_dir: PathBuf,

    /// Number of peers to keep connected
    #[arg(long, value_name = "N", default_value_t = 3)]
    peers: usize,

    /// Peer address to try first (repeatable), e.g. 1.2.3.4:8333
    #[arg(long, value_name = "IP:PORT")]
    addr: Vec<SocketAddr>,

    /// DNS seed host to resolve (repeatable; defaults to the mainnet seeds)
    #[arg(long, value_name = "HOST")]
    seed: Vec<String>,

    /// Maximum number of peer connections to attempt
    #[arg(long, value_name = "N", default_value_t = 8)]
    limit: usize,

    /// Rootstock peer for IBD header download (repeatable; defaults to the
    /// network's shipped bootnodes)
    #[arg(long, value_name = "HOST:PORT")]
    rsk_peer: Vec<String>,
}

impl SyncArgs {
    fn run(self) -> Result<()> {
        let config = Config {
            data_dir: self.data_dir,
            rsk_peers: self.rsk_peer,
            peers: self.peers,
            limit: self.limit,
            addrs: self.addr,
            seeds: self.seed,
            ..Config::default()
        };
        Node::open(config)?.sync()?;
        Ok(())
    }
}

#[derive(clap::Args)]
struct ExportArgs {
    /// Directory of the chain store to read
    #[arg(long, value_name = "DIR", default_value = DEFAULT_DATA_DIR)]
    data_dir: PathBuf,

    /// Write to this file instead of stdout
    #[arg(long, value_name = "FILE")]
    out: Option<PathBuf>,
}

impl ExportArgs {
    fn run(self) -> Result<()> {
        // Never create a store for a read: an export from an empty directory
        // would otherwise seed one and export nothing.
        let store = self.data_dir.join("chain.redb");
        anyhow::ensure!(store.exists(), "no store at {}", store.display());
        let config = Config {
            data_dir: self.data_dir,
            ..Config::default()
        };
        let text = Node::open(config)?.export_checkpoints()?;
        match self.out {
            Some(path) => {
                std::fs::write(&path, &text)
                    .with_context(|| format!("writing {}", path.display()))?;
                info!("wrote checkpoints to {}", path.display());
            }
            None => {
                std::io::stdout()
                    .write_all(text.as_bytes())
                    .context("writing checkpoints to stdout")?;
            }
        }
        Ok(())
    }
}

/// Logs to stderr (`RUST_LOG` override, default `info`); stdout is left for
/// data output.
fn init_tracing() {
    let filter =
        tracing_subscriber::EnvFilter::try_from_default_env().unwrap_or_else(|_| "info".into());
    tracing_subscriber::fmt()
        .with_env_filter(filter)
        .with_target(false)
        .with_writer(std::io::stderr)
        .init();
}

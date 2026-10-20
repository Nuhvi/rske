//! How the node is configured: where its store lives, which network it speaks,
//! how to find peers, and how to anchor an empty store.

use std::net::SocketAddr;
use std::path::PathBuf;

use bitcoin::Network;

/// The default data directory, relative to the working directory (the repo
/// gitignores `data/`). Product-scoped rather than bitcoin-scoped because the
/// same store carries RSK chain state.
pub const DEFAULT_DATA_DIR: &str = "data/rsk-one";

/// The BIP 14 user agent the node advertises on the wire.
pub const USER_AGENT: &str = "/rsk-one:0.1.0/";

/// How an empty store is anchored before the first sync, and the ceiling for
/// adopting a newer shipped checkpoint when resuming an existing store.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Anchor {
    /// Trust the embedded checkpoint spine (one header per difficulty period),
    /// minus the last `cut` periods, and sync the tail above the newest trusted
    /// checkpoint. `cut = 0` trusts the whole spine.
    Spine { cut: u64 },
    /// Trust only genesis and validate the whole chain from block 1.
    Genesis,
}

/// Everything the node needs to run.
#[derive(Debug, Clone)]
pub struct Config {
    /// Directory holding the chain store (`chain.redb`).
    pub data_dir: PathBuf,
    /// Bitcoin network to validate against.
    pub network: Network,
    /// BIP 14 user agent to advertise.
    pub user_agent: String,
    /// Target number of connected peers.
    pub peers: usize,
    /// Maximum number of connection attempts.
    pub limit: usize,
    /// Explicit peer addresses to try first.
    pub addrs: Vec<SocketAddr>,
    /// DNS seed hosts (defaults to the mainnet seeds when empty).
    pub seeds: Vec<String>,
    /// How to anchor an empty store.
    pub anchor: Anchor,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            data_dir: PathBuf::from(DEFAULT_DATA_DIR),
            network: Network::Bitcoin,
            user_agent: USER_AGENT.to_string(),
            peers: 3,
            limit: 8,
            addrs: Vec::new(),
            seeds: Vec::new(),
            anchor: Anchor::Spine { cut: 0 },
        }
    }
}

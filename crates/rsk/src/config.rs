//! How the node is configured: where its store lives, which network it speaks
//! and how to find peers. The trust anchor is not a node option: `rsk-core`
//! selects it from the network.

use std::net::SocketAddr;
use std::path::PathBuf;

use bitcoin::Network;

/// The default data directory, relative to the working directory (the repo
/// gitignores `data/`). Product-scoped rather than bitcoin-scoped because the
/// same store carries RSK chain state.
pub const DEFAULT_DATA_DIR: &str = "data/rske";

/// The BIP 14 user agent the node advertises on the wire.
pub const USER_AGENT: &str = "/rske:0.1.0/";

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
    /// The merge-mining share the fraud proof assumes will tag honestly.
    pub honest_share: f64,
    /// Rootstock peers to ask for headers in IBD (`host:port`). Empty uses the
    /// shipped bootnodes for [`Config::network`].
    pub rsk_peers: Vec<String>,
    /// Cap on Bitcoin blocks scanned for tags in one IBD pass.
    pub ibd_max_blocks: u64,
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
            honest_share: 0.10,
            rsk_peers: Vec::new(),
            ibd_max_blocks: 4_000,
        }
    }
}

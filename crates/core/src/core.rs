//! The [`Core`] handle: one redb-backed store, opened once and shared by the
//! Bitcoin and Rootstock trees.
//!
//! This module owns only the handle and how it is built. The behaviour lives
//! with the domain: [`crate::btc`] for the Bitcoin chain and [`crate::rsk`] for
//! the Rootstock tree, both as `impl Core`.

use std::path::Path;
use std::sync::Arc;

use bitcoin::Network;
use rsk_consensus::config::ChainConfig;

use crate::btc::finality;
use crate::error::Error;
use crate::rsk;
use crate::store::Store;

/// The side-effect-free core: a thin, deterministic layer over a redb-backed
/// store.
///
/// No concurrency, no network, no clock, no hidden I/O — the caller supplies
/// data and reads back outcomes.
pub struct Core {
    pub(crate) store: Store,
    pub(crate) network: Network,
    /// Rootstock consensus parameters the RSK tree validates against.
    pub(crate) rsk_chain: Arc<ChainConfig>,
    /// The finality policy: how much work the chain since the mark must carry.
    pub(crate) bar: finality::Bar,
}

impl Core {
    /// Open a file-backed store for the given network.
    ///
    /// Mainnet uses the price-law estimated-work bar; other networks use the
    /// linear reference rule, because the price law is a mainnet calibration.
    pub fn open(path: &Path, network: Network) -> Result<Self, Error> {
        Ok(Self {
            store: Store::open(path)?,
            network,
            rsk_chain: rsk::chain_config(network),
            bar: bar_for(network),
        })
    }

    /// Open an in-memory store (tests, no disk I/O) for the given network.
    #[cfg(test)]
    pub fn in_memory(network: Network) -> Result<Self, Error> {
        Self::in_memory_with_bar(network, bar_for(network))
    }

    /// Open an in-memory store with an explicit finality policy (tests).
    #[cfg(test)]
    pub fn in_memory_with_bar(network: Network, bar: finality::Bar) -> Result<Self, Error> {
        Ok(Self {
            store: Store::in_memory()?,
            network,
            rsk_chain: rsk::chain_config(network),
            bar,
        })
    }
}

/// The finality policy for a network. The price law is calibrated on Bitcoin
/// mainnet; other networks fall back to the linear reference rule.
fn bar_for(network: Network) -> finality::Bar {
    match network {
        Network::Bitcoin => finality::estimated_work_bar,
        _ => finality::linear_reference_bar,
    }
}

//! Bitcoin peer discovery and a small pool of connected peers.
//!
//! A header sync is sequential in *validation* (each batch links to the one
//! before it), but the network fetch is overlapped with it: the sync round-robins
//! across every pooled peer and prefetches the next batch while the current one
//! is validated. Keeping several peers connected also means the driver keeps
//! answering their pings, so they stay alive.
//!
//! Deliberately minimal: real peer management (maintain a target count with a
//! minimum that never blocks progress, ping idle peers, drop unresponsive ones,
//! grow the address table via `getaddr`/`addr`, score peers) is a later slice.

use std::collections::VecDeque;
use std::fs;
use std::net::{SocketAddr, TcpStream, ToSocketAddrs};
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use btc_p2p::Handshake;
use btc_p2p::seeds;
use btc_p2p::session::Session;
use tracing::{info, warn};

/// File under the node data directory that remembers peers which completed a
/// handshake. Bitcoin and Rootstock peers are kept in one file, tagged by kind.
const PEER_CACHE_FILE: &str = "peers.txt";
const MAX_CACHED_PEERS: usize = 64;

#[derive(Debug, Default)]
struct CachedPeers {
    btc: Vec<SocketAddr>,
    rsk: Vec<String>,
}

/// Persistent, node-level peer cache.
///
/// This is deliberately outside [`rsk_core`]: the core store is chain state and
/// has no business remembering which host answered a handshake. The cache is a
/// small text file in the node's data directory (`peers.txt`), written after a
/// successful connection and tried first on the next run.
pub(crate) struct PeerCache {
    path: PathBuf,
    peers: Mutex<CachedPeers>,
}

impl PeerCache {
    pub(crate) fn load(dir: &Path) -> Self {
        let path = dir.join(PEER_CACHE_FILE);
        let mut peers = CachedPeers::default();
        if let Ok(text) = fs::read_to_string(&path) {
            for line in text.lines() {
                let line = line.trim();
                if let Some(addr) = line.strip_prefix("btc ")
                    && let Ok(addr) = addr.parse()
                {
                    peers.btc.push(addr);
                } else if let Some(peer) = line.strip_prefix("rsk ") {
                    peers.rsk.push(peer.to_string());
                }
            }
        }
        peers.btc.dedup();
        peers.rsk.dedup();
        peers.btc.truncate(MAX_CACHED_PEERS);
        peers.rsk.truncate(MAX_CACHED_PEERS);
        Self {
            path,
            peers: Mutex::new(peers),
        }
    }

    pub(crate) fn btc(&self) -> Vec<SocketAddr> {
        self.lock().btc.clone()
    }

    pub(crate) fn rsk(&self) -> Vec<String> {
        self.lock().rsk.clone()
    }

    pub(crate) fn record_btc(&self, addr: SocketAddr) {
        let mut peers = self.lock();
        peers.btc.retain(|cached| *cached != addr);
        peers.btc.insert(0, addr);
        peers.btc.truncate(MAX_CACHED_PEERS);
        self.save(&peers);
    }

    pub(crate) fn record_rsk(&self, peer: &str) {
        let mut peers = self.lock();
        peers.rsk.retain(|cached| cached != peer);
        peers.rsk.insert(0, peer.to_string());
        peers.rsk.truncate(MAX_CACHED_PEERS);
        self.save(&peers);
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, CachedPeers> {
        self.peers
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    fn save(&self, peers: &CachedPeers) {
        let mut text = String::new();
        for addr in &peers.btc {
            text.push_str(&format!("btc {addr}\n"));
        }
        for peer in &peers.rsk {
            text.push_str(&format!("rsk {peer}\n"));
        }
        let tmp = self.path.with_extension("txt.tmp");
        if let Err(e) = fs::write(&tmp, text) {
            warn!("peer cache: cannot write {}: {e}", tmp.display());
            return;
        }
        if let Err(e) = fs::rename(&tmp, &self.path) {
            warn!("peer cache: cannot replace {}: {e}", self.path.display());
        }
    }
}

/// Resolve seed hosts on `port`, returning the addresses and the hosts that
/// failed (which is routine — seeds come and go).
pub(crate) fn resolve(hosts: &[String], port: u16) -> (Vec<SocketAddr>, Vec<String>) {
    let mut addrs = Vec::new();
    let mut failed = Vec::new();
    for host in hosts {
        match (host.as_str(), port).to_socket_addrs() {
            Ok(list) => addrs.extend(list),
            Err(e) => failed.push(format!("{host} ({e})")),
        }
    }
    (addrs, failed)
}

/// Build the candidate list: explicit addresses first, then the resolved seeds
/// (defaulting to the mainnet seeds when none were given), deduped while
/// keeping that order.
pub(crate) fn candidates(
    addrs: Vec<SocketAddr>,
    given_seeds: &[String],
) -> (Vec<SocketAddr>, Vec<String>) {
    let hosts: Vec<String> = if given_seeds.is_empty() {
        seeds::MAINNET_SEEDS.iter().map(|s| s.to_string()).collect()
    } else {
        given_seeds.to_vec()
    };
    let (resolved, failed) = resolve(&hosts, seeds::DEFAULT_PORT);
    let mut out = addrs;
    for addr in resolved {
        if !out.contains(&addr) {
            out.push(addr);
        }
    }
    (out, failed)
}

/// One connected, handshaken peer.
pub(crate) struct Peer {
    pub(crate) addr: SocketAddr,
    pub(crate) handshake: Handshake,
    pub(crate) session: Session<TcpStream>,
}

/// A small pool of connected peers. It spends a bounded number of connection
/// attempts (shared across refills) so a bad seed list cannot loop forever.
pub(crate) struct Pool<'a> {
    peers: Vec<Peer>,
    attempts: usize,
    user_agent: String,
    cache: Option<&'a PeerCache>,
}

impl<'a> Pool<'a> {
    pub(crate) fn new(user_agent: impl Into<String>, cache: Option<&'a PeerCache>) -> Self {
        Self {
            peers: Vec::new(),
            attempts: 0,
            user_agent: user_agent.into(),
            cache,
        }
    }

    pub(crate) fn is_empty(&self) -> bool {
        self.peers.is_empty()
    }

    /// A one-line description of every pooled peer, for logging.
    pub(crate) fn summary(&self) -> String {
        self.peers
            .iter()
            .map(|p| {
                format!(
                    "{}@{} ({})",
                    p.handshake.user_agent, p.handshake.start_height, p.addr
                )
            })
            .collect::<Vec<_>>()
            .join(", ")
    }

    /// Connect toward `want` peers, spending at most `max_attempts` total
    /// connections over the pool's life. Returns how many are now connected.
    pub(crate) fn fill(
        &mut self,
        candidates: &mut VecDeque<SocketAddr>,
        want: usize,
        max_attempts: usize,
        start_height: i32,
    ) -> usize {
        while self.peers.len() < want && self.attempts < max_attempts {
            let Some(addr) = candidates.pop_front() else {
                break;
            };
            self.attempts += 1;
            match Session::connect(addr, &self.user_agent, start_height) {
                Ok((session, handshake)) => {
                    info!(
                        "  peer {} @ {} — {} height {}",
                        handshake.user_agent, addr, handshake.services, handshake.start_height
                    );
                    if let Some(cache) = self.cache {
                        cache.record_btc(addr);
                    }
                    self.peers.push(Peer {
                        addr,
                        handshake,
                        session,
                    });
                }
                Err(e) => warn!("  {addr} failed: {e:#}"),
            }
        }
        self.peers.len()
    }

    /// Take every connected peer out; the sync drives them all.
    pub(crate) fn into_peers(self) -> Vec<Peer> {
        self.peers
    }
}

#[cfg(test)]
mod tests {
    use std::net::SocketAddr;

    use super::PeerCache;

    #[test]
    fn peer_cache_round_trips_btc_and_rsk() {
        let dir = tempfile::tempdir().unwrap();
        let cache = PeerCache::load(dir.path());
        let addr: SocketAddr = "1.2.3.4:8333".parse().unwrap();
        cache.record_btc(addr);
        cache.record_rsk("bootstrap04.rsk.co:5050");

        let loaded = PeerCache::load(dir.path());
        assert_eq!(loaded.btc(), vec![addr]);
        assert_eq!(loaded.rsk(), vec!["bootstrap04.rsk.co:5050".to_string()]);
    }

    #[test]
    fn recording_the_same_peer_moves_it_to_the_front() {
        let dir = tempfile::tempdir().unwrap();
        let cache = PeerCache::load(dir.path());
        let a: SocketAddr = "1.1.1.1:8333".parse().unwrap();
        let b: SocketAddr = "2.2.2.2:8333".parse().unwrap();
        cache.record_btc(a);
        cache.record_btc(b);
        cache.record_btc(a);
        assert_eq!(cache.btc(), vec![a, b]);
    }

    #[test]
    fn malformed_lines_are_ignored() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join("peers.txt"),
            "btc not-an-address\nrsk good.example:5050\nnonsense\n",
        )
        .unwrap();
        let cache = PeerCache::load(dir.path());
        assert!(cache.btc().is_empty());
        assert_eq!(cache.rsk(), vec!["good.example:5050".to_string()]);
    }
}

//! Coinbase scan: fetch the canonical coinbases a tag window needs.
//!
//! The scan walks backwards from the Bitcoin tip until it has seen enough
//! `RSKBLOCK` tags. A coinbase already in the store is read from disk; a
//! missing one is fetched on demand and stored with its inclusion proof. It
//! deliberately does **not** prefetch the whole retained difficulty period:
//! the tag window is a few hundred blocks, not the 2016-block retention unit.
//!
//! The primary path is BIP37. BIP34 puts the block height in the coinbase
//! scriptSig as a data element; a bloom filter containing just that element
//! makes `getdata MSG_FILTERED_BLOCK` return the coinbase and a partial merkle
//! tree (~700 bytes) instead of the whole block (~740 KB). A peer that does not
//! serve BIP37 (or withholds the match) is dropped from the pool; if no bloom
//! peer is left, the same loop falls back to full blocks, which every
//! `NODE_NETWORK` peer serves.
//!
//! Everything is verified against the locally validated header before it is
//! stored: [`rsk_core::Core::store_coinbase`] re-checks the merkle proof, the
//! coinbase index, and the canonical block hash. A peer can omit data, but it
//! cannot make us store a coinbase that is not in the canonical block.

use std::collections::{HashMap, VecDeque};
use std::net::SocketAddr;
use std::time::{Duration, Instant};

use anyhow::{Context, Result, bail};
use bitcoin::block::BlockHash;
use bitcoin::consensus::Encodable;
use bitcoin::merkle_tree::MerkleBlock;
use bitcoin::p2p::ServiceFlags;
use bitcoin::p2p::message_bloom::BloomFlags;
use btc_p2p::BloomFilter;
use btc_p2p::session::Session;
use rsk_core::{CoinbaseTag, Core, Tag, bip34_height};
use tracing::{info, warn};

use crate::config::Config;
use crate::peers;
use crate::peers::PeerCache;

/// BIP37 filter shape. One element, 100 bytes and five hash functions gives a
/// false-positive rate around 1e-11 per transaction, so a block normally
/// returns exactly the coinbase.
const FILTER_BYTES: usize = 100;
const FILTER_HASH_FUNCS: u32 = 5;

/// How long a coinbase peer may take to answer one request.
const PEER_TIMEOUT: Duration = Duration::from_secs(20);

/// How many cached tag states to preload around the tip at a time. The tag
/// window is a few hundred blocks; loading the whole retention period up front
/// is slower than a couple of small snapshots.
const TAG_PREFETCH: u64 = 256;

/// What one coinbase scan did.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct CoinbaseReport {
    /// Lowest height actually scanned, if the store has a tip.
    pub start: Option<u64>,
    /// Canonical tip the scan started from.
    pub tip: Option<u64>,
    /// Coinbases fetched and written this run.
    pub stored: usize,
    /// Heights whose coinbase could not be fetched from any peer.
    pub failed: usize,
    /// Coinbases already present and matching the canonical chain.
    pub cached: usize,
    /// Wire payload bytes downloaded for coinbases and their proofs.
    pub bytes: u64,
}

/// The tags one backwards scan found, and what it had to fetch.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct TagScan {
    pub tags: Vec<Tag>,
    /// Heights visited, including ones whose fetch failed.
    pub blocks: u64,
    pub report: CoinbaseReport,
}

/// Scan backwards from the tip until `needed` tags are found or `max_blocks`
/// heights have been visited.
///
/// Convenience wrapper around [`Scanner`] for callers that only want the whole
/// window at once. The IBD loop drives [`Scanner::next`] directly so it can
/// fetch and score RSK headers between coinbases.
pub fn scan_for_tags(
    core: &Core,
    config: &Config,
    needed: u64,
    max_blocks: u64,
) -> Result<TagScan> {
    let Some(mut scanner) = Scanner::new(core, config, max_blocks)? else {
        return Ok(TagScan::default());
    };
    let mut tags = Vec::new();
    while let Some((_, tag)) = scanner.next(core)? {
        if let Some(tag) = tag {
            tags.push(tag);
        }
        if tags.len() as u64 >= needed {
            break;
        }
    }
    let blocks = scanner.blocks();
    Ok(TagScan {
        tags,
        blocks,
        report: scanner.into_report(),
    })
}

/// One backwards coinbase scan, one height at a time.
///
/// The scan never goes below [`Core::coinbase_retention_floor`]: rows below the
/// newest finalized difficulty boundary are pruned by policy, so refetching them
/// would just create work for the next prune.
pub struct Scanner<'a> {
    config: &'a Config,
    /// Next height to visit, descending. `None` once `stop_at` is passed.
    next: Option<u64>,
    /// Lowest height this scan may visit.
    stop_at: u64,
    blocks: u64,
    tags: usize,
    report: CoinbaseReport,
    /// The peer pool is built only if a fetch is actually needed, so a fully
    /// cached scan never touches DNS or the network.
    pool: Option<Pool<'a>>,
    pool_tried: bool,
    /// Tag states preloaded in read snapshots for the scanned range.
    cached_tags: HashMap<u64, CoinbaseTag>,
    /// Lowest height currently present in `cached_tags`.
    cache_floor: u64,
    last_progress: Instant,
    cache: Option<&'a PeerCache>,
}

impl<'a> Scanner<'a> {
    /// Prepare a scan from the current tip down to the retention floor or
    /// `max_blocks` heights, whichever is nearer.
    pub fn new(core: &Core, config: &'a Config, max_blocks: u64) -> Result<Option<Self>> {
        Self::new_with_cache(core, config, max_blocks, None)
    }

    /// Like [`Scanner::new`], but prioritise previously successful peers.
    pub(crate) fn new_with_cache(
        core: &Core,
        config: &'a Config,
        max_blocks: u64,
        cache: Option<&'a PeerCache>,
    ) -> Result<Option<Self>> {
        let Some(tip) = core.tip_height()? else {
            return Ok(None);
        };
        let Some(floor) = core.coinbase_retention_floor()? else {
            return Ok(None);
        };
        if floor > tip {
            return Ok(None);
        }

        let span = max_blocks.max(1);
        let start = floor.max(tip.saturating_sub(span - 1));
        info!("coinbase: scanning #{tip} down to #{start} (at most {max_blocks} block(s))");
        let cache_floor = start.max(tip.saturating_sub(TAG_PREFETCH - 1));
        let cached_tags: HashMap<u64, CoinbaseTag> = core
            .coinbase_tags(cache_floor, tip)?
            .collect::<Result<_, _>>()?;

        Ok(Some(Self {
            config,
            next: Some(tip),
            stop_at: start,
            blocks: 0,
            tags: 0,
            report: CoinbaseReport {
                start: Some(tip),
                tip: Some(tip),
                ..CoinbaseReport::default()
            },
            pool: None,
            pool_tried: false,
            cached_tags,
            cache_floor,
            last_progress: Instant::now(),
            cache,
        }))
    }

    /// Visit the next height, fetching its coinbase if needed.
    ///
    /// Returns `None` once the scan floor or block cap is reached. The tag is
    /// `None` when the coinbase is untagged or could not be fetched.
    pub fn next(&mut self, core: &Core) -> Result<Option<(u64, Option<Tag>)>> {
        let Some(height) = self.next else {
            return Ok(None);
        };
        self.next = height.checked_sub(1).filter(|next| *next >= self.stop_at);
        self.blocks += 1;
        self.report.start = Some(height);

        if height < self.cache_floor {
            let from = self.stop_at.max(height.saturating_sub(TAG_PREFETCH - 1));
            let tags: Vec<_> = core
                .coinbase_tags(from, height)?
                .collect::<Result<_, _>>()?;
            self.cached_tags.extend(tags);
            self.cache_floor = from;
        }

        let tag = match self
            .cached_tags
            .remove(&height)
            .unwrap_or(CoinbaseTag::Missing)
        {
            CoinbaseTag::Tagged(tag) => {
                self.report.cached += 1;
                Some(tag)
            }
            CoinbaseTag::Untagged => {
                self.report.cached += 1;
                None
            }
            CoinbaseTag::Legacy => {
                // A row written before tag caching: parse it once and backfill.
                self.report.cached += 1;
                let tag = core
                    .coinbase_at(height)?
                    .and_then(|coinbase| Tag::parse(&coinbase.coinbase));
                core.set_coinbase_tag(height, tag)?;
                tag
            }
            CoinbaseTag::Missing => {
                let header = core
                    .header_at(height)?
                    .with_context(|| format!("no canonical header at {height}"))?;
                let wanted = header.block_hash();
                if !self.pool_tried {
                    self.pool_tried = true;
                    self.pool =
                        pool_for(self.config, self.report.tip.unwrap_or(height), self.cache);
                }
                let fetched = match self.pool.as_mut() {
                    Some(pool) => match pool.fetch(core, height, wanted) {
                        Ok(bytes) => {
                            self.report.bytes += bytes;
                            self.report.stored += 1;
                            true
                        }
                        Err(e) => {
                            self.report.failed += 1;
                            warn!("coinbase #{height}: {e:#}");
                            false
                        }
                    },
                    None => {
                        self.report.failed += 1;
                        false
                    }
                };
                if fetched {
                    match core.coinbase_tag_at(height)? {
                        CoinbaseTag::Tagged(tag) => Some(tag),
                        CoinbaseTag::Untagged => None,
                        _ => None,
                    }
                } else {
                    None
                }
            }
        };
        if tag.is_some() {
            self.tags += 1;
        }

        if self.last_progress.elapsed() >= Duration::from_secs(5) {
            info!(
                "coinbase: scanned {} block(s), {} tag(s), {} stored, {} failed (height #{height})",
                self.blocks, self.tags, self.report.stored, self.report.failed
            );
            self.last_progress = Instant::now();
        }

        Ok(Some((height, tag)))
    }

    pub fn blocks(&self) -> u64 {
        self.blocks
    }

    pub fn into_report(self) -> CoinbaseReport {
        self.report
    }
}

/// Build the peer pool on first use. `None` means no candidate to try; the
/// scan then keeps reading whatever is already cached.
fn pool_for<'a>(config: &'a Config, tip: u64, cache: Option<&'a PeerCache>) -> Option<Pool<'a>> {
    let mut addrs = cache.map(PeerCache::btc).unwrap_or_default();
    addrs.extend(config.addrs.iter().copied());
    let (candidates, seed_failures) = peers::candidates(addrs, &config.seeds);
    for failure in &seed_failures {
        warn!("coinbase: seed failed: {failure}");
    }
    if candidates.is_empty() {
        warn!("coinbase: no candidate peers; scanning cached rows only");
        return None;
    }
    Some(Pool::new(candidates, config, tip, cache))
}

/// One connected peer, classified by what it can serve.
struct Peer {
    addr: SocketAddr,
    session: Session<std::net::TcpStream>,
    /// The peer advertises BIP37 and is still trusted to answer it.
    bloom: bool,
    /// The peer advertises `NODE_NETWORK`, so full blocks are an option.
    full: bool,
}

/// A small, self-refilling peer pool. Sequential by design: one request at a
/// time is enough for the few hundred coinbases a tag window spans, and keeps
/// the code (and the peer's filter state) simple.
struct Pool<'a> {
    peers: Vec<Peer>,
    candidates: VecDeque<SocketAddr>,
    user_agent: String,
    start_height: i32,
    want: usize,
    max_attempts: usize,
    attempts: usize,
    cursor: usize,
    /// Whether the first successful pool fill has been logged.
    ready_logged: bool,
    cache: Option<&'a PeerCache>,
}

impl<'a> Pool<'a> {
    fn new(
        candidates: Vec<SocketAddr>,
        config: &Config,
        tip: u64,
        cache: Option<&'a PeerCache>,
    ) -> Self {
        Self {
            peers: Vec::new(),
            candidates: candidates.into(),
            user_agent: config.user_agent.clone(),
            start_height: tip.min(i32::MAX as u64) as i32,
            want: config.peers.max(1),
            // BIP37 is a minority service, so try harder than the header pool
            // before concluding there is no bloom peer.
            max_attempts: config.limit.max(32),
            attempts: 0,
            cursor: 0,
            ready_logged: false,
            cache,
        }
    }

    /// Fill until at least one bloom peer is connected.
    ///
    /// A single bloom peer is enough to start fetching; waiting for the whole
    /// configured pool is not. Full-only peers are kept as a last-resort
    /// fallback, but only after the candidate list is exhausted: downloading
    /// full blocks instead of filtered ones is orders of magnitude more data,
    /// so it is worth looking for BIP37 first.
    fn fill(&mut self) {
        while self.attempts < self.max_attempts {
            if self.peers.iter().any(|p| p.bloom) {
                break;
            }
            let Some(addr) = self.candidates.pop_front() else {
                break;
            };
            self.attempts += 1;

            let services = ServiceFlags::BLOOM | ServiceFlags::WITNESS;
            match Session::connect_with_services(
                addr,
                &self.user_agent,
                self.start_height,
                services,
            ) {
                Ok((mut session, handshake)) => {
                    let bloom = handshake.services.has(ServiceFlags::BLOOM);
                    let full = handshake.services.has(ServiceFlags::NETWORK);
                    if !bloom && !full {
                        continue;
                    }
                    if !bloom && self.peers.iter().filter(|p| p.full).count() >= self.want {
                        continue;
                    }
                    if bloom && self.peers.iter().filter(|p| p.bloom).count() >= self.want {
                        // Keep it only if it can also serve full blocks and we
                        // do not have a fallback yet.
                        if !full || self.peers.iter().any(|p| p.full) {
                            continue;
                        }
                    }
                    if session.set_read_timeout(Some(PEER_TIMEOUT)).is_err() {
                        continue;
                    }
                    info!(
                        "coinbase peer {addr}: {} (bloom={bloom}, full={full})",
                        handshake.user_agent
                    );
                    if let Some(cache) = self.cache {
                        cache.record_btc(addr);
                    }
                    self.peers.push(Peer {
                        addr,
                        session,
                        bloom,
                        full,
                    });
                }
                Err(e) => {
                    warn!("coinbase peer {addr} failed: {e:#}");
                }
            }
        }
    }

    fn bloom_peers(&self) -> usize {
        self.peers.iter().filter(|p| p.bloom).count()
    }

    /// Fetch one coinbase, trying bloom peers first and full blocks second.
    fn fetch(&mut self, core: &Core, height: u64, hash: BlockHash) -> Result<u64> {
        if self.peers.is_empty() {
            self.fill();
        }
        if self.peers.is_empty() {
            bail!("no coinbase-capable peer");
        }
        if !self.ready_logged {
            self.ready_logged = true;
            info!(
                "coinbase: pool ready with {} peer(s) ({} bloom); first request at height #{height}",
                self.peers.len(),
                self.bloom_peers()
            );
        }

        // Try every bloom peer once, starting where the last success left off.
        // Heights <= 16 have no BIP34 data push to filter on; use a full block.
        let n = self.peers.len();
        for step in 0..n {
            let index = (self.cursor + step) % n;
            if !self.peers[index].bloom || height <= 16 {
                continue;
            }
            match fetch_bloom(core, &mut self.peers[index].session, height, hash) {
                Ok(bytes) => {
                    self.cursor = (index + 1) % n;
                    return Ok(bytes);
                }
                Err(e) => {
                    warn!(
                        "coinbase #{height}: bloom peer {} failed: {e:#}",
                        self.peers[index].addr
                    );
                    self.peers[index].bloom = false;
                }
            }
        }

        // No bloom peer could do it: fall back to a full block.
        let n = self.peers.len();
        for step in 0..n {
            let index = (self.cursor + step) % n;
            if !self.peers[index].full {
                continue;
            }
            match fetch_full(core, &mut self.peers[index].session, height, hash) {
                Ok(bytes) => {
                    self.cursor = (index + 1) % n;
                    return Ok(bytes);
                }
                Err(e) => {
                    warn!(
                        "coinbase #{height}: full peer {} failed: {e:#}",
                        self.peers[index].addr
                    );
                    self.peers[index].full = false;
                }
            }
        }

        // Everything we had is gone. Try a fresh batch once; if that also
        // yields nothing, the caller records the block as failed.
        self.peers.retain(|p| p.bloom || p.full);
        if self.peers.is_empty() {
            self.fill();
            if self.peers.is_empty() {
                bail!("all coinbase peers failed and no replacements are available");
            }
            return self.fetch(core, height, hash);
        }
        bail!(
            "no peer could fetch the coinbase (bloom peers left: {})",
            self.bloom_peers()
        )
    }
}

fn fetch_bloom(
    core: &Core,
    session: &mut Session<std::net::TcpStream>,
    height: u64,
    hash: BlockHash,
) -> Result<u64> {
    let element = bip34_height(height);
    let filter = BloomFilter::with_elements([element], FILTER_BYTES, FILTER_HASH_FUNCS, 0)?
        .to_filter_load(BloomFlags::None);
    let filtered = session.get_filtered_block(hash, filter)?;
    let coinbase = filtered
        .transactions
        .iter()
        .find(|tx| tx.is_coinbase())
        .context("filtered block carried no coinbase")?;

    let mut proof = Vec::new();
    filtered
        .merkle_block
        .consensus_encode(&mut proof)
        .context("encoding the merkle proof")?;
    let mut transactions = 0usize;
    for tx in &filtered.transactions {
        let mut raw = Vec::new();
        tx.consensus_encode(&mut raw)
            .context("encoding a matched transaction")?;
        transactions += raw.len();
    }

    core.store_coinbase(height, hash, coinbase, &filtered.merkle_block)?;
    Ok((proof.len() + transactions) as u64)
}

fn fetch_full(
    core: &Core,
    session: &mut Session<std::net::TcpStream>,
    height: u64,
    hash: BlockHash,
) -> Result<u64> {
    let block = session.get_block(hash)?;
    if block.header.block_hash() != hash {
        bail!(
            "peer returned block {} for {hash}",
            block.header.block_hash()
        );
    }
    let merkle_root = block
        .compute_merkle_root()
        .context("block has no transactions")?;
    if merkle_root != block.header.merkle_root {
        bail!("block merkle root does not match its header");
    }
    let mut raw = Vec::new();
    block
        .consensus_encode(&mut raw)
        .context("encoding the full block")?;
    let coinbase = block.txdata.first().context("block has no coinbase")?;
    let proof =
        MerkleBlock::from_block_with_predicate(&block, |txid| *txid == coinbase.compute_txid());
    core.store_coinbase(height, hash, coinbase, &proof)?;
    Ok(raw.len() as u64)
}

#[cfg(test)]
mod tests {
    use bitcoin::absolute::LockTime;
    use bitcoin::block::{Block, Header as BitcoinHeader, Version as BlockVersion};
    use bitcoin::hashes::Hash;
    use bitcoin::merkle_tree::MerkleBlock;
    use bitcoin::pow::CompactTarget;
    use bitcoin::script::ScriptBuf;
    use bitcoin::transaction::{OutPoint, Transaction, TxIn, TxOut, Version as TxVersion};
    use bitcoin::{Amount, Network, Sequence, Witness};
    use rsk_core::Core;

    use super::scan_for_tags;
    use crate::config::Config;

    /// A coinbase whose only output is an `OP_RETURN` carrying a tag.
    fn coinbase_with_tag(bn: u32) -> Transaction {
        let mut payload = [0u8; 32];
        payload[..20].copy_from_slice(&[0x11; 20]);
        payload[20..27].copy_from_slice(&[1, 2, 3, 4, 5, 6, 7]);
        payload[27] = 0;
        payload[28..].copy_from_slice(&bn.to_be_bytes());

        let mut script = vec![0x6a, 0x29];
        script.extend_from_slice(b"RSKBLOCK:");
        script.extend_from_slice(&payload);

        Transaction {
            version: TxVersion::ONE,
            lock_time: LockTime::ZERO,
            input: vec![TxIn {
                previous_output: OutPoint::null(),
                script_sig: ScriptBuf::from_bytes(vec![0x03, 0x40, 0x0d, 0x0f]),
                sequence: Sequence::MAX,
                witness: Witness::default(),
            }],
            output: vec![TxOut {
                value: Amount::ZERO,
                script_pubkey: ScriptBuf::from_bytes(script),
            }],
        }
    }

    #[test]
    fn scan_reads_a_cached_op_return_tag_without_peers() {
        let dir = tempfile::tempdir().unwrap();
        let core = Core::open(&dir.path().join("chain.redb"), Network::Regtest).unwrap();

        // A non-mainnet store starts from the network genesis.
        let genesis = core.header_at(0).unwrap().expect("regtest genesis");

        let coinbase = coinbase_with_tag(7);
        let mut header = BitcoinHeader {
            version: BlockVersion::ONE,
            prev_blockhash: genesis.block_hash(),
            merkle_root: bitcoin::TxMerkleNode::all_zeros(),
            time: genesis.time + 1,
            bits: CompactTarget::from_consensus(0x207f_ffff),
            nonce: 0,
        };
        loop {
            let block = Block {
                header,
                txdata: vec![coinbase.clone()],
            };
            header.merkle_root = block.compute_merkle_root().expect("one transaction");
            if header.validate_pow(header.target()).is_ok() {
                break;
            }
            header.nonce = header.nonce.wrapping_add(1);
        }
        core.connect_headers(&[header], genesis.time as u64 + 100)
            .unwrap();

        let block = Block {
            header,
            txdata: vec![coinbase.clone()],
        };
        let proof =
            MerkleBlock::from_block_with_predicate(&block, |txid| *txid == coinbase.compute_txid());
        core.store_coinbase(1, header.block_hash(), &coinbase, &proof)
            .unwrap();

        let scan = scan_for_tags(&core, &Config::default(), 1, 4).unwrap();
        assert_eq!(scan.blocks, 1);
        assert_eq!(scan.tags.len(), 1);
        assert_eq!(scan.tags[0].bn, 7);
        assert_eq!(scan.report.cached, 1);
        assert_eq!(scan.report.stored, 0);
        assert_eq!(scan.report.failed, 0);
    }
}

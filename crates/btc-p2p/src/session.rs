//! A connected Bitcoin peer: the `version`/`verack` handshake and the message
//! session a headers client drives.
//!
//! [`Session`] is generic over the byte stream so offline tests can drive the
//! exact same code path over an in-memory mock; [`Session::connect`] is the
//! TCP entry point.

use std::io::{Read, Write};
use std::net::{SocketAddr, TcpStream};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use bitcoin::block::{Block, BlockHash, Header as BlockHeader};
use bitcoin::hashes::Hash as _;
use bitcoin::merkle_tree::MerkleBlock;
use bitcoin::p2p::address::Address;
use bitcoin::p2p::message::NetworkMessage;
use bitcoin::p2p::message::RawNetworkMessage;
use bitcoin::p2p::message_blockdata::{GetHeadersMessage, Inventory};
use bitcoin::p2p::message_bloom::FilterLoad;
use bitcoin::p2p::message_network::VersionMessage;
use bitcoin::p2p::{Magic, ServiceFlags};
use bitcoin::transaction::Transaction;

use crate::codec;
use crate::error::Error;

/// Mainnet magic bytes; this client speaks mainnet only for now.
const MAGIC: Magic = Magic::BITCOIN;

/// Timeout for the initial TCP connection.
const CONNECT_TIMEOUT: Duration = Duration::from_secs(5);

/// Default read timeout once connected. Without it a peer that accepts a
/// request and then goes silent hangs the caller forever; callers that expect
/// slow replies (large blocks) can raise it with [`Session::set_read_timeout`].
const DEFAULT_READ_TIMEOUT: Duration = Duration::from_secs(30);

/// `MSG_FILTERED_BLOCK` from BIP37. `bitcoin`'s [`Inventory`] does not model it,
/// so requests use [`Inventory::Unknown`] with this type.
pub const MSG_FILTERED_BLOCK: u32 = 3;

/// What the peer told us in its `version` message.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Handshake {
    /// The peer's protocol version.
    pub version: u32,
    /// The peer's advertised service flags.
    pub services: ServiceFlags,
    /// The peer's user agent (BIP 14), e.g. `/Satoshi:27.0/`.
    pub user_agent: String,
    /// The peer's best block height at handshake time.
    pub start_height: i32,
}

impl Session<TcpStream> {
    /// Connect to `addr`, run the handshake, and return the session plus what
    /// the peer advertised. `start_height` is our own best height (we only
    /// advertise, nothing is done with it beyond honesty).
    ///
    /// Advertises [`ServiceFlags::NONE`]: we serve nothing. For BIP37 filtered
    /// blocks use [`Session::connect_with_services`] with `NODE_BLOOM`, because
    /// Bitcoin Core checks the requester's advertised services before accepting
    /// `filterload` or `MSG_FILTERED_BLOCK`.
    pub fn connect(
        addr: SocketAddr,
        user_agent: &str,
        start_height: i32,
    ) -> Result<(Self, Handshake), Error> {
        Self::connect_with_services(addr, user_agent, start_height, ServiceFlags::NONE)
    }

    /// Connect and advertise `services` in our `version` message.
    pub fn connect_with_services(
        addr: SocketAddr,
        user_agent: &str,
        start_height: i32,
        services: ServiceFlags,
    ) -> Result<(Self, Handshake), Error> {
        let stream = TcpStream::connect_timeout(&addr, CONNECT_TIMEOUT)?;
        stream.set_nodelay(true).ok();
        stream.set_read_timeout(Some(DEFAULT_READ_TIMEOUT))?;
        let sender = stream.local_addr()?;
        let mut session = Session { stream };
        let handshake =
            session.handshake_with_services(addr, sender, user_agent, start_height, services)?;
        Ok((session, handshake))
    }

    /// Change the socket read timeout. `None` restores blocking reads.
    pub fn set_read_timeout(&mut self, timeout: Option<Duration>) -> Result<(), Error> {
        self.stream.set_read_timeout(timeout)?;
        Ok(())
    }
}

/// The answer to a BIP37 `getdata MSG_FILTERED_BLOCK`: the header plus a
/// partial merkle tree, and every transaction that matched the filter.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FilteredBlock {
    /// The block header and the partial merkle proof for the matched txids.
    pub merkle_block: MerkleBlock,
    /// The matching transactions the peer sent after the `merkleblock`.
    pub transactions: Vec<Transaction>,
}

/// A post-handshake message session with one peer.
pub struct Session<S: Read + Write> {
    stream: S,
}

impl<S: Read + Write> Session<S> {
    /// Run the version/verack handshake. `receiver` is the peer we just
    /// connected to; `sender` is our own address as we see it.
    ///
    /// We advertise protocol 70001 (the `bitcoin` crate's default), so the
    /// negotiation messages that may interleave between `version` and `verack`
    /// for newer peers (`wtxidrelay`, `sendaddrv2`) are not sent to us — but
    /// they are tolerated here regardless, so a peer that assumes 70016 does
    /// not break the handshake.
    pub fn handshake(
        &mut self,
        receiver: SocketAddr,
        sender: SocketAddr,
        user_agent: &str,
        start_height: i32,
    ) -> Result<Handshake, Error> {
        self.handshake_with_services(
            receiver,
            sender,
            user_agent,
            start_height,
            ServiceFlags::NONE,
        )
    }

    /// Like [`Session::handshake`], but advertises `services` instead of none.
    pub fn handshake_with_services(
        &mut self,
        receiver: SocketAddr,
        sender: SocketAddr,
        user_agent: &str,
        start_height: i32,
        services: ServiceFlags,
    ) -> Result<Handshake, Error> {
        let version = VersionMessage::new(
            services,
            now_unix(),
            Address::new(&receiver, ServiceFlags::NONE),
            Address::new(&sender, ServiceFlags::NONE),
            nonce(),
            user_agent.to_string(),
            start_height,
        );
        self.send(NetworkMessage::Version(version))?;

        let msg = self.next_message()?;
        let handshake = match msg.into_payload() {
            NetworkMessage::Version(v) => Handshake {
                version: v.version,
                services: v.services,
                user_agent: v.user_agent.clone(),
                start_height: v.start_height,
            },
            other => {
                return Err(Error::UnexpectedMessage {
                    expected: "version",
                    got: other.cmd().to_string(),
                });
            }
        };
        self.send(NetworkMessage::Verack)?;

        loop {
            let msg = self.next_message()?;
            match msg.into_payload() {
                NetworkMessage::Verack => break,
                NetworkMessage::WtxidRelay | NetworkMessage::SendAddrV2 => continue,
                other => {
                    return Err(Error::UnexpectedMessage {
                        expected: "verack",
                        got: other.cmd().to_string(),
                    });
                }
            }
        }
        Ok(handshake)
    }

    /// Send one message.
    pub fn send(&mut self, payload: NetworkMessage) -> Result<(), Error> {
        codec::write_message(&mut self.stream, &RawNetworkMessage::new(MAGIC, payload))
    }

    /// Read the next message, answering `ping` inline (keepalive). Returns
    /// everything else for the caller to interpret.
    pub fn next_message(&mut self) -> Result<RawNetworkMessage, Error> {
        let msg = codec::read_message_for(&mut self.stream, MAGIC)?;
        let pong = match msg.payload() {
            NetworkMessage::Ping(nonce) => Some(*nonce),
            _ => None,
        };
        if let Some(nonce) = pong {
            self.send(NetworkMessage::Pong(nonce))?;
        }
        Ok(msg)
    }

    /// Send a `getheaders` request without waiting for the reply. Pair with
    /// [`Session::read_headers`] to overlap requests: send the next one, do
    /// work, then read.
    pub fn request_headers(&mut self, locator: &[BlockHash], stop: BlockHash) -> Result<(), Error> {
        self.send(NetworkMessage::GetHeaders(GetHeadersMessage::new(
            locator.to_vec(),
            stop,
        )))
    }

    /// Read until a `headers` reply arrives, answering `ping` inline and
    /// skipping other chatter.
    pub fn read_headers(&mut self) -> Result<Vec<BlockHeader>, Error> {
        loop {
            let msg = self.next_message()?;
            if let NetworkMessage::Headers(headers) = msg.into_payload() {
                return Ok(headers);
            }
        }
    }

    /// One `getheaders` round trip: send the locator, return the first
    /// `headers` response. An empty response means the peer knows nothing
    /// after our locator — i.e. we are caught up with it.
    pub fn get_headers(
        &mut self,
        locator: &[BlockHash],
        stop: BlockHash,
    ) -> Result<Vec<BlockHeader>, Error> {
        self.request_headers(locator, stop)?;
        self.read_headers()
    }

    /// One BIP37 filtered-block round trip: load `filter`, request the block,
    /// and return the partial merkle proof plus every matched transaction.
    ///
    /// The peer must serve BIP37 (`NODE_BLOOM`); this session must also have
    /// advertised it (see [`Session::connect_with_services`]). The proof is
    /// checked against the header inside the `merkleblock`, so a peer cannot
    /// return a matching txid that is not in the block. It can still omit
    /// matches; the caller decides whether the returned set is complete.
    pub fn get_filtered_block(
        &mut self,
        hash: BlockHash,
        filter: FilterLoad,
    ) -> Result<FilteredBlock, Error> {
        self.send(NetworkMessage::FilterClear)?;
        self.send(NetworkMessage::FilterLoad(filter))?;
        self.send(NetworkMessage::GetData(vec![Inventory::Unknown {
            inv_type: MSG_FILTERED_BLOCK,
            hash: hash.to_byte_array(),
        }]))?;

        let merkle_block = loop {
            match self.next_message()?.into_payload() {
                NetworkMessage::MerkleBlock(block) => break block,
                NetworkMessage::NotFound(_) => return Err(Error::NotFound),
                _ => continue,
            }
        };

        let mut matches = Vec::new();
        let mut indexes = Vec::new();
        merkle_block
            .extract_matches(&mut matches, &mut indexes)
            .map_err(|e| Error::MerkleProof(e.to_string()))?;

        let mut transactions = Vec::with_capacity(matches.len());
        while transactions.len() < matches.len() {
            match self.next_message()?.into_payload() {
                NetworkMessage::Tx(tx) => transactions.push(tx),
                NetworkMessage::NotFound(_) => return Err(Error::NotFound),
                _ => continue,
            }
        }
        Ok(FilteredBlock {
            merkle_block,
            transactions,
        })
    }

    /// One `getdata` round trip for a whole block. Blocks are large; callers
    /// that only want the coinbase still have to fetch the block, because the
    /// wire has no message that serves a transaction out of it.
    pub fn get_block(&mut self, hash: BlockHash) -> Result<Block, Error> {
        self.send(NetworkMessage::GetData(vec![Inventory::Block(hash)]))?;
        loop {
            let msg = self.next_message()?;
            if let NetworkMessage::Block(block) = msg.into_payload() {
                return Ok(block);
            }
        }
    }
}

fn now_unix() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

/// A connection nonce. Peers use it to reject self-connections; time-salted
/// uniqueness is more than enough without a random-number dependency.
fn nonce() -> u64 {
    static NEXT: AtomicU64 = AtomicU64::new(1);
    let seq = NEXT.fetch_add(1, Ordering::Relaxed);
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_nanos() as u64)
        .unwrap_or(0);
    nanos ^ seq
}

#[cfg(test)]
mod tests {
    use super::*;
    use bitcoin::Network;
    use bitcoin::block::Version;
    use bitcoin::blockdata::constants::genesis_block;
    use bitcoin::hash_types::TxMerkleNode;
    use bitcoin::hashes::Hash;
    use bitcoin::pow::CompactTarget;
    use std::net::{IpAddr, Ipv4Addr};

    use crate::mock::MockPeer;

    const MAGIC: Magic = Magic::BITCOIN;

    fn addr(port: u16) -> SocketAddr {
        SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), port)
    }

    fn genesis_hash() -> BlockHash {
        genesis_block(Network::Bitcoin).header.block_hash()
    }

    /// A syntactically valid header hanging off `prev`, no PoW needed here —
    /// this crate moves headers, it does not judge them.
    fn header(prev: BlockHash, time: u32) -> BlockHeader {
        BlockHeader {
            version: Version::ONE,
            prev_blockhash: prev,
            merkle_root: TxMerkleNode::all_zeros(),
            time,
            bits: CompactTarget::from_consensus(0x207f_ffff),
            nonce: 0,
        }
    }

    #[test]
    fn handshake_exchanges_versions_and_veracks() -> Result<(), Error> {
        let (receiver, sender) = (addr(8333), addr(40000));
        let their_version = VersionMessage::new(
            ServiceFlags::NETWORK,
            1_600_000_000,
            Address::new(&receiver, ServiceFlags::NONE),
            Address::new(&sender, ServiceFlags::NONE),
            7,
            "/mock:1/".to_string(),
            42,
        );
        let their = their_version.clone();

        // Script what the peer sends: version, then verack.
        let mock = MockPeer::scripted(vec![
            RawNetworkMessage::new(MAGIC, NetworkMessage::Version(their_version)),
            RawNetworkMessage::new(MAGIC, NetworkMessage::Verack),
        ]);
        let mut session = Session { stream: mock };
        let handshake = session.handshake(receiver, sender, "/rske:0.1.0/", 0)?;

        assert_eq!(handshake.user_agent, "/mock:1/");
        assert_eq!(handshake.version, their.version);
        assert_eq!(handshake.services, their.services);
        assert_eq!(handshake.start_height, 42);

        // We sent version, then verack.
        let sent = session.stream.sent();
        assert_eq!(
            sent.len(),
            2,
            "expected version + verack, got {:?}",
            sent.iter().map(|m| m.cmd().to_string()).collect::<Vec<_>>()
        );
        assert_eq!(sent[0].cmd(), "version");
        assert_eq!(sent[1].cmd(), "verack");
        Ok(())
    }

    #[test]
    fn pre_verack_chatter_is_tolerated() -> Result<(), Error> {
        // A peer that assumes protocol 70016 sends wtxidrelay before verack.
        let (receiver, sender) = (addr(8333), addr(40000));
        let mock = MockPeer::scripted(
            vec![
                NetworkMessage::Version(VersionMessage::new(
                    ServiceFlags::NETWORK | ServiceFlags::WITNESS,
                    1_600_000_000,
                    Address::new(&receiver, ServiceFlags::NONE),
                    Address::new(&sender, ServiceFlags::NONE),
                    7,
                    "/mock:70016/".to_string(),
                    0,
                )),
                NetworkMessage::WtxidRelay,
                NetworkMessage::SendAddrV2,
                NetworkMessage::Verack,
            ]
            .into_iter()
            .map(|payload| RawNetworkMessage::new(MAGIC, payload))
            .collect(),
        );
        let mut session = Session { stream: mock };
        let handshake = session.handshake(receiver, sender, "/rske:0.1.0/", 0)?;
        assert_eq!(handshake.user_agent, "/mock:70016/");
        Ok(())
    }

    #[test]
    fn get_headers_answers_pings_and_returns_headers() -> Result<(), Error> {
        let genesis = genesis_hash();
        let h1 = header(genesis, 1_600_000_001);
        let h2 = header(h1.block_hash(), 1_600_000_002);

        let mock = MockPeer::scripted(vec![
            RawNetworkMessage::new(MAGIC, NetworkMessage::Ping(7)),
            RawNetworkMessage::new(MAGIC, NetworkMessage::Headers(vec![h1, h2])),
        ]);
        let mut session = Session { stream: mock };
        let headers = session.get_headers(&[genesis], BlockHash::all_zeros())?;

        assert_eq!(headers, vec![h1, h2]);
        // We sent getheaders first, and the ping was answered with a pong.
        let sent = session.stream.sent();
        assert_eq!(sent[0].cmd(), "getheaders");
        assert_eq!(sent[1].cmd(), "pong");
        Ok(())
    }

    #[test]
    fn filtered_block_returns_the_proof_and_matching_tx() -> Result<(), Error> {
        use bitcoin::blockdata::constants::genesis_block;
        use bitcoin::merkle_tree::MerkleBlock;
        use bitcoin::p2p::message_bloom::BloomFlags;

        let block = genesis_block(Network::Bitcoin);
        let coinbase = block.txdata[0].clone();
        let proof =
            MerkleBlock::from_block_with_predicate(&block, |txid| *txid == coinbase.compute_txid());
        let filter = crate::BloomFilter::new(10, 3, 0)
            .expect("valid filter")
            .to_filter_load(BloomFlags::None);

        let mock = MockPeer::scripted(vec![
            RawNetworkMessage::new(MAGIC, NetworkMessage::MerkleBlock(proof.clone())),
            RawNetworkMessage::new(MAGIC, NetworkMessage::Tx(coinbase.clone())),
        ]);
        let mut session = Session { stream: mock };
        let got = session.get_filtered_block(block.block_hash(), filter)?;

        // The wire pads the partial tree's flag bits to a byte, so compare the
        // header and the proof's hashes rather than the padded bit vector.
        assert_eq!(got.merkle_block.header, proof.header);
        assert_eq!(got.merkle_block.txn.hashes(), proof.txn.hashes());
        assert_eq!(got.transactions, vec![coinbase]);
        let sent = session.stream.sent();
        assert_eq!(
            sent.iter().map(|m| m.cmd().to_string()).collect::<Vec<_>>(),
            vec!["filterclear", "filterload", "getdata"]
        );
        Ok(())
    }

    #[test]
    fn filtered_block_propagates_notfound() -> Result<(), Error> {
        use bitcoin::p2p::message_bloom::BloomFlags;
        let filter = crate::BloomFilter::new(10, 3, 0)
            .expect("valid filter")
            .to_filter_load(BloomFlags::None);
        let mock = MockPeer::scripted(vec![RawNetworkMessage::new(
            MAGIC,
            NetworkMessage::NotFound(vec![Inventory::Error]),
        )]);
        let mut session = Session { stream: mock };
        let err = session
            .get_filtered_block(genesis_hash(), filter)
            .unwrap_err();
        assert!(matches!(err, Error::NotFound), "got {err:?}");
        Ok(())
    }

    #[test]
    fn handshake_rejects_a_verack_first_peer() -> Result<(), Error> {
        let mock = MockPeer::scripted(vec![RawNetworkMessage::new(MAGIC, NetworkMessage::Verack)]);
        let mut session = Session { stream: mock };
        let err = session
            .handshake(addr(8333), addr(40000), "/rske:0.1.0/", 0)
            .unwrap_err();
        assert!(
            matches!(
                err,
                Error::UnexpectedMessage {
                    expected: "version",
                    ..
                }
            ),
            "got {err:?}"
        );
        Ok(())
    }
}

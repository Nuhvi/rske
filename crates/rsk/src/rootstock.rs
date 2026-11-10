//! Rootstock peer access: connect, and the requests a header sync needs.
//!
//! A [`Session`] is one handshaken peer. Everything is driven explicitly — the
//! caller asks, the session answers or errors — so the sync logic lives in the
//! caller, not here.
//!
//! The wire has no forward request: `BlockHeadersRequest` is `{hash, count}`
//! and the server walks *backward* from the hash. The `SkeletonRequest` is the
//! only height-addressed message, and it is what lets a sync fetch many
//! descending chunks in parallel.

use std::net::SocketAddr;
use std::time::Duration;

use alloy_primitives::{B256, B512, Bytes, U256};
use anyhow::{Context, Result};
use futures::{SinkExt, StreamExt};
use k256::SecretKey;
use k256::elliptic_curve::sec1::ToEncodedPoint;
use rsk_consensus::Header;
use rsk_consensus::config::ChainConfig;
use rsk_p2p::discovery::{
    DiscoveryEndpoint, DiscoveryPacket, DiscoveryPayload, PingMessage, PongMessage,
};
use rsk_p2p::protocol::{
    BlockHashRequest, BlockHeadersQuery, BlockHeadersRequest, BlockIdentifier, P2pMessage,
    RskMessage, RskStatus, RskSubMessage, SkeletonRequest,
};
use rsk_p2p::{Handshake, HandshakeCodec, NodeConfig, PeerCapabilities};
use tokio::net::{TcpStream, UdpSocket};
use tokio::time::timeout;
use tokio_util::codec::Framed;
use tracing::{debug, info, warn};

const MAINNET_GENESIS: &str = "0xf88529d4ab262c0f4d042e9d8d3f2472848eaafe1a9b7213f57617eb40a9f9e0";
const TESTNET_GENESIS: &str = "0xcabb7fbe88cd6d922042a32ffc08ce8b1fbb37d650b9d4e7dbfe2a7469adfa42";

/// How long to wait for a request's answer before giving up on the peer.
const REQUEST_TIMEOUT: Duration = Duration::from_secs(30);

/// A Rootstock network to talk to.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Chain {
    Mainnet,
    Testnet,
}

impl Chain {
    /// Parse `"mainnet"` or `"testnet"`.
    pub fn parse(s: &str) -> Result<Self> {
        match s {
            "mainnet" => Ok(Chain::Mainnet),
            "testnet" => Ok(Chain::Testnet),
            other => anyhow::bail!("unknown chain {other:?} (mainnet|testnet)"),
        }
    }

    /// The RSK chain matching a Bitcoin network, if there is one.
    pub(crate) fn for_network(network: bitcoin::Network) -> Option<Self> {
        match network {
            bitcoin::Network::Bitcoin => Some(Chain::Mainnet),
            bitcoin::Network::Testnet | bitcoin::Network::Testnet4 | bitcoin::Network::Signet => {
                Some(Chain::Testnet)
            }
            bitcoin::Network::Regtest => None,
        }
    }

    /// The shipped discovery bootnodes for this chain.
    pub(crate) fn bootnodes(self) -> Vec<String> {
        match self {
            Chain::Mainnet => ChainConfig::mainnet().bootnodes(),
            Chain::Testnet => ChainConfig::testnet().bootnodes(),
        }
    }

    fn default_port(self) -> u16 {
        match self {
            Chain::Mainnet => 5050,
            Chain::Testnet => 50505,
        }
    }

    fn network_id(self) -> u64 {
        match self {
            Chain::Mainnet => 775,
            Chain::Testnet => 8100,
        }
    }

    fn chain_id(self) -> u8 {
        match self {
            Chain::Mainnet => 30,
            Chain::Testnet => 31,
        }
    }

    fn genesis_hash(self) -> B256 {
        match self {
            Chain::Mainnet => MAINNET_GENESIS.parse().expect("valid genesis hash"),
            Chain::Testnet => TESTNET_GENESIS.parse().expect("valid genesis hash"),
        }
    }
}

/// One header as the peer sent it: the decoded header and its exact wire bytes.
///
/// The bytes matter: Java's RLP is non-canonical (leading-zero `BigInteger`
/// values), so the consensus hash is over the peer's encoding, not a re-encode.
#[derive(Debug, Clone)]
pub struct WireHeader {
    pub header: Header,
    pub raw: Bytes,
}

/// A handshaken Rootstock peer.
pub struct Session {
    framed: Framed<TcpStream, HandshakeCodec>,
    status: RskStatus,
    caps: PeerCapabilities,
    next_id: u64,
    addr: SocketAddr,
}

impl Session {
    /// Resolve the bootnode, learn its node ID over signed UDP discovery (DNS
    /// bootnodes carry no public key), and run the RLPx + devp2p + `rsk` handshake.
    pub async fn connect(host: &str, chain: Chain) -> Result<Self> {
        let addr = resolve(host, chain).await?;
        let (secret_key, our_id) = make_identity();
        let peer_id = discover_node_id(&secret_key, addr, chain.network_id() as u32).await?;

        let stream = TcpStream::connect(addr)
            .await
            .with_context(|| format!("TCP connect to {addr}"))?;
        let handshake = Handshake::new(
            stream,
            node_config(chain, secret_key, our_id),
            Some(peer_id),
        );
        let (_peer, status, caps, framed) = timeout(Duration::from_secs(15), handshake.run())
            .await
            .context("RLPx/p2p handshake timed out")??;

        info!(
            "connected {addr}: rsk/{} (snap: {}) tip #{} 0x{}…",
            caps.rsk_version,
            caps.snap,
            status.best_block_number,
            hex8(status.best_block_hash.as_slice())
        );
        Ok(Self {
            framed,
            status,
            caps,
            next_id: 1,
            addr,
        })
    }

    /// The TCP address this session actually connected to.
    pub fn addr(&self) -> SocketAddr {
        self.addr
    }

    /// What the peer advertised at handshake time.
    pub fn status(&self) -> &RskStatus {
        &self.status
    }

    /// The capabilities both ends negotiated.
    pub fn caps(&self) -> PeerCapabilities {
        self.caps
    }

    fn id(&mut self) -> u64 {
        let id = self.next_id;
        self.next_id += 1;
        id
    }

    /// Read the next `rsk` message, answering `ping` inline.
    async fn next_rsk(&mut self) -> Result<RskSubMessage> {
        loop {
            match timeout(REQUEST_TIMEOUT, self.framed.next()).await {
                Ok(Some(Ok(P2pMessage::Ping))) => {
                    self.framed.send(P2pMessage::Pong).await.context("pong")?;
                }
                Ok(Some(Ok(P2pMessage::RskMessage(m)))) => return Ok(m.sub_message),
                Ok(Some(Ok(_))) => {}
                Ok(Some(Err(e))) => anyhow::bail!("session error: {e:#}"),
                Ok(None) => anyhow::bail!("peer closed the connection"),
                Err(_) => anyhow::bail!("timed out waiting for a message"),
            }
        }
    }

    async fn send(&mut self, sub: RskSubMessage) -> Result<()> {
        self.framed
            .send(P2pMessage::RskMessage(RskMessage::new(sub)))
            .await
            .context("sending an rsk message")
    }

    /// `count` headers walking back from `hash` (newest first), with their
    /// exact wire bytes. `count` of 0 answers with headers up to the server cap.
    pub async fn headers(&mut self, hash: B256, count: u32) -> Result<Vec<WireHeader>> {
        let id = self.id();
        self.send(RskSubMessage::BlockHeadersRequest(BlockHeadersRequest {
            id,
            query: BlockHeadersQuery { hash, count },
        }))
        .await?;
        loop {
            match self.next_rsk().await? {
                RskSubMessage::BlockHeadersResponse(r) if r.id == id => {
                    return Ok(r
                        .headers
                        .into_iter()
                        .zip(r.raw)
                        .map(|(header, raw)| WireHeader { header, raw })
                        .collect());
                }
                _ => continue,
            }
        }
    }

    /// The hash of the block at `height` (type 8/18) — the height-addressed
    /// probe a checkpoint bisect needs. Not used by the current IBD flow.
    pub async fn block_hash_at(&mut self, height: u64) -> Result<B256> {
        let id = self.id();
        self.send(RskSubMessage::BlockHashRequest(BlockHashRequest {
            id,
            height,
        }))
        .await?;
        loop {
            match self.next_rsk().await? {
                RskSubMessage::BlockHashResponse(r) if r.id == id => return Ok(r.hash),
                _ => continue,
            }
        }
    }

    /// Block identifiers every 192 blocks from `start` (type 16/13), capped by
    /// the server. These are *claims* about where blocks sit, used only to
    /// decide where to ask — never believed on their own. Not wired yet.
    pub async fn skeleton(&mut self, start: u64) -> Result<Vec<BlockIdentifier>> {
        let id = self.id();
        self.send(RskSubMessage::SkeletonRequest(SkeletonRequest {
            id,
            start_number: start,
        }))
        .await?;
        loop {
            match self.next_rsk().await? {
                RskSubMessage::SkeletonResponse(r) if r.id == id => return Ok(r.block_identifiers),
                _ => continue,
            }
        }
    }
}

fn node_config(chain: Chain, secret_key: [u8; 32], id: B512) -> NodeConfig {
    NodeConfig {
        client_id: "rske:0.1.0".to_string(),
        listen_port: 0,
        id,
        chain_id: chain.chain_id(),
        network_id: chain.network_id(),
        genesis_hash: chain.genesis_hash(),
        best_hash: chain.genesis_hash(),
        best_block_number: 0,
        best_block_parent_hash: None,
        total_difficulty: U256::ZERO,
        earliest_block: 0,
        snap_capability: false,
        bootnodes: vec![],
        closed_network: false,
        read_only: true,
        secret_key,
        discovery_port: 0,
        data_dir: ".".to_string(),
        external_ip: None,
        max_outbound_peers: 8,
        max_inbound_peers: 0,
        max_inbound_per_ip: 0,
        max_inbound_per_cidr: 0,
        inbound_cidr_prefix: 24,
    }
}

/// Split `host:port` (defaulting the port for the chain) and resolve the host,
/// preferring IPv4.
async fn resolve(host: &str, chain: Chain) -> Result<SocketAddr> {
    let (host, port) = match host.rsplit_once(':') {
        Some((h, p)) => (h, p.parse()?),
        None => (host, chain.default_port()),
    };
    let mut addrs = tokio::net::lookup_host((host, port))
        .await
        .with_context(|| format!("resolving {host}:{port}"))?;
    addrs
        .find(|a| a.is_ipv4())
        .or_else(|| addrs.next())
        .with_context(|| format!("no address for {host}:{port}"))
}

/// A one-shot node identity: a random secp256k1 key, `id` = its uncompressed
/// public key (what devp2p's `HelloMessage` carries).
fn make_identity() -> ([u8; 32], B512) {
    let sk = SecretKey::random(&mut k256::elliptic_curve::rand_core::OsRng);
    let encoded = sk.public_key().to_encoded_point(false);
    let mut pk = [0u8; 64];
    pk.copy_from_slice(&encoded.as_bytes()[1..]);
    (sk.to_bytes().into(), B512::from_slice(&pk))
}

/// Send a signed UDP discovery `Ping` and read back the `Pong`, recovering the
/// peer's node ID from the packet signature. Retries, answering any `Ping` we
/// receive with a `Pong`.
async fn discover_node_id(
    secret_key: &[u8; 32],
    addr: SocketAddr,
    network_id: u32,
) -> Result<B512> {
    use k256::ecdsa::SigningKey;
    let key = SigningKey::from_slice(&secret_key[..])?;
    let socket = UdpSocket::bind("0.0.0.0:0").await?;
    let local = socket.local_addr()?;

    for attempt in 1..=6 {
        let message_id = uuid::Uuid::new_v4().to_string();
        let ping = PingMessage {
            from: DiscoveryEndpoint {
                ip: Bytes::from(vec![127, 0, 0, 1]),
                udp_port: local.port(),
                tcp_port: local.port(),
            },
            to: addr_to_endpoint(addr),
            message_id: message_id.clone(),
            network_id,
        };
        let packet = DiscoveryPacket::create(DiscoveryPayload::Ping(ping.clone()), &key)?;
        socket.send_to(&packet.encode(), addr).await?;
        debug!("  pinged {addr} (attempt {attempt})…");

        let deadline = tokio::time::Instant::now() + Duration::from_secs(4);
        let mut buf = vec![0u8; 4096];
        loop {
            let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
            if remaining.is_zero() {
                break;
            }
            match timeout(remaining, socket.recv_from(&mut buf)).await {
                Ok(Ok((len, from))) if from == addr => match DiscoveryPacket::decode(&buf[..len]) {
                    Ok(packet) => match packet.payload {
                        DiscoveryPayload::Pong(_) => {
                            return packet.recover_id().context("recovering node id from pong");
                        }
                        DiscoveryPayload::Ping(_) => {
                            let reply = DiscoveryPacket::create(
                                DiscoveryPayload::Pong(PongMessage {
                                    from: ping.from.clone(),
                                    to: ping.to.clone(),
                                    message_id: message_id.clone(),
                                    network_id,
                                }),
                                &key,
                            )?;
                            let _ = socket.send_to(&reply.encode(), addr).await;
                        }
                        _ => {}
                    },
                    Err(e) => warn!("  discovery decode error: {e}"),
                },
                _ => break,
            }
        }
    }
    anyhow::bail!("could not learn {addr}'s node id over discovery")
}

fn addr_to_endpoint(addr: SocketAddr) -> DiscoveryEndpoint {
    DiscoveryEndpoint {
        ip: match addr {
            SocketAddr::V4(v4) => Bytes::from(v4.ip().octets().to_vec()),
            SocketAddr::V6(v6) => Bytes::from(v6.ip().octets().to_vec()),
        },
        udp_port: addr.port(),
        tcp_port: addr.port(),
    }
}

fn hex8(bytes: &[u8]) -> String {
    bytes.iter().take(8).map(|b| format!("{b:02x}")).collect()
}

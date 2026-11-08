//! Errors from the Bitcoin P2P client.

use thiserror::Error;

/// Everything that can go wrong talking to a peer. Every variant is
/// deterministic given the bytes on the wire and the connection state.
#[derive(Debug, Error)]
pub enum Error {
    #[error("i/o error: {0}")]
    Io(#[from] std::io::Error),

    #[error("consensus decode: {0}")]
    Consensus(#[from] bitcoin::consensus::encode::Error),

    #[error("protocol violation: {0}")]
    Protocol(String),

    #[error("expected the `{expected}` message, got `{got}`")]
    UnexpectedMessage { expected: &'static str, got: String },

    #[error("peer does not have the requested object")]
    NotFound,

    #[error("invalid merkle proof: {0}")]
    MerkleProof(String),
}

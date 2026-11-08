//! # btc-p2p — the Bitcoin P2P protocol client
//!
//! Transport, the `version`/`verack` handshake, and the small set of requests
//! a headers/coinbase client needs, against the real Bitcoin network.
//!
//! This crate is deliberately **chain-agnostic protocol mechanics**: it never
//! interprets a header, never stores anything, and has no notion of the chain
//! state. Chain knowledge (the checkpoint spine, what to sync) lives with the
//! caller — `rsk-core` and above — which hands this crate a locator and gets
//! headers back.
//!
//! Deliberately minimal for now: mainnet only, P2P v1 transport (no BIP 324),
//! one blocking connection at a time, no tokio. `ServiceFlags::NONE` is
//! advertised by default because we serve nothing; callers that need BIP37
//! filtered blocks use [`Session::connect_with_services`] to advertise
//! `NODE_BLOOM` (Bitcoin Core gates filtered-block service on the *requester's*
//! advertised services).

// Tests conventionally assert with `unwrap`-style calls; keep the strict lint
// for production code only.
#![cfg_attr(test, allow(clippy::unwrap_used))]

pub mod bloom;
pub mod codec;
pub mod error;
pub mod seeds;
pub mod session;

#[cfg(test)]
pub(crate) mod mock;

pub use bloom::BloomFilter;
pub use error::Error;
pub use session::{FilteredBlock, Handshake, Session};

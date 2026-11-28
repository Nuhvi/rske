//! Executable specifications for `Core`.
//!
//! One Rust file per capability. `bitcoin_header_validation.rs` holds the HDR
//! rule scenarios; `bitcoin_header_chain.rs` holds the BTC chain machine.
//! `bitcoin_headers.rs` holds the HDR and finality machines until their
//! capabilities exist. `rootstock_checkpoints.rs` holds the RSK checkpoint
//! machine. `support.rs` holds the shared synthetic-chain fixtures and the
//! submit helpers.
//!
//! Requirement IDs in the Rust comments point at the OpenSpec capability or at
//! the machine clauses in the repository-root `specs/` documents.

mod bitcoin_header_chain;
mod bitcoin_header_validation;
mod bitcoin_headers;
mod rootstock_checkpoints;
mod support;

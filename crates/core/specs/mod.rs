//! Executable specifications for `Core`.
//!
//! One Rust file per capability. `bitcoin_header_validation.rs` holds the rule
//! scenarios for the OpenSpec capability; `bitcoin_headers.rs` holds the state
//! machines for chain ingestion. `rootstock_checkpoints.rs` holds the RSK
//! checkpoint machine. `support.rs` holds the shared synthetic-chain fixtures.
//!
//! Requirement IDs in the Rust comments point at the OpenSpec capability or at
//! the machine clauses in the repository-root `specs/` documents.

mod bitcoin_header_validation;
mod bitcoin_headers;
mod rootstock_checkpoints;
mod support;

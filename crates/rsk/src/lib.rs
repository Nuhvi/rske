//! # rsk — the RSK One node, as a library
//!
//! The node's I/O lives here, kept out of the side-effect-free [`rsk_core`]
//! chain engine so any application can embed it: [`Node`] owns the chain store
//! and syncs Bitcoin headers into it ([`Node::sync`]).
//!
//! Nothing here runs on its own: the caller drives every operation and reads
//! back the result. Progress is reported through [`tracing`], which the host
//! app filters and formats.

// Tests read/write a deterministic temp store and conventionally use `unwrap`;
// keep the strict lint for production code only.
#![cfg_attr(test, allow(clippy::unwrap_used))]

pub mod config;
pub mod node;
pub mod rootstock;

mod peers;
mod sync;

pub use config::{Anchor, Config, DEFAULT_DATA_DIR, USER_AGENT};
pub use node::Node;
pub use rootstock::Chain;
pub use sync::SyncReport;

//! Arbitrary-input robustness tests for the byte parsers.
//!
//! These run under `cargo test`, with the `bolero` proptest engine. The same
//! harnesses run under a coverage-guided engine when one is available:
//!
//! ```text
//! cargo bolero test checkpoint_text_never_panics --engine libfuzzer
//! ```
//!
//! `fuzz/` holds the equivalent `cargo-fuzz` targets for a host with a C++
//! toolchain. A crash that either layer finds is a real bug: the parsers read
//! bytes from an untrusted peer.

use bolero::check;

/// The trusted-checkpoint text parser rejects arbitrary bytes without a panic.
#[test]
fn checkpoint_text_never_panics() {
    check!().for_each(|bytes: &[u8]| {
        if let Ok(text) = std::str::from_utf8(bytes) {
            let _ = rsk_core::btc::checkpoints::parse_text(text);
        }
    });
}

/// RSK header RLP decoding accepts or rejects arbitrary bytes without a panic.
#[test]
fn rsk_header_rlp_never_panics() {
    check!().for_each(|bytes: &[u8]| {
        let mut buf = bytes;
        let _ = rsk_consensus::Header::decode_with_hash(&mut buf);
    });
}

/// Bitcoin header decoding accepts or rejects arbitrary bytes without a panic.
#[test]
fn bitcoin_header_decode_never_panics() {
    use bitcoin::consensus::Decodable;

    check!().for_each(|bytes: &[u8]| {
        let mut buf = bytes;
        let _ = bitcoin::block::Header::consensus_decode(&mut buf);
    });
}

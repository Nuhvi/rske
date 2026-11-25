#![no_main]

//! RSK header RLP decoding. The header comes straight off the wire from an
//! untrusted peer, so every byte pattern must decode or fail cleanly.

use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    let mut buf = data;
    let _ = rsk_consensus::Header::decode_with_hash(&mut buf);
});

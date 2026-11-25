#![no_main]

//! The trusted-checkpoint text parser. Input is arbitrary bytes: invalid UTF-8
//! is a valid case, and the parser must reject it without a panic.

use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    if let Ok(text) = std::str::from_utf8(data) {
        let _ = rsk_core::btc::checkpoints::parse_text(text);
    }
});

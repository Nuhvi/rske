#![no_main]

//! Bitcoin P2P frame decoding. The frame envelope carries an attacker-chosen
//! payload length, so the codec must reject an oversized or truncated frame
//! without allocating past its cap.

use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    let mut reader = data;
    let _ = btc_p2p::codec::read_message(&mut reader);
});

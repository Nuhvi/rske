//! The pure Rootstock checkpoint rules: where a trusted checkpoint block is,
//! and when a Rootstock tail has enough work to trust one.
//!
//! Everything here is a function of its inputs — a [`Core`] (the Bitcoin chain)
//! and a Rootstock header — with no I/O, so it is unit testable. The *state*
//! (the Rootstock tree) lives in [`crate::rsk`].
//!
//! ## The anchor is the parent
//!
//! A Rootstock header's `bitcoin_merged_mining_header` is a Bitcoin-header-
//! shaped **solution** meeting Rootstock's (easier) difficulty, so its own hash
//! is rarely a canonical Bitcoin block. What ties it to Bitcoin is its
//! `prev_blockhash`: the real Bitcoin tip it was built on. That parent is the
//! anchor.
//!
//! A checkpoint is a canonical Rootstock block whose **own** anchor is inside
//! the trusted, finalized Bitcoin range `[B, mark]` — at or after the last
//! finalized checkpoint `B`, and at or below the finality mark. The tail is
//! trusted once the work built on the checkpoint (GHOST-weighted, see
//! [`crate::rsk`]) exceeds `2/3` of the Bitcoin tail's work.
//!
//! These are *ephemeral chain* checkpoints — trusted places to start syncing
//! from — and must not be confused with the hardcoded Rootstock checkpoints
//! that mark hardfork activation.
//!
// TODO(VETIVER-731b9d8): every block we validate is newer than the Vetiver
// hardfork, because the checkpoint is always newer given >67% participation.
// The pre-Vetiver validation rules (and the activation-height branches that only
// apply to older blocks) can therefore be dropped.

use alloy_primitives::U256;
use bitcoin::block::Header as BitcoinHeader;
use rsk_consensus::Header as RskHeader;

use crate::Core;
use crate::error::Error;

/// The trust threshold, `2/3`: the Rootstock tail's work must exceed
/// `CHECKPOINT_THRESHOLD_NUM / CHECKPOINT_THRESHOLD_DEN` of the Bitcoin tail's
/// work. Kept as a fraction so the comparison stays exact in integer
/// arithmetic.
pub const CHECKPOINT_THRESHOLD_NUM: u64 = 2;
pub const CHECKPOINT_THRESHOLD_DEN: u64 = 3;

/// The embedded merge-mining solution header of a Rootstock header, decoded.
/// `None` if it is absent or not a decodable 80-byte Bitcoin header.
pub fn merge_mining_header(header: &RskHeader) -> Option<BitcoinHeader> {
    let bytes = header.bitcoin_merged_mining_header.as_ref()?;
    if bytes.len() != 80 {
        return None;
    }
    bitcoin::consensus::deserialize(bytes.as_ref()).ok()
}

/// The canonical Bitcoin height a Rootstock header was built on — the anchor.
/// `None` if the header carries no usable merge-mining header, or its anchor is
/// not on the Bitcoin chain we hold.
pub fn anchor_height(btc: &Core, header: &RskHeader) -> Result<Option<u64>, Error> {
    let Some(solution) = merge_mining_header(header) else {
        return Ok(None);
    };
    btc.height_of(&solution.prev_blockhash)
}

/// Whether a Bitcoin anchor height is inside the trusted range: at or after the
/// last finalized checkpoint, and at or below the finality mark. The mark is
/// itself kept one reorg depth below the tip, so it carries the depth.
pub fn in_tail(anchor: u64, btc_tail_start: u64, btc_mark: u64) -> bool {
    anchor >= btc_tail_start && anchor <= btc_mark
}

/// The trust rule: the Rootstock tail's work exceeds `2/3` of the Bitcoin
/// tail's work. (Strict `>`, so a tie is not trusted.)
pub fn clears_threshold(rsk_work: U256, btc_tail_work: U256) -> bool {
    rsk_work * U256::from(CHECKPOINT_THRESHOLD_DEN)
        > btc_tail_work * U256::from(CHECKPOINT_THRESHOLD_NUM)
}

#[cfg(test)]
mod tests {
    use super::*;
    use bitcoin::Network;
    use bitcoin::block::{BlockHash, Version};
    use bitcoin::consensus::Encodable;
    use bitcoin::hashes::Hash;
    use bitcoin::pow::CompactTarget;

    /// The easiest valid Bitcoin target: nearly any hash passes.
    const EASY_BITS: u32 = 0x207f_ffff;

    fn mine(prev: BlockHash, tag: u32) -> BitcoinHeader {
        let target = CompactTarget::from_consensus(EASY_BITS);
        let mut nonce = 0u32;
        loop {
            let header = BitcoinHeader {
                version: Version::ONE,
                prev_blockhash: prev,
                merkle_root: Hash::all_zeros(),
                time: 1_600_000_000 + tag,
                bits: target,
                nonce,
            };
            if header.validate_pow(header.target()).is_ok() {
                return header;
            }
            nonce += 1;
        }
    }

    /// An RSK header whose embedded merge-mining solution points at `anchor`.
    fn rsk(anchor: BlockHash, number: u64, difficulty: u64) -> RskHeader {
        let solution = BitcoinHeader {
            version: Version::ONE,
            prev_blockhash: anchor,
            merkle_root: Hash::all_zeros(),
            time: 1_600_000_000,
            bits: CompactTarget::from_consensus(EASY_BITS),
            nonce: 0,
        };
        let mut bytes = Vec::new();
        solution.consensus_encode(&mut bytes).unwrap();
        RskHeader {
            parent_hash: Default::default(),
            ommers_hash: Default::default(),
            beneficiary: Default::default(),
            state_root: Default::default(),
            transactions_root: Default::default(),
            receipts_root: Default::default(),
            logs_bloom: Default::default(),
            extension_data: None,
            difficulty: U256::from(difficulty),
            number,
            gas_limit: Default::default(),
            gas_used: 0,
            timestamp: 1_600_000_000,
            extra_data: Default::default(),
            paid_fees: Default::default(),
            minimum_gas_price: Default::default(),
            uncle_count: 0,
            umm_root: None,
            bitcoin_merged_mining_header: Some(bytes.into()),
            bitcoin_merged_mining_merkle_proof: None,
            bitcoin_merged_mining_coinbase_transaction: None,
            cached_hash: None,
            cached_hash_for_merged_mining: None,
        }
    }

    #[test]
    fn merge_mining_header_decodes_only_real_headers() {
        let solution = mine(BlockHash::all_zeros(), 0);
        let mut bytes = Vec::new();
        solution.consensus_encode(&mut bytes).unwrap();
        let mut header = rsk(BlockHash::all_zeros(), 1, 1);
        header.bitcoin_merged_mining_header = Some(bytes.into());
        assert!(merge_mining_header(&header).is_some());

        header.bitcoin_merged_mining_header = Some(vec![1, 2, 3].into());
        assert!(merge_mining_header(&header).is_none());

        header.bitcoin_merged_mining_header = None;
        assert!(merge_mining_header(&header).is_none());
    }

    #[test]
    fn anchor_resolves_to_the_bitcoin_parent_height() {
        let chain = Core::in_memory(Network::Regtest).unwrap();
        let h0 = mine(BlockHash::all_zeros(), 0);
        let h2016 = mine(h0.block_hash(), 1);
        chain.seed_checkpoints(&[(0, h0), (2016, h2016)]).unwrap();

        // Anchored on the 2016 checkpoint.
        let header = rsk(h2016.block_hash(), 5_000_000, 10);
        assert_eq!(anchor_height(&chain, &header).unwrap(), Some(2016));

        // Anchored on an unknown Bitcoin block.
        let header = rsk(BlockHash::all_zeros(), 5_000_001, 10);
        assert_eq!(anchor_height(&chain, &header).unwrap(), None);
    }

    #[test]
    fn trust_needs_more_than_two_thirds() {
        assert!(clears_threshold(U256::from(100), U256::from(100)));
        assert!(!clears_threshold(U256::from(66), U256::from(100)));
        assert!(clears_threshold(U256::from(67), U256::from(100)));
        assert!(!clears_threshold(U256::from(0), U256::from(0)));
    }
}

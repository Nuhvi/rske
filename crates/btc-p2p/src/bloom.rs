//! BIP37 bloom filters.
//!
//! `bitcoin` models the `filterload` message but not the filter itself, so this
//! module builds the bit vector: BIP37's MurmurHash3-based insertion. It is
//! deliberately only the data structure — what to put in it (e.g. a block
//! height, a txid, an address) is the caller's business.
//!
//! A filter is immutable once built except through [`BloomFilter::insert`];
//! [`BloomFilter::to_filter_load`] turns it into the wire message.

use bitcoin::p2p::message_bloom::{BloomFlags, FilterLoad};

use crate::error::Error;

/// BIP37's maximum filter size, in bytes (36,000 bytes = 288,000 bits).
pub const MAX_FILTER_BYTES: usize = 36_000;
/// BIP37's maximum number of hash functions.
pub const MAX_HASH_FUNCS: u32 = 50;

/// A BIP37 bloom filter.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BloomFilter {
    data: Vec<u8>,
    hash_funcs: u32,
    tweak: u32,
}

impl BloomFilter {
    /// An empty filter of `size_bytes` bytes using `hash_funcs` hash functions.
    ///
    /// BIP37 requires `1..=36_000` bytes and `1..=50` hash functions.
    pub fn new(size_bytes: usize, hash_funcs: u32, tweak: u32) -> Result<Self, Error> {
        if size_bytes == 0 || size_bytes > MAX_FILTER_BYTES {
            return Err(Error::Protocol(format!(
                "bloom filter size {size_bytes} is outside 1..={MAX_FILTER_BYTES}"
            )));
        }
        if hash_funcs == 0 || hash_funcs > MAX_HASH_FUNCS {
            return Err(Error::Protocol(format!(
                "bloom hash function count {hash_funcs} is outside 1..={MAX_HASH_FUNCS}"
            )));
        }
        Ok(Self {
            data: vec![0; size_bytes],
            hash_funcs,
            tweak,
        })
    }

    /// A filter with every `element` inserted.
    pub fn with_elements<I, T>(
        elements: I,
        size_bytes: usize,
        hash_funcs: u32,
        tweak: u32,
    ) -> Result<Self, Error>
    where
        I: IntoIterator<Item = T>,
        T: AsRef<[u8]>,
    {
        let mut filter = Self::new(size_bytes, hash_funcs, tweak)?;
        for element in elements {
            filter.insert(element.as_ref());
        }
        Ok(filter)
    }

    /// Insert one element, setting the `hash_funcs` bits BIP37 derives from it.
    pub fn insert(&mut self, element: &[u8]) {
        let bits = self.data.len() * 8;
        for i in 0..self.hash_funcs {
            let seed = i.wrapping_mul(0xFBA4_C795).wrapping_add(self.tweak);
            let hash = murmur3(seed, element);
            let bit = (hash as usize) % bits;
            self.data[bit / 8] |= 1 << (bit % 8);
        }
    }

    /// The filter bytes, in wire order.
    pub fn as_bytes(&self) -> &[u8] {
        &self.data
    }

    /// The `filterload` message carrying this filter.
    pub fn to_filter_load(&self, flags: BloomFlags) -> FilterLoad {
        FilterLoad {
            filter: self.data.clone(),
            hash_funcs: self.hash_funcs,
            tweak: self.tweak,
            flags,
        }
    }
}

/// MurmurHash3 x86 32-bit, as used by BIP37 (`MurmurHash3(nHashNum *
/// 0xFBA4C795 + nTweak, vData)`).
fn murmur3(seed: u32, data: &[u8]) -> u32 {
    const C1: u32 = 0xcc9e_2d51;
    const C2: u32 = 0x1b87_3593;

    let mut hash = seed;
    let (chunks, tail) = data.as_chunks::<4>();
    for chunk in chunks {
        let mut k = u32::from_le_bytes(*chunk);
        k = k.wrapping_mul(C1);
        k = k.rotate_left(15);
        k = k.wrapping_mul(C2);
        hash ^= k;
        hash = hash.rotate_left(13);
        hash = hash.wrapping_mul(5).wrapping_add(0xe654_6b64);
    }

    if !tail.is_empty() {
        let mut k = 0u32;
        for (i, byte) in tail.iter().enumerate() {
            k |= (*byte as u32) << (8 * i);
        }
        k = k.wrapping_mul(C1);
        k = k.rotate_left(15);
        k = k.wrapping_mul(C2);
        hash ^= k;
    }

    hash ^= data.len() as u32;
    hash ^= hash >> 16;
    hash = hash.wrapping_mul(0x85eb_ca6b);
    hash ^= hash >> 13;
    hash = hash.wrapping_mul(0xc2b2_ae35);
    hash ^= hash >> 16;
    hash
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Reference values from the canonical MurmurHash3 x86_32 implementation.
    #[test]
    fn murmur3_matches_reference_values() {
        assert_eq!(murmur3(0, b""), 0x0000_0000);
        assert_eq!(murmur3(0, b"foo"), 0xf6a5_c420);
        assert_eq!(murmur3(1, b"foo"), 0x34be_5f72);
        assert_eq!(murmur3(0xdead_beef, &[1, 2, 3, 4, 5]), 0x1862_f9b6);
        assert_eq!(murmur3(0xFBA4_C795, &[0xf1, 0x0d, 0x08]), 0x428e_ab3c);
    }

    #[test]
    fn inserting_sets_exactly_hash_funcs_bits() {
        let mut filter = BloomFilter::new(100, 5, 0).expect("valid filter");
        filter.insert(b"foo");
        assert_eq!(
            filter
                .as_bytes()
                .iter()
                .map(|b| b.count_ones())
                .sum::<u32>(),
            5
        );
    }

    #[test]
    fn rejects_out_of_range_parameters() {
        assert!(BloomFilter::new(0, 1, 0).is_err());
        assert!(BloomFilter::new(MAX_FILTER_BYTES + 1, 1, 0).is_err());
        assert!(BloomFilter::new(10, 0, 0).is_err());
        assert!(BloomFilter::new(10, MAX_HASH_FUNCS + 1, 0).is_err());
    }

    #[test]
    fn filter_load_round_trips_the_bits() {
        let filter =
            BloomFilter::with_elements([b"foo".as_slice()], 16, 3, 7).expect("valid filter");
        let message = filter.to_filter_load(BloomFlags::None);
        assert_eq!(message.filter, filter.as_bytes());
        assert_eq!(message.hash_funcs, 3);
        assert_eq!(message.tweak, 7);
        assert_eq!(message.flags, BloomFlags::None);
    }
}

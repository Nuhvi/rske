//! Scoring the RSK header forest against the tags of one Bitcoin window.
//!
//! A tag endorses the RSK header it names: the 20-byte prefix at its `BN`, or
//! — when no prefix matches — the header at the oldest CPV sample. A tag
//! that names nothing is dissent. A root's score is the number of tags that
//! resolve into its component (itself plus every descendant).

use std::collections::HashMap;

use crate::Tag;

/// One staged header: its number, identity hashes and parent hash (zero for a
/// root).
///
/// RSK has two hashes: the block hash (what parent links and the store key on)
/// and `hashForMergedMining` (what a Bitcoin tag commits to). They cover
/// different fields, so a tag's 20-byte prefix matches
/// [`Self::merged_mining_hash`], never [`Self::hash`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct HeaderRef {
    pub number: u64,
    /// The canonical RSK block hash; parent links use this.
    pub hash: [u8; 32],
    /// `BlockHeader.getHashForMergedMining`, the hash a tag's prefix commits to.
    pub merged_mining_hash: [u8; 32],
    /// The parent's canonical block hash (`[0; 32]` for a root).
    pub parent: [u8; 32],
    /// Least-significant byte of this header's Bitcoin block hash — the byte a
    /// CPV commits to. `None` when the header carries no decodable Bitcoin
    /// merged-mining header.
    pub bitcoin_lsb: Option<u8>,
}

/// What one root (a header without a stored parent) scored.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RootScore {
    pub root: [u8; 32],
    pub number: u64,
    /// Tags that resolve into this component.
    pub endorse: usize,
    /// Tags that resolve nowhere, or into another component.
    pub dissent: usize,
}

/// The number of bytes in a CPV.
const CPV_SIZE: usize = 7;
/// The spacing between CPV samples, in RSK blocks.
const CPV_INTERVAL: u64 = 64;
/// The CPV carries seven samples, so the oldest is six intervals below the
/// newest one's section.
const CPV_REACH: u64 = CPV_INTERVAL * 6;

/// The oldest CPV sample height for a tag at `bn`, if it has one.
///
/// The newest sample sits at `64 * floor((bn - 1) / 64)`; the bytes are newest
/// first, so the last one is `CPV_REACH` blocks below it.
pub fn oldest_checkpoint(bn: u32) -> Option<u64> {
    let bn = u64::from(bn);
    if bn <= CPV_REACH {
        return None;
    }
    let newest = CPV_INTERVAL * ((bn - 1) / CPV_INTERVAL);
    Some(newest - CPV_REACH)
}

/// An incrementally updated score over one staged forest.
///
/// Building the parent index is the expensive part of scoring; the IBD loop
/// can keep a `Scoreboard` and add tags one at a time, rebuilding only when the
/// set of headers changes. `roots()` returns the same result as [`score`].
pub struct Scoreboard {
    headers: Vec<HeaderRef>,
    by_hash: HashMap<[u8; 32], usize>,
    by_number: HashMap<u64, Vec<usize>>,
    root_of: Vec<usize>,
    roots: HashMap<usize, RootScore>,
    tag_count: usize,
}

impl Scoreboard {
    /// Build the parent index for `headers`; no tags scored yet.
    pub fn new(headers: Vec<HeaderRef>) -> Self {
        let mut by_hash: HashMap<[u8; 32], usize> = HashMap::with_capacity(headers.len());
        let mut by_number: HashMap<u64, Vec<usize>> = HashMap::new();
        for (i, header) in headers.iter().enumerate() {
            by_hash.insert(header.hash, i);
            by_number.entry(header.number).or_default().push(i);
        }

        let mut root_of = vec![usize::MAX; headers.len()];
        for i in 0..headers.len() {
            let mut at = i;
            let mut guard = 0;
            while root_of[at] == usize::MAX {
                let Some(&parent) = by_hash.get(&headers[at].parent) else {
                    root_of[at] = at;
                    break;
                };
                at = parent;
                guard += 1;
                if guard > headers.len() {
                    break;
                }
            }
            let root = if root_of[at] == usize::MAX {
                at
            } else {
                root_of[at]
            };
            let mut at = i;
            while root_of[at] == usize::MAX {
                root_of[at] = root;
                let Some(&parent) = by_hash.get(&headers[at].parent) else {
                    break;
                };
                at = parent;
            }
        }

        let mut roots: HashMap<usize, RootScore> = HashMap::new();
        for (i, header) in headers.iter().enumerate() {
            if root_of[i] == i {
                roots.insert(
                    i,
                    RootScore {
                        root: header.hash,
                        number: header.number,
                        endorse: 0,
                        dissent: 0,
                    },
                );
            }
        }

        Self {
            headers,
            by_hash,
            by_number,
            root_of,
            roots,
            tag_count: 0,
        }
    }

    /// Build a scoreboard and score every tag in order.
    pub fn with_tags(headers: Vec<HeaderRef>, tags: &[Tag]) -> Self {
        let mut board = Self::new(headers);
        for tag in tags {
            board.add_tag(tag);
        }
        board
    }

    /// Add one tag, updating the root it resolves into.
    pub fn add_tag(&mut self, tag: &Tag) {
        self.tag_count += 1;
        let Some(i) = resolve(tag, &self.headers, &self.by_hash, &self.by_number) else {
            return;
        };
        let root = self.root_of[i];
        if let Some(score) = self.roots.get_mut(&root) {
            score.endorse += 1;
        }
    }

    /// The per-root scores, best endorsement first.
    pub fn roots(&self) -> Vec<RootScore> {
        let mut out: Vec<RootScore> = self
            .roots
            .values()
            .map(|score| RootScore {
                dissent: self.tag_count - score.endorse,
                ..*score
            })
            .collect();
        out.sort_by_key(|score| std::cmp::Reverse(score.endorse));
        out
    }
}

/// Score every root in `headers` against `tags`.
pub fn score(headers: &[HeaderRef], tags: &[Tag]) -> Vec<RootScore> {
    Scoreboard::with_tags(headers.to_vec(), tags).roots()
}

/// The header a tag endorses, if any.
///
/// The strong pointer is the header at `BN` whose merged-mining hash starts
/// with the tag prefix. If that block is not staged, the tag may still commit to
/// a chain: its seven CPV bytes are the LSBs of the Bitcoin block hashes at the
/// seven CPV sample ancestors. All seven must match one candidate chain; then
/// the tag endorses the header at the oldest CPV sample (the deepest common
/// point), which is what ties an off-trunk tag back to a root.
fn resolve(
    tag: &Tag,
    headers: &[HeaderRef],
    by_hash: &HashMap<[u8; 32], usize>,
    by_number: &HashMap<u64, Vec<usize>>,
) -> Option<usize> {
    if let Some(&i) = by_number.get(&u64::from(tag.bn)).and_then(|candidates| {
        candidates
            .iter()
            .find(|&&i| headers[i].merged_mining_hash[..20] == tag.prefix)
    }) {
        return Some(i);
    }

    let oldest = oldest_checkpoint(tag.bn)?;
    let newest = oldest + CPV_REACH;
    let mut matches = Vec::new();
    for &candidate in by_number.get(&newest)? {
        if let Some(oldest_header) = cpv_matches(candidate, tag, oldest, headers, by_hash) {
            matches.push(oldest_header);
        }
    }
    matches.dedup();
    if matches.len() == 1 {
        Some(matches[0])
    } else {
        None
    }
}

/// Check that the chain from `newest` down to `oldest` commits to every CPV
/// byte of `tag`, returning the oldest header index on success.
fn cpv_matches(
    newest: usize,
    tag: &Tag,
    oldest: u64,
    headers: &[HeaderRef],
    by_hash: &HashMap<[u8; 32], usize>,
) -> Option<usize> {
    let mut at = newest;
    for (i, expected) in tag.cpv.iter().enumerate() {
        let height = oldest + (CPV_SIZE - 1 - i) as u64 * CPV_INTERVAL;
        if headers[at].number != height || headers[at].bitcoin_lsb != Some(*expected) {
            return None;
        }
        if i == CPV_SIZE - 1 {
            return Some(at);
        }
        for _ in 0..CPV_INTERVAL {
            let parent = by_hash.get(&headers[at].parent)?;
            at = *parent;
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::{HeaderRef, oldest_checkpoint, score};
    use crate::Tag;

    fn header(number: u64, byte: u8, parent: [u8; 32]) -> HeaderRef {
        HeaderRef {
            number,
            hash: [byte; 32],
            merged_mining_hash: [byte; 32],
            parent,
            bitcoin_lsb: Some(byte),
        }
    }

    /// A header whose two hashes differ, as every real RSK header's do.
    fn header_with_hashes(
        number: u64,
        hash: [u8; 32],
        merged_mining_hash: [u8; 32],
        parent: [u8; 32],
    ) -> HeaderRef {
        HeaderRef {
            number,
            hash,
            merged_mining_hash,
            parent,
            bitcoin_lsb: Some(0),
        }
    }

    fn strong(bn: u32, hash: [u8; 32]) -> Tag {
        let mut prefix = [0u8; 20];
        prefix.copy_from_slice(&hash[..20]);
        Tag {
            prefix,
            cpv: [0; 7],
            nu: 0,
            bn,
        }
    }

    #[test]
    fn splits_tags_between_roots() {
        let a0 = header(0, 0xA0, [0; 32]);
        let a1 = header(1, 0xA1, a0.hash);
        let a2 = header(2, 0xA2, a1.hash);
        let b0 = header(10, 0xB0, [0; 32]);
        let b1 = header(11, 0xB1, b0.hash);
        let headers = [a0, a1, a2, b0, b1];
        let tags = [strong(2, a2.hash), strong(0, a0.hash), strong(11, b1.hash)];

        let roots = score(&headers, &tags);
        assert_eq!(roots.len(), 2);
        assert_eq!(roots[0].number, 0);
        assert_eq!(roots[0].endorse, 2);
        assert_eq!(roots[0].dissent, 1);
        assert_eq!(roots[1].number, 10);
        assert_eq!(roots[1].endorse, 1);
    }

    #[test]
    fn unresolved_tags_count_as_dissent() {
        let a0 = header(0, 0xA0, [0; 32]);
        let tags = [strong(0, [0x99; 32])];
        let roots = score(&[a0], &tags);
        assert_eq!(roots[0].endorse, 0);
        assert_eq!(roots[0].dissent, 1);
    }

    /// A tag commits to `hashForMergedMining`, not to the block hash. A parser
    /// that compared the wrong one would see every tag as dissent on real data.
    #[test]
    fn the_strong_pointer_uses_the_merged_mining_hash() {
        let block_hash = [0xA0; 32];
        let merged_mining = [0xB0; 32];
        let a0 = header_with_hashes(0, block_hash, merged_mining, [0; 32]);

        let matching = score(&[a0], &[strong(0, merged_mining)]);
        assert_eq!(matching[0].endorse, 1);

        let wrong_hash = score(&[a0], &[strong(0, block_hash)]);
        assert_eq!(wrong_hash[0].endorse, 0);
        assert_eq!(wrong_hash[0].dissent, 1);
    }

    #[test]
    fn oldest_checkpoint_is_six_intervals_back() {
        assert_eq!(oldest_checkpoint(448), Some(0));
        assert_eq!(oldest_checkpoint(512), Some(64));
        assert_eq!(oldest_checkpoint(384), None);
    }

    /// A contiguous chain with a deterministic Bitcoin-hash byte per height.
    fn chain(n: u64) -> Vec<HeaderRef> {
        let mut out = Vec::with_capacity(n as usize);
        let mut parent = [0u8; 32];
        for number in 0..n {
            let mut hash = [0u8; 32];
            hash[..8].copy_from_slice(&(number + 1).to_be_bytes());
            let mut merged_mining_hash = [0u8; 32];
            merged_mining_hash[..8].copy_from_slice(&(number + 1).to_be_bytes());
            out.push(HeaderRef {
                number,
                hash,
                merged_mining_hash,
                parent,
                bitcoin_lsb: Some((number % 251) as u8),
            });
            parent = hash;
        }
        out
    }

    fn soft_tag(headers: &[HeaderRef], bn: u32) -> Tag {
        let oldest = oldest_checkpoint(bn).unwrap();
        let newest = oldest + 384;
        let mut cpv = [0u8; 7];
        for (i, slot) in cpv.iter_mut().enumerate() {
            *slot = headers[(newest - i as u64 * 64) as usize]
                .bitcoin_lsb
                .unwrap();
        }
        Tag {
            prefix: [0xEE; 20],
            cpv,
            nu: 0,
            bn,
        }
    }

    #[test]
    fn a_soft_tag_lands_on_its_oldest_checkpoint() {
        let headers = chain(449);
        let roots = score(&headers, &[soft_tag(&headers, 448)]);
        assert_eq!(roots[0].number, 0);
        assert_eq!(roots[0].endorse, 1);
    }

    #[test]
    fn a_soft_tag_must_match_all_seven_cpv_bytes() {
        let headers = chain(449);
        let mut tag = soft_tag(&headers, 448);
        tag.cpv[3] ^= 0xFF;
        let roots = score(&headers, &[tag]);
        assert_eq!(roots[0].endorse, 0);
        assert_eq!(roots[0].dissent, 1);
    }

    #[test]
    fn cpv_selects_the_matching_fork() {
        let mut headers = chain(449);
        let mut fork = chain(449);
        for (i, header) in fork.iter_mut().enumerate() {
            header.hash[0] ^= 0x55;
            header.bitcoin_lsb = Some(((i as u64 + 100) % 251) as u8);
        }
        for i in 1..fork.len() {
            fork[i].parent = fork[i - 1].hash;
        }
        let tag = soft_tag(&headers, 448);
        headers.extend(fork);

        let roots = score(&headers, &[tag]);
        // Two components exist; the tag's CPV selects the first chain.
        assert_eq!(roots.len(), 2);
        assert_eq!(roots[0].endorse, 1);
        assert_eq!(roots[1].endorse, 0);
    }

    #[test]
    fn a_soft_tag_with_a_missing_checkpoint_is_unresolved() {
        let mut headers = chain(449);
        headers.remove(128);
        let roots = score(&headers, &[soft_tag(&chain(449), 448)]);
        assert_eq!(roots[0].endorse, 0);
    }
}

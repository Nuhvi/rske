//! Executable specification for `specs/rootstock-checkpoints.md`.
//!
//! Requirement IDs in comments (`R1`, `R2`, ...) point at clauses in that
//! document. The Markdown is the human source; this file is the executable
//! form that is expected to fail when a clause is broken.

use std::collections::{HashMap, HashSet};
use std::sync::OnceLock;

use alloy_primitives::{B256, U256};
use alloy_rlp::Encodable as _;
use bitcoin::Network;
use bitcoin::TxMerkleNode;
use bitcoin::block::{BlockHash, Header as BitcoinHeader, Version};
use bitcoin::consensus::Encodable as _;
use bitcoin::hashes::Hash as _;
use bitcoin::pow::CompactTarget;
use proptest::prelude::*;
use proptest_state_machine::{ReferenceStateMachine, StateMachineTest, prop_state_machine};
use rsk_consensus::types::header::Header as RskHeader;

use crate::{Action, Config, Core, EphemeralChainCheckpoint, Outcome, RskEntry};

const EASY_BITS: u32 = 0x207f_ffff;
const BASE_TIME: u32 = 1_600_000_000;

fn mine(prev: BlockHash, time: u32, tag: u32) -> BitcoinHeader {
    let target = CompactTarget::from_consensus(EASY_BITS);
    let mut nonce = tag;
    loop {
        let header = BitcoinHeader {
            version: Version::ONE,
            prev_blockhash: prev,
            merkle_root: TxMerkleNode::all_zeros(),
            time,
            bits: target,
            nonce,
        };
        if header.validate_pow(header.target()).is_ok() {
            return header;
        }
        nonce = nonce.wrapping_add(1);
    }
}

// ── 3. Rootstock trust policy over a finalized Bitcoin tail ─────────────────

const TAIL_START: u64 = 2016;

/// A regtest chain with a finalized tail `[2016, 3932]`: spine at 0 and 2016,
/// then easy headers up to 4032.
fn btc_with_tail() -> Core {
    let g0 = mine(BlockHash::all_zeros(), BASE_TIME, 0);
    let g2016 = mine(g0.block_hash(), BASE_TIME + 1, 1);
    let core =
        Core::new(Config::in_memory(Network::Regtest).with_anchor(&[(0, g0), (2016, g2016)]))
            .expect("in-memory store");

    let mut cur = g2016;
    let mut run = Vec::new();
    for i in 0..2016u32 {
        cur = mine(cur.block_hash(), BASE_TIME + 2 + i, 0);
        run.push(cur);
    }
    core.connect_headers(&run, u64::from(BASE_TIME) + 1 + 1_200_000)
        .expect("connect");
    core
}

struct RskFixtureEntry {
    header: RskHeader,
    raw: Vec<u8>,
    anchor: BlockHash,
    difficulty: u64,
}

impl RskFixtureEntry {
    fn hash(&self) -> B256 {
        self.header.hash()
    }

    fn to_entry(&self) -> RskEntry {
        RskEntry {
            header: self.header.clone(),
            raw: self.raw.clone(),
            uncles: Vec::new(),
        }
    }
}

fn rsk_entry(
    number: u64,
    parent: B256,
    anchor: BlockHash,
    difficulty: u64,
    tag: u32,
) -> RskFixtureEntry {
    let solution = BitcoinHeader {
        version: Version::ONE,
        prev_blockhash: anchor,
        merkle_root: TxMerkleNode::all_zeros(),
        time: BASE_TIME,
        bits: CompactTarget::from_consensus(EASY_BITS),
        nonce: tag,
    };
    let mut mm = Vec::new();
    solution
        .consensus_encode(&mut mm)
        .expect("encode merge-mining solution");
    let header = RskHeader {
        parent_hash: parent,
        ommers_hash: B256::ZERO,
        beneficiary: Default::default(),
        state_root: B256::ZERO,
        transactions_root: B256::ZERO,
        receipts_root: B256::ZERO,
        logs_bloom: Default::default(),
        extension_data: None,
        difficulty: U256::from(difficulty),
        number,
        gas_limit: U256::from(10_000_000u64),
        gas_used: 0,
        timestamp: u64::from(BASE_TIME) + number + u64::from(tag),
        extra_data: Default::default(),
        paid_fees: U256::ZERO,
        minimum_gas_price: U256::from(1),
        uncle_count: 0,
        umm_root: None,
        bitcoin_merged_mining_header: Some(mm.into()),
        bitcoin_merged_mining_merkle_proof: None,
        bitcoin_merged_mining_coinbase_transaction: None,
        cached_hash: None,
        cached_hash_for_merged_mining: None,
    };
    let mut raw = Vec::new();
    header.encode(&mut raw);
    RskFixtureEntry {
        header,
        raw,
        anchor,
        difficulty,
    }
}

/// What the model needs to know about one proposable RSK header.
#[derive(Clone, Debug)]
struct RskEntryInfo {
    hash: [u8; 32],
    parent: [u8; 32],
    number: u64,
    difficulty: u64,
    /// Whether the merge-mining anchor is canonical and inside the finalized
    /// Bitcoin tail: exactly what `propose_checkpoint` accepts.
    anchor_ok: bool,
}

struct RskFixture {
    core: Core,
    entries: Vec<RskFixtureEntry>,
    info: Vec<RskEntryInfo>,
    tail_work: U256,
    tail_start: u64,
}

fn rsk_fixture() -> &'static RskFixture {
    static FIXTURE: OnceLock<RskFixture> = OnceLock::new();
    FIXTURE.get_or_init(|| {
        let core = btc_with_tail();
        let mark = core.finalized_height().expect("read").expect("seeded");
        let tail_start = core
            .finalized_checkpoint()
            .expect("read")
            .expect("a finalized period");
        assert_eq!(tail_start, TAIL_START);

        let header = |height| {
            core.header_at(height)
                .expect("read")
                .unwrap_or_else(|| panic!("no canonical header at {height}"))
                .block_hash()
        };
        let h2016 = header(2016);
        let h2500 = header(2500);
        let h3000 = header(3000);
        let h_mark = header(mark);
        let h4000 = header(4000);

        let a = rsk_entry(0, B256::ZERO, h2016, 2_000, 0);
        let b = rsk_entry(1, a.hash(), h2500, 1_000, 1);
        let c = rsk_entry(2, b.hash(), h3000, 500, 2);
        let d = rsk_entry(0, B256::ZERO, h_mark, 3_000, 3);
        let e = rsk_entry(0, B256::ZERO, h4000, 10, 4);
        let f = rsk_entry(0, B256::ZERO, BlockHash::all_zeros(), 10, 5);
        let g = rsk_entry(0, B256::ZERO, BlockHash::from_byte_array([0x77; 32]), 10, 6);
        let entries = vec![a, b, c, d, e, f, g];

        let info = entries
            .iter()
            .map(|entry| {
                let anchor_height = core.height_of(&entry.anchor).expect("read");
                RskEntryInfo {
                    hash: entry.hash().0,
                    parent: entry.header.parent_hash.0,
                    number: entry.header.number,
                    difficulty: entry.difficulty,
                    anchor_ok: anchor_height
                        .is_some_and(|height| (tail_start..=mark).contains(&height)),
                }
            })
            .collect();

        let tail_work = U256::from_be_slice(
            &core
                .btc_tail_work(tail_start)
                .expect("read")
                .expect("a full tail period")
                .to_be_bytes(),
        );

        RskFixture {
            core,
            entries,
            info,
            tail_work,
            tail_start,
        }
    })
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct RskCheckpoint {
    hash: [u8; 32],
    number: u64,
    work: U256,
}

#[derive(Clone, Debug, PartialEq, Eq)]
enum RskPrediction {
    Noop,
    Proposed,
    Refused,
    Attempt(Option<RskCheckpoint>),
    Synced,
}

#[derive(Clone, Debug)]
struct RskModel {
    info: Vec<RskEntryInfo>,
    stored: HashSet<[u8; 32]>,
    roots: HashSet<[u8; 32]>,
    /// Parent -> children, but only edges created while the parent was already
    /// stored. `rsk_put_entry` adds the edge at insert time, so proposing a
    /// child before its parent leaves it unlinked; the model must do the same.
    children: HashMap<[u8; 32], Vec<[u8; 32]>>,
    sync: Option<[u8; 32]>,
    tail_work: U256,
    last: RskPrediction,
}

impl RskModel {
    fn sorted_roots(&self) -> Vec<[u8; 32]> {
        let mut roots: Vec<_> = self.roots.iter().copied().collect();
        roots.sort_unstable();
        roots
    }

    // R6: subtree work is the root plus every stored descendant.
    fn subtree_work(&self, root: [u8; 32]) -> U256 {
        let mut total = U256::ZERO;
        let mut stack = vec![root];
        let mut seen = HashSet::new();
        while let Some(hash) = stack.pop() {
            if !seen.insert(hash) {
                continue;
            }
            if let Some(entry) = self.info.iter().find(|entry| entry.hash == hash) {
                total = total.saturating_add(U256::from(entry.difficulty));
            }
            if let Some(children) = self.children.get(&hash) {
                stack.extend(children.iter().copied());
            }
        }
        total
    }

    // R4/R5: strict 2/3, then the heaviest clearing root.
    fn clearing_root(&self) -> Option<RskCheckpoint> {
        let mut best: Option<RskCheckpoint> = None;
        for root in self.sorted_roots() {
            let work = self.subtree_work(root);
            if work * U256::from(3u64) <= self.tail_work * U256::from(2u64) {
                continue;
            }
            if best.as_ref().is_none_or(|best| work > best.work) {
                let entry = self
                    .info
                    .iter()
                    .find(|entry| entry.hash == root)
                    .expect("roots are known headers");
                best = Some(RskCheckpoint {
                    hash: root,
                    number: entry.number,
                    work,
                });
            }
        }
        best
    }

    fn heaviest_root(&self) -> Option<RskCheckpoint> {
        let mut best: Option<RskCheckpoint> = None;
        for root in self.sorted_roots() {
            let work = self.subtree_work(root);
            if best.as_ref().is_none_or(|best| work > best.work) {
                let entry = self
                    .info
                    .iter()
                    .find(|entry| entry.hash == root)
                    .expect("roots are known headers");
                best = Some(RskCheckpoint {
                    hash: root,
                    number: entry.number,
                    work,
                });
            }
        }
        best
    }

    fn resolve_sync(&self) -> Option<([u8; 32], u64)> {
        let hash = self.sync?;
        let entry = self
            .info
            .iter()
            .find(|entry| entry.hash == hash && self.stored.contains(&hash))?;
        Some((hash, entry.number))
    }
}

#[derive(Clone, Debug)]
enum RskTransition {
    Propose(usize),
    Attempt,
    Sync(usize),
}

struct RskMachine;

impl ReferenceStateMachine for RskMachine {
    type State = RskModel;
    type Transition = RskTransition;

    fn init_state() -> BoxedStrategy<Self::State> {
        let fixture = rsk_fixture();
        Just(RskModel {
            info: fixture.info.clone(),
            stored: HashSet::new(),
            roots: HashSet::new(),
            children: HashMap::new(),
            sync: None,
            tail_work: fixture.tail_work,
            last: RskPrediction::Noop,
        })
        .boxed()
    }

    fn transitions(state: &Self::State) -> BoxedStrategy<Self::Transition> {
        let count = state.info.len();
        prop_oneof![
            (0..count).prop_map(RskTransition::Propose),
            Just(RskTransition::Attempt),
            (0..count).prop_map(RskTransition::Sync),
        ]
        .boxed()
    }

    fn apply(mut state: Self::State, transition: &Self::Transition) -> Self::State {
        match transition {
            // R1/R3: anchor acceptance and storing the root.
            RskTransition::Propose(index) => {
                let entry = &state.info[*index];
                if entry.anchor_ok {
                    if state.stored.contains(&entry.parent) {
                        let children = state.children.entry(entry.parent).or_default();
                        if !children.contains(&entry.hash) {
                            children.push(entry.hash);
                        }
                    }
                    state.stored.insert(entry.hash);
                    state.roots.insert(entry.hash);
                    state.last = RskPrediction::Proposed;
                } else {
                    state.last = RskPrediction::Refused;
                }
            }
            // R4: predict the attempt from the model's forest.
            RskTransition::Attempt => {
                let checkpoint = state.clearing_root();
                state.last = RskPrediction::Attempt(checkpoint);
            }
            RskTransition::Sync(index) => {
                state.sync = Some(state.info[*index].hash);
                state.last = RskPrediction::Synced;
            }
        }
        state
    }
}

struct RskSut {
    core: &'static Core,
}

impl StateMachineTest for RskMachine {
    type SystemUnderTest = RskSut;
    type Reference = RskMachine;

    fn init_test(_model: &RskModel) -> RskSut {
        let fixture = rsk_fixture();
        // One expensive Bitcoin fixture per process; each generated case starts
        // from an empty Rootstock forest.
        fixture.core.reset_rsk().expect("reset rsk tables");
        RskSut {
            core: &fixture.core,
        }
    }

    fn apply(sut: RskSut, model: &RskModel, transition: RskTransition) -> RskSut {
        let fixture = rsk_fixture();
        let before = sut.core.state().expect("state");
        let outcome = match &transition {
            RskTransition::Propose(index) => {
                let entry = fixture.entries[*index].to_entry();
                sut.core.apply(Action::ProposeCheckpoint { entry: &entry })
            }
            RskTransition::Attempt => sut.core.apply(Action::ConnectRskHeaders { entries: &[] }),
            RskTransition::Sync(index) => sut.core.apply(Action::SetRskSyncTip {
                hash: B256::from(fixture.entries[*index].hash()),
            }),
        };
        let after = sut.core.state().expect("state");

        match (&model.last, &transition, &outcome) {
            (
                RskPrediction::Proposed,
                RskTransition::Propose(index),
                Ok(Outcome::CheckpointProposed(hash)),
            ) => {
                assert_eq!(hash.0, fixture.info[*index].hash, "proposed the wrong root");
                assert!(after.rootstock.forest.contains(&hash.0));
                assert!(after.rootstock.roots.contains(&hash.0));
            }
            // R2: refusal is atomic.
            (RskPrediction::Refused, RskTransition::Propose(_), Err(_)) => {
                assert_eq!(before, after, "a refused proposal changed the forest");
            }
            (
                RskPrediction::Attempt(None),
                RskTransition::Attempt,
                Ok(Outcome::RskConnected(outcome)),
            ) => {
                // R8: an attempt is read-only.
                assert_eq!(outcome.connected, 0);
                assert!(outcome.checkpoint.is_none(), "model expected no checkpoint");
                assert_eq!(before, after, "an attempt mutated state");
            }
            (
                RskPrediction::Attempt(Some(expected)),
                RskTransition::Attempt,
                Ok(Outcome::RskConnected(outcome)),
            ) => {
                assert_eq!(outcome.connected, 0);
                assert_eq!(
                    outcome.checkpoint,
                    Some(EphemeralChainCheckpoint {
                        hash: B256::from(expected.hash),
                        number: expected.number,
                        work: expected.work,
                        btc_tail_work: fixture.tail_work,
                        tail_start: fixture.tail_start,
                    })
                );
                assert_eq!(before, after, "an attempt mutated state");
            }
            (RskPrediction::Synced, RskTransition::Sync(index), Ok(Outcome::SyncTipSet)) => {
                assert_eq!(
                    after.rootstock.sync_tip,
                    Some(fixture.info[*index].hash),
                    "sync tip not written"
                );
            }
            (expected, transition, other) => {
                panic!("model expected {expected:?} for {transition:?}, core returned {other:?}")
            }
        }

        sut
    }

    fn check_invariants(sut: &RskSut, model: &RskModel) {
        let state = sut.core.state().expect("state");

        // R3: forest and root set are exact.
        let mut forest: Vec<_> = model.stored.iter().copied().collect();
        forest.sort_unstable();
        assert_eq!(state.rootstock.forest, forest, "stored forest diverged");

        let mut roots: Vec<_> = model.roots.iter().copied().collect();
        roots.sort_unstable();
        assert_eq!(state.rootstock.roots, roots, "root set diverged");

        // R7: the sync tip is recorded and resolves iff stored.
        assert_eq!(state.rootstock.sync_tip, model.sync);
        assert_eq!(state.rootstock.sync_tip_header, model.resolve_sync());

        // R6: GHOST work agrees with the store.
        for root in &state.rootstock.roots {
            let entry = model
                .info
                .iter()
                .find(|entry| entry.hash == *root)
                .expect("roots are known headers");
            assert!(entry.anchor_ok, "a root's anchor is outside the tail");
            assert_eq!(
                sut.core.rsk_subtree_work(&B256::from(*root)).expect("read"),
                model.subtree_work(*root),
                "GHOST work diverged for {root:?}"
            );
        }

        // GHOST: a parent's subtree strictly outweighs each child's.
        for (parent, children) in &model.children {
            for child in children {
                assert!(
                    model.subtree_work(*parent) > model.subtree_work(*child),
                    "parent weight does not exceed the child"
                );
            }
        }

        assert_eq!(
            sut.core
                .rsk_best_checkpoint()
                .expect("read")
                .map(|(hash, work)| (hash.0, work)),
            model.heaviest_root().map(|root| (root.hash, root.work)),
            "the heaviest root diverged"
        );
    }
}

prop_state_machine! {
    #![proptest_config(ProptestConfig {
        cases: 48,
        .. ProptestConfig::default()
    })]

    /// Proposals, checkpoint attempts and sync-tip writes over a real finalized
    /// Bitcoin tail must leave the forest, roots and sync tip exactly as the
    /// model predicts.
    #[test]
    fn rsk_checkpoint_policy_matches_the_spec(sequential 1..24 => RskMachine);
}

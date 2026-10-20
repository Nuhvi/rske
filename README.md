# RSK One

A persistent Rootstock (RSK) light node that syncs Bitcoin and Rootstock
headers to disk, and whose stated goal is to download and verify RSK block
headers **faster than rskj or rustock** — by pulling headers over the RSK P2P
network with a parallel skeleton walk, anchored to Bitcoin via merge mining.

## Project Structure

```
crates/
  rsk-core/      — side-effect-free Bitcoin + Rootstock chain state (redb): checkpoint spine, fork tree, RSK header forest, checkpoint rules
  rsk/           — the RSK One node as a library: Bitcoin header sync, peer pool, checkpoint export
  cli/           — rsk-cli: a thin command-line wrapper over the `rsk` library
  btc-p2p/       — Bitcoin P2P protocol client: framed transport, version/verack handshake, headers
  rsk-consensus/ — vendored rustock consensus: Header, HeaderVerifier, ChainConfig, checkpoint (88 tests)
  rsk-p2p/       — vendored rustock RLPx networking + rsk subprotocol + discovery (80 tests)
```

## Usage

```bash
# sync Bitcoin headers into ./data/rsk-one/chain.redb (validates every header)
cargo run -p rsk-cli -- sync

# write the store's finalized period boundaries as a checkpoints.txt
cargo run -p rsk-cli -- export-checkpoints > checkpoints.txt
```

See `cargo run -p rsk-cli -- <command> --help` for the flags of each command.

## How it works

### Syncing Bitcoin headers first

First, we sync Bitcoin headers like any SPV wallet.

Second, because our Rootstock checkpoints are derived from evidence on Bitcoin,
the node must first decide which Bitcoin blocks it can treat as practically
impossible to reorg. The simplest definition of a final block is a block with 100
confirmations. However, waiting to find the best chain from your peers slows this
finalization down, which delays the next steps — and it assumes no network
partitioning attack.

The solution is to estimate the cumulative work expected since the latest trusted 
final block, and treat a block as **final** once the work above the previous mark passes 
that bar. A **checkpoint** is the newest difficulty-period start at or below the mark. 
The bar is the larger of:

- 100 blocks' worth of work at the current final block's difficulty
- the cumulative work we estimate the network would have done since that block.


Creating a bitcoin non-canonical fork that passes that bar is assumed to cost 100s of millions
of dollars worth energy consumption and opportunity cost.

See [Partition-safe IBD](https://github.com/Nuhvi/partition-safe-ibd) for the derivation 
and backtest of the estimation of cumulative, and cost calculations. 

## Acknowledgment

Reuses consensus and P2P code from [rustock](https://github.com/SergioDemianLerner/rustock) (MIT).

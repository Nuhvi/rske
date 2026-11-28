#!/usr/bin/env bash
# The verification gate for this workspace.
#
#   scripts/verify.sh          fast gate: format, lints, tests, specs
#   scripts/verify.sh --full   adds the Kani proofs
#
# Run it from the repository root. It stops at the first failing step and names
# that step. CI runs the same steps in `.github/workflows/ci.yml`.

set -euo pipefail

full=0
case "${1:-}" in
  "") ;;
  --full) full=1 ;;
  *)
    echo "usage: scripts/verify.sh [--full]" >&2
    exit 2
    ;;
esac

step() {
  printf '\n== %s\n' "$1"
}

step "fmt"
cargo fmt --all --check

step "clippy (workspace, warnings reported)"
cargo clippy --workspace --all-targets

step "clippy (our crates, warnings denied)"
cargo clippy -p rsk-core -p rsk --all-targets -- -D warnings

step "tests"
if cargo nextest --version >/dev/null 2>&1; then
  cargo nextest run --workspace
  cargo test --workspace --doc
else
  echo "nextest not installed; falling back to cargo test" >&2
  echo "install it with: cargo install cargo-nextest --locked" >&2
  cargo test --workspace --all-targets
fi

step "specification structure"
if ! command -v openspec >/dev/null 2>&1; then
  echo "openspec is not installed: npm install --global @fission-ai/openspec" >&2
  exit 1
fi
openspec validate --all --strict

if ((full)); then
  step "kani proofs"
  if ! cargo kani --version >/dev/null 2>&1; then
    echo "kani is not installed: cargo install --locked kani-verifier && cargo kani setup" >&2
    exit 1
  fi
  cargo kani
fi

printf '\nok\n'

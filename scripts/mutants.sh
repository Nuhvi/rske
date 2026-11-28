#!/usr/bin/env bash
# Diff-scoped mutation check. Reproduces the CI `mutants` job locally.
#
#   scripts/mutants.sh              list the mutants in the diff
#   scripts/mutants.sh --run        run them all
#   scripts/mutants.sh --run --shard 0/4   run one quarter
#
# A run takes about one minute for each mutant. The survivors are in
# `mutants.out/missed.txt`. Confirm each survivor at the full sample before you
# record it: unset RSK_PROPTEST_CASES.

set -euo pipefail

base=master
files=()
filter=
run=0
shard=

usage() {
  cat <<'EOF'
usage: scripts/mutants.sh [--run] [--base REF] [--shard I/N] [--file PATH] [--filter TEXT]

  (no --run)     list the mutants in the diff
  --run          run them; about one minute for each mutant
  --base REF     branch to diff against (default: master)
  --shard I/N    run one shard; cargo-mutants counts from zero, so I < N
  --file PATH    restrict to one source file; repeat the flag for more
  --filter TEXT  test filter after --lib; a fast loop only
EOF
}

while (($#)); do
  case "$1" in
    --base) base=${2:?}; shift 2 ;;
    --file) files+=("$2"); shift 2 ;;
    --filter) filter=${2:?}; shift 2 ;;
    --shard) shard=${2:?}; shift 2 ;;
    --run) run=1; shift ;;
    -h|--help) usage; exit 0 ;;
    *) echo "unknown argument: $1" >&2; usage >&2; exit 2 ;;
  esac
done

cd "$(git rev-parse --show-toplevel)"

if ! git rev-parse --verify -q "origin/$base" >/dev/null; then
  echo "missing origin/$base; run: git fetch origin $base" >&2
  exit 1
fi

tmp=$(mktemp -d)
trap 'rm -rf "$tmp"' EXIT

# Three dots: diff from the merge base, the same command that CI runs.
git diff "$(git merge-base "origin/$base" HEAD)" >"$tmp/pr.diff"
echo "== diff against origin/$base: $(wc -l <"$tmp/pr.diff" | tr -d ' ') lines"

args=(--in-diff "$tmp/pr.diff" -p rsk-core)
if ((${#files[@]})); then
  for file in "${files[@]}"; do
    args+=(-f "$file")
  done
fi
if [[ -n $shard ]]; then
  args+=(--shard "$shard")
fi

if ((!run)); then
  cargo mutants --list "${args[@]}"
  echo
  echo "== pass --run to start. Count the lines above for the run time in minutes."
  exit 0
fi

test_args=(--lib)
if [[ -n $filter ]]; then
  test_args+=("$filter")
fi

echo "== mutation run at RSK_PROPTEST_CASES=${RSK_PROPTEST_CASES:-16}"
RSK_PROPTEST_CASES=${RSK_PROPTEST_CASES:-16} \
  cargo mutants "${args[@]}" -- "${test_args[@]}"

if [[ -s mutants.out/missed.txt ]]; then
  echo
  echo "== survivors"
  cat mutants.out/missed.txt
fi

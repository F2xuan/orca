#!/usr/bin/env bash
# Generate gui/src/lib/generated/*.ts from the Rust wire types.
#
# The daemon's JSON is the contract between the two sides, so the Rust
# definitions are the source and the TypeScript is derived. Run this after
# changing any type that carries a ts-rs derive, and commit the result.
#
#   scripts/codegen-types.sh          regenerate
#   scripts/codegen-types.sh --check  fail if the checked-in files are stale
#
# `TS_RS_EXPORT_DIR` is set explicitly rather than using `#[ts(export_to = ...)]`:
# that attribute resolved to somewhere other than intended (it wrote into
# crates/gui/), so the output directory is pinned here instead.
set -euo pipefail

REPO="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
OUT="$REPO/gui/src/lib/generated"

generate() {
  local target="$1" log
  log="$(mktemp)"
  rm -rf "$target"
  mkdir -p "$target"

  # `TS_RS_LARGE_INT=number`: ts-rs renders i64/u64 as `bigint` by default, but
  # JSON numbers arrive in JavaScript as `number`, so `bigint` describes a value
  # the runtime never produces — `tsc` rejects the existing arithmetic on it.
  # (Fourth type-vs-wire difference; §32.)
  if ! (cd "$REPO" && TS_RS_EXPORT_DIR="$target" TS_RS_LARGE_INT=number \
    cargo test -p orca-core --features ts-export --quiet export_bindings) >"$log" 2>&1; then
    cat "$log"
    return 1
  fi
  # ts-rs warns and generates anyway when it cannot model an attribute.
  python3 "$REPO/scripts/check-ts-unmodelled-attrs.py" <"$log" || { rm -f "$log"; return 1; }
  rm -f "$log"
}

# ts-rs models the Rust *type*, not the serde *wire format*, and the two differ
# here in two directions. A drift check cannot catch a generator that is wrong
# the same way every time, so assert the output directly. Both rules below were
# learned the hard way — see §30 and §31 of komodo-borrowings-triage.md.
assert_shape() {
  local dir="$1" bad=0

  # `#[serde(other)]` constrains *deserialisation* only. A daemon that reads a
  # record written by a newer build gets the catch-all variant and re-serialises
  # it as "unknown", so the arm is reachable on the wire and MUST be present.
  # (An earlier revision of this script asserted the opposite, from a comment in
  # types.ts that claimed the daemon "cannot emit it". It can.)
  grep -q '"kind": "unknown"' "$dir/AlertData.ts" || { echo "  AlertData lost its 'unknown' arm; #[serde(other)] is on the wire (§31)"; bad=1; }
  grep -q '"unknown"' "$dir/OperationKind.ts" || { echo "  OperationKind lost its 'unknown' arm"; bad=1; }

  # `skip_serializing_if = "Option::is_none"` means the key is *absent*, not
  # null; ts-rs maps Option<T> to `T | null` unless told otherwise.
  grep -q 'error?: string | null' "$dir/AlertData.ts" && { echo "  error is 'string | null', but skip_serializing_if omits the key"; bad=1; }
  grep -q 'resolved_ts?: string | null' "$dir/Alert.ts" && { echo "  resolved_ts is 'string | null', but skip_serializing_if omits the key"; bad=1; }
  grep -q 'finished_at?: string | null' "$dir/OperationRecord.ts" && { echo "  finished_at is 'string | null', but skip_serializing_if omits the key"; bad=1; }
  grep -q 'kind": "host_disk_high"' "$dir/AlertData.ts" || { echo "  AlertData lost its host_disk_high arm"; bad=1; }

  # A `bigint` here means TS_RS_LARGE_INT was lost, and the binding would then
  # claim a type JSON cannot produce.
  grep -l 'bigint' "$dir"/*.ts >/dev/null 2>&1 && { echo "  a generated file uses 'bigint'; JSON numbers are 'number' (§32)"; bad=1; }
  return $bad
}

if [[ "${1:-}" == "--check" ]]; then
  tmp="$(mktemp -d)"
  trap 'rm -rf "$tmp"' EXIT
  generate "$tmp"
  assert_shape "$tmp" || exit 1
  if ! diff -rq "$tmp" "$OUT" >/dev/null 2>&1; then
    echo "generated types are STALE — run scripts/codegen-types.sh"
    diff -ru "$OUT" "$tmp" | head -40
    exit 1
  fi
  echo "ok: gui/src/lib/generated matches the Rust types"
else
  generate "$OUT"
  assert_shape "$OUT" || exit 1
  echo "regenerated $(ls "$OUT" | wc -l | tr -d ' ') file(s) in gui/src/lib/generated"
fi

#!/usr/bin/env bash
#
# Regenerates the committed interfaces for both Rust chains.
#
#   ./scripts/generate_abis.sh
#
# An interface describes code, not a deployment, so one set covers every network running
# the same build — which is also why this is a build step and not part of deploying.
#
# With a network argument it additionally checks each Soroban file against the live
# contract, which is the only way to know the committed interface matches what is actually
# deployed:
#
#   ./scripts/generate_abis.sh testnet

set -euo pipefail

NETWORK="${1:-}"
ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
WASM="$ROOT/target/wasm32v1-none/release"

mkdir -p "$ROOT/abi/soroban" "$ROOT/abi/solana"

# ------------------------------------------------------------ Soroban
#
# `stellar contract info interface --output json` emits the contract's SCSpecEntry set:
# every function, type and error code, which is what a client needs to encode a call.

echo "Building Soroban contracts..."
(cd "$ROOT" && cargo build --target wasm32v1-none --release --quiet)

echo "Soroban interfaces:"
for w in "$WASM"/*.wasm; do
  n="$(basename "$w" .wasm)"
  stellar contract info interface --wasm "$w" --output json-formatted \
    > "$ROOT/abi/soroban/$n.json"
  printf "  %-24s %7d bytes\n" "$n.json" "$(wc -c < "$ROOT/abi/soroban/$n.json")"
done

# ------------------------------------------------------------ Solana
#
# `anchor idl build` needs a workspace, which solana/Anchor.toml exists to provide — the
# programs sit outside the Rust workspace because cargo-build-sbf manages its own
# toolchain. Each manifest's `idl-build` feature must include anchor-spl's, or the SPL
# account types in these contexts have no IDL traits and the emit fails.

echo "Solana IDLs:"
# `-p` takes the lib name, not the package name: piron_locked_solana, not
# piron-locked-solana. Passing the package name lists the available programs and exits 0,
# so this is checked by the instruction count below rather than by the exit status.
for p in single_asset locked stable_yield; do
  out="$ROOT/abi/solana/piron_${p}_solana.json"
  (cd "$ROOT/solana" && anchor idl build -p "piron_${p}_solana" > "$out" 2>/dev/null) \
    || { echo "  FAILED: $p" >&2; exit 1; }
  printf "  %-34s %7d bytes  instructions=%d\n" "$(basename "$out")" \
    "$(wc -c < "$out")" \
    "$(python3 -c 'import json,sys; print(len(json.load(open(sys.argv[1]))["instructions"]))' "$out")"
done

# ------------------------------------------------------------ verify against chain

if [[ -n "$NETWORK" ]]; then
  ENV_FILE="$ROOT/deployments/soroban-$NETWORK.env"
  if [[ ! -f "$ENV_FILE" ]]; then
    echo "No deployment record for $NETWORK; skipping the on-chain check." >&2
    exit 0
  fi
  # shellcheck disable=SC1090
  set -a; source "$ENV_FILE"; set +a

  echo "Checking committed interfaces against $NETWORK..."
  tmp="$(mktemp -d)"
  trap 'rm -rf "$tmp"' EXIT
  fail=0
  check() {
    local name="$1" id="$2"
    [[ -z "$id" ]] && return 0
    if ! stellar contract info interface --contract-id "$id" --network "$NETWORK" \
        --output json-formatted > "$tmp/$name.json" 2>/dev/null; then
      printf "  UNREAD    %s (%s)\n" "$name" "$id"; fail=1; return 0
    fi
    if diff -q "$tmp/$name.json" "$ROOT/abi/soroban/$name.json" >/dev/null; then
      printf "  IDENTICAL %s\n" "$name"
    else
      printf "  DIFFERS   %s — the deployed contract is not this build\n" "$name"; fail=1
    fi
  }
  check access_manager   "${PIRON_ACCESS_MANAGER:-}"
  check pool_registry    "${PIRON_POOL_REGISTRY:-}"
  check fee_manager      "${PIRON_FEE_MANAGER:-}"
  check pool_factory     "${PIRON_POOL_FACTORY:-}"
  check upgrade_timelock "${PIRON_UPGRADE_TIMELOCK:-}"
  check yield_reserve    "${PIRON_YIELD_RESERVE:-}"

  # Pools, if any exist. Each is its own contract instance, so each is checked.
  POOLS="$ROOT/deployments/soroban-$NETWORK-pools.env"
  if [[ -f "$POOLS" ]]; then
    # shellcheck disable=SC1090
    set -a; source "$POOLS"; set +a
    # The digit matters: these are PIRON_LOCKED_2_MANAGER, not PIRON_LOCKED_MANAGER.
    for v in $(grep -o '^PIRON_[A-Z0-9_]*_MANAGER' "$POOLS" || true); do
      id="${!v}"
      case "$v" in
        PIRON_SINGLE_ASSET_*) check single_asset_manager "$id" ;;
        PIRON_LOCKED_*)       check locked_pool_manager  "$id" ;;
        PIRON_STABLE_YIELD_*) check stable_yield_manager "$id" ;;
      esac
    done
  fi

  if [[ $fail -ne 0 ]]; then
    echo "At least one interface does not match the chain." >&2
    exit 1
  fi
  echo "Every committed interface matches $NETWORK."
fi

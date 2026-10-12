#!/usr/bin/env bash
#
# Deploys the three Piron programs to a Solana cluster and hands their upgrade authority
# to a Squads multisig.
#
#   PIRON_UPGRADE_AUTHORITY=<squads-vault-pubkey> ./scripts/deploy_solana.sh devnet
#
# Programs only. Pools are PDAs created by `initialize_pool`, which an admin calls when
# they want one — the same shape as the EVM side, where the admin's wallet submits the
# factory transaction and the backend confirms it from the receipt. Nothing here creates
# a pool, so nothing here needs a pool's terms.
#
# Writes deployments/solana-<cluster>.env, in the key=value form the Stellar script uses,
# so the backend's address registry can consume both the same way.

set -euo pipefail

CLUSTER="${1:-devnet}"
ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
SO="$ROOT/solana"
OUT="$ROOT/deployments/solana-$CLUSTER.env"
KEYPAIR="${PIRON_DEPLOYER_KEYPAIR:-$HOME/.config/solana/id.json}"

PROGRAMS=(single-asset locked stable-yield)

case "$CLUSTER" in
  mainnet|mainnet-beta) RPC="https://api.mainnet-beta.solana.com"; CLUSTER="mainnet-beta" ;;
  devnet)               RPC="https://api.devnet.solana.com" ;;
  testnet)              RPC="https://api.testnet.solana.com" ;;
  localnet|localhost)   RPC="http://127.0.0.1:8899" ;;
  *) echo "unknown cluster: $CLUSTER" >&2; exit 1 ;;
esac

# ---------------------------------------------------------------- preflight
#
# A program deployed without its upgrade authority moved is a program one hot key can
# replace entirely, with no delay and nobody able to intervene — strictly weaker than the
# 72-hour timelock the EVM and Stellar deployments enforce. So the authority is required
# up front rather than left as a step somebody is trusted to remember afterwards.

if [[ -z "${PIRON_UPGRADE_AUTHORITY:-}" ]]; then
  cat >&2 <<'MSG'
PIRON_UPGRADE_AUTHORITY is not set.

It must be the Squads v4 multisig vault that will hold upgrade authority for these
programs. Deploying without it would leave the deployer keypair able to replace all
three programs at will, which is the one thing this script exists to prevent.

  PIRON_UPGRADE_AUTHORITY=<squads-vault-pubkey> ./scripts/deploy_solana.sh <cluster>
MSG
  exit 1
fi

AUTHORITY="$PIRON_UPGRADE_AUTHORITY"

if ! solana address -k "$KEYPAIR" >/dev/null 2>&1; then
  echo "deployer keypair not readable: $KEYPAIR" >&2; exit 1
fi
DEPLOYER="$(solana address -k "$KEYPAIR")"

if ! solana account "$AUTHORITY" --url "$RPC" >/dev/null 2>&1; then
  echo "upgrade authority $AUTHORITY does not exist on $CLUSTER" >&2
  echo "create the Squads multisig first; an authority that is not there yet is not an authority" >&2
  exit 1
fi

echo "cluster:   $CLUSTER ($RPC)"
echo "deployer:  $DEPLOYER"
echo "authority: $AUTHORITY"
echo

# ---------------------------------------------------------------- build

echo "Building..."
for p in "${PROGRAMS[@]}"; do
  ( cd "$SO/$p" && cargo-build-sbf >/dev/null )
  echo "  $p"
done

# The harnesses refuse to run against a stale artefact, so this also confirms the binaries
# about to be deployed are the ones the tests passed against.
echo "Verifying the built artefacts are what the tests cover..."
for p in "${PROGRAMS[@]}"; do
  ( cd "$SO/$p" && cargo test --quiet >/dev/null ) || {
    echo "FAILED: $p tests do not pass against the built program" >&2; exit 1; }
  echo "  $p"
done
echo

# ---------------------------------------------------------------- deploy

# Plain variables rather than an associative array: macOS ships bash 3.2, where
# `declare -A` does not exist, and a deploy script that only runs on a brew-upgraded
# shell is a deploy script that fails on somebody else's laptop.
ID_single_asset=""
ID_locked=""
ID_stable_yield=""

set_id() { eval "ID_$(echo "$1" | tr '-' '_')=\"$2\""; }
get_id() { eval "printf '%s' \"\$ID_$(echo "$1" | tr '-' '_')\""; }

for p in "${PROGRAMS[@]}"; do
  crate="piron_$(echo "$p" | tr '-' '_')_solana"
  wasm="$SO/$p/target/deploy/$crate.so"
  kp="$SO/$p/target/deploy/$crate-keypair.json"

  [[ -f "$wasm" ]] || { echo "missing artefact: $wasm" >&2; exit 1; }

  echo "Deploying $p..."
  solana program deploy "$wasm" \
    --keypair "$KEYPAIR" \
    --url "$RPC" \
    ${kp:+--program-id "$kp"} >/dev/null

  id="$(solana address -k "$kp")"
  set_id "$p" "$id"
  echo "  $id"
done
echo

# ---------------------------------------------------------------- hand over authority
#
# Set, then read back. A `set-upgrade-authority` that silently did nothing would leave the
# deployer in control of a program everybody believes is behind a multisig, which is worse
# than a deploy that failed outright.

echo "Transferring upgrade authority to $AUTHORITY..."
for p in "${PROGRAMS[@]}"; do
  id="$(get_id "$p")"
  solana program set-upgrade-authority "$id" \
    --keypair "$KEYPAIR" \
    --new-upgrade-authority "$AUTHORITY" \
    --url "$RPC" \
    --skip-new-upgrade-authority-signer-check >/dev/null

  actual="$(solana program show "$id" --url "$RPC" | awk '/Authority/ {print $NF}')"
  if [[ "$actual" != "$AUTHORITY" ]]; then
    echo "FAILED: $p upgrade authority is $actual, expected $AUTHORITY" >&2
    echo "the program is deployed but still replaceable by the deployer; fix before use" >&2
    exit 1
  fi
  echo "  $p -> $actual"
done
echo

# ---------------------------------------------------------------- record

mkdir -p "$ROOT/deployments"
cat > "$OUT" <<EOF
# Piron on Solana — $CLUSTER
# Generated $(date -u +%Y-%m-%dT%H:%M:%SZ) by scripts/deploy_solana.sh
PIRON_SOLANA_CLUSTER=$CLUSTER
PIRON_SOLANA_RPC=$RPC
PIRON_SOLANA_UPGRADE_AUTHORITY=$AUTHORITY

# Program ids. Pools are PDAs under these, derived from a pool id, so there is no
# per-pool address to record here — the admin creates one by calling initialize_pool and
# the backend confirms it from the transaction, as it does on EVM.
PIRON_SOLANA_SINGLE_ASSET_PROGRAM=$(get_id single-asset)
PIRON_SOLANA_LOCKED_PROGRAM=$(get_id locked)
PIRON_SOLANA_STABLE_YIELD_PROGRAM=$(get_id stable-yield)
EOF

echo "Wrote $OUT"
echo
cat <<EOF
Next, and not done here:

  - Pools. An admin calls initialize_pool on the relevant program with a fresh pool id.
    The stable-yield pool additionally needs its withdrawal queue account created by the
    client and funded for rent: it is $(printf '%d' 22600) bytes, which exceeds what a CPI can
    allocate, so the program cannot create it itself.

  - The backend's address registry needs a Solana entry pointing at these program ids,
    alongside the EVM chains in src/contracts/chains/.

  - Confirm on the Squads side that the vault above is the one you intend, and that its
    signer set and delay policy are what you want. This script checked the account exists;
    it cannot check that it is governed sensibly.
EOF

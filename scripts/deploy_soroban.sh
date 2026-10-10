#!/usr/bin/env bash
#
# Deploys the Piron infrastructure to a Stellar network.
#
#   ./scripts/deploy_soroban.sh testnet
#
# Infrastructure only — no pools. A pool is created by an admin calling the factory, which
# is `scripts/create_soroban_pool.sh`, and is a separate decision from standing the system
# up: it has a term, a target raise, an asset and a share name, none of which a deploy
# script is in a position to choose. This matches EVM, where `DeployUpgradeable.s.sol`
# deploys the system and the admin creates pools afterwards.
#
# Order follows the dependency graph: access control first, because everything authorises
# against it; the registry and factory next, because a pool has to register somewhere; and
# the factory's upgrade authority before anything can be built with it.
#
# Writes deployments/soroban-<network>.env.

set -euo pipefail

NETWORK="${1:-testnet}"
SOURCE="${SOURCE:-piron-deployer}"
ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
WASM="$ROOT/target/wasm32v1-none/release"
OUT="$ROOT/deployments/soroban-$NETWORK.env"

deploy() { local n="$1"; shift
  stellar contract deploy --wasm "$WASM/$n.wasm" --source-account "$SOURCE" \
    --network "$NETWORK" -- "$@" 2>/dev/null | tail -1; }

upload() {
  stellar contract upload --wasm "$WASM/$1.wasm" --source-account "$SOURCE" \
    --network "$NETWORK" 2>/dev/null | tail -1; }

# Errors go to the terminal rather than /dev/null: a deploy that dies without saying why
# is worse than one that fails loudly, and `set -e` cannot see past a pipeline anyway.
call() { local id="$1"; shift
  local out
  if ! out="$(stellar contract invoke --id "$id" --source-account "$SOURCE" \
      --network "$NETWORK" -- "$@" 2>&1)"; then
    echo "FAILED: invoke $id $*" >&2
    echo "$out" | tail -5 >&2
    return 1
  fi
  echo "$out" | tail -1; }

echo "Building..."
(cd "$ROOT" && cargo build --target wasm32v1-none --release --quiet)

ADMIN="$(stellar keys address "$SOURCE")"
for r in spv operator emergency multisig treasury reserve ops; do
  stellar keys generate "piron-$r" --network "$NETWORK" >/dev/null 2>&1 || true
done
SPV="$(stellar keys address piron-spv)"
OPERATOR="$(stellar keys address piron-operator)"
EMERGENCY="$(stellar keys address piron-emergency)"
MULTISIG="$(stellar keys address piron-multisig)"
TREASURY="$(stellar keys address piron-treasury)"
RESERVE="$(stellar keys address piron-reserve)"
OPS="$(stellar keys address piron-ops)"
ASSET="$(stellar contract id asset --asset native --network "$NETWORK")"

echo "Uploading contract code..."
ESCROW_WASM="$(upload pool_escrow)"
SHARE_WASM="$(upload pool_share)"
SINGLE_WASM="$(upload single_asset_manager)"
LOCKED_WASM="$(upload locked_pool_manager)"
STABLE_WASM="$(upload stable_yield_manager)"

echo "1/6 access-manager"
ACCESS="$(deploy access_manager --admin "$ADMIN" --spv "$SPV" --operator "$OPERATOR" \
  --emergency "$EMERGENCY" --multisig "$MULTISIG")"

echo "2/6 pool-registry"
REGISTRY="$(deploy pool_registry --access "$ACCESS")"

echo "3/6 fee-manager"
FEES="$(deploy fee_manager --access "$ACCESS" \
  --destinations "{\"treasury\":\"$TREASURY\",\"reserve\":\"$RESERVE\",\"ops\":\"$OPS\"}")"

echo "4/6 pool-factory"
FACTORY="$(deploy pool_factory --access "$ACCESS" --registry "$REGISTRY" \
  --escrow_wasm "$ESCROW_WASM" --share_wasm "$SHARE_WASM")"

echo "5/6 upgrade-timelock"
TIMELOCK="$(deploy upgrade_timelock --access "$ACCESS" --guardian "$EMERGENCY")"

echo "6/6 yield-reserve"
RESERVE_C="$(deploy yield_reserve --access "$ACCESS" --asset "$ASSET" --treasury "$TREASURY" \
  --config "{\"min_floor\":\"0\",\"treasury_split_bps\":3000}")"

echo "Wiring..."
# The factory registers what it builds, so it needs the role to do so.
call "$ACCESS" grant_during_deployment --caller "$ADMIN" --role 5 --account "$FACTORY" >/dev/null
call "$REGISTRY" set_asset_approved --caller "$ADMIN" --asset "$ASSET" --approved true >/dev/null
call "$REGISTRY" set_spv_authorized --caller "$ADMIN" --spv "$SPV" --authorized true >/dev/null
call "$FACTORY" set_manager_wasm --caller "$ADMIN" --kind SingleAsset --wasm_hash "$SINGLE_WASM" >/dev/null
call "$FACTORY" set_manager_wasm --caller "$ADMIN" --kind Locked --wasm_hash "$LOCKED_WASM" >/dev/null
call "$FACTORY" set_manager_wasm --caller "$ADMIN" --kind StableYield --wasm_hash "$STABLE_WASM" >/dev/null

# The SPV repays the reserve out of the capital it returns, so it settles debts directly
# rather than through the pool, which holds no funds of its own. Protocol-level, not
# per-pool: the same SPV serves every pool it is authorised for.
call "$RESERVE_C" set_source --caller "$ADMIN" --source "$SPV" --allowed true >/dev/null

# The factory stamps this onto every pool it builds, and refuses to build any without it
# rather than deploying a pool whose upgrade authority nobody holds. It has to be set
# before the first pool exists, which is why it belongs here and not in the pool script.
call "$FACTORY" set_timelock --caller "$ADMIN" --timelock "$TIMELOCK" >/dev/null

# Every singleton names the timelock as its upgrader, so no part of the system's own
# bytecode can be replaced without passing the 72-hour delay and the guardian's brake.
# Pools bind theirs in their own constructor, from the timelock set above.
echo "Binding upgraders..."
for c in "$ACCESS" "$REGISTRY" "$FEES" "$FACTORY" "$RESERVE_C"; do
  call "$c" bind_upgrader --caller "$ADMIN" --upgrader "$TIMELOCK" >/dev/null
done
call "$TIMELOCK" bind_upgrader --caller "$ADMIN" --upgrader "$TIMELOCK" >/dev/null

# Read it back. A factory that accepted `set_timelock` but did not store it would create
# pools whose upgrade slot is open for the taking, and the failure would be silent until
# somebody claimed one — so this is checked here rather than assumed.
CONFIGURED="$(call "$FACTORY" timelock | tr -d '"')"
if [[ "$CONFIGURED" != "$TIMELOCK" ]]; then
  echo "FAILED: factory timelock is '$CONFIGURED', expected '$TIMELOCK'" >&2
  exit 1
fi
echo "Factory verified: every pool it creates binds $TIMELOCK as its upgrader"

mkdir -p "$ROOT/deployments"
cat > "$OUT" <<EOF
# Piron on Stellar — $NETWORK
# Generated $(date -u +%Y-%m-%dT%H:%M:%SZ) by scripts/deploy_soroban.sh
#
# Infrastructure. Pools are created by an admin through the factory and recorded in
# soroban-$NETWORK-pools.env by scripts/create_soroban_pool.sh.
PIRON_NETWORK=$NETWORK
PIRON_ASSET=$ASSET
PIRON_ACCESS_MANAGER=$ACCESS
PIRON_POOL_REGISTRY=$REGISTRY
PIRON_FEE_MANAGER=$FEES
PIRON_POOL_FACTORY=$FACTORY
PIRON_UPGRADE_TIMELOCK=$TIMELOCK
PIRON_YIELD_RESERVE=$RESERVE_C
PIRON_ESCROW_WASM=$ESCROW_WASM
PIRON_SHARE_WASM=$SHARE_WASM
PIRON_SINGLE_ASSET_WASM=$SINGLE_WASM
PIRON_LOCKED_WASM=$LOCKED_WASM
PIRON_STABLE_YIELD_WASM=$STABLE_WASM
PIRON_ADMIN=$ADMIN
PIRON_SPV=$SPV
PIRON_OPERATOR=$OPERATOR
PIRON_EMERGENCY=$EMERGENCY
PIRON_MULTISIG=$MULTISIG
PIRON_TREASURY=$TREASURY
PIRON_FEE_RESERVE_DESTINATION=$RESERVE
PIRON_FEE_OPS_DESTINATION=$OPS
EOF

echo
echo "Deployed to $NETWORK:"
sed 's/^/  /' "$OUT"
echo
echo "No pools exist yet. Create one:"
echo "  ./scripts/create_soroban_pool.sh single-asset  $NETWORK"
echo "  ./scripts/create_soroban_pool.sh locked        $NETWORK"
echo "  ./scripts/create_soroban_pool.sh stable-yield  $NETWORK"
echo
echo "Still operational, and deliberately not done here:"
echo "  - access-manager finalize_deployment, which closes the instant-grant window."
echo "    Leave it open until every pool a locked deployment needs exists, because a"
echo "    locked pool's manager needs the Operator role and after finalisation that is a"
echo "    24-hour propose-then-execute by the multisig."

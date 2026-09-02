#!/usr/bin/env bash
#
# Deploys Piron to a Stellar network and creates a pool through the factory.
#
#   ./scripts/deploy_soroban.sh testnet
#
# Order follows the dependency graph: access control first, because everything authorises
# against it; the registry and factory next, because a pool has to register somewhere; and
# pools last, built by the factory so that a manager and its escrow are wired atomically.
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

echo "Creating a pool through the factory..."
NOW="$(date +%s)"
SALT="$(openssl rand -hex 32)"
ROLES="{\"admin\":\"$ADMIN\",\"operator\":\"$OPERATOR\",\"spv\":\"$SPV\",\"emergency\":\"$EMERGENCY\",\"treasury\":\"$TREASURY\"}"
CONFIG="{\"instrument_type\":0,\"face_value\":\"0\",\"purchase_price\":\"1000000000\",\"target_raise\":\"1000000000\",\"epoch_end_time\":$((NOW+604800)),\"maturity_date\":$((NOW+7776000)),\"coupon_dates\":[],\"coupon_rates\":[],\"discount_rate\":500,\"minimum_funding_threshold\":8000,\"min_investment\":\"10000000\",\"withdrawal_fee_bps\":50}"

# Deterministic: the addresses are knowable before the transaction that creates them.
META='{"name":"Piron 90d Bill","symbol":"pBILL90","decimals":7}'
PREDICTED="$(call "$FACTORY" predict --salt "$SALT")"
POOL="$(call "$FACTORY" create_single_asset_pool --caller "$ADMIN" --salt "$SALT" \
  --config "$CONFIG" --roles "$ROLES" --asset "$ASSET" --share_metadata "$META")"
MANAGER="$(echo "$POOL" | python3 -c 'import json,sys; print(json.load(sys.stdin)["manager"])')"
ESCROW="$(echo "$POOL" | python3 -c 'import json,sys; print(json.load(sys.stdin)["escrow"])')"
SHARE="$(echo "$POOL" | python3 -c 'import json,sys; print(json.load(sys.stdin)["share"])')"

call "$FEES" set_collector --caller "$ADMIN" --collector "$MANAGER" --allowed true >/dev/null
call "$RESERVE_C" set_source --caller "$ADMIN" --source "$MANAGER" --allowed true >/dev/null

# A locked pool, wired to the reserve.
#
# Early exit pays principal plus interest accrued to today, and the escrow holds only the
# principal until the SPV returns yield — so the exit is short by the interest earned, in
# the ordinary case rather than an edge one. The reserve lends the difference, which means
# the pool has to be able to call it (Operator) and to repay it (a registered source).
echo "Creating a locked pool..."
LOCKED_SALT="$(openssl rand -hex 32)"
LOCKED="$(call "$FACTORY" create_locked_pool --caller "$ADMIN" --salt "$LOCKED_SALT" \
  --roles "$ROLES" --asset "$ASSET" --min_investment 10000000 --reserve "\"$RESERVE_C\"")"
LOCKED_MANAGER="$(echo "$LOCKED" | python3 -c 'import json,sys; print(json.load(sys.stdin)["manager"])')"
LOCKED_ESCROW="$(echo "$LOCKED" | python3 -c 'import json,sys; print(json.load(sys.stdin)["escrow"])')"

call "$ACCESS" grant_during_deployment --caller "$ADMIN" --role 2 --account "$LOCKED_MANAGER" >/dev/null
call "$RESERVE_C" set_source --caller "$ADMIN" --source "$LOCKED_MANAGER" --allowed true >/dev/null
call "$FEES" set_collector --caller "$ADMIN" --collector "$LOCKED_MANAGER" --allowed true >/dev/null

# The SPV repays the reserve out of the capital it returns, so it settles debts directly
# rather than through the pool, which holds no funds of its own.
call "$RESERVE_C" set_source --caller "$ADMIN" --source "$SPV" --allowed true >/dev/null

# Every contract names the timelock as its upgrader, so no bytecode in the system can be
# replaced without passing the 72-hour delay and the guardian's brake.
echo "Binding upgraders..."
for c in "$ACCESS" "$REGISTRY" "$FEES" "$FACTORY" "$RESERVE_C" "$MANAGER" "$LOCKED_MANAGER"; do
  call "$c" bind_upgrader --caller "$ADMIN" --upgrader "$TIMELOCK" >/dev/null
done
call "$TIMELOCK" bind_upgrader --caller "$ADMIN" --upgrader "$TIMELOCK" >/dev/null

mkdir -p "$ROOT/deployments"
cat > "$OUT" <<EOF
# Piron on Stellar — $NETWORK
# Generated $(date -u +%Y-%m-%dT%H:%M:%SZ) by scripts/deploy_soroban.sh
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
PIRON_POOL_MANAGER=$MANAGER
PIRON_POOL_ESCROW=$ESCROW
PIRON_POOL_SHARE=$SHARE
PIRON_LOCKED_MANAGER=$LOCKED_MANAGER
PIRON_LOCKED_ESCROW=$LOCKED_ESCROW
PIRON_ADMIN=$ADMIN
PIRON_SPV=$SPV
PIRON_OPERATOR=$OPERATOR
PIRON_TREASURY=$TREASURY
EOF

echo
echo "Predicted before creation: $PREDICTED"
echo "Deployed to $NETWORK:"
sed 's/^/  /' "$OUT"

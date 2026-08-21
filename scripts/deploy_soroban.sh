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

call() { local id="$1"; shift
  stellar contract invoke --id "$id" --source-account "$SOURCE" \
    --network "$NETWORK" -- "$@" 2>/dev/null | tail -1; }

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
SINGLE_WASM="$(upload single_asset_manager)"
LOCKED_WASM="$(upload locked_pool_manager)"
STABLE_WASM="$(upload stable_yield_manager)"

echo "1/4 access-manager"
ACCESS="$(deploy access_manager --admin "$ADMIN" --spv "$SPV" --operator "$OPERATOR" \
  --emergency "$EMERGENCY" --multisig "$MULTISIG")"

echo "2/4 pool-registry"
REGISTRY="$(deploy pool_registry --access "$ACCESS")"

echo "3/4 fee-manager"
FEES="$(deploy fee_manager --access "$ACCESS" \
  --destinations "{\"treasury\":\"$TREASURY\",\"reserve\":\"$RESERVE\",\"ops\":\"$OPS\"}")"

echo "4/4 pool-factory"
FACTORY="$(deploy pool_factory --access "$ACCESS" --registry "$REGISTRY" \
  --escrow_wasm "$ESCROW_WASM")"

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
PREDICTED="$(call "$FACTORY" predict --salt "$SALT")"
POOL="$(call "$FACTORY" create_single_asset_pool --caller "$ADMIN" --salt "$SALT" \
  --config "$CONFIG" --roles "$ROLES" --asset "$ASSET")"
MANAGER="$(echo "$POOL" | python3 -c 'import json,sys; print(json.load(sys.stdin)["manager"])')"
ESCROW="$(echo "$POOL" | python3 -c 'import json,sys; print(json.load(sys.stdin)["escrow"])')"

call "$FEES" set_collector --caller "$ADMIN" --collector "$MANAGER" --allowed true >/dev/null

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
PIRON_ESCROW_WASM=$ESCROW_WASM
PIRON_SINGLE_ASSET_WASM=$SINGLE_WASM
PIRON_LOCKED_WASM=$LOCKED_WASM
PIRON_STABLE_YIELD_WASM=$STABLE_WASM
PIRON_POOL_MANAGER=$MANAGER
PIRON_POOL_ESCROW=$ESCROW
PIRON_ADMIN=$ADMIN
PIRON_SPV=$SPV
PIRON_OPERATOR=$OPERATOR
PIRON_TREASURY=$TREASURY
EOF

echo
echo "Predicted before creation: $PREDICTED"
echo "Deployed to $NETWORK:"
sed 's/^/  /' "$OUT"

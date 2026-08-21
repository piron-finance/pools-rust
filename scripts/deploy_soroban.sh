#!/usr/bin/env bash
#
# Deploys the Piron contracts to a Stellar network and wires them together.
#
#   ./scripts/deploy_soroban.sh testnet
#
# Order matters: access control first, because everything else authorises against it;
# the registry before any pool, because a pool cannot register on an unapproved asset;
# and the escrow before its manager, because the manager takes the escrow's address at
# construction. The escrow is bound to its manager afterwards — the two reference each
# other, so one has to exist first.
#
# Writes deployments/soroban-<network>.env, which the backend and the apps read.

set -euo pipefail

NETWORK="${1:-testnet}"
SOURCE="${SOURCE:-piron-deployer}"
ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
WASM="$ROOT/target/wasm32v1-none/release"
OUT="$ROOT/deployments/soroban-$NETWORK.env"

deploy() {           # deploy <wasm-name> [constructor args...]
  local name="$1"; shift
  stellar contract deploy \
    --wasm "$WASM/$name.wasm" \
    --source-account "$SOURCE" \
    --network "$NETWORK" \
    -- "$@" 2>/dev/null | tail -1
}

invoke() {           # invoke <contract-id> <fn> [args...]
  local id="$1"; shift
  stellar contract invoke \
    --id "$id" \
    --source-account "$SOURCE" \
    --network "$NETWORK" \
    -- "$@" >/dev/null 2>&1
}

echo "Building..."
(cd "$ROOT" && cargo build --target wasm32v1-none --release --quiet)

ADMIN="$(stellar keys address "$SOURCE")"
for role in spv operator emergency multisig treasury reserve ops; do
  stellar keys generate "piron-$role" --network "$NETWORK" >/dev/null 2>&1 || true
done
SPV="$(stellar keys address piron-spv)"
OPERATOR="$(stellar keys address piron-operator)"
EMERGENCY="$(stellar keys address piron-emergency)"
MULTISIG="$(stellar keys address piron-multisig)"
TREASURY="$(stellar keys address piron-treasury)"
RESERVE="$(stellar keys address piron-reserve)"
OPS="$(stellar keys address piron-ops)"

echo "Resolving asset (native XLM SAC)..."
ASSET="$(stellar contract id asset --asset native --network "$NETWORK")"

echo "1/5 access-manager"
ACCESS="$(deploy access_manager \
  --admin "$ADMIN" --spv "$SPV" --operator "$OPERATOR" \
  --emergency "$EMERGENCY" --multisig "$MULTISIG")"

echo "2/5 pool-registry"
REGISTRY="$(deploy pool_registry --access "$ACCESS")"

echo "3/5 fee-manager"
FEES="$(deploy fee_manager --access "$ACCESS" \
  --destinations "{\"treasury\":\"$TREASURY\",\"reserve\":\"$RESERVE\",\"ops\":\"$OPS\"}")"

echo "4/5 pool-escrow"
ESCROW="$(deploy pool_escrow --deployer "$ADMIN" --asset "$ASSET")"

echo "5/5 single-asset-manager"
NOW="$(date +%s)"
EPOCH_END=$((NOW + 604800))     # funding open for a week
MATURITY=$((NOW + 7776000))     # 90-day bill
CONFIG="{\"instrument_type\":0,\"face_value\":\"0\",\"purchase_price\":\"1000000000\",\"target_raise\":\"1000000000\",\"epoch_end_time\":$EPOCH_END,\"maturity_date\":$MATURITY,\"coupon_dates\":[],\"coupon_rates\":[],\"discount_rate\":500,\"minimum_funding_threshold\":8000,\"min_investment\":\"10000000\",\"withdrawal_fee_bps\":50}"
ROLES="{\"admin\":\"$ADMIN\",\"operator\":\"$OPERATOR\",\"spv\":\"$SPV\",\"emergency\":\"$EMERGENCY\",\"treasury\":\"$TREASURY\"}"
MANAGER="$(deploy single_asset_manager --config "$CONFIG" --roles "$ROLES" --escrow "$ESCROW")"

echo "Wiring..."
invoke "$ESCROW" bind_manager --manager "$MANAGER"
invoke "$REGISTRY" set_asset_approved --caller "$ADMIN" --asset "$ASSET" --approved true
invoke "$REGISTRY" set_spv_authorized --caller "$ADMIN" --spv "$SPV" --authorized true
invoke "$REGISTRY" register_pool --caller "$ADMIN" --pool "$MANAGER" \
  --manager "$MANAGER" --escrow "$ESCROW" --asset "$ASSET" --pool_type SingleAsset
invoke "$FEES" set_collector --caller "$ADMIN" --collector "$MANAGER" --allowed true

mkdir -p "$ROOT/deployments"
cat > "$OUT" <<EOF
# Piron on Stellar — $NETWORK
# Generated $(date -u +%Y-%m-%dT%H:%M:%SZ) by scripts/deploy_soroban.sh
PIRON_NETWORK=$NETWORK
PIRON_ASSET=$ASSET
PIRON_ACCESS_MANAGER=$ACCESS
PIRON_POOL_REGISTRY=$REGISTRY
PIRON_FEE_MANAGER=$FEES
PIRON_POOL_ESCROW=$ESCROW
PIRON_SINGLE_ASSET_MANAGER=$MANAGER
PIRON_ADMIN=$ADMIN
PIRON_SPV=$SPV
PIRON_OPERATOR=$OPERATOR
PIRON_TREASURY=$TREASURY
EOF

echo
echo "Deployed to $NETWORK:"
sed 's/^/  /' "$OUT"

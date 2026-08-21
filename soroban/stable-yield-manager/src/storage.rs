//! Storage layout for a Stable Yield pool.
//!
//! The portfolio lives whole in one Instance entry rather than one entry per instrument.
//! NAV needs every holding on every call, so a single read beats `MAX_INSTRUMENTS`
//! separate ones — which is also why the portfolio is bounded at all.

use soroban_sdk::{contracttype, Address, Env};

pub use piron_types::ttl;
pub use piron_types::{Config, Roles, StoredInstrument, StoredRequest};

#[contracttype]
#[derive(Clone)]
pub enum DataKey {
    Roles,
    Escrow,
    Asset,
    Config,
    /// The whole portfolio, in one entry.
    Instruments,
    TotalShares,
    QueueHead,
    QueueTail,
    QueuePending,
    /// Capital with the SPV that has not yet been recorded as an instrument.
    ///
    /// Without this, `deploy_capital` would shrink NAV the moment funds left the escrow,
    /// understating the pool until the operator records what was bought — and the reserve
    /// ratio, computed against that understated NAV, would release more capital than it
    /// should. This is the counterpart of the Solidity's `PendingAllocation`.
    DeployedOutstanding,
    Shares(Address),
    DepositTime(Address),
    Request(u64),
}

pub fn bump_instance(env: &Env) {
    env.storage()
        .instance()
        .extend_ttl(ttl::INSTANCE_THRESHOLD_LEDGERS, ttl::INSTANCE_BUMP_LEDGERS);
}

pub fn read_shares(env: &Env, who: &Address) -> i128 {
    let key = DataKey::Shares(who.clone());
    match env.storage().persistent().get::<_, i128>(&key) {
        Some(v) => {
            env.storage().persistent().extend_ttl(
                &key,
                ttl::PERSISTENT_THRESHOLD_LEDGERS,
                ttl::PERSISTENT_BUMP_LEDGERS,
            );
            v
        }
        None => 0,
    }
}

pub fn write_shares(env: &Env, who: &Address, shares: i128) {
    let key = DataKey::Shares(who.clone());
    if shares == 0 {
        if env.storage().persistent().has(&key) {
            env.storage().persistent().remove(&key);
        }
        return;
    }
    env.storage().persistent().set(&key, &shares);
    env.storage().persistent().extend_ttl(
        &key,
        ttl::PERSISTENT_THRESHOLD_LEDGERS,
        ttl::PERSISTENT_BUMP_LEDGERS,
    );
}

pub fn read_request(env: &Env, index: u64) -> Option<StoredRequest> {
    let key = DataKey::Request(index);
    match env.storage().persistent().get::<_, StoredRequest>(&key) {
        Some(r) => {
            env.storage().persistent().extend_ttl(
                &key,
                ttl::PERSISTENT_THRESHOLD_LEDGERS,
                ttl::PERSISTENT_BUMP_LEDGERS,
            );
            Some(r)
        }
        None => None,
    }
}

pub fn write_request(env: &Env, index: u64, request: &StoredRequest) {
    let key = DataKey::Request(index);
    env.storage().persistent().set(&key, request);
    env.storage().persistent().extend_ttl(
        &key,
        ttl::PERSISTENT_THRESHOLD_LEDGERS,
        ttl::PERSISTENT_BUMP_LEDGERS,
    );
}

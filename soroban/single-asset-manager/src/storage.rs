//! Storage keys and TTL discipline for a Single-Asset pool.
//!
//! The stored shapes live in `piron-types`, which carries no contract of its own — that is
//! what lets the factory build a manager's constructor arguments without dragging this
//! contract's exported symbols into its wasm.

use soroban_sdk::{contracttype, Address, Env};

pub use piron_types::ttl;
pub use piron_types::{Roles, StoredConfig, StoredState, StoredUser};

#[contracttype]
#[derive(Clone)]
pub enum DataKey {
    /// Deal terms. Instance — one bump covers config, state and every address below.
    Config,
    /// Running pool state.
    State,
    /// Who may drive the pool.
    Roles,
    /// Total shares outstanding.
    TotalShares,
    /// Collaborating contracts.
    Registry,
    Escrow,
    Vault,
    /// Per-investor share balance.
    ///
    /// PARITY: on EVM these live in the ERC-4626 `LiquidityPool` token. Held here until
    /// the vault contract exists, so the manager is self-consistent and testable rather
    /// than trusting balances passed as arguments.
    Shares(Address),
    /// Per-investor accounting. Persistent, one entry per address, bumped on touch.
    User(Address),
    /// Replay guard for backend-submitted calls. Temporary — meant to expire.
    Idem(soroban_sdk::BytesN<32>),
}

/// Extends the contract instance TTL. **Call this first in every entry point.**
pub fn bump_instance(env: &Env) {
    env.storage()
        .instance()
        .extend_ttl(ttl::INSTANCE_THRESHOLD_LEDGERS, ttl::INSTANCE_BUMP_LEDGERS);
}

/// Reads a user's accounting and extends that entry's TTL in the same call.
///
/// Extending a key that does not exist is a host error, so the bump only happens on the
/// branch where there is something to bump.
pub fn read_user(env: &Env, who: &Address) -> StoredUser {
    let key = DataKey::User(who.clone());
    match env.storage().persistent().get::<_, StoredUser>(&key) {
        Some(user) => {
            env.storage().persistent().extend_ttl(
                &key,
                ttl::PERSISTENT_THRESHOLD_LEDGERS,
                ttl::PERSISTENT_BUMP_LEDGERS,
            );
            user
        }
        None => StoredUser::default(),
    }
}

pub fn write_user(env: &Env, who: &Address, user: &StoredUser) {
    let key = DataKey::User(who.clone());
    env.storage().persistent().set(&key, user);
    env.storage().persistent().extend_ttl(
        &key,
        ttl::PERSISTENT_THRESHOLD_LEDGERS,
        ttl::PERSISTENT_BUMP_LEDGERS,
    );
}

pub fn read_shares(env: &Env, who: &Address) -> i128 {
    let key = DataKey::Shares(who.clone());
    match env.storage().persistent().get::<_, i128>(&key) {
        Some(shares) => {
            env.storage().persistent().extend_ttl(
                &key,
                ttl::PERSISTENT_THRESHOLD_LEDGERS,
                ttl::PERSISTENT_BUMP_LEDGERS,
            );
            shares
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

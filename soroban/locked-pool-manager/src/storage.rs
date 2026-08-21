//! Storage layout for a Locked pool.
//!
//! The shape differs from Single Asset in one way that matters: a locked pool holds many
//! independent positions rather than one deal, so almost everything lives in Persistent
//! storage keyed by position id, and only the tier table and pool totals sit in Instance.

use soroban_sdk::{contracttype, Env};

pub use piron_types::ttl;
pub use piron_types::{
    payment_from_u32, payment_to_u32, saturating_add, status_from_u32, status_to_u32, Roles,
    StoredMetrics, StoredPosition, StoredTier,
};

#[contracttype]
#[derive(Clone)]
pub enum DataKey {
    Roles,
    Escrow,
    Asset,
    MinInvestment,
    IsActive,
    /// The tier table, held whole in Instance because it is small and read on every call.
    Tiers,
    Metrics,
    /// Monotonic position counter.
    ///
    /// PARITY: the Solidity derives ids from `keccak256(pool, user, nonce, timestamp)`.
    /// A counter is cheaper, collision-free by construction, and gives the indexer a
    /// stable ordering. Ids are not comparable across chains, which nothing relies on.
    NextPositionId,
    Position(u64),
}

pub fn bump_instance(env: &Env) {
    env.storage()
        .instance()
        .extend_ttl(ttl::INSTANCE_THRESHOLD_LEDGERS, ttl::INSTANCE_BUMP_LEDGERS);
}

/// Reads a position, extending its TTL. A locked position can outlive several bump
/// windows, so the keeper re-bumps untouched ones — see `ttl.rs`.
pub fn read_position(env: &Env, id: u64) -> Option<StoredPosition> {
    let key = DataKey::Position(id);
    match env.storage().persistent().get::<_, StoredPosition>(&key) {
        Some(p) => {
            env.storage().persistent().extend_ttl(
                &key,
                ttl::PERSISTENT_THRESHOLD_LEDGERS,
                ttl::PERSISTENT_BUMP_LEDGERS,
            );
            Some(p)
        }
        None => None,
    }
}

pub fn write_position(env: &Env, id: u64, position: &StoredPosition) {
    let key = DataKey::Position(id);
    env.storage().persistent().set(&key, position);
    env.storage().persistent().extend_ttl(
        &key,
        ttl::PERSISTENT_THRESHOLD_LEDGERS,
        ttl::PERSISTENT_BUMP_LEDGERS,
    );
}






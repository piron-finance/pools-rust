//! The registry of what the protocol recognises.
//!
//! Ported from `src/PoolRegistry.sol`. Three questions, one place to ask them: is this a
//! pool we deployed, is this an asset we accept, and is this an SPV we have approved to
//! hold capital.
//!
//! Every manager checks here before it moves money, which is what stops a pool being
//! pointed at an arbitrary token or an unvetted counterparty.

#![no_std]

use soroban_sdk::{contract, contracterror, contractevent, contractimpl, contracttype, Address, Env};

use piron_core::governance::roles::Role;
pub use piron_types::PoolType;

const LEDGERS_PER_DAY: u32 = 17_280;
const BUMP: u32 = LEDGERS_PER_DAY * 60;
const THRESHOLD: u32 = LEDGERS_PER_DAY * 45;

#[contracterror]
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
#[repr(u32)]
pub enum Error {
    PoolNotRegistered = 610,
    AssetNotApproved = 611,
    SpvNotAuthorized = 612,
    AlreadyRegistered = 613,
    NotInitialized = 700,
    Unauthorized = 701,
}

#[contracttype]
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PoolInfo {
    pub manager: Address,
    pub escrow: Address,
    pub asset: Address,
    pub pool_type: PoolType,
    pub created_at: u64,
    pub is_active: bool,
}

#[contracttype]
#[derive(Clone)]
pub enum DataKey {
    Access,
    Pool(Address),
    ApprovedAsset(Address),
    AuthorizedSpv(Address),
    PoolCount,
}

#[contractclient(name = "AccessClient")]
pub trait Access {
    fn require_role(env: Env, role: u32, account: Address);
}

use soroban_sdk::contractclient;

#[contractevent]
pub struct PoolRegistered {
    #[topic]
    pub pool: Address,
    pub asset: Address,
    pub created_at: u64,
}

#[contractevent]
pub struct AssetApproval {
    #[topic]
    pub asset: Address,
    pub approved: bool,
}

#[contractevent]
pub struct SpvAuthorization {
    #[topic]
    pub spv: Address,
    pub authorized: bool,
}

#[contract]
pub struct PoolRegistry;

#[contractimpl]
impl PoolRegistry {
    pub fn __constructor(env: Env, access: Address) {
        env.storage().instance().set(&DataKey::Access, &access);
        env.storage().instance().set(&DataKey::PoolCount, &0u32);
        env.storage().instance().extend_ttl(THRESHOLD, BUMP);
    }

    /// Records a newly deployed pool. Factory or pool-creator role.
    pub fn register_pool(
        env: Env,
        caller: Address,
        pool: Address,
        manager: Address,
        escrow: Address,
        asset: Address,
        pool_type: PoolType,
    ) -> Result<(), Error> {
        Self::require_any(&env, &caller, Role::Factory, Role::PoolCreator)?;

        if env.storage().persistent().has(&DataKey::Pool(pool.clone())) {
            return Err(Error::AlreadyRegistered);
        }
        // A pool may only hold an asset the protocol has vetted.
        if !Self::is_approved_asset(env.clone(), asset.clone()) {
            return Err(Error::AssetNotApproved);
        }

        let created_at = env.ledger().timestamp();
        let key = DataKey::Pool(pool.clone());
        env.storage().persistent().set(
            &key,
            &PoolInfo {
                manager,
                escrow,
                asset: asset.clone(),
                pool_type,
                created_at,
                is_active: true,
            },
        );
        env.storage().persistent().extend_ttl(&key, THRESHOLD, BUMP);

        let count: u32 = env.storage().instance().get(&DataKey::PoolCount).unwrap_or(0);
        env.storage().instance().set(&DataKey::PoolCount, &(count + 1));

        PoolRegistered {
            pool,
            asset,
            created_at,
        }
        .publish(&env);
        Ok(())
    }

    /// Adds or removes an asset from the accepted list. Admin only.
    pub fn set_asset_approved(
        env: Env,
        caller: Address,
        asset: Address,
        approved: bool,
    ) -> Result<(), Error> {
        Self::require(&env, &caller, Role::DefaultAdmin)?;
        let key = DataKey::ApprovedAsset(asset.clone());
        env.storage().persistent().set(&key, &approved);
        env.storage().persistent().extend_ttl(&key, THRESHOLD, BUMP);
        AssetApproval { asset, approved }.publish(&env);
        Ok(())
    }

    /// Adds or removes an SPV from the approved counterparty list. Admin only.
    pub fn set_spv_authorized(
        env: Env,
        caller: Address,
        spv: Address,
        authorized: bool,
    ) -> Result<(), Error> {
        Self::require(&env, &caller, Role::DefaultAdmin)?;
        let key = DataKey::AuthorizedSpv(spv.clone());
        env.storage().persistent().set(&key, &authorized);
        env.storage().persistent().extend_ttl(&key, THRESHOLD, BUMP);
        SpvAuthorization { spv, authorized }.publish(&env);
        Ok(())
    }

    /// Retires a pool without forgetting it. Its positions still resolve.
    pub fn set_pool_active(
        env: Env,
        caller: Address,
        pool: Address,
        is_active: bool,
    ) -> Result<(), Error> {
        Self::require(&env, &caller, Role::DefaultAdmin)?;
        let key = DataKey::Pool(pool);
        let mut info: PoolInfo = env
            .storage()
            .persistent()
            .get(&key)
            .ok_or(Error::PoolNotRegistered)?;
        info.is_active = is_active;
        env.storage().persistent().set(&key, &info);
        Ok(())
    }

    // ------------------------------------------------------------ queries

    pub fn is_registered_pool(env: Env, pool: Address) -> bool {
        env.storage().persistent().has(&DataKey::Pool(pool))
    }

    pub fn is_approved_asset(env: Env, asset: Address) -> bool {
        env.storage()
            .persistent()
            .get(&DataKey::ApprovedAsset(asset))
            .unwrap_or(false)
    }

    pub fn is_authorized_spv(env: Env, spv: Address) -> bool {
        env.storage()
            .persistent()
            .get(&DataKey::AuthorizedSpv(spv))
            .unwrap_or(false)
    }

    pub fn pool_info(env: Env, pool: Address) -> Result<PoolInfo, Error> {
        env.storage()
            .persistent()
            .get(&DataKey::Pool(pool))
            .ok_or(Error::PoolNotRegistered)
    }

    pub fn pool_count(env: Env) -> u32 {
        env.storage().instance().get(&DataKey::PoolCount).unwrap_or(0)
    }

    /// Asserts all three preconditions a manager needs before moving capital to an SPV.
    pub fn require_deployable(env: Env, pool: Address, spv: Address) -> Result<(), Error> {
        let info = Self::pool_info(env.clone(), pool)?;
        if !info.is_active {
            return Err(Error::PoolNotRegistered);
        }
        if !Self::is_approved_asset(env.clone(), info.asset) {
            return Err(Error::AssetNotApproved);
        }
        if !Self::is_authorized_spv(env, spv) {
            return Err(Error::SpvNotAuthorized);
        }
        Ok(())
    }

    // ------------------------------------------------------------ internals

    fn access(env: &Env) -> Result<AccessClient<'_>, Error> {
        let address: Address = env
            .storage()
            .instance()
            .get(&DataKey::Access)
            .ok_or(Error::NotInitialized)?;
        Ok(AccessClient::new(env, &address))
    }

    fn require(env: &Env, caller: &Address, role: Role) -> Result<(), Error> {
        caller.require_auth();
        env.storage().instance().extend_ttl(THRESHOLD, BUMP);
        Self::access(env)?.require_role(&(role as u32), caller);
        Ok(())
    }

    /// Accepts either of two roles — a factory deploying pools, or an operator doing it
    /// by hand.
    fn require_any(env: &Env, caller: &Address, a: Role, b: Role) -> Result<(), Error> {
        caller.require_auth();
        env.storage().instance().extend_ttl(THRESHOLD, BUMP);
        let access = Self::access(env)?;
        if access.try_require_role(&(a as u32), caller).is_ok() {
            return Ok(());
        }
        if access.try_require_role(&(b as u32), caller).is_ok() {
            return Ok(());
        }
        Err(Error::Unauthorized)
    }
}

#[cfg(test)]
mod test;

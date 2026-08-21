//! Deterministic deployment of pools and their escrows.
//!
//! Ported from `src/factories/PoolFactory.sol` and `src/factories/ManagedPoolFactory.sol`,
//! which use CREATE2. The Soroban equivalent is a salt handed to the deployer: the same
//! factory and the same salt always produce the same address, so a pool's address can be
//! published, funded, or referenced before it exists.
//!
//! The factory also resolves a chicken-and-egg problem that the contracts cannot solve
//! alone. A manager takes its escrow's address at construction, and an escrow will only
//! obey one manager — so one has to exist first. Deploying both here, in one call, means
//! the pair is wired atomically: there is no window in which an escrow exists unbound, or
//! a manager points at an escrow that will take orders from someone else.

#![no_std]

use soroban_sdk::{
    contract, contractclient, contracterror, contractevent, contractimpl, contracttype, Address,
    Bytes, BytesN, Env,
};

use piron_core::governance::roles::Role;
use piron_types::{Config as StableYieldConfig, PoolType, Roles, StoredConfig};

const LEDGERS_PER_DAY: u32 = 17_280;
const BUMP: u32 = LEDGERS_PER_DAY * 60;
const THRESHOLD: u32 = LEDGERS_PER_DAY * 45;

/// Salt tags, so one user-supplied salt yields two distinct, still-deterministic
/// addresses.
const TAG_ESCROW: u8 = 1;
const TAG_MANAGER: u8 = 2;

#[contracterror]
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
#[repr(u32)]
pub enum Error {
    NotInitialized = 700,
    Unauthorized = 701,
    WasmNotSet = 711,
    SaltAlreadyUsed = 712,
}

/// A pool and the escrow that holds its money.
#[contracttype]
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PoolAddresses {
    pub manager: Address,
    pub escrow: Address,
}

#[contracttype]
#[derive(Clone)]
pub enum DataKey {
    Access,
    Registry,
    EscrowWasm,
    ManagerWasm(PoolType),
    /// Salts already spent, so a repeat call fails loudly rather than at the host level.
    Used(BytesN<32>),
    PoolCount,
}

#[contractclient(name = "AccessClient")]
pub trait Access {
    fn require_role(env: Env, role: u32, account: Address);
}

#[contractclient(name = "RegistryClient")]
pub trait Registry {
    fn register_pool(
        env: Env,
        caller: Address,
        pool: Address,
        manager: Address,
        escrow: Address,
        asset: Address,
        pool_type: PoolType,
    );
}

#[contractclient(name = "EscrowClient")]
pub trait Escrow {
    fn bind_manager(env: Env, manager: Address);
}

#[contractevent]
pub struct PoolCreated {
    #[topic]
    pub manager: Address,
    pub escrow: Address,
    pub asset: Address,
    pub kind: u32,
}

#[contractevent]
pub struct WasmRegistered {
    pub kind: u32,
}

#[contract]
pub struct PoolFactory;

#[contractimpl]
impl PoolFactory {
    pub fn __constructor(env: Env, access: Address, registry: Address, escrow_wasm: BytesN<32>) {
        env.storage().instance().set(&DataKey::Access, &access);
        env.storage().instance().set(&DataKey::Registry, &registry);
        env.storage()
            .instance()
            .set(&DataKey::EscrowWasm, &escrow_wasm);
        env.storage().instance().set(&DataKey::PoolCount, &0u32);
        env.storage().instance().extend_ttl(THRESHOLD, BUMP);
    }

    /// Points a pool kind at an uploaded wasm hash. Admin only.
    ///
    /// Changing this affects only future deployments — a pool already created keeps the
    /// code it was born with, and upgrades it separately.
    pub fn set_manager_wasm(
        env: Env,
        caller: Address,
        kind: PoolType,
        wasm_hash: BytesN<32>,
    ) -> Result<(), Error> {
        Self::require(&env, &caller, Role::DefaultAdmin)?;
        env.storage()
            .instance()
            .set(&DataKey::ManagerWasm(kind), &wasm_hash);
        WasmRegistered { kind: kind_u32(kind) }.publish(&env);
        Ok(())
    }

    // ------------------------------------------------------------ creation

    /// Deploys a Single-Asset pool: escrow, manager, wired and registered.
    pub fn create_single_asset_pool(
        env: Env,
        caller: Address,
        salt: BytesN<32>,
        config: StoredConfig,
        roles: Roles,
        asset: Address,
    ) -> Result<PoolAddresses, Error> {
        Self::require_creator(&env, &caller)?;
        let escrow = Self::deploy_escrow(&env, &salt, &asset)?;
        let manager = env
            .deployer()
            .with_current_contract(Self::salt_for(&env, &salt, TAG_MANAGER))
            .deploy_v2(
                Self::manager_wasm(&env, PoolType::SingleAsset)?,
                (config, roles, escrow.clone()),
            );
        Self::finish(&env, &salt, PoolType::SingleAsset, manager, escrow, asset)
    }

    /// Deploys a Locked pool.
    pub fn create_locked_pool(
        env: Env,
        caller: Address,
        salt: BytesN<32>,
        roles: Roles,
        asset: Address,
        min_investment: i128,
    ) -> Result<PoolAddresses, Error> {
        Self::require_creator(&env, &caller)?;
        let escrow = Self::deploy_escrow(&env, &salt, &asset)?;
        let manager = env
            .deployer()
            .with_current_contract(Self::salt_for(&env, &salt, TAG_MANAGER))
            .deploy_v2(
                Self::manager_wasm(&env, PoolType::Locked)?,
                (roles, escrow.clone(), asset.clone(), min_investment),
            );
        Self::finish(&env, &salt, PoolType::Locked, manager, escrow, asset)
    }

    /// Deploys a Stable Yield pool.
    pub fn create_stable_yield_pool(
        env: Env,
        caller: Address,
        salt: BytesN<32>,
        roles: Roles,
        asset: Address,
        config: StableYieldConfig,
    ) -> Result<PoolAddresses, Error> {
        Self::require_creator(&env, &caller)?;
        let escrow = Self::deploy_escrow(&env, &salt, &asset)?;
        let manager = env
            .deployer()
            .with_current_contract(Self::salt_for(&env, &salt, TAG_MANAGER))
            .deploy_v2(
                Self::manager_wasm(&env, PoolType::StableYield)?,
                (roles, escrow.clone(), asset.clone(), config),
            );
        Self::finish(&env, &salt, PoolType::StableYield, manager, escrow, asset)
    }

    // ------------------------------------------------------------ queries

    /// Where a given salt will put a pool, before it is deployed.
    ///
    /// The point of a deterministic factory: an address can be funded, whitelisted or
    /// published ahead of the transaction that creates it.
    pub fn predict(env: Env, salt: BytesN<32>) -> PoolAddresses {
        PoolAddresses {
            manager: env
                .deployer()
                .with_current_contract(Self::salt_for(&env, &salt, TAG_MANAGER))
                .deployed_address(),
            escrow: env
                .deployer()
                .with_current_contract(Self::salt_for(&env, &salt, TAG_ESCROW))
                .deployed_address(),
        }
    }

    pub fn is_salt_used(env: Env, salt: BytesN<32>) -> bool {
        env.storage().persistent().has(&DataKey::Used(salt))
    }

    pub fn pool_count(env: Env) -> u32 {
        env.storage().instance().get(&DataKey::PoolCount).unwrap_or(0)
    }

    pub fn manager_wasm_for(env: Env, kind: PoolType) -> Result<BytesN<32>, Error> {
        Self::manager_wasm(&env, kind)
    }

    // ------------------------------------------------------------ internals

    fn deploy_escrow(env: &Env, salt: &BytesN<32>, asset: &Address) -> Result<Address, Error> {
        if env.storage().persistent().has(&DataKey::Used(salt.clone())) {
            return Err(Error::SaltAlreadyUsed);
        }
        let wasm: BytesN<32> = env
            .storage()
            .instance()
            .get(&DataKey::EscrowWasm)
            .ok_or(Error::WasmNotSet)?;

        // The factory is the escrow's deployer, which is what lets it bind the manager
        // below and nothing else afterwards.
        Ok(env
            .deployer()
            .with_current_contract(Self::salt_for(env, salt, TAG_ESCROW))
            .deploy_v2(wasm, (env.current_contract_address(), asset.clone())))
    }

    /// Binds the pair, registers the pool, and marks the salt spent.
    fn finish(
        env: &Env,
        salt: &BytesN<32>,
        kind: PoolType,
        manager: Address,
        escrow: Address,
        asset: Address,
    ) -> Result<PoolAddresses, Error> {
        EscrowClient::new(env, &escrow).bind_manager(&manager);

        let registry: Address = env
            .storage()
            .instance()
            .get(&DataKey::Registry)
            .ok_or(Error::NotInitialized)?;
        RegistryClient::new(env, &registry).register_pool(
            &env.current_contract_address(),
            &manager,
            &manager,
            &escrow,
            &asset,
            &kind,
        );

        env.storage()
            .persistent()
            .set(&DataKey::Used(salt.clone()), &true);
        env.storage().persistent().extend_ttl(
            &DataKey::Used(salt.clone()),
            THRESHOLD,
            BUMP,
        );

        let count: u32 = env.storage().instance().get(&DataKey::PoolCount).unwrap_or(0);
        env.storage().instance().set(&DataKey::PoolCount, &(count + 1));

        PoolCreated {
            manager: manager.clone(),
            escrow: escrow.clone(),
            asset,
            kind: kind_u32(kind),
        }
        .publish(env);

        Ok(PoolAddresses { manager, escrow })
    }

    /// One salt, two addresses: tagged so escrow and manager never collide.
    fn salt_for(env: &Env, base: &BytesN<32>, tag: u8) -> BytesN<32> {
        let mut bytes = Bytes::from_slice(env, &base.to_array());
        bytes.push_back(tag);
        env.crypto().sha256(&bytes).into()
    }

    fn manager_wasm(env: &Env, kind: PoolType) -> Result<BytesN<32>, Error> {
        env.storage()
            .instance()
            .get(&DataKey::ManagerWasm(kind))
            .ok_or(Error::WasmNotSet)
    }

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

    /// Either role may create a pool: a governance process holding PoolCreator, or
    /// another factory holding Factory.
    fn require_creator(env: &Env, caller: &Address) -> Result<(), Error> {
        caller.require_auth();
        env.storage().instance().extend_ttl(THRESHOLD, BUMP);
        let access = Self::access(env)?;
        if access
            .try_require_role(&(Role::PoolCreator as u32), caller)
            .is_ok()
        {
            return Ok(());
        }
        if access.try_require_role(&(Role::Factory as u32), caller).is_ok() {
            return Ok(());
        }
        Err(Error::Unauthorized)
    }
}

/// Numeric form for events, matching the enum's declaration order.
fn kind_u32(kind: PoolType) -> u32 {
    match kind {
        PoolType::SingleAsset => 0,
        PoolType::Locked => 1,
        PoolType::StableYield => 2,
    }
}

#[cfg(test)]
mod test;

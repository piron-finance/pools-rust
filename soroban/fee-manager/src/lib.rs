//! Protocol fee collection and distribution.
//!
//! Ported from `src/FeeManager.sol`. Fees arrive here from every pool type, are split
//! three ways by fee type, and are held until an admin sweeps them out.
//!
//! Splitting at collection rather than at distribution is deliberate: the split in force
//! when a fee was charged is the split that fee is paid under, so changing the policy
//! never retroactively re-cuts money already taken.

#![no_std]

use soroban_sdk::{
    contract, contractclient, contracterror, contractevent, contractimpl, contracttype, token,
    Address, Env,
};

use piron_core::governance::fees::{self, FeeSplit, FeeType};
use piron_core::governance::roles::Role;

const LEDGERS_PER_DAY: u32 = 17_280;
const BUMP: u32 = LEDGERS_PER_DAY * 60;
const THRESHOLD: u32 = LEDGERS_PER_DAY * 45;

#[contracterror]
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
#[repr(u32)]
pub enum Error {
    Overflow = 100,
    InvalidAmount = 103,
    InvalidSplitRatio = 600,
    FeeTypeInactive = 601,
    UnknownFeeType = 616,
    NotInitialized = 700,
    Unauthorized = 701,
    NothingToDistribute = 709,
    DistributionPaused = 710,
    AlreadyBound = 715,
}

impl From<piron_core::Error> for Error {
    fn from(err: piron_core::Error) -> Self {
        match err {
            piron_core::Error::Overflow => Error::Overflow,
            piron_core::Error::InvalidSplitRatio => Error::InvalidSplitRatio,
            piron_core::Error::FeeTypeInactive => Error::FeeTypeInactive,
            piron_core::Error::UnknownFeeType => Error::UnknownFeeType,
            piron_core::Error::NegativeAmount => Error::InvalidAmount,
            _ => Error::InvalidAmount,
        }
    }
}

/// Where the three shares of a fee go.
#[contracttype]
#[derive(Clone)]
pub struct Destinations {
    pub treasury: Address,
    pub reserve: Address,
    pub ops: Address,
}

#[contracttype]
#[derive(Clone)]
pub struct StoredSplit {
    pub treasury_bps: u32,
    pub reserve_bps: u32,
    pub ops_bps: u32,
    pub active: bool,
}

impl StoredSplit {
    fn to_core(&self) -> FeeSplit {
        FeeSplit {
            treasury_bps: self.treasury_bps,
            reserve_bps: self.reserve_bps,
            ops_bps: self.ops_bps,
            active: self.active,
        }
    }
}

/// Running totals for one asset.
#[contracttype]
#[derive(Clone)]
pub struct AssetAccounting {
    pub total_collected: i128,
    pub total_distributed: i128,
    pub pending_treasury: i128,
    pub pending_reserve: i128,
    pub pending_ops: i128,
}

impl AssetAccounting {
    fn empty() -> Self {
        Self {
            total_collected: 0,
            total_distributed: 0,
            pending_treasury: 0,
            pending_reserve: 0,
            pending_ops: 0,
        }
    }
}

#[contracttype]
#[derive(Clone)]
pub enum DataKey {
    Access,
    Destinations,
    DistributionPaused,
    Split(u32),
    Accounting(Address),
    Collector(Address),
}

#[contractclient(name = "AccessClient")]
pub trait Access {
    fn require_role(env: Env, role: u32, account: Address);
}

#[contractevent]
pub struct FeeCollected {
    #[topic]
    pub pool: Address,
    pub asset: Address,
    pub amount: i128,
    pub fee_type: u32,
}

#[contractevent]
pub struct FeesDistributed {
    #[topic]
    pub asset: Address,
    pub to_treasury: i128,
    pub to_reserve: i128,
    pub to_ops: i128,
}

#[contractevent]
pub struct SplitUpdated {
    pub fee_type: u32,
    pub treasury_bps: u32,
    pub reserve_bps: u32,
    pub ops_bps: u32,
}

#[contract]
pub struct FeeManager;

#[contractimpl]
impl FeeManager {
    pub fn __constructor(env: Env, access: Address, destinations: Destinations) {
        env.storage().instance().set(&DataKey::Access, &access);
        env.storage()
            .instance()
            .set(&DataKey::Destinations, &destinations);
        env.storage()
            .instance()
            .set(&DataKey::DistributionPaused, &false);
        env.storage().instance().extend_ttl(THRESHOLD, BUMP);
    }

    // ------------------------------------------------------------ configuration

    /// Sets how one fee type divides. Admin only.
    pub fn set_split(
        env: Env,
        caller: Address,
        fee_type: u32,
        split: StoredSplit,
    ) -> Result<(), Error> {
        Self::require(&env, &caller, Role::DefaultAdmin)?;
        let parsed = FeeType::from_u32(fee_type)?;
        fees::validate_split(&split.to_core())?;

        env.storage()
            .instance()
            .set(&DataKey::Split(parsed as u32), &split);

        SplitUpdated {
            fee_type,
            treasury_bps: split.treasury_bps,
            reserve_bps: split.reserve_bps,
            ops_bps: split.ops_bps,
        }
        .publish(&env);
        Ok(())
    }

    /// Permits a pool manager to pay fees in. Admin only.
    pub fn set_collector(
        env: Env,
        caller: Address,
        collector: Address,
        allowed: bool,
    ) -> Result<(), Error> {
        Self::require(&env, &caller, Role::DefaultAdmin)?;
        env.storage()
            .instance()
            .set(&DataKey::Collector(collector), &allowed);
        Ok(())
    }

    pub fn set_distribution_paused(
        env: Env,
        caller: Address,
        paused: bool,
    ) -> Result<(), Error> {
        Self::require(&env, &caller, Role::DefaultAdmin)?;
        env.storage()
            .instance()
            .set(&DataKey::DistributionPaused, &paused);
        Ok(())
    }

    // ------------------------------------------------------------ collection

    /// Takes a fee in and books its three shares.
    ///
    /// The tokens move now; the split is recorded now; the payout happens later. That
    /// separation is what lets a pool charge a fee without three transfers on the hot
    /// path.
    pub fn collect_fee(
        env: Env,
        collector: Address,
        pool: Address,
        asset: Address,
        amount: i128,
        fee_type: u32,
    ) -> Result<(), Error> {
        collector.require_auth();
        env.storage().instance().extend_ttl(THRESHOLD, BUMP);

        let allowed: bool = env
            .storage()
            .instance()
            .get(&DataKey::Collector(collector.clone()))
            .unwrap_or(false);
        if !allowed {
            return Err(Error::Unauthorized);
        }
        if amount <= 0 {
            return Err(Error::InvalidAmount);
        }

        let parsed = FeeType::from_u32(fee_type)?;
        let split = Self::split(&env, parsed);
        let allocation = fees::split_fee(amount, &split)?;

        token::Client::new(&env, &asset).transfer(
            &collector,
            &env.current_contract_address(),
            &amount,
        );

        let mut acc = Self::accounting(&env, &asset);
        acc.total_collected = acc.total_collected.checked_add(amount).ok_or(Error::Overflow)?;
        acc.pending_treasury = acc
            .pending_treasury
            .checked_add(allocation.to_treasury)
            .ok_or(Error::Overflow)?;
        acc.pending_reserve = acc
            .pending_reserve
            .checked_add(allocation.to_reserve)
            .ok_or(Error::Overflow)?;
        acc.pending_ops = acc
            .pending_ops
            .checked_add(allocation.to_ops)
            .ok_or(Error::Overflow)?;
        Self::save_accounting(&env, &asset, &acc);

        FeeCollected {
            pool,
            asset,
            amount,
            fee_type,
        }
        .publish(&env);
        Ok(())
    }

    // ------------------------------------------------------------ distribution

    /// Sweeps everything pending for one asset out to its three destinations.
    pub fn distribute(env: Env, caller: Address, asset: Address) -> Result<i128, Error> {
        Self::require(&env, &caller, Role::DefaultAdmin)?;

        let paused: bool = env
            .storage()
            .instance()
            .get(&DataKey::DistributionPaused)
            .unwrap_or(false);
        if paused {
            return Err(Error::DistributionPaused);
        }

        let mut acc = Self::accounting(&env, &asset);
        let total = acc
            .pending_treasury
            .checked_add(acc.pending_reserve)
            .and_then(|v| v.checked_add(acc.pending_ops))
            .ok_or(Error::Overflow)?;
        if total == 0 {
            return Err(Error::NothingToDistribute);
        }

        let destinations: Destinations = env
            .storage()
            .instance()
            .get(&DataKey::Destinations)
            .ok_or(Error::NotInitialized)?;
        let client = token::Client::new(&env, &asset);
        let here = env.current_contract_address();

        if acc.pending_treasury > 0 {
            client.transfer(&here, &destinations.treasury, &acc.pending_treasury);
        }
        if acc.pending_reserve > 0 {
            client.transfer(&here, &destinations.reserve, &acc.pending_reserve);
        }
        if acc.pending_ops > 0 {
            client.transfer(&here, &destinations.ops, &acc.pending_ops);
        }

        FeesDistributed {
            asset: asset.clone(),
            to_treasury: acc.pending_treasury,
            to_reserve: acc.pending_reserve,
            to_ops: acc.pending_ops,
        }
        .publish(&env);

        acc.total_distributed = acc
            .total_distributed
            .checked_add(total)
            .ok_or(Error::Overflow)?;
        acc.pending_treasury = 0;
        acc.pending_reserve = 0;
        acc.pending_ops = 0;
        Self::save_accounting(&env, &asset, &acc);

        Ok(total)
    }

    // ------------------------------------------------------------ queries

    pub fn accounting_for(env: Env, asset: Address) -> AssetAccounting {
        Self::accounting(&env, &asset)
    }

    pub fn split_for(env: Env, fee_type: u32) -> Result<StoredSplit, Error> {
        let parsed = FeeType::from_u32(fee_type)?;
        Ok(env
            .storage()
            .instance()
            .get(&DataKey::Split(parsed as u32))
            .unwrap_or(StoredSplit {
                treasury_bps: 10_000,
                reserve_bps: 0,
                ops_bps: 0,
                active: true,
            }))
    }

    pub fn is_collector(env: Env, collector: Address) -> bool {
        env.storage()
            .instance()
            .get(&DataKey::Collector(collector))
            .unwrap_or(false)
    }

    // ------------------------------------------------------------ internals

    /// The split for a fee type, defaulting to all-treasury when none is configured.
    fn split(env: &Env, fee_type: FeeType) -> FeeSplit {
        env.storage()
            .instance()
            .get::<_, StoredSplit>(&DataKey::Split(fee_type as u32))
            .map(|s| s.to_core())
            .unwrap_or(FeeSplit::all_treasury())
    }

    fn accounting(env: &Env, asset: &Address) -> AssetAccounting {
        env.storage()
            .persistent()
            .get(&DataKey::Accounting(asset.clone()))
            .unwrap_or(AssetAccounting::empty())
    }

    fn save_accounting(env: &Env, asset: &Address, acc: &AssetAccounting) {
        let key = DataKey::Accounting(asset.clone());
        env.storage().persistent().set(&key, acc);
        env.storage().persistent().extend_ttl(&key, THRESHOLD, BUMP);
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
}


// ---------------------------------------------------------------- upgrades
//
// Soroban contracts replace their own bytecode; there is no proxy. So each one names an
// upgrader once — in production the timelock — and only that address can swap the code.
// The 72-hour delay and the guardian's brake live there, in front of every upgrade in the
// system.

#[contractimpl]
impl FeeManager {
    /// Names the contract permitted to upgrade this one. Callable once.
    ///
    /// One-way on purpose: a contract whose upgrader could be repointed has no timelock,
    /// only the appearance of one.
    pub fn bind_upgrader(env: Env, caller: Address, upgrader: Address) -> Result<(), Error> {
        Self::require(&env, &caller, Role::DefaultAdmin)?;
        piron_types::upgradeable::bind(&env, &upgrader).map_err(|_| Error::AlreadyBound)
    }

    /// Replaces this contract's code. Only the bound upgrader may call it.
    pub fn upgrade(env: Env, wasm_hash: soroban_sdk::BytesN<32>) -> Result<(), Error> {
        piron_types::upgradeable::perform(&env, wasm_hash).map_err(|e| match e {
            piron_types::upgradeable::UpgradeError::NotBound => Error::NotInitialized,
            piron_types::upgradeable::UpgradeError::AlreadyBound => Error::AlreadyBound,
        })
    }

    /// Who may upgrade this contract, if anyone yet.
    pub fn upgrader(env: Env) -> Option<Address> {
        piron_types::upgradeable::upgrader(&env)
    }
}

#[cfg(test)]
mod test;

//! Segregated fund custody for a single Piron pool.
//!
//! Ported from `src/escrows/PoolEscrow.sol`. One escrow per pool, holding that pool's
//! asset and nothing else. It makes no protocol decisions and knows nothing about
//! funding windows, maturity or shares — it moves tokens when its manager says so, and
//! refuses everyone else.
//!
//! Keeping custody in its own contract is the point: an upgrade to pool logic cannot
//! reach the money, and a bug in the manager is bounded by what this contract will do.

#![no_std]

use soroban_sdk::{
    contract, contracterror, contractevent, contractimpl, contracttype, token, Address, Env,
};

/// Ledgers in a day at a ~5s close. Mirrors the manager's TTL policy.
const LEDGERS_PER_DAY: u32 = 17_280;
const BUMP: u32 = LEDGERS_PER_DAY * 60;
const THRESHOLD: u32 = LEDGERS_PER_DAY * 45;

#[contracterror]
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
#[repr(u32)]
pub enum Error {
    /// 500-block: escrow / custody.
    NotManager = 500,
    InvalidAmount = 501,
    InsufficientBalance = 502,
    NotInitialized = 503,
    AlreadyBound = 504,
}

#[contracttype]
#[derive(Clone)]
pub enum DataKey {
    Deployer,
    Manager,
    Asset,
    /// Cumulative in and out, for reconciliation against the manager's accounting.
    TotalIn,
    TotalOut,
}

#[contractevent]
pub struct FundsReceived {
    #[topic]
    pub from: Address,
    pub amount: i128,
}

#[contractevent]
pub struct FundsReleased {
    #[topic]
    pub to: Address,
    pub amount: i128,
}

#[contract]
pub struct PoolEscrow;

#[contractimpl]
impl PoolEscrow {
    /// Creates the escrow for one asset. The manager is bound separately, because the
    /// two contracts reference each other and one has to exist first.
    pub fn __constructor(env: Env, deployer: Address, asset: Address) {
        env.storage().instance().set(&DataKey::Deployer, &deployer);
        env.storage().instance().set(&DataKey::Asset, &asset);
        env.storage().instance().set(&DataKey::TotalIn, &0i128);
        env.storage().instance().set(&DataKey::TotalOut, &0i128);
        env.storage().instance().extend_ttl(THRESHOLD, BUMP);
    }

    /// Names the manager permitted to move these funds. Callable once, by the deployer.
    ///
    /// Once set it cannot change: an escrow that could be re-pointed at a new manager
    /// would be an escrow that does not segregate anything.
    pub fn bind_manager(env: Env, manager: Address) -> Result<(), Error> {
        let deployer: Address = env
            .storage()
            .instance()
            .get(&DataKey::Deployer)
            .ok_or(Error::NotInitialized)?;
        deployer.require_auth();

        if env.storage().instance().has(&DataKey::Manager) {
            return Err(Error::AlreadyBound);
        }
        env.storage().instance().set(&DataKey::Manager, &manager);
        env.storage().instance().extend_ttl(THRESHOLD, BUMP);
        Ok(())
    }

    /// Pulls `amount` from `from` into custody.
    ///
    /// `from` authorises the transfer; the manager authorises that it should happen at
    /// all. Both signatures are required, which is what stops either side moving money
    /// alone.
    pub fn take(env: Env, from: Address, amount: i128) -> Result<(), Error> {
        Self::require_manager(&env)?;
        from.require_auth();
        if amount <= 0 {
            return Err(Error::InvalidAmount);
        }

        let asset = Self::asset(&env)?;
        token::Client::new(&env, &asset).transfer(&from, &env.current_contract_address(), &amount);

        let total: i128 = env.storage().instance().get(&DataKey::TotalIn).unwrap_or(0);
        env.storage()
            .instance()
            .set(&DataKey::TotalIn, &(total + amount));

        FundsReceived { from, amount }.publish(&env);
        Ok(())
    }

    /// Releases `amount` to `to`. Manager only.
    pub fn release(env: Env, to: Address, amount: i128) -> Result<(), Error> {
        Self::require_manager(&env)?;
        if amount <= 0 {
            return Err(Error::InvalidAmount);
        }

        let asset = Self::asset(&env)?;
        let client = token::Client::new(&env, &asset);
        if client.balance(&env.current_contract_address()) < amount {
            return Err(Error::InsufficientBalance);
        }
        client.transfer(&env.current_contract_address(), &to, &amount);

        let total: i128 = env.storage().instance().get(&DataKey::TotalOut).unwrap_or(0);
        env.storage()
            .instance()
            .set(&DataKey::TotalOut, &(total + amount));

        FundsReleased { to, amount }.publish(&env);
        Ok(())
    }

    /// What the escrow currently holds.
    pub fn balance(env: Env) -> Result<i128, Error> {
        let asset = Self::asset(&env)?;
        Ok(token::Client::new(&env, &asset).balance(&env.current_contract_address()))
    }

    pub fn total_in(env: Env) -> i128 {
        env.storage().instance().get(&DataKey::TotalIn).unwrap_or(0)
    }

    pub fn total_out(env: Env) -> i128 {
        env.storage()
            .instance()
            .get(&DataKey::TotalOut)
            .unwrap_or(0)
    }

    pub fn manager(env: Env) -> Result<Address, Error> {
        env.storage()
            .instance()
            .get(&DataKey::Manager)
            .ok_or(Error::NotInitialized)
    }

    fn asset(env: &Env) -> Result<Address, Error> {
        env.storage()
            .instance()
            .get(&DataKey::Asset)
            .ok_or(Error::NotInitialized)
    }

    /// Every mutating path goes through here. The manager is a contract address, so
    /// `require_auth` succeeds only inside a call the manager itself made.
    fn require_manager(env: &Env) -> Result<(), Error> {
        let manager: Address = env
            .storage()
            .instance()
            .get(&DataKey::Manager)
            .ok_or(Error::NotInitialized)?;
        manager.require_auth();
        env.storage().instance().extend_ttl(THRESHOLD, BUMP);
        Ok(())
    }
}

#[cfg(test)]
mod test;

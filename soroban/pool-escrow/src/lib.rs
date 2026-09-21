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
pub struct UntrackedSynced {
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
        // Against the tracked figure, not the raw balance: untracked tokens are not the
        // pool's to spend until they have been credited.
        if Self::balance(env.clone())? < amount {
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

    /// What the escrow holds on the protocol's books.
    ///
    /// Deliberately the tracked figure — everything that came in through `take` less
    /// everything released — and not the token balance. NAV is struck on this, so a raw
    /// balance would let anyone move the price of a pool by transferring tokens straight
    /// to the address: seed a new pool with the minimum, donate to push the price up, and
    /// later depositors round down to fewer shares. Tokens that arrive that way are
    /// invisible here until someone credits them deliberately with `sync_untracked`.
    pub fn balance(env: Env) -> Result<i128, Error> {
        let tracked = Self::total_in(env.clone())
            .checked_sub(Self::total_out(env.clone()))
            .ok_or(Error::InsufficientBalance)?;
        Ok(tracked.max(0))
    }

    /// Tokens sitting at this address that never came through `take`.
    pub fn untracked(env: Env) -> Result<i128, Error> {
        let asset = Self::asset(&env)?;
        let held = token::Client::new(&env, &asset).balance(&env.current_contract_address());
        Ok((held - Self::balance(env.clone())?).max(0))
    }

    /// Credits an untracked balance to the pool. Manager only.
    ///
    /// A donation, a misdirected transfer or a token airdrop is not free money until
    /// somebody decides it is. This makes that a deliberate act with an event attached
    /// rather than a silent move in the share price.
    pub fn sync_untracked(env: Env) -> Result<i128, Error> {
        Self::require_manager(&env)?;
        let found = Self::untracked(env.clone())?;
        if found <= 0 {
            return Err(Error::InvalidAmount);
        }
        let total: i128 = env.storage().instance().get(&DataKey::TotalIn).unwrap_or(0);
        env.storage()
            .instance()
            .set(&DataKey::TotalIn, &(total + found));
        UntrackedSynced { amount: found }.publish(&env);
        Ok(found)
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

    /// The address that created this escrow, and the only one that may name its
    /// upgrader.
    fn require_deployer(env: &Env, caller: &Address) -> Result<(), Error> {
        let deployer: Address = env
            .storage()
            .instance()
            .get(&DataKey::Deployer)
            .ok_or(Error::NotInitialized)?;
        caller.require_auth();
        if *caller != deployer {
            return Err(Error::NotManager);
        }
        Ok(())
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


// ---------------------------------------------------------------- upgrades
//
// Soroban contracts replace their own bytecode; there is no proxy. So each one names an
// upgrader once — in production the timelock — and only that address can swap the code.
// The 72-hour delay and the guardian's brake live there, in front of every upgrade in the
// system.

#[contractimpl]
impl PoolEscrow {
    /// Names the contract permitted to upgrade this one. Callable once.
    ///
    /// One-way on purpose: a contract whose upgrader could be repointed has no timelock,
    /// only the appearance of one.
    pub fn bind_upgrader(env: Env, caller: Address, upgrader: Address) -> Result<(), Error> {
        Self::require_deployer(&env, &caller)?;
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

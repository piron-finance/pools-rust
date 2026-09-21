//! A pool's share token.
//!
//! The Soroban counterpart of `LiquidityPool.sol` and `StableYieldPool.sol`, which are
//! ERC-4626 tokens on EVM. Holdings are a real SEP-41 balance rather than a number in the
//! manager's storage, so a position can be sent to someone else, held in a custodian's
//! account, or posted as collateral — the things owning a fixed-income position is
//! supposed to let you do.
//!
//! Supply is not the token's to decide. Only the bound manager may mint or burn, because
//! the manager is what knows whether assets actually arrived.
//!
//! Conversion between shares and assets stays in the manager: this contract deliberately
//! knows nothing about NAV. A share token that priced itself would be a second opinion on
//! what the pool is worth.

#![no_std]

use soroban_sdk::{
    contract, contracterror, contractevent, contractimpl, contracttype, Address, Env, String,
};

use piron_types::ttl;

#[contracterror]
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
#[repr(u32)]
pub enum Error {
    InvalidAmount = 103,
    NotInitialized = 700,
    Unauthorized = 701,
    InsufficientBalance = 717,
    InsufficientAllowance = 718,
    AllowanceExpired = 719,
    AlreadyBound = 715,
}

#[contracttype]
#[derive(Clone)]
pub struct Metadata {
    pub name: String,
    pub symbol: String,
    pub decimals: u32,
}

#[contracttype]
#[derive(Clone)]
pub enum DataKey {
    Manager,
    Meta,
    TotalSupply,
    Balance(Address),
    Allowance(Address, Address),
}

#[contractevent]
pub struct Transfer {
    #[topic]
    pub from: Address,
    #[topic]
    pub to: Address,
    pub amount: i128,
}

#[contractevent]
pub struct Mint {
    #[topic]
    pub to: Address,
    pub amount: i128,
}

#[contractevent]
pub struct Burn {
    #[topic]
    pub from: Address,
    pub amount: i128,
}

#[contractevent]
pub struct Approve {
    #[topic]
    pub from: Address,
    #[topic]
    pub spender: Address,
    pub amount: i128,
}

#[contract]
pub struct PoolShare;

#[contractimpl]
impl PoolShare {
    pub fn __constructor(env: Env, manager: Address, metadata: Metadata) {
        env.storage().instance().set(&DataKey::Manager, &manager);
        env.storage().instance().set(&DataKey::Meta, &metadata);
        env.storage().instance().set(&DataKey::TotalSupply, &0i128);
        env.storage()
            .instance()
            .extend_ttl(ttl::INSTANCE_THRESHOLD_LEDGERS, ttl::INSTANCE_BUMP_LEDGERS);
    }

    // ------------------------------------------------------------ supply

    /// Issues shares. Manager only.
    pub fn mint(env: Env, to: Address, amount: i128) -> Result<(), Error> {
        Self::require_manager(&env)?;
        if amount <= 0 {
            return Err(Error::InvalidAmount);
        }
        Self::credit(&env, &to, amount)?;
        Self::set_supply(&env, Self::total_supply(env.clone()) + amount);
        Mint { to, amount }.publish(&env);
        Ok(())
    }

    /// Retires shares. Manager only.
    ///
    /// The manager burns on the holder's behalf during a redemption it has already
    /// authorised, so there is no second signature here.
    pub fn burn(env: Env, from: Address, amount: i128) -> Result<(), Error> {
        Self::require_manager(&env)?;
        if amount <= 0 {
            return Err(Error::InvalidAmount);
        }
        Self::debit(&env, &from, amount)?;
        Self::set_supply(&env, Self::total_supply(env.clone()) - amount);
        Burn { from, amount }.publish(&env);
        Ok(())
    }

    // ------------------------------------------------------------ SEP-41

    pub fn transfer(env: Env, from: Address, to: Address, amount: i128) -> Result<(), Error> {
        from.require_auth();
        Self::move_shares(&env, &from, &to, amount)
    }

    pub fn transfer_from(
        env: Env,
        spender: Address,
        from: Address,
        to: Address,
        amount: i128,
    ) -> Result<(), Error> {
        spender.require_auth();
        Self::spend_allowance(&env, &from, &spender, amount)?;
        Self::move_shares(&env, &from, &to, amount)
    }

    /// SEP-41 approval, expiring at `expiration_ledger`.
    ///
    /// The expiry is honoured rather than discarded: a caller who sets one is stating
    /// when the allowance should stop working, and an implementation that ignores it
    /// leaves a permanent approval behind a signature that did not grant one.
    pub fn approve(
        env: Env,
        from: Address,
        spender: Address,
        amount: i128,
        expiration_ledger: u32,
    ) -> Result<(), Error> {
        from.require_auth();
        if amount < 0 {
            return Err(Error::InvalidAmount);
        }
        if amount > 0 && expiration_ledger < env.ledger().sequence() {
            return Err(Error::AllowanceExpired);
        }
        let key = DataKey::Allowance(from.clone(), spender.clone());
        env.storage()
            .persistent()
            .set(&key, &(amount, expiration_ledger));
        env.storage().persistent().extend_ttl(
            &key,
            ttl::PERSISTENT_THRESHOLD_LEDGERS,
            ttl::PERSISTENT_BUMP_LEDGERS,
        );
        Approve {
            from,
            spender,
            amount,
        }
        .publish(&env);
        Ok(())
    }

    /// What `spender` may still move, zero once the approval's ledger has passed.
    pub fn allowance(env: Env, from: Address, spender: Address) -> i128 {
        let (amount, expires): (i128, u32) = env
            .storage()
            .persistent()
            .get(&DataKey::Allowance(from, spender))
            .unwrap_or((0, 0));
        if amount > 0 && expires < env.ledger().sequence() {
            return 0;
        }
        amount
    }

    pub fn balance(env: Env, id: Address) -> i128 {
        let key = DataKey::Balance(id);
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

    pub fn total_supply(env: Env) -> i128 {
        env.storage()
            .instance()
            .get(&DataKey::TotalSupply)
            .unwrap_or(0)
    }

    pub fn decimals(env: Env) -> u32 {
        Self::meta(&env).map(|m| m.decimals).unwrap_or(7)
    }

    pub fn name(env: Env) -> Result<String, Error> {
        Ok(Self::meta(&env)?.name)
    }

    pub fn symbol(env: Env) -> Result<String, Error> {
        Ok(Self::meta(&env)?.symbol)
    }

    pub fn manager(env: Env) -> Result<Address, Error> {
        env.storage()
            .instance()
            .get(&DataKey::Manager)
            .ok_or(Error::NotInitialized)
    }

    // ------------------------------------------------------------ internals

    fn move_shares(env: &Env, from: &Address, to: &Address, amount: i128) -> Result<(), Error> {
        if amount <= 0 {
            return Err(Error::InvalidAmount);
        }
        Self::debit(env, from, amount)?;
        Self::credit(env, to, amount)?;
        Transfer {
            from: from.clone(),
            to: to.clone(),
            amount,
        }
        .publish(env);
        Ok(())
    }

    fn credit(env: &Env, who: &Address, amount: i128) -> Result<(), Error> {
        let key = DataKey::Balance(who.clone());
        let next = Self::balance(env.clone(), who.clone())
            .checked_add(amount)
            .ok_or(Error::InvalidAmount)?;
        env.storage().persistent().set(&key, &next);
        env.storage().persistent().extend_ttl(
            &key,
            ttl::PERSISTENT_THRESHOLD_LEDGERS,
            ttl::PERSISTENT_BUMP_LEDGERS,
        );
        Ok(())
    }

    fn debit(env: &Env, who: &Address, amount: i128) -> Result<(), Error> {
        let current = Self::balance(env.clone(), who.clone());
        if current < amount {
            return Err(Error::InsufficientBalance);
        }
        let key = DataKey::Balance(who.clone());
        let next = current - amount;
        if next == 0 {
            if env.storage().persistent().has(&key) {
                env.storage().persistent().remove(&key);
            }
        } else {
            env.storage().persistent().set(&key, &next);
            env.storage().persistent().extend_ttl(
                &key,
                ttl::PERSISTENT_THRESHOLD_LEDGERS,
                ttl::PERSISTENT_BUMP_LEDGERS,
            );
        }
        Ok(())
    }

    fn spend_allowance(
        env: &Env,
        from: &Address,
        spender: &Address,
        amount: i128,
    ) -> Result<(), Error> {
        let key = DataKey::Allowance(from.clone(), spender.clone());
        let allowed = Self::allowance(env.clone(), from.clone(), spender.clone());
        if allowed < amount {
            return Err(Error::InsufficientAllowance);
        }
        let expires: u32 = env
            .storage()
            .persistent()
            .get::<_, (i128, u32)>(&key)
            .map(|(_, e)| e)
            .unwrap_or(0);
        env.storage().persistent().set(&key, &(allowed - amount, expires));
        Ok(())
    }

    fn set_supply(env: &Env, value: i128) {
        env.storage().instance().set(&DataKey::TotalSupply, &value);
        env.storage()
            .instance()
            .extend_ttl(ttl::INSTANCE_THRESHOLD_LEDGERS, ttl::INSTANCE_BUMP_LEDGERS);
    }

    fn meta(env: &Env) -> Result<Metadata, Error> {
        env.storage()
            .instance()
            .get(&DataKey::Meta)
            .ok_or(Error::NotInitialized)
    }

    /// Minting and burning are the manager's alone: it is what knows whether assets
    /// actually arrived.
    fn require_manager(env: &Env) -> Result<(), Error> {
        let manager: Address = env
            .storage()
            .instance()
            .get(&DataKey::Manager)
            .ok_or(Error::NotInitialized)?;
        manager.require_auth();
        env.storage()
            .instance()
            .extend_ttl(ttl::INSTANCE_THRESHOLD_LEDGERS, ttl::INSTANCE_BUMP_LEDGERS);
        Ok(())
    }
}

#[cfg(test)]
mod test;

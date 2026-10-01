//! The protocol yield reserve.
//!
//! Ported from `src/escrows/YieldReserveEscrow.sol`.
//!
//! Three jobs a pool cannot do for itself. It **lends** liquidity to a pool that owes a
//! redemption before its capital has come back. It **absorbs** the loss when an SPV
//! returns less than it promised, so the shortfall lands on the protocol rather than on
//! whichever investor happened to redeem last. And it **sweeps** what is genuinely surplus
//! to the treasury.
//!
//! The floor is what keeps all three honest: lending and sweeping stop at it, so the
//! reserve cannot be drained by the ordinary operation of the thing it exists to backstop.
//! Covering a shortfall may go below it — the floor exists so losses can be absorbed, and
//! refusing to absorb one in order to protect it would be backwards.

#![no_std]

use soroban_sdk::{
    contract, contractclient, contracterror, contractevent, contractimpl, contracttype, token,
    Address, Env,
};

use piron_core::governance::reserve::{self, ReserveConfig, ReserveState};
use piron_core::governance::roles::Role;
use piron_types::ttl;

#[contracterror]
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
#[repr(u32)]
pub enum Error {
    Overflow = 100,
    InvalidAmount = 103,
    InvalidBps = 104,
    ReserveFloorBreached = 623,
    RepaymentExceedsLoan = 624,
    InsufficientReserve = 625,
    NotInitialized = 700,
    Unauthorized = 701,
    AlreadyBound = 715,
    NothingToSweep = 716,
    PoolEscrowNotSet = 717,
    WrongRecipient = 718,
    NothingToSync = 719,
}

impl From<piron_core::Error> for Error {
    fn from(err: piron_core::Error) -> Self {
        match err {
            piron_core::Error::Overflow => Error::Overflow,
            piron_core::Error::InvalidAmount => Error::InvalidAmount,
            piron_core::Error::InvalidBps => Error::InvalidBps,
            piron_core::Error::NegativeAmount => Error::InvalidAmount,
            piron_core::Error::ReserveFloorBreached => Error::ReserveFloorBreached,
            piron_core::Error::RepaymentExceedsLoan => Error::RepaymentExceedsLoan,
            piron_core::Error::InsufficientReserve => Error::InsufficientReserve,
            _ => Error::InvalidAmount,
        }
    }
}

#[contracttype]
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Config {
    /// Balance the reserve will not lend or sweep below.
    pub min_floor: i128,
    /// Share of incoming yield paid straight out to the treasury.
    pub treasury_split_bps: u32,
}

#[contracttype]
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Stats {
    pub balance: i128,
    pub total_loaned: i128,
    pub total_yield_received: i128,
    pub total_shortfall_covered: i128,
    pub total_swept: i128,
}

#[contracttype]
#[derive(Clone)]
pub enum DataKey {
    Access,
    Asset,
    Treasury,
    Config,
    Stats,
    /// Outstanding loan per pool.
    Loan(Address),
    /// Managers permitted to draw on the reserve.
    Source(Address),
    /// The escrow money for a given pool may be sent to.
    PoolEscrow(Address),
}

#[contractclient(name = "AccessClient")]
pub trait Access {
    fn require_role(env: Env, role: u32, account: Address);
}

#[contractevent]
pub struct YieldReceived {
    #[topic]
    pub from: Address,
    pub amount: i128,
    pub to_treasury: i128,
    pub retained: i128,
}

#[contractevent]
pub struct UntrackedSynced {
    pub amount: i128,
    pub balance: i128,
}

#[contractevent]
pub struct PoolEscrowSet {
    #[topic]
    pub pool: Address,
    pub escrow: Address,
}

#[contractevent]
pub struct LoanIssued {
    #[topic]
    pub pool: Address,
    pub amount: i128,
    pub outstanding: i128,
}

#[contractevent]
pub struct LoanRepaid {
    #[topic]
    pub pool: Address,
    pub amount: i128,
    pub outstanding: i128,
}

#[contractevent]
pub struct ShortfallCovered {
    #[topic]
    pub pool: Address,
    pub amount: i128,
}

#[contractevent]
pub struct SweptToTreasury {
    pub amount: i128,
}

#[contract]
pub struct YieldReserve;

#[contractimpl]
impl YieldReserve {
    pub fn __constructor(
        env: Env,
        access: Address,
        asset: Address,
        treasury: Address,
        config: Config,
    ) {
        env.storage().instance().set(&DataKey::Access, &access);
        env.storage().instance().set(&DataKey::Asset, &asset);
        env.storage().instance().set(&DataKey::Treasury, &treasury);
        env.storage().instance().set(&DataKey::Config, &config);
        env.storage().instance().set(
            &DataKey::Stats,
            &Stats {
                balance: 0,
                total_loaned: 0,
                total_yield_received: 0,
                total_shortfall_covered: 0,
                total_swept: 0,
            },
        );
        env.storage()
            .instance()
            .extend_ttl(ttl::INSTANCE_THRESHOLD_LEDGERS, ttl::INSTANCE_BUMP_LEDGERS);
    }

    // ------------------------------------------------------------ inflow

    /// Takes surplus yield in, paying the treasury its share and retaining the rest.
    pub fn receive_yield(env: Env, from: Address, amount: i128) -> Result<(), Error> {
        Self::require_source(&env, &from)?;
        if amount <= 0 {
            return Err(Error::InvalidAmount);
        }

        let config = Self::config(&env)?;
        let split = reserve::split_yield(amount, &config)?;

        let asset = Self::asset(&env)?;
        let client = token::Client::new(&env, &asset);
        client.transfer(&from, &env.current_contract_address(), &amount);
        if split.to_treasury > 0 {
            client.transfer(
                &env.current_contract_address(),
                &Self::treasury(&env)?,
                &split.to_treasury,
            );
        }

        let mut stats = Self::stats(&env);
        stats.balance = stats
            .balance
            .checked_add(split.retained)
            .ok_or(Error::Overflow)?;
        stats.total_yield_received = stats
            .total_yield_received
            .checked_add(amount)
            .ok_or(Error::Overflow)?;
        Self::save(&env, &stats);

        YieldReceived {
            from,
            amount,
            to_treasury: split.to_treasury,
            retained: split.retained,
        }
        .publish(&env);
        Ok(())
    }

    // ------------------------------------------------------------ lending

    /// Lends to a pool that owes a redemption before its capital has come back.
    ///
    /// `recipient` must be the escrow registered for `pool`. Taking the destination from
    /// the caller while booking the debt against a pool would let an operator lend to one
    /// address and charge another, which is the one thing a segregated reserve exists to
    /// prevent.
    pub fn loan_to_pool(
        env: Env,
        caller: Address,
        pool: Address,
        recipient: Address,
        amount: i128,
    ) -> Result<(), Error> {
        Self::require(&env, &caller, Role::Operator)?;
        Self::require_pool_escrow(&env, &pool, &recipient)?;

        let stats = Self::stats(&env);
        let config = Self::config(&env)?;
        reserve::validate_loan(amount, &Self::to_core(&stats), &config)?;

        token::Client::new(&env, &Self::asset(&env)?).transfer(
            &env.current_contract_address(),
            &recipient,
            &amount,
        );

        let outstanding = Self::loan_of(env.clone(), pool.clone())
            .checked_add(amount)
            .ok_or(Error::Overflow)?;
        env.storage()
            .persistent()
            .set(&DataKey::Loan(pool.clone()), &outstanding);

        let mut stats = stats;
        stats.balance = stats.balance.checked_sub(amount).ok_or(Error::Overflow)?;
        stats.total_loaned = stats
            .total_loaned
            .checked_add(amount)
            .ok_or(Error::Overflow)?;
        Self::save(&env, &stats);

        LoanIssued {
            pool,
            amount,
            outstanding,
        }
        .publish(&env);
        Ok(())
    }

    /// Takes a loan repayment back in.
    pub fn repay_loan(
        env: Env,
        from: Address,
        pool: Address,
        amount: i128,
    ) -> Result<(), Error> {
        Self::require_source(&env, &from)?;

        let outstanding = Self::loan_of(env.clone(), pool.clone());
        reserve::validate_repayment(amount, outstanding)?;

        token::Client::new(&env, &Self::asset(&env)?).transfer(
            &from,
            &env.current_contract_address(),
            &amount,
        );

        let remaining = outstanding - amount;
        if remaining == 0 {
            env.storage().persistent().remove(&DataKey::Loan(pool.clone()));
        } else {
            env.storage()
                .persistent()
                .set(&DataKey::Loan(pool.clone()), &remaining);
        }

        let mut stats = Self::stats(&env);
        stats.balance = stats.balance.checked_add(amount).ok_or(Error::Overflow)?;
        stats.total_loaned = stats
            .total_loaned
            .checked_sub(amount)
            .ok_or(Error::Overflow)?;
        Self::save(&env, &stats);

        LoanRepaid {
            pool,
            amount,
            outstanding: remaining,
        }
        .publish(&env);
        Ok(())
    }

    // ------------------------------------------------------------ loss

    /// Absorbs a shortfall so it lands on the protocol rather than on whichever investor
    /// happened to redeem last.
    ///
    /// This may take the reserve below its floor, deliberately.
    pub fn cover_shortfall(
        env: Env,
        caller: Address,
        pool: Address,
        recipient: Address,
        amount: i128,
    ) -> Result<(), Error> {
        Self::require(&env, &caller, Role::Operator)?;
        Self::require_pool_escrow(&env, &pool, &recipient)?;

        let mut stats = Self::stats(&env);
        reserve::validate_shortfall_cover(amount, &Self::to_core(&stats))?;

        token::Client::new(&env, &Self::asset(&env)?).transfer(
            &env.current_contract_address(),
            &recipient,
            &amount,
        );

        stats.balance = stats.balance.checked_sub(amount).ok_or(Error::Overflow)?;
        stats.total_shortfall_covered = stats
            .total_shortfall_covered
            .checked_add(amount)
            .ok_or(Error::Overflow)?;
        Self::save(&env, &stats);

        ShortfallCovered { pool, amount }.publish(&env);
        Ok(())
    }

    // ------------------------------------------------------------ outflow

    /// Sweeps genuine surplus to the treasury: whatever is above the floor and not out on
    /// loan.
    pub fn sweep_to_treasury(env: Env, caller: Address) -> Result<i128, Error> {
        Self::require(&env, &caller, Role::Operator)?;

        let mut stats = Self::stats(&env);
        let config = Self::config(&env)?;
        let amount = reserve::available_to_sweep(&Self::to_core(&stats), &config)?;
        if amount == 0 {
            return Err(Error::NothingToSweep);
        }
        reserve::validate_sweep(amount, &Self::to_core(&stats), &config)?;

        token::Client::new(&env, &Self::asset(&env)?).transfer(
            &env.current_contract_address(),
            &Self::treasury(&env)?,
            &amount,
        );

        stats.balance = stats.balance.checked_sub(amount).ok_or(Error::Overflow)?;
        stats.total_swept = stats.total_swept.checked_add(amount).ok_or(Error::Overflow)?;
        Self::save(&env, &stats);

        SweptToTreasury { amount }.publish(&env);
        Ok(amount)
    }

    // ------------------------------------------------------------ configuration

    pub fn set_config(env: Env, caller: Address, config: Config) -> Result<(), Error> {
        Self::require(&env, &caller, Role::DefaultAdmin)?;
        if config.treasury_split_bps > piron_core::BASIS_POINTS || config.min_floor < 0 {
            return Err(Error::InvalidBps);
        }
        env.storage().instance().set(&DataKey::Config, &config);
        Ok(())
    }

    /// Permits a pool manager or escrow to pay into and draw on the reserve.
    pub fn set_source(
        env: Env,
        caller: Address,
        source: Address,
        allowed: bool,
    ) -> Result<(), Error> {
        Self::require(&env, &caller, Role::DefaultAdmin)?;
        env.storage()
            .instance()
            .set(&DataKey::Source(source), &allowed);
        Ok(())
    }

    pub fn set_treasury(env: Env, caller: Address, treasury: Address) -> Result<(), Error> {
        Self::require(&env, &caller, Role::DefaultAdmin)?;
        env.storage().instance().set(&DataKey::Treasury, &treasury);
        Ok(())
    }

    // ------------------------------------------------------------ queries

    pub fn stats_of(env: Env) -> Stats {
        Self::stats(&env)
    }

    /// Credits tokens that reached the reserve without passing through a tracked path.
    ///
    /// `receive_yield` is the only crediting inflow and it *pulls* from a registered
    /// source. The fee manager pays the reserve's share of a distribution by pushing a
    /// plain transfer, which no tracked path sees — so without this the money sits at the
    /// address, invisible to every `balance` check that governs loans, deployments and
    /// shortfall cover, and `receive_yield` cannot retroactively credit it.
    ///
    /// Permissionless on purpose: it only recognises funds already held, so there is
    /// nothing to gain by calling it and real harm in the reserve sitting insolvent on
    /// paper because nobody did.
    pub fn sync_untracked(env: Env) -> Result<i128, Error> {
        let asset = Self::asset(&env)?;
        let held = token::Client::new(&env, &asset).balance(&env.current_contract_address());

        let mut stats = Self::stats(&env);
        if held <= stats.balance {
            return Err(Error::NothingToSync);
        }
        let credited = held - stats.balance;
        stats.balance = held;
        Self::save(&env, &stats);

        UntrackedSynced {
            amount: credited,
            balance: stats.balance,
        }
        .publish(&env);
        Ok(credited)
    }

    /// Tokens at this address that no tracked path has credited.
    pub fn untracked(env: Env) -> Result<i128, Error> {
        let asset = Self::asset(&env)?;
        let held = token::Client::new(&env, &asset).balance(&env.current_contract_address());
        let stats = Self::stats(&env);
        Ok((held - stats.balance).max(0))
    }

    pub fn available_for_loan(env: Env) -> Result<i128, Error> {
        let config = Self::config(&env)?;
        Ok(reserve::available_for_loan(
            &Self::to_core(&Self::stats(&env)),
            &config,
        ))
    }

    pub fn available_to_sweep(env: Env) -> Result<i128, Error> {
        let config = Self::config(&env)?;
        Ok(reserve::available_to_sweep(
            &Self::to_core(&Self::stats(&env)),
            &config,
        )?)
    }

    pub fn loan_of(env: Env, pool: Address) -> i128 {
        env.storage()
            .persistent()
            .get(&DataKey::Loan(pool))
            .unwrap_or(0)
    }

    pub fn is_source(env: Env, who: Address) -> bool {
        env.storage()
            .instance()
            .get(&DataKey::Source(who))
            .unwrap_or(false)
    }

    pub fn config_of(env: Env) -> Result<Config, Error> {
        env.storage()
            .instance()
            .get(&DataKey::Config)
            .ok_or(Error::NotInitialized)
    }

    // ------------------------------------------------------------ internals

    fn to_core(stats: &Stats) -> ReserveState {
        ReserveState {
            balance: stats.balance,
            total_loaned: stats.total_loaned,
            total_yield_received: stats.total_yield_received,
            total_shortfall_covered: stats.total_shortfall_covered,
            total_swept: stats.total_swept,
        }
    }

    fn stats(env: &Env) -> Stats {
        env.storage().instance().get(&DataKey::Stats).unwrap_or(Stats {
            balance: 0,
            total_loaned: 0,
            total_yield_received: 0,
            total_shortfall_covered: 0,
            total_swept: 0,
        })
    }

    fn save(env: &Env, stats: &Stats) {
        env.storage().instance().set(&DataKey::Stats, stats);
        env.storage()
            .instance()
            .extend_ttl(ttl::INSTANCE_THRESHOLD_LEDGERS, ttl::INSTANCE_BUMP_LEDGERS);
    }

    /// The stored config in core's shape. The contract type is the wire form; core does
    /// the arithmetic.
    fn config(env: &Env) -> Result<ReserveConfig, Error> {
        let c: Config = env
            .storage()
            .instance()
            .get(&DataKey::Config)
            .ok_or(Error::NotInitialized)?;
        Ok(ReserveConfig {
            min_floor: c.min_floor,
            treasury_split_bps: c.treasury_split_bps,
        })
    }

    fn asset(env: &Env) -> Result<Address, Error> {
        env.storage()
            .instance()
            .get(&DataKey::Asset)
            .ok_or(Error::NotInitialized)
    }

    fn treasury(env: &Env) -> Result<Address, Error> {
        env.storage()
            .instance()
            .get(&DataKey::Treasury)
            .ok_or(Error::NotInitialized)
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
        env.storage()
            .instance()
            .extend_ttl(ttl::INSTANCE_THRESHOLD_LEDGERS, ttl::INSTANCE_BUMP_LEDGERS);
        Self::access(env)?.require_role(&(role as u32), caller);
        Ok(())
    }

    fn require_admin(env: &Env, caller: &Address) -> Result<(), Error> {
        Self::require(env, caller, Role::DefaultAdmin)
    }

    /// A manager or escrow the admin has permitted to move money in and out.
    /// Names the escrow that money for `pool` may be sent to. Admin only.
    pub fn set_pool_escrow(
        env: Env,
        caller: Address,
        pool: Address,
        escrow: Address,
    ) -> Result<(), Error> {
        Self::require(&env, &caller, Role::DefaultAdmin)?;
        env.storage()
            .persistent()
            .set(&DataKey::PoolEscrow(pool.clone()), &escrow);
        PoolEscrowSet { pool, escrow }.publish(&env);
        Ok(())
    }

    pub fn pool_escrow(env: Env, pool: Address) -> Option<Address> {
        env.storage().persistent().get(&DataKey::PoolEscrow(pool))
    }

    /// Refuses to pay anywhere but the escrow registered for the pool being charged.
    fn require_pool_escrow(env: &Env, pool: &Address, recipient: &Address) -> Result<(), Error> {
        match Self::pool_escrow(env.clone(), pool.clone()) {
            Some(escrow) if escrow == *recipient => Ok(()),
            Some(_) => Err(Error::WrongRecipient),
            None => Err(Error::PoolEscrowNotSet),
        }
    }

    fn require_source(env: &Env, caller: &Address) -> Result<(), Error> {
        caller.require_auth();
        env.storage()
            .instance()
            .extend_ttl(ttl::INSTANCE_THRESHOLD_LEDGERS, ttl::INSTANCE_BUMP_LEDGERS);
        if !Self::is_source(env.clone(), caller.clone()) {
            return Err(Error::Unauthorized);
        }
        Ok(())
    }
}

// ---------------------------------------------------------------- upgrades

#[contractimpl]
impl YieldReserve {
    /// Names the contract permitted to upgrade this one. Callable once.
    pub fn bind_upgrader(env: Env, caller: Address, upgrader: Address) -> Result<(), Error> {
        Self::require_admin(&env, &caller)?;
        piron_types::upgradeable::bind(&env, &upgrader).map_err(|_| Error::AlreadyBound)
    }

    /// Replaces this contract's code. Only the bound upgrader may call it.
    pub fn upgrade(env: Env, wasm_hash: soroban_sdk::BytesN<32>) -> Result<(), Error> {
        piron_types::upgradeable::perform(&env, wasm_hash).map_err(|e| match e {
            piron_types::upgradeable::UpgradeError::NotBound => Error::NotInitialized,
            piron_types::upgradeable::UpgradeError::AlreadyBound => Error::AlreadyBound,
        })
    }

    pub fn upgrader(env: Env) -> Option<Address> {
        piron_types::upgradeable::upgrader(&env)
    }
}

#[cfg(test)]
mod test;

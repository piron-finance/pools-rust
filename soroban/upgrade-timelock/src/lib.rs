//! The delay in front of every upgrade, and the brake in front of the delay.
//!
//! Ported from `src/governance/TimelockController.sol` and
//! `src/governance/UpgradeGuardian.sol`, which are one contract here because on Soroban
//! they were never really two: the guardian's only power is to pause this timelock.
//!
//! Every Piron contract names this contract as its upgrader, so no bytecode in the system
//! can be replaced without passing through here. The sequence is: schedule, wait 72 hours,
//! execute inside a 7-day window — or a guardian pauses and nothing executes for up to 30
//! days, after which the pause lapses so a lost guardian key cannot freeze governance.

#![no_std]

use soroban_sdk::{
    contract, contractclient, contracterror, contractevent, contractimpl, contracttype, Address,
    BytesN, Env,
};

use piron_core::governance::roles::Role;
use piron_core::governance::upgrade::{self, UpgradeOperation};
use piron_types::ttl;

#[contracterror]
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
#[repr(u32)]
pub enum Error {
    UpgradeDelayNotElapsed = 617,
    UpgradeExpired = 618,
    UpgradeAlreadyExecuted = 619,
    UpgradeCancelled = 620,
    UpgradesPaused = 621,
    UpgradeNotFound = 622,
    NotInitialized = 700,
    Unauthorized = 701,
    NotPaused = 713,
    AlreadyPaused = 714,
    AlreadyBound = 715,
}

impl From<piron_core::Error> for Error {
    fn from(err: piron_core::Error) -> Self {
        match err {
            piron_core::Error::UpgradeDelayNotElapsed => Error::UpgradeDelayNotElapsed,
            piron_core::Error::UpgradeExpired => Error::UpgradeExpired,
            piron_core::Error::UpgradeAlreadyExecuted => Error::UpgradeAlreadyExecuted,
            piron_core::Error::UpgradeCancelled => Error::UpgradeCancelled,
            piron_core::Error::UpgradesPaused => Error::UpgradesPaused,
            _ => Error::Unauthorized,
        }
    }
}

#[contracttype]
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ScheduledUpgrade {
    pub target: Address,
    pub wasm_hash: BytesN<32>,
    pub scheduled_at: u64,
    pub executed: bool,
    pub cancelled: bool,
}

impl ScheduledUpgrade {
    fn to_core(&self) -> UpgradeOperation {
        UpgradeOperation {
            scheduled_at: self.scheduled_at,
            executed: self.executed,
            cancelled: self.cancelled,
        }
    }
}

#[contracttype]
#[derive(Clone)]
pub enum DataKey {
    Access,
    Guardian,
    /// When the guardian paused, if it did.
    PausedAt,
    Operation(u64),
    NextId,
}

#[contractclient(name = "AccessClient")]
pub trait Access {
    fn require_role(env: Env, role: u32, account: Address);
}

#[contractclient(name = "UpgradeableClient")]
pub trait Upgradeable {
    fn upgrade(env: Env, wasm_hash: BytesN<32>);
}

#[contractevent]
pub struct UpgradeScheduled {
    #[topic]
    pub target: Address,
    pub id: u64,
    pub wasm_hash: BytesN<32>,
    pub ready_at: u64,
    pub expires_at: u64,
}

#[contractevent]
pub struct UpgradeExecuted {
    #[topic]
    pub target: Address,
    pub id: u64,
}

#[contractevent]
pub struct UpgradeCancelled {
    pub id: u64,
}

#[contractevent]
pub struct UpgradesPausedEvent {
    #[topic]
    pub guardian: Address,
    pub until: u64,
}

#[contractevent]
pub struct UpgradesResumed {
    #[topic]
    pub admin: Address,
}

#[contract]
pub struct UpgradeTimelock;

#[contractimpl]
impl UpgradeTimelock {
    pub fn __constructor(env: Env, access: Address, guardian: Address) {
        env.storage().instance().set(&DataKey::Access, &access);
        env.storage().instance().set(&DataKey::Guardian, &guardian);
        env.storage().instance().set(&DataKey::NextId, &1u64);
        env.storage()
            .instance()
            .extend_ttl(ttl::INSTANCE_THRESHOLD_LEDGERS, ttl::INSTANCE_BUMP_LEDGERS);
    }

    /// Queues an upgrade. The clock starts now.
    pub fn schedule(
        env: Env,
        caller: Address,
        target: Address,
        wasm_hash: BytesN<32>,
    ) -> Result<u64, Error> {
        Self::require(&env, &caller, Role::DefaultAdmin)?;

        let id: u64 = env.storage().instance().get(&DataKey::NextId).unwrap_or(1);
        env.storage().instance().set(&DataKey::NextId, &(id + 1));

        let now = env.ledger().timestamp();
        let op = ScheduledUpgrade {
            target: target.clone(),
            wasm_hash: wasm_hash.clone(),
            scheduled_at: now,
            executed: false,
            cancelled: false,
        };
        Self::save(&env, id, &op);

        let core = op.to_core();
        UpgradeScheduled {
            target,
            id,
            wasm_hash,
            ready_at: core.ready_at(),
            expires_at: core.expires_at(),
        }
        .publish(&env);
        Ok(id)
    }

    /// Runs a matured upgrade. Multisig only — scheduling and executing are different
    /// hands, as they are for role grants.
    pub fn execute(env: Env, caller: Address, id: u64) -> Result<(), Error> {
        Self::require(&env, &caller, Role::MultisigAdmin)?;

        let mut op = Self::operation(&env, id)?;
        upgrade::validate_execution(&op.to_core(), Self::paused_at(&env), env.ledger().timestamp())?;

        // Mark before calling out, so a target that re-enters cannot run it twice.
        op.executed = true;
        Self::save(&env, id, &op);

        UpgradeableClient::new(&env, &op.target).upgrade(&op.wasm_hash);

        UpgradeExecuted {
            target: op.target,
            id,
        }
        .publish(&env);
        Ok(())
    }

    /// Withdraws a queued upgrade. Admin or multisig.
    pub fn cancel(env: Env, caller: Address, id: u64) -> Result<(), Error> {
        caller.require_auth();
        let access = Self::access(&env)?;
        let allowed = access
            .try_require_role(&(Role::DefaultAdmin as u32), &caller)
            .is_ok()
            || access
                .try_require_role(&(Role::MultisigAdmin as u32), &caller)
                .is_ok();
        if !allowed {
            return Err(Error::Unauthorized);
        }

        let mut op = Self::operation(&env, id)?;
        upgrade::validate_cancellation(&op.to_core())?;
        op.cancelled = true;
        Self::save(&env, id, &op);

        UpgradeCancelled { id }.publish(&env);
        Ok(())
    }

    // ------------------------------------------------------------ the brake

    /// Stops all upgrades. Guardian or emergency role.
    ///
    /// Lapses on its own after 30 days: a guardian may stop an upgrade, but must not be
    /// able to freeze governance indefinitely.
    pub fn pause_upgrades(env: Env, caller: Address) -> Result<(), Error> {
        caller.require_auth();
        env.storage()
            .instance()
            .extend_ttl(ttl::INSTANCE_THRESHOLD_LEDGERS, ttl::INSTANCE_BUMP_LEDGERS);

        let guardian: Address = env
            .storage()
            .instance()
            .get(&DataKey::Guardian)
            .ok_or(Error::NotInitialized)?;
        if caller != guardian
            && Self::access(&env)?
                .try_require_role(&(Role::Emergency as u32), &caller)
                .is_err()
        {
            return Err(Error::Unauthorized);
        }

        let now = env.ledger().timestamp();
        if upgrade::pause_is_active(Self::paused_at(&env), now) {
            return Err(Error::AlreadyPaused);
        }
        env.storage().instance().set(&DataKey::PausedAt, &now);

        UpgradesPausedEvent {
            guardian: caller,
            until: now + upgrade::MAX_PAUSE_DURATION,
        }
        .publish(&env);
        Ok(())
    }

    /// Lifts the brake early. Admin only.
    pub fn resume_upgrades(env: Env, caller: Address) -> Result<(), Error> {
        Self::require(&env, &caller, Role::DefaultAdmin)?;
        if !upgrade::pause_is_active(Self::paused_at(&env), env.ledger().timestamp()) {
            return Err(Error::NotPaused);
        }
        env.storage().instance().remove(&DataKey::PausedAt);
        UpgradesResumed { admin: caller }.publish(&env);
        Ok(())
    }

    pub fn set_guardian(env: Env, caller: Address, guardian: Address) -> Result<(), Error> {
        Self::require(&env, &caller, Role::DefaultAdmin)?;
        env.storage().instance().set(&DataKey::Guardian, &guardian);
        Ok(())
    }

    // ------------------------------------------------------------ queries

    pub fn is_paused(env: Env) -> bool {
        upgrade::pause_is_active(Self::paused_at(&env), env.ledger().timestamp())
    }

    pub fn operation_of(env: Env, id: u64) -> Result<ScheduledUpgrade, Error> {
        Self::operation(&env, id)
    }

    /// 0 waiting, 1 ready, 2 expired, 3 executed, 4 cancelled.
    pub fn operation_state(env: Env, id: u64) -> Result<u32, Error> {
        let op = Self::operation(&env, id)?;
        Ok(match op.to_core().state(env.ledger().timestamp()) {
            upgrade::OperationState::Waiting => 0,
            upgrade::OperationState::Ready => 1,
            upgrade::OperationState::Expired => 2,
            upgrade::OperationState::Executed => 3,
            upgrade::OperationState::Cancelled => 4,
        })
    }

    pub fn guardian(env: Env) -> Result<Address, Error> {
        env.storage()
            .instance()
            .get(&DataKey::Guardian)
            .ok_or(Error::NotInitialized)
    }

    // ------------------------------------------------------------ internals

    fn paused_at(env: &Env) -> Option<u64> {
        env.storage().instance().get(&DataKey::PausedAt)
    }

    fn operation(env: &Env, id: u64) -> Result<ScheduledUpgrade, Error> {
        env.storage()
            .persistent()
            .get(&DataKey::Operation(id))
            .ok_or(Error::UpgradeNotFound)
    }

    fn save(env: &Env, id: u64, op: &ScheduledUpgrade) {
        let key = DataKey::Operation(id);
        env.storage().persistent().set(&key, op);
        env.storage().persistent().extend_ttl(
            &key,
            ttl::PERSISTENT_THRESHOLD_LEDGERS,
            ttl::PERSISTENT_BUMP_LEDGERS,
        );
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
}


// ---------------------------------------------------------------- upgrades
//
// Soroban contracts replace their own bytecode; there is no proxy. So each one names an
// upgrader once — in production the timelock — and only that address can swap the code.
// The 72-hour delay and the guardian's brake live there, in front of every upgrade in the
// system.

#[contractimpl]
impl UpgradeTimelock {
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

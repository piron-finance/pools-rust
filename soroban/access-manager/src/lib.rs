//! Role-based access control with timelocked grants.
//!
//! Ported from `src/AccessManager.sol`. Every other Piron contract authorises against
//! this one, so it is the single place where "who may do what" is decided.
//!
//! The shape worth understanding: during deployment roles are granted freely, because
//! nothing is live yet. `finalize_deployment` closes that window permanently. Afterwards
//! a grant must be proposed, wait out 24 hours, and be executed by the multisig — a
//! compromised admin key cannot mint itself an SPV role and drain a pool in one
//! transaction. Renouncing is disabled outright: a renounced admin role is an
//! unrecoverable protocol.

#![no_std]

use soroban_sdk::{contract, contracterror, contractevent, contractimpl, contracttype, Address, Env};

use piron_core::governance::roles::{self, ProposalState, Role, RoleProposal};

const LEDGERS_PER_DAY: u32 = 17_280;
const BUMP: u32 = LEDGERS_PER_DAY * 60;
const THRESHOLD: u32 = LEDGERS_PER_DAY * 45;

#[contracterror]
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
#[repr(u32)]
pub enum Error {
    RoleDelayNotElapsed = 602,
    ProposalNotFound = 603,
    ProposalAlreadyExecuted = 604,
    ProposalCancelled = 605,
    DeploymentFinalized = 606,
    DeploymentNotFinalized = 607,
    RoleNotHeld = 608,
    CannotRenounce = 609,
    ProtocolPaused = 614,
    UnknownRole = 615,
    NotInitialized = 700,
    Unauthorized = 701,
    InvalidAccount = 708,
    AlreadyBound = 715,
}

impl From<piron_core::Error> for Error {
    fn from(err: piron_core::Error) -> Self {
        match err {
            piron_core::Error::RoleDelayNotElapsed => Error::RoleDelayNotElapsed,
            piron_core::Error::ProposalAlreadyExecuted => Error::ProposalAlreadyExecuted,
            piron_core::Error::ProposalCancelled => Error::ProposalCancelled,
            piron_core::Error::UnknownRole => Error::UnknownRole,
            // Nothing else in core is reachable from this contract's call paths.
            _ => Error::Unauthorized,
        }
    }
}

#[contracttype]
#[derive(Clone)]
pub struct StoredProposal {
    pub role: u32,
    pub account: Address,
    pub proposed_at: u64,
    /// 0 = pending, 1 = executed, 2 = cancelled.
    pub state: u32,
}

#[contracttype]
#[derive(Clone)]
pub enum DataKey {
    /// Whether `account` holds `role`.
    Holds(u32, Address),
    Proposal(u64),
    NextProposalId,
    DeploymentComplete,
    Paused,
}

#[contractevent]
pub struct RoleGranted {
    #[topic]
    pub account: Address,
    pub role: u32,
}

#[contractevent]
pub struct RoleRevoked {
    #[topic]
    pub account: Address,
    pub role: u32,
}

#[contractevent]
pub struct RoleProposed {
    #[topic]
    pub account: Address,
    pub proposal_id: u64,
    pub role: u32,
    pub executable_at: u64,
}

#[contractevent]
pub struct RoleProposalExecuted {
    pub proposal_id: u64,
}

#[contractevent]
pub struct RoleProposalCancelled {
    pub proposal_id: u64,
}

#[contractevent]
pub struct DeploymentFinalised {
    pub timestamp: u64,
}

#[contractevent]
pub struct EmergencyPause {
    #[topic]
    pub pauser: Address,
    pub timestamp: u64,
}

#[contractevent]
pub struct EmergencyUnpause {
    #[topic]
    pub unpauser: Address,
    pub timestamp: u64,
}

#[contract]
pub struct AccessManager;

#[contractimpl]
impl AccessManager {
    /// Seeds the initial roles.
    ///
    /// Emergency and multisig must differ from the admin: an emergency wallet that is
    /// also the admin can unpause its own pause, and a multisig that is also the admin
    /// makes the timelock decorative.
    pub fn __constructor(
        env: Env,
        admin: Address,
        spv: Address,
        operator: Address,
        emergency: Address,
        multisig: Address,
    ) -> Result<(), Error> {
        if emergency == admin || multisig == admin {
            return Err(Error::InvalidAccount);
        }

        Self::set_role(&env, Role::DefaultAdmin, &admin, true);
        Self::set_role(&env, Role::Operator, &admin, true);
        Self::set_role(&env, Role::PoolCreator, &admin, true);
        Self::set_role(&env, Role::Operator, &operator, true);
        Self::set_role(&env, Role::Spv, &spv, true);
        Self::set_role(&env, Role::Emergency, &emergency, true);
        Self::set_role(&env, Role::MultisigAdmin, &multisig, true);

        env.storage()
            .instance()
            .set(&DataKey::DeploymentComplete, &false);
        env.storage().instance().set(&DataKey::Paused, &false);
        env.storage().instance().set(&DataKey::NextProposalId, &1u64);
        env.storage().instance().extend_ttl(THRESHOLD, BUMP);
        Ok(())
    }

    // ------------------------------------------------------------ queries

    pub fn has_role(env: Env, role: u32, account: Address) -> bool {
        env.storage()
            .instance()
            .get(&DataKey::Holds(role, account))
            .unwrap_or(false)
    }

    /// Asserts `account` holds `role` and the protocol is not paused.
    ///
    /// The single entry point other contracts call. Bundling the pause check in means a
    /// contract cannot accidentally honour a role while the protocol is halted.
    pub fn require_role(env: Env, role: u32, account: Address) -> Result<(), Error> {
        if Self::is_paused(env.clone()) {
            return Err(Error::ProtocolPaused);
        }
        if !Self::has_role(env, role, account) {
            return Err(Error::RoleNotHeld);
        }
        Ok(())
    }

    pub fn is_paused(env: Env) -> bool {
        env.storage()
            .instance()
            .get(&DataKey::Paused)
            .unwrap_or(false)
    }

    pub fn deployment_complete(env: Env) -> bool {
        env.storage()
            .instance()
            .get(&DataKey::DeploymentComplete)
            .unwrap_or(false)
    }

    // ------------------------------------------------------------ deployment window

    /// Grants a role immediately. Only before the deployment window closes.
    pub fn grant_during_deployment(
        env: Env,
        caller: Address,
        role: u32,
        account: Address,
    ) -> Result<(), Error> {
        Self::require_admin(&env, &caller)?;
        if Self::deployment_complete(env.clone()) {
            return Err(Error::DeploymentFinalized);
        }
        let role = Role::from_u32(role)?;
        Self::set_role(&env, role, &account, true);
        RoleGranted {
            account,
            role: role as u32,
        }
        .publish(&env);
        Ok(())
    }

    /// Closes the deployment window for good. Every later grant goes through the timelock.
    pub fn finalize_deployment(env: Env, caller: Address) -> Result<(), Error> {
        Self::require_admin(&env, &caller)?;
        if Self::deployment_complete(env.clone()) {
            return Err(Error::DeploymentFinalized);
        }
        env.storage()
            .instance()
            .set(&DataKey::DeploymentComplete, &true);
        DeploymentFinalised {
            timestamp: env.ledger().timestamp(),
        }
        .publish(&env);
        Ok(())
    }

    // ------------------------------------------------------------ timelocked grants

    /// Proposes a role grant. Executable by the multisig after the delay.
    pub fn propose_role_grant(
        env: Env,
        caller: Address,
        role: u32,
        account: Address,
    ) -> Result<u64, Error> {
        Self::require_admin(&env, &caller)?;
        if !Self::deployment_complete(env.clone()) {
            return Err(Error::DeploymentNotFinalized);
        }
        let role = Role::from_u32(role)?;

        let id: u64 = env
            .storage()
            .instance()
            .get(&DataKey::NextProposalId)
            .unwrap_or(1);
        env.storage()
            .instance()
            .set(&DataKey::NextProposalId, &(id + 1));

        let now = env.ledger().timestamp();
        env.storage().persistent().set(
            &DataKey::Proposal(id),
            &StoredProposal {
                role: role as u32,
                account: account.clone(),
                proposed_at: now,
                state: 0,
            },
        );
        env.storage().persistent().extend_ttl(
            &DataKey::Proposal(id),
            THRESHOLD,
            BUMP,
        );

        RoleProposed {
            account,
            proposal_id: id,
            role: role as u32,
            executable_at: now + roles::ROLE_GRANT_DELAY,
        }
        .publish(&env);
        Ok(id)
    }

    /// Executes a matured proposal. Multisig only.
    pub fn execute_role_grant(env: Env, caller: Address, proposal_id: u64) -> Result<(), Error> {
        Self::require_multisig(&env, &caller)?;

        let mut stored = Self::proposal(&env, proposal_id)?;
        let role = Role::from_u32(stored.role)?;
        roles::validate_proposal_execution(
            &RoleProposal {
                role,
                proposed_at: stored.proposed_at,
                state: state_from_u32(stored.state),
            },
            env.ledger().timestamp(),
        )?;

        stored.state = 1;
        env.storage()
            .persistent()
            .set(&DataKey::Proposal(proposal_id), &stored);
        Self::set_role(&env, role, &stored.account, true);

        RoleGranted {
            account: stored.account,
            role: stored.role,
        }
        .publish(&env);
        RoleProposalExecuted { proposal_id }.publish(&env);
        Ok(())
    }

    /// Withdraws a proposal before it is executed. Admin or multisig.
    pub fn cancel_role_grant(env: Env, caller: Address, proposal_id: u64) -> Result<(), Error> {
        caller.require_auth();
        let is_admin = Self::has_role(env.clone(), Role::DefaultAdmin as u32, caller.clone());
        let is_multisig = Self::has_role(env.clone(), Role::MultisigAdmin as u32, caller);
        if !is_admin && !is_multisig {
            return Err(Error::Unauthorized);
        }

        let mut stored = Self::proposal(&env, proposal_id)?;
        if stored.state == 1 {
            return Err(Error::ProposalAlreadyExecuted);
        }
        stored.state = 2;
        env.storage()
            .persistent()
            .set(&DataKey::Proposal(proposal_id), &stored);

        RoleProposalCancelled { proposal_id }.publish(&env);
        Ok(())
    }

    /// Removes a role. Multisig only, and immediate — revocation is the safe direction,
    /// so it does not wait out a delay the way granting does.
    pub fn revoke_role(
        env: Env,
        caller: Address,
        role: u32,
        account: Address,
    ) -> Result<(), Error> {
        Self::require_multisig(&env, &caller)?;
        let role = Role::from_u32(role)?;
        if !Self::has_role(env.clone(), role as u32, account.clone()) {
            return Err(Error::RoleNotHeld);
        }
        Self::set_role(&env, role, &account, false);
        RoleRevoked {
            account,
            role: role as u32,
        }
        .publish(&env);
        Ok(())
    }

    /// Always fails.
    ///
    /// Present so the impossibility is explicit in the interface rather than an absence
    /// someone might later "fix". An admin who can renounce can brick the protocol.
    pub fn renounce_role(_env: Env, _caller: Address, _role: u32) -> Result<(), Error> {
        Err(Error::CannotRenounce)
    }

    // ------------------------------------------------------------ emergency

    /// Halts the protocol. Admin or emergency.
    pub fn pause(env: Env, caller: Address) -> Result<(), Error> {
        caller.require_auth();
        env.storage().instance().extend_ttl(THRESHOLD, BUMP);
        if !Self::caller_can(&env, &caller, |r| r.can_pause()) {
            return Err(Error::Unauthorized);
        }
        env.storage().instance().set(&DataKey::Paused, &true);
        EmergencyPause {
            pauser: caller,
            timestamp: env.ledger().timestamp(),
        }
        .publish(&env);
        Ok(())
    }

    /// Resumes the protocol. Admin only — whoever hit the brake should not release it.
    pub fn unpause(env: Env, caller: Address) -> Result<(), Error> {
        caller.require_auth();
        env.storage().instance().extend_ttl(THRESHOLD, BUMP);
        if !Self::caller_can(&env, &caller, |r| r.can_unpause()) {
            return Err(Error::Unauthorized);
        }
        env.storage().instance().set(&DataKey::Paused, &false);
        EmergencyUnpause {
            unpauser: caller,
            timestamp: env.ledger().timestamp(),
        }
        .publish(&env);
        Ok(())
    }

    // ------------------------------------------------------------ internals

    fn set_role(env: &Env, role: Role, account: &Address, held: bool) {
        let key = DataKey::Holds(role as u32, account.clone());
        if held {
            env.storage().instance().set(&key, &true);
        } else {
            env.storage().instance().set(&key, &false);
        }
        env.storage().instance().extend_ttl(THRESHOLD, BUMP);
    }

    fn proposal(env: &Env, id: u64) -> Result<StoredProposal, Error> {
        env.storage()
            .persistent()
            .get(&DataKey::Proposal(id))
            .ok_or(Error::ProposalNotFound)
    }

    fn caller_can(env: &Env, caller: &Address, predicate: fn(Role) -> bool) -> bool {
        for value in 0..Role::COUNT {
            if let Ok(role) = Role::from_u32(value) {
                if predicate(role) && Self::has_role(env.clone(), value, caller.clone()) {
                    return true;
                }
            }
        }
        false
    }

    fn require_admin(env: &Env, caller: &Address) -> Result<(), Error> {
        caller.require_auth();
        env.storage().instance().extend_ttl(THRESHOLD, BUMP);
        if !Self::has_role(env.clone(), Role::DefaultAdmin as u32, caller.clone()) {
            return Err(Error::Unauthorized);
        }
        Ok(())
    }

    fn require_multisig(env: &Env, caller: &Address) -> Result<(), Error> {
        caller.require_auth();
        env.storage().instance().extend_ttl(THRESHOLD, BUMP);
        if !Self::has_role(env.clone(), Role::MultisigAdmin as u32, caller.clone()) {
            return Err(Error::Unauthorized);
        }
        Ok(())
    }
}

fn state_from_u32(value: u32) -> ProposalState {
    match value {
        1 => ProposalState::Executed,
        2 => ProposalState::Cancelled,
        _ => ProposalState::Pending,
    }
}


// ---------------------------------------------------------------- upgrades
//
// Soroban contracts replace their own bytecode; there is no proxy. So each one names an
// upgrader once — in production the timelock — and only that address can swap the code.
// The 72-hour delay and the guardian's brake live there, in front of every upgrade in the
// system.

#[contractimpl]
impl AccessManager {
    /// Names the contract permitted to upgrade this one. Callable once.
    ///
    /// One-way on purpose: a contract whose upgrader could be repointed has no timelock,
    /// only the appearance of one.
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

    /// Who may upgrade this contract, if anyone yet.
    pub fn upgrader(env: Env) -> Option<Address> {
        piron_types::upgradeable::upgrader(&env)
    }
}

#[cfg(test)]
mod test;

//! Role definitions and the timelock on granting them.
//!
//! Ported from `src/AccessManager.sol`.
//!
//! The design point: after deployment is finalised, no role can be granted immediately.
//! A grant is proposed, waits out [`ROLE_GRANT_DELAY`], and is then executed by the
//! multisig — so a compromised admin key cannot mint itself an SPV role and drain a pool
//! in the same transaction. Renouncing is disabled outright, because a renounced admin
//! role is an unrecoverable protocol.

use crate::error::Error;

/// Delay between proposing a role grant and being able to execute it.
pub const ROLE_GRANT_DELAY: u64 = 24 * 60 * 60;

/// The seven roles. Ordering is fixed: it is the wire encoding on every runtime.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u32)]
pub enum Role {
    /// System configuration, fee parameters, asset and SPV approval.
    DefaultAdmin = 0,
    /// Role revocation, implementation approval, upgrade authorisation.
    MultisigAdmin = 1,
    /// Pool operations, queue processing, maturity, tier configuration.
    Operator = 2,
    /// Capital deployment and return.
    Spv = 3,
    /// Pool deployment. Held by factories and managers.
    PoolCreator = 4,
    /// Pool creation via a factory contract.
    Factory = 5,
    /// Emergency pause. Cannot unpause.
    Emergency = 6,
}

impl Role {
    pub const COUNT: u32 = 7;

    pub fn from_u32(value: u32) -> Result<Self, Error> {
        Ok(match value {
            0 => Role::DefaultAdmin,
            1 => Role::MultisigAdmin,
            2 => Role::Operator,
            3 => Role::Spv,
            4 => Role::PoolCreator,
            5 => Role::Factory,
            6 => Role::Emergency,
            _ => return Err(Error::UnknownRole),
        })
    }

    /// Whether this role may pause the protocol.
    ///
    /// PARITY: the Solidity keeps a separate `emergencyPausers` map seeded with the admin
    /// and the emergency wallet. Expressed here as a property of the role, which is the
    /// same set without a second thing to keep in sync.
    pub fn can_pause(self) -> bool {
        matches!(self, Role::DefaultAdmin | Role::Emergency)
    }

    /// Unpausing is admin-only: whoever hit the brake should not also release it.
    pub fn can_unpause(self) -> bool {
        self == Role::DefaultAdmin
    }
}

/// Where a proposed grant has got to.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProposalState {
    Pending,
    Executed,
    Cancelled,
}

/// A role grant waiting out its delay.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RoleProposal {
    pub role: Role,
    pub proposed_at: u64,
    pub state: ProposalState,
}

impl RoleProposal {
    pub fn new(role: Role, now: u64) -> Self {
        Self {
            role,
            proposed_at: now,
            state: ProposalState::Pending,
        }
    }

    /// When this proposal becomes executable.
    pub fn executable_at(&self) -> u64 {
        self.proposed_at.saturating_add(ROLE_GRANT_DELAY)
    }
}

/// Checks a proposal may be executed now.
pub fn validate_proposal_execution(proposal: &RoleProposal, now: u64) -> Result<(), Error> {
    match proposal.state {
        ProposalState::Executed => return Err(Error::ProposalAlreadyExecuted),
        ProposalState::Cancelled => return Err(Error::ProposalCancelled),
        ProposalState::Pending => {}
    }
    if now < proposal.executable_at() {
        return Err(Error::RoleDelayNotElapsed);
    }
    Ok(())
}

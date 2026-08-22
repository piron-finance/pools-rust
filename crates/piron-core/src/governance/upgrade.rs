//! The timelock on upgrades, and the brake on the timelock.
//!
//! Ported from `src/governance/TimelockController.sol` and
//! `src/governance/UpgradeGuardian.sol`.
//!
//! Three delays, each doing a different job:
//!
//! - [`UPGRADE_DELAY`] — 72 hours between scheduling an upgrade and being able to run it.
//!   Long enough that anyone holding a position can read the proposed code and leave.
//! - [`GRACE_PERIOD`] — 7 days after that, then the operation expires. A stale approval is
//!   not an approval; code reviewed three months ago has to go round again.
//! - [`MAX_PAUSE_DURATION`] — 30 days, after which a guardian's emergency pause lapses on
//!   its own. A guardian can stop an upgrade; a guardian cannot freeze governance forever.

use crate::error::Error;

/// Time between scheduling an upgrade and its execution window opening.
pub const UPGRADE_DELAY: u64 = 72 * 60 * 60;

/// How long the window stays open before the operation expires.
pub const GRACE_PERIOD: u64 = 7 * 24 * 60 * 60;

/// How long a guardian's pause can hold before it lapses.
pub const MAX_PAUSE_DURATION: u64 = 30 * 24 * 60 * 60;

/// Where a scheduled upgrade stands.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OperationState {
    /// Scheduled, still inside the delay.
    Waiting,
    /// Inside the execution window.
    Ready,
    /// The window closed without it being run.
    Expired,
    Executed,
    Cancelled,
}

/// An upgrade waiting on the clock.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct UpgradeOperation {
    pub scheduled_at: u64,
    pub executed: bool,
    pub cancelled: bool,
}

impl UpgradeOperation {
    pub fn new(now: u64) -> Self {
        Self {
            scheduled_at: now,
            executed: false,
            cancelled: false,
        }
    }

    /// When the execution window opens.
    pub fn ready_at(&self) -> u64 {
        self.scheduled_at.saturating_add(UPGRADE_DELAY)
    }

    /// When it closes.
    pub fn expires_at(&self) -> u64 {
        self.ready_at().saturating_add(GRACE_PERIOD)
    }

    pub fn state(&self, now: u64) -> OperationState {
        if self.cancelled {
            return OperationState::Cancelled;
        }
        if self.executed {
            return OperationState::Executed;
        }
        if now < self.ready_at() {
            return OperationState::Waiting;
        }
        if now > self.expires_at() {
            return OperationState::Expired;
        }
        OperationState::Ready
    }
}

/// Whether a guardian's pause, set at `paused_at`, is still holding.
///
/// A pause that has run past [`MAX_PAUSE_DURATION`] is treated as lifted without anyone
/// having to lift it — otherwise a lost guardian key would freeze upgrades permanently.
pub fn pause_is_active(paused_at: Option<u64>, now: u64) -> bool {
    match paused_at {
        Some(at) => now < at.saturating_add(MAX_PAUSE_DURATION),
        None => false,
    }
}

/// Checks an upgrade may run right now.
pub fn validate_execution(
    operation: &UpgradeOperation,
    paused_at: Option<u64>,
    now: u64,
) -> Result<(), Error> {
    if pause_is_active(paused_at, now) {
        return Err(Error::UpgradesPaused);
    }
    match operation.state(now) {
        OperationState::Ready => Ok(()),
        OperationState::Waiting => Err(Error::UpgradeDelayNotElapsed),
        OperationState::Expired => Err(Error::UpgradeExpired),
        OperationState::Executed => Err(Error::UpgradeAlreadyExecuted),
        OperationState::Cancelled => Err(Error::UpgradeCancelled),
    }
}

/// Checks an upgrade may be cancelled.
pub fn validate_cancellation(operation: &UpgradeOperation) -> Result<(), Error> {
    if operation.executed {
        return Err(Error::UpgradeAlreadyExecuted);
    }
    if operation.cancelled {
        return Err(Error::UpgradeCancelled);
    }
    Ok(())
}

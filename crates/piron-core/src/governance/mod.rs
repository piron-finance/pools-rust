//! Protocol-wide governance: who may act, and how fees are split.
//!
//! Ported from `src/AccessManager.sol`, `src/FeeManager.sol` and `src/PoolRegistry.sol`.
//! These are shared services — all three pool types authorise against the same roles and
//! pay into the same fee accounting.

pub mod fees;
pub mod reserve;
pub mod roles;
pub mod upgrade;

pub use fees::{split_fee, validate_split, FeeAllocation, FeeSplit, FeeType};
pub use reserve::{
    available_for_loan, available_to_sweep, split_yield, validate_loan, validate_repayment,
    validate_shortfall_cover, validate_sweep, ReserveConfig, ReserveState, YieldSplit,
};
pub use upgrade::{
    pause_is_active, validate_cancellation, validate_execution, OperationState, UpgradeOperation,
    GRACE_PERIOD, MAX_PAUSE_DURATION, UPGRADE_DELAY,
};
pub use roles::{
    validate_proposal_execution, Role, RoleProposal, ProposalState, ROLE_GRANT_DELAY,
};

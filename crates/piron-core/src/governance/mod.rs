//! Protocol-wide governance: who may act, and how fees are split.
//!
//! Ported from `src/AccessManager.sol`, `src/FeeManager.sol` and `src/PoolRegistry.sol`.
//! These are shared services — all three pool types authorise against the same roles and
//! pay into the same fee accounting.

pub mod fees;
pub mod roles;

pub use fees::{split_fee, validate_split, FeeAllocation, FeeSplit, FeeType};
pub use roles::{
    validate_proposal_execution, Role, RoleProposal, ProposalState, ROLE_GRANT_DELAY,
};

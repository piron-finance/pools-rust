//! Stable Yield pools: a revolving, NAV-priced portfolio.
//!
//! Ported from `src/StableYieldManager.sol`, `src/libraries/StableYieldNAVLibrary.sol`
//! and `src/types/IStableYieldTypes.sol`.
//!
//! Unlike the other two pool types there is no single deal and no fixed term. Capital
//! rolls in and out continuously, priced against what the pool holds, and withdrawals
//! queue when the manager has more deployed than liquid.

/// Most instruments one pool may hold at once.
///
/// NAV is the sum over every holding, so the portfolio has to fit inside one Soroban
/// footprint and one Solana account list. Bounding it here makes that a protocol rule
/// rather than a runtime surprise — the same reasoning as `MAX_COUPON_SCHEDULE`.
pub const MAX_INSTRUMENTS: u32 = 32;

/// Ceiling on a quoted annual coupon, in basis points. Fixed income does not pay 500%, so
/// a rate above this is a fat finger, and NAV accrues against it until someone notices.
pub const MAX_ANNUAL_COUPON_RATE: u32 = 5_000;

/// Ceiling on coupon payments per year. Monthly is the shortest schedule Piron buys.
pub const MAX_COUPON_FREQUENCY: u32 = 12;

pub mod nav;
pub mod queue;
pub mod types;
pub mod validate;

pub use nav::{
    assets_for_shares, discounted_value, gross_asset_value, instrument_value,
    interest_bearing_value, nav_per_share, pool_nav, shares_for_deposit, NAV_PRECISION,
    SECONDS_PER_YEAR,
};
pub use queue::{
    enqueue, is_expired, process_batch, quote_withdrawal, BatchOutcome, WithdrawalQuote,
    MAX_WITHDRAWAL_BATCH,
};
pub use types::{
    AllocationStatus, InstrumentHolding, InstrumentType, PendingAllocation, ReserveConfig,
    WithdrawalQueue, WithdrawalRequest,
};
pub use validate::{
    deployable_amount, required_reserve, validate_deployment, validate_deposit,
    validate_holding_period, validate_withdrawal_request,
};

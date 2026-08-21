//! Types for Stable Yield (revolving, NAV-priced) pools.
//!
//! Ported from `src/types/IStableYieldTypes.sol`.

use crate::math::Amount;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InstrumentType {
    Discounted,
    InterestBearing,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AllocationStatus {
    Pending,
    Invested,
    Returned,
    Matured,
    Cancelled,
}

/// One instrument the pool holds.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct InstrumentHolding {
    pub instrument_type: InstrumentType,
    pub purchase_price: Amount,
    pub face_value: Amount,
    /// Unix seconds.
    pub purchase_date: u64,
    pub maturity_date: u64,
    /// Basis points per year. Interest-bearing only.
    pub annual_coupon_rate: u32,
    /// Coupons per year. Interest-bearing only; must not be zero.
    pub coupon_frequency: u32,
    pub next_coupon_due_date: u64,
    pub coupons_paid: u32,
    pub is_active: bool,
}

/// A request to leave the pool, waiting for liquidity.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct WithdrawalRequest {
    pub shares: Amount,
    pub request_time: u64,
    /// After this, the request may be cancelled rather than served.
    pub deadline: u64,
    /// Value quoted when the request was made, at that moment's NAV.
    pub estimated_value: Amount,
    pub fee_amount: Amount,
    pub processed: bool,
    pub processed_time: u64,
}

/// Cursor over the pending-withdrawal ring.
///
/// `head` is the next request to serve, `tail` the next slot to write. Requests are
/// served strictly in order, so a large one cannot be skipped in favour of a small one
/// that happens to fit.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct WithdrawalQueue {
    pub head: u64,
    pub tail: u64,
    pub total_pending_value: Amount,
}

impl WithdrawalQueue {
    pub fn len(&self) -> u64 {
        self.tail.saturating_sub(self.head)
    }

    pub fn is_empty(&self) -> bool {
        self.head >= self.tail
    }
}

/// How much of the pool must stay liquid.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ReserveConfig {
    /// A floor in asset units, whatever the pool's size.
    pub min_absolute_reserve: Amount,
    /// A floor as a share of NAV.
    pub reserve_ratio_bps: u32,
}

/// Capital earmarked for an SPV but not yet deployed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PendingAllocation {
    pub amount: Amount,
    pub used_amount: Amount,
    pub returned_amount: Amount,
    pub created_at: u64,
    pub expires_at: u64,
    pub status: AllocationStatus,
}

//! Protocol types for Single-Asset (deal) pools.
//!
//! Ported from `src/types/IPoolTypes.sol`. These are plain Rust: no `contracttype`
//! derive, no host types. The contract crate keeps its own storage-shaped mirrors and
//! converts at the call boundary, which is what lets this crate stay platform-free.

use crate::math::Amount;
use crate::schedule::CouponSchedule;

/// How the instrument earns: a discount accreting to face value, or periodic coupons.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InstrumentType {
    Discounted,
    InterestBearing,
}

/// Pool lifecycle. Order matches the Solidity enum so numeric values agree across chains.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PoolStatus {
    Funding,
    Filled,
    PendingInvestment,
    Invested,
    Matured,
    Withdrawn,
    Emergency,
}

/// Terms of the deal. Fixed once funding opens.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PoolConfig {
    pub instrument_type: InstrumentType,
    pub face_value: Amount,
    pub purchase_price: Amount,
    pub target_raise: Amount,
    /// Unix seconds. Funding closes here.
    pub epoch_end_time: u64,
    /// Unix seconds.
    pub maturity_date: u64,
    /// Coupon dates and rates. Empty for discounted instruments.
    pub coupons: CouponSchedule,
    /// Basis points. Must be below `BASIS_POINTS`.
    pub discount_rate: u32,
    /// Basis points of `target_raise` that must be raised for the deal to proceed.
    ///
    /// PARITY: the Solidity declares this `uint256` but uses it as basis points
    /// (`targetRaise * minimumFundingThreshold / 10000`) and validates it `<= 10000` in
    /// `Manager.sol`. Typed here as what it is.
    pub minimum_funding_threshold: u32,
    pub min_investment: Amount,
    pub withdrawal_fee_bps: u32,
}

/// Running state of a pool.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PoolData {
    pub config: PoolConfig,
    pub status: PoolStatus,
    pub total_raised: Amount,
    pub actual_invested: Amount,
    pub total_discount_earned: Amount,
    pub total_coupons_received: Amount,
    pub total_coupons_distributed: Amount,
    pub total_coupons_claimed: Amount,
    pub funds_withdrawn_by_spv: Amount,
    pub funds_returned_by_spv: Amount,
    pub total_fees_collected: Amount,
}

impl PoolData {
    /// A pool at the moment of creation, before any deposit.
    pub fn new(config: PoolConfig) -> Self {
        Self {
            config,
            status: PoolStatus::Funding,
            total_raised: 0,
            actual_invested: 0,
            total_discount_earned: 0,
            total_coupons_received: 0,
            total_coupons_distributed: 0,
            total_coupons_claimed: 0,
            funds_withdrawn_by_spv: 0,
            funds_returned_by_spv: 0,
            total_fees_collected: 0,
        }
    }

    pub fn is_discounted(&self) -> bool {
        self.config.instrument_type == InstrumentType::Discounted
    }

    pub fn is_interest_bearing(&self) -> bool {
        self.config.instrument_type == InstrumentType::InterestBearing
    }
}

/// Per-investor accounting held in Persistent storage, one entry per address.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct UserPoolData {
    /// Unix seconds of first deposit.
    pub deposit_time: u64,
    /// Cumulative entitlement already paid out, not the amount of the last claim.
    pub coupons_claimed: Amount,
}

/// Movement categories, mirrored from the Solidity for the shared event schema.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TransferType {
    Deposit,
    Withdrawal,
    InvestmentToSpv,
    MaturityReturn,
    CouponPayment,
    FeeCollection,
    EmergencyRefund,
}

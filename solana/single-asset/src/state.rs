//! Account layouts and the boundary between Solana types and core types.
//!
//! The shape difference that matters: Soroban stores a key-value map that grows as it
//! likes, while a Solana account is sized once at creation and can only grow 10KB per
//! realloc. So the coupon schedule is a fixed-length array with a count, not a `Vec` —
//! which is why `piron_core::MAX_COUPON_SCHEDULE` exists at all.

use anchor_lang::prelude::*;

use piron_core::types as core_types;
use piron_core::MAX_COUPON_SCHEDULE;

/// PDA seeds. The analogue of a Soroban storage key, except the address is derived
/// rather than looked up, and the caller must pass the derived account in explicitly.
pub const POOL_SEED: &[u8] = b"pool";
pub const POSITION_SEED: &[u8] = b"position";

/// Pool terms and running state. One account per pool, at
/// `[POOL_SEED, pool_id.to_le_bytes()]`.
#[account]
pub struct Pool {
    pub pool_id: u64,
    pub authority: Pubkey,
    pub asset_mint: Pubkey,
    pub vault: Pubkey,
    pub escrow: Pubkey,

    // --- terms ---
    pub instrument_type: u8,
    pub face_value: u64,
    pub purchase_price: u64,
    pub target_raise: u64,
    pub epoch_end_time: i64,
    pub maturity_date: i64,
    pub coupon_count: u8,
    pub coupon_dates: [i64; MAX_COUPON_SCHEDULE],
    pub coupon_rates: [u32; MAX_COUPON_SCHEDULE],
    pub discount_rate: u32,
    pub minimum_funding_threshold: u32,
    pub min_investment: u64,
    pub withdrawal_fee_bps: u32,

    // --- state ---
    pub status: u8,
    pub total_raised: u64,
    pub actual_invested: u64,
    pub total_discount_earned: u64,
    pub total_coupons_received: u64,
    pub total_coupons_distributed: u64,
    pub total_coupons_claimed: u64,
    pub funds_withdrawn_by_spv: u64,
    pub funds_returned_by_spv: u64,
    pub total_fees_collected: u64,
    /// Share supply frozen at settlement. Redemption divides by this, not live supply.
    pub shares_at_maturity: u64,

    pub bump: u8,
}

impl Pool {
    /// Rent-exempt size. Anchor's 8-byte discriminator plus every field.
    ///
    /// Unlike Soroban there is no TTL and no keeper: fund this once at the rent-exempt
    /// minimum and it lives indefinitely.
    pub const LEN: usize = 8
        + 8                                 // pool_id
        + 32 * 4                            // authority, asset_mint, vault, escrow
        + 1 + 8 + 8 + 8 + 8 + 8             // instrument_type, face..target, epoch, maturity
        + 1                                 // coupon_count
        + 8 * MAX_COUPON_SCHEDULE           // coupon_dates
        + 4 * MAX_COUPON_SCHEDULE           // coupon_rates
        + 4 + 4 + 8 + 4                     // discount_rate, funding threshold, min investment, fee bps
        + 1 + 8 * 8                         // status + eight running totals
        + 1; // bump

    /// Builds the core view of this pool.
    ///
    /// Token amounts are `u64` on Solana and `i128` in core. Widening is always safe;
    /// the narrowing direction is checked at the point of return.
    pub fn to_core(&self) -> core::result::Result<core_types::PoolData, piron_core::Error> {
        // The account stores the schedule as two parallel fixed arrays, because an account
        // is sized once at creation and cannot hold a growable Vec. `CouponSchedule` is the
        // same shape, so this rebuilds rather than converts.
        let count = (self.coupon_count as usize).min(MAX_COUPON_SCHEDULE);
        let mut dates = [0u64; MAX_COUPON_SCHEDULE];
        for i in 0..count {
            dates[i] = self.coupon_dates[i].max(0) as u64;
        }
        let coupons = piron_core::schedule::CouponSchedule::from_parts(&dates[..count], &self.coupon_rates[..count])?;

        Ok(core_types::PoolData {
            config: core_types::PoolConfig {
                instrument_type: if self.instrument_type == 0 {
                    core_types::InstrumentType::Discounted
                } else {
                    core_types::InstrumentType::InterestBearing
                },
                face_value: self.face_value as i128,
                purchase_price: self.purchase_price as i128,
                target_raise: self.target_raise as i128,
                epoch_end_time: self.epoch_end_time.max(0) as u64,
                maturity_date: self.maturity_date.max(0) as u64,
                coupons,
                discount_rate: self.discount_rate,
                minimum_funding_threshold: self.minimum_funding_threshold,
                min_investment: self.min_investment as i128,
                withdrawal_fee_bps: self.withdrawal_fee_bps,
            },
            status: status_from_u8(self.status),
            total_raised: self.total_raised as i128,
            actual_invested: self.actual_invested as i128,
            total_discount_earned: self.total_discount_earned as i128,
            total_coupons_received: self.total_coupons_received as i128,
            total_coupons_distributed: self.total_coupons_distributed as i128,
            total_coupons_claimed: self.total_coupons_claimed as i128,
            funds_withdrawn_by_spv: self.funds_withdrawn_by_spv as i128,
            funds_returned_by_spv: self.funds_returned_by_spv as i128,
            total_fees_collected: self.total_fees_collected as i128,
            shares_at_maturity: self.shares_at_maturity as i128,
        })
    }
}

/// Per-investor accounting. One account per (pool, investor), at
/// `[POSITION_SEED, pool.key(), investor.key()]`.
///
/// The Soroban equivalent is a Persistent entry under `DataKey::User(Address)`. Same
/// idea; here the address is derived and the account must be created and rent-funded
/// before first use.
#[account]
pub struct Position {
    pub pool: Pubkey,
    pub owner: Pubkey,
    pub deposit_time: i64,
    pub coupons_claimed: u64,
    pub bump: u8,
}

impl Position {
    pub const LEN: usize = 8 + 32 + 32 + 8 + 8 + 1;
}

/// Numeric values match the Solidity `PoolStatus` enum ordering, same as the Soroban
/// binding. Three runtimes, one numbering.
pub fn status_from_u8(value: u8) -> core_types::PoolStatus {
    match value {
        0 => core_types::PoolStatus::Funding,
        1 => core_types::PoolStatus::Filled,
        2 => core_types::PoolStatus::PendingInvestment,
        3 => core_types::PoolStatus::Invested,
        4 => core_types::PoolStatus::Matured,
        5 => core_types::PoolStatus::Withdrawn,
        _ => core_types::PoolStatus::Emergency,
    }
}

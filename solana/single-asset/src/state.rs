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
#[derive(InitSpace)]
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
    /// Rent-exempt size: Anchor's 8-byte discriminator plus every field.
    ///
    /// Derived rather than hand-counted. The hand-written version was 24 bytes short
    /// within one change of being written — it said "eight running totals" when there
    /// were ten — and an account sized short truncates on write rather than failing
    /// loudly. `InitSpace` recomputes from the struct, so adding a field cannot silently
    /// leave the size behind.
    ///
    /// Unlike Soroban there is no TTL and no keeper: fund this once at the rent-exempt
    /// minimum and it lives indefinitely.
    pub const LEN: usize = 8 + Self::INIT_SPACE;

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
#[derive(InitSpace)]
pub struct Position {
    pub pool: Pubkey,
    pub owner: Pubkey,
    pub deposit_time: i64,
    pub coupons_claimed: u64,
    pub bump: u8,
}

impl Position {
    pub const LEN: usize = 8 + Self::INIT_SPACE;
}

/// Narrows a core amount back into the `u64` an SPL token and an account field hold.
///
/// The widening direction is free and litters this file as `x as i128`, which is exactly
/// what makes `as u64` look reasonable on the way back. It is not: `as` truncates, so a
/// figure that overflowed would be written to an account as a small positive number and
/// every later read would believe it. Core works in `i128` precisely so intermediate
/// products cannot wrap, and that guarantee is worth nothing if the boundary throws it away.
///
/// Negative is a distinct failure from too-large and stays distinct, because a negative
/// amount means a calculation went wrong upstream rather than a pool got big.
pub fn to_u64(value: piron_core::Amount) -> core::result::Result<u64, piron_core::Error> {
    if value < 0 {
        return Err(piron_core::Error::NegativeAmount);
    }
    u64::try_from(value).map_err(|_| piron_core::Error::Overflow)
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

#[cfg(test)]
mod tests {
    use super::*;
    use anchor_lang::prelude::borsh;

    /// `LEN` has to match what a serialised account actually occupies, or `init` allocates
    /// short and writes truncate. The hand-counted version was 24 bytes short within one
    /// change of being written, which is the whole argument for deriving it.
    #[test]
    fn the_declared_size_matches_what_is_written() {
        let pool = Pool {
            pool_id: 1,
            authority: Pubkey::new_unique(),
            asset_mint: Pubkey::new_unique(),
            vault: Pubkey::new_unique(),
            escrow: Pubkey::new_unique(),
            instrument_type: 0,
            face_value: 0,
            purchase_price: 0,
            target_raise: 0,
            epoch_end_time: 0,
            maturity_date: 0,
            coupon_count: 0,
            coupon_dates: [0; MAX_COUPON_SCHEDULE],
            coupon_rates: [0; MAX_COUPON_SCHEDULE],
            discount_rate: 0,
            minimum_funding_threshold: 0,
            min_investment: 0,
            withdrawal_fee_bps: 0,
            status: 0,
            total_raised: 0,
            actual_invested: 0,
            total_discount_earned: 0,
            total_coupons_received: 0,
            total_coupons_distributed: 0,
            total_coupons_claimed: 0,
            funds_withdrawn_by_spv: 0,
            funds_returned_by_spv: 0,
            total_fees_collected: 0,
            shares_at_maturity: 0,
            bump: 0,
        };
        // 8 for the discriminator, which try_to_vec does not include.
        assert_eq!(Pool::LEN, 8 + borsh::to_vec(&pool).unwrap().len());

        let position = Position {
            pool: Pubkey::new_unique(),
            owner: Pubkey::new_unique(),
            deposit_time: 0,
            coupons_claimed: 0,
            bump: 0,
        };
        assert_eq!(Position::LEN, 8 + borsh::to_vec(&position).unwrap().len());
    }

    #[test]
    fn narrowing_refuses_what_it_cannot_represent() {
        assert_eq!(to_u64(0), Ok(0));
        assert_eq!(to_u64(u64::MAX as i128), Ok(u64::MAX));

        // One past what a u64 holds. `as u64` would wrap this to 0 and write it.
        assert_eq!(
            to_u64(u64::MAX as i128 + 1),
            Err(piron_core::Error::Overflow)
        );

        // Negative is its own failure: it means a calculation went wrong upstream, not
        // that a pool got large.
        assert_eq!(to_u64(-1), Err(piron_core::Error::NegativeAmount));
    }

    /// The account holds the schedule as two fixed arrays; core holds a `CouponSchedule`.
    #[test]
    fn the_coupon_schedule_survives_the_round_trip() {
        let mut pool = Pool {
            pool_id: 1,
            authority: Pubkey::new_unique(),
            asset_mint: Pubkey::new_unique(),
            vault: Pubkey::new_unique(),
            escrow: Pubkey::new_unique(),
            instrument_type: 1,
            face_value: 1_000,
            purchase_price: 950,
            target_raise: 1_000,
            epoch_end_time: 100,
            maturity_date: 1_000,
            coupon_count: 3,
            coupon_dates: [0; MAX_COUPON_SCHEDULE],
            coupon_rates: [0; MAX_COUPON_SCHEDULE],
            discount_rate: 0,
            minimum_funding_threshold: 8_000,
            min_investment: 10,
            withdrawal_fee_bps: 50,
            status: 0,
            total_raised: 0,
            actual_invested: 0,
            total_discount_earned: 0,
            total_coupons_received: 0,
            total_coupons_distributed: 0,
            total_coupons_claimed: 0,
            funds_withdrawn_by_spv: 0,
            funds_returned_by_spv: 0,
            total_fees_collected: 0,
            shares_at_maturity: 0,
            bump: 0,
        };
        pool.coupon_dates[..3].copy_from_slice(&[200, 400, 600]);
        pool.coupon_rates[..3].copy_from_slice(&[100, 100, 100]);

        let data = pool.to_core().unwrap();
        assert_eq!(data.config.coupons.len(), 3);
        assert_eq!(data.config.coupons.dates(), &[200, 400, 600]);
        assert_eq!(data.config.coupons.rates(), &[100, 100, 100]);

        // Entries past the count are ignored rather than read as zero-dated coupons.
        assert_eq!(data.config.coupons.iter().count(), 3);
    }
}

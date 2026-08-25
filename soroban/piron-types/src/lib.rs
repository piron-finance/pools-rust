//! Soroban wire types shared across the Piron contracts.
//!
//! Deliberately contains **no** `#[contract]`. A factory needs a manager's constructor
//! types to deploy it, but depending on the manager crate for them drags that contract's
//! exported symbols into the factory's wasm and the two collide at link time. Shared
//! types live here instead, so both sides agree on the layout without either importing
//! the other's code.
//!
//! Each type mirrors a `piron-core` type and converts to it. Core stays platform-free;
//! this crate is the Soroban-shaped face of it.

#![no_std]

use soroban_sdk::{contracttype, Address, Env};

use piron_core::locked::types as lt;
use piron_core::schedule::CouponSchedule;
use piron_core::stable_yield::types as sy;
use piron_core::types as pt;

/// Which kind of pool a registration describes.
///
/// Shared rather than owned by the registry, because the factory has to name it when it
/// registers a pool it just built.
#[contracttype]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PoolType {
    SingleAsset,
    Locked,
    StableYield,
}

/// Addresses permitted to drive a pool.
///
/// A placeholder for the full seven-role `AccessManager`: these are the roles a pool's own
/// lifecycle needs, resolved once at construction rather than looked up per call.
#[contracttype]
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Roles {
    pub admin: Address,
    pub operator: Address,
    pub spv: Address,
    pub emergency: Address,
    pub treasury: Address,
}

// ---------------------------------------------------------------- single asset

#[contracttype]
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct StoredConfig {
    pub instrument_type: u32,
    pub face_value: i128,
    pub purchase_price: i128,
    pub target_raise: i128,
    pub epoch_end_time: u64,
    pub maturity_date: u64,
    // Fully qualified: `#[contracttype]` resolves types textually and cannot see through
    // a `use ... as` alias.
    pub coupon_dates: soroban_sdk::Vec<u64>,
    pub coupon_rates: soroban_sdk::Vec<u32>,
    pub discount_rate: u32,
    pub minimum_funding_threshold: u32,
    pub min_investment: i128,
    pub withdrawal_fee_bps: u32,
}

impl StoredConfig {
    /// Converts to the core view, validating the coupon schedule on the way.
    ///
    /// Fallible because a stored schedule longer than `MAX_COUPON_SCHEDULE` cannot be
    /// represented in core — which construction prevents, but storage is not a place to
    /// assume anything.
    pub fn to_core(&self) -> Result<pt::PoolConfig, piron_core::Error> {
        if self.coupon_dates.len() != self.coupon_rates.len() {
            return Err(piron_core::Error::CouponScheduleMismatch);
        }
        let mut coupons = CouponSchedule::empty();
        for (date, rate) in self.coupon_dates.iter().zip(self.coupon_rates.iter()) {
            coupons.push(date, rate)?;
        }

        Ok(pt::PoolConfig {
            instrument_type: if self.instrument_type == 0 {
                pt::InstrumentType::Discounted
            } else {
                pt::InstrumentType::InterestBearing
            },
            face_value: self.face_value,
            purchase_price: self.purchase_price,
            target_raise: self.target_raise,
            epoch_end_time: self.epoch_end_time,
            maturity_date: self.maturity_date,
            coupons,
            discount_rate: self.discount_rate,
            minimum_funding_threshold: self.minimum_funding_threshold,
            min_investment: self.min_investment,
            withdrawal_fee_bps: self.withdrawal_fee_bps,
        })
    }
}

#[contracttype]
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct StoredState {
    pub status: u32,
    pub total_raised: i128,
    pub actual_invested: i128,
    pub total_discount_earned: i128,
    pub total_coupons_received: i128,
    pub total_coupons_distributed: i128,
    pub total_coupons_claimed: i128,
    pub funds_withdrawn_by_spv: i128,
    pub funds_returned_by_spv: i128,
    pub total_fees_collected: i128,
    /// Share supply frozen at settlement. Redemption divides by this, not live supply.
    pub shares_at_maturity: i128,
}

#[contracttype]
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct StoredUser {
    pub deposit_time: u64,
    pub coupons_claimed: i128,
}

// ---------------------------------------------------------------- locked

#[contracttype]
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct StoredTier {
    pub duration_days: u32,
    pub apy_bps: u32,
    pub early_exit_penalty_bps: u32,
    pub min_deposit: i128,
    pub is_active: bool,
}

impl StoredTier {
    pub fn to_core(&self) -> lt::LockTier {
        lt::LockTier {
            duration_days: self.duration_days,
            apy_bps: self.apy_bps,
            early_exit_penalty_bps: self.early_exit_penalty_bps,
            min_deposit: self.min_deposit,
            is_active: self.is_active,
        }
    }
}

#[contracttype]
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct StoredPosition {
    pub owner: Address,
    pub principal_deposited: i128,
    pub full_interest_amount: i128,
    pub apy_bps_at_deposit: u32,
    /// 0 = upfront, 1 = at maturity.
    pub payment_choice: u32,
    pub interest_paid: bool,
    pub invested_amount: i128,
    pub expected_maturity_payout: i128,
    pub lock_start: u64,
    pub lock_end: u64,
    pub tier_index: u32,
    /// Matches `PositionStatus` ordering.
    pub status: u32,
    pub actual_payout: i128,
    pub penalty_paid: i128,
    pub interest_earned: i128,
    pub auto_rollover: bool,
}

impl StoredPosition {
    pub fn to_core(&self) -> lt::Position {
        lt::Position {
            principal_deposited: self.principal_deposited,
            full_interest_amount: self.full_interest_amount,
            apy_bps_at_deposit: self.apy_bps_at_deposit,
            payment_choice: payment_from_u32(self.payment_choice),
            interest_paid: self.interest_paid,
            invested_amount: self.invested_amount,
            expected_maturity_payout: self.expected_maturity_payout,
            lock_start: self.lock_start,
            lock_end: self.lock_end,
            tier_index: self.tier_index,
            status: status_from_u32(self.status),
            actual_payout: self.actual_payout,
            penalty_paid: self.penalty_paid,
            interest_earned: self.interest_earned,
            auto_rollover: self.auto_rollover,
        }
    }

    pub fn from_core(owner: Address, p: &lt::Position) -> Self {
        Self {
            owner,
            principal_deposited: p.principal_deposited,
            full_interest_amount: p.full_interest_amount,
            apy_bps_at_deposit: p.apy_bps_at_deposit,
            payment_choice: payment_to_u32(p.payment_choice),
            interest_paid: p.interest_paid,
            invested_amount: p.invested_amount,
            expected_maturity_payout: p.expected_maturity_payout,
            lock_start: p.lock_start,
            lock_end: p.lock_end,
            tier_index: p.tier_index,
            status: status_to_u32(p.status),
            actual_payout: p.actual_payout,
            penalty_paid: p.penalty_paid,
            interest_earned: p.interest_earned,
            auto_rollover: p.auto_rollover,
        }
    }
}

#[contracttype]
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct StoredMetrics {
    pub total_principal_locked: i128,
    pub total_interest_committed: i128,
    pub total_interest_paid_upfront: i128,
    pub interest_pending_maturity: i128,
    /// Capital currently with the SPV.
    pub total_invested: i128,
    pub active_positions: u32,
    pub total_positions: u32,
}

impl StoredMetrics {
    pub fn empty() -> Self {
        Self {
            total_principal_locked: 0,
            total_interest_committed: 0,
            total_interest_paid_upfront: 0,
            interest_pending_maturity: 0,
            total_invested: 0,
            active_positions: 0,
            total_positions: 0,
        }
    }
}

pub fn payment_from_u32(v: u32) -> lt::InterestPayment {
    if v == 0 {
        lt::InterestPayment::Upfront
    } else {
        lt::InterestPayment::AtMaturity
    }
}

pub fn payment_to_u32(p: lt::InterestPayment) -> u32 {
    match p {
        lt::InterestPayment::Upfront => 0,
        lt::InterestPayment::AtMaturity => 1,
    }
}

pub fn status_from_u32(v: u32) -> lt::PositionStatus {
    match v {
        0 => lt::PositionStatus::Active,
        1 => lt::PositionStatus::Matured,
        2 => lt::PositionStatus::Redeemed,
        3 => lt::PositionStatus::EarlyExit,
        _ => lt::PositionStatus::RolledOver,
    }
}

pub fn status_to_u32(s: lt::PositionStatus) -> u32 {
    match s {
        lt::PositionStatus::Active => 0,
        lt::PositionStatus::Matured => 1,
        lt::PositionStatus::Redeemed => 2,
        lt::PositionStatus::EarlyExit => 3,
        lt::PositionStatus::RolledOver => 4,
    }
}

// ---------------------------------------------------------------- stable yield

#[contracttype]
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Config {
    pub min_investment: i128,
    /// Seconds a depositor must stay in before they may queue to leave.
    pub holding_period: u64,
    pub withdrawal_fee_bps: u32,
    pub min_absolute_reserve: i128,
    pub reserve_ratio_bps: u32,
    pub is_active: bool,
}

impl Config {
    pub fn reserve(&self) -> sy::ReserveConfig {
        sy::ReserveConfig {
            min_absolute_reserve: self.min_absolute_reserve,
            reserve_ratio_bps: self.reserve_ratio_bps,
        }
    }
}

#[contracttype]
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct StoredInstrument {
    /// 0 = discounted, 1 = interest-bearing.
    pub instrument_type: u32,
    pub purchase_price: i128,
    pub face_value: i128,
    pub purchase_date: u64,
    pub maturity_date: u64,
    pub annual_coupon_rate: u32,
    pub coupon_frequency: u32,
    pub next_coupon_due_date: u64,
    pub coupons_paid: u32,
    pub is_active: bool,
}

impl StoredInstrument {
    pub fn to_core(&self) -> sy::InstrumentHolding {
        sy::InstrumentHolding {
            instrument_type: if self.instrument_type == 0 {
                sy::InstrumentType::Discounted
            } else {
                sy::InstrumentType::InterestBearing
            },
            purchase_price: self.purchase_price,
            face_value: self.face_value,
            purchase_date: self.purchase_date,
            maturity_date: self.maturity_date,
            annual_coupon_rate: self.annual_coupon_rate,
            coupon_frequency: self.coupon_frequency,
            next_coupon_due_date: self.next_coupon_due_date,
            coupons_paid: self.coupons_paid,
            is_active: self.is_active,
        }
    }
}

#[contracttype]
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct StoredRequest {
    pub owner: Address,
    pub shares: i128,
    pub request_time: u64,
    pub deadline: u64,
    pub estimated_value: i128,
    pub fee_amount: i128,
    pub processed: bool,
    pub processed_time: u64,
}

impl StoredRequest {
    pub fn to_core(&self) -> sy::WithdrawalRequest {
        sy::WithdrawalRequest {
            shares: self.shares,
            request_time: self.request_time,
            deadline: self.deadline,
            estimated_value: self.estimated_value,
            fee_amount: self.fee_amount,
            processed: self.processed,
            processed_time: self.processed_time,
        }
    }
}

/// TTL policy, shared so every contract bumps on the same schedule.
pub mod ttl {
    /// Ledgers in a day at a ~5s close.
    pub const LEDGERS_PER_DAY: u32 = 17_280;
    pub const INSTANCE_BUMP_LEDGERS: u32 = LEDGERS_PER_DAY * 60;
    pub const INSTANCE_THRESHOLD_LEDGERS: u32 = LEDGERS_PER_DAY * 45;
    pub const PERSISTENT_BUMP_LEDGERS: u32 = LEDGERS_PER_DAY * 60;
    pub const PERSISTENT_THRESHOLD_LEDGERS: u32 = LEDGERS_PER_DAY * 45;
    pub const TEMPORARY_BUMP_LEDGERS: u32 = LEDGERS_PER_DAY;

    /// Ledgers a tenor of `days` spans, for sizing keeper work against maturities.
    pub const fn ledgers_for_days(days: u32) -> u32 {
        days * LEDGERS_PER_DAY
    }
}

/// Helper used by the locked pool's metrics arithmetic.
pub fn saturating_add(a: i128, b: i128) -> i128 {
    a.saturating_add(b)
}

/// Unused elsewhere; keeps `Env` in scope for the macro expansions above.
#[allow(dead_code)]
fn _env_marker(_: &Env) {}

/// The upgrade hook every Piron contract carries.
///
/// Soroban contracts replace their own bytecode — there is no proxy — so "who may upgrade
/// this" is a question each contract answers for itself. Each one names an upgrader once
/// and thereafter only that address can swap the code. In production the upgrader is the
/// timelock, which is what puts the 72-hour delay and the guardian's brake in front of
/// every upgrade in the system.
///
/// Binding is one-way. A contract whose upgrader could be repointed has no timelock at
/// all, only the appearance of one.
pub mod upgradeable {
    use soroban_sdk::{symbol_short, Address, BytesN, Env, Symbol};

    const UPGRADER: Symbol = symbol_short!("upgrader");

    /// Names the upgrader. Fails if one is already set.
    pub fn bind(env: &Env, upgrader: &Address) -> Result<(), UpgradeError> {
        if env.storage().instance().has(&UPGRADER) {
            return Err(UpgradeError::AlreadyBound);
        }
        env.storage().instance().set(&UPGRADER, upgrader);
        Ok(())
    }

    pub fn upgrader(env: &Env) -> Option<Address> {
        env.storage().instance().get(&UPGRADER)
    }

    /// Replaces this contract's code. Only the bound upgrader may call it.
    pub fn perform(env: &Env, wasm_hash: BytesN<32>) -> Result<(), UpgradeError> {
        let who: Address = env
            .storage()
            .instance()
            .get(&UPGRADER)
            .ok_or(UpgradeError::NotBound)?;
        who.require_auth();
        env.deployer().update_current_contract_wasm(wasm_hash);
        Ok(())
    }

    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    pub enum UpgradeError {
        NotBound,
        AlreadyBound,
    }
}

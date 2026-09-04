//! Account layouts for stable-yield pools.
//!
//! Both large accounts are zero-copy. Anchor's ordinary `Account<T>` deserialises the
//! whole struct onto the stack, and a Solana stack frame is 4KB: the 256-entry queue is
//! 20,801 bytes and overran by 18,544. `AccountLoader` memory-maps the account instead and
//! hands out references, so nothing is copied and the frame stays small.
//!
//! The cost is that every field has to be plain old data — no `bool`, no enums, no
//! `Option` — which is why flags below are `u8` rather than `bool`.

use anchor_lang::prelude::*;

use piron_core::stable_yield::types as sy;
use piron_core::Amount;

/// Instruments held in the pool account. Fixed because an account is sized once.
pub const MAX_INSTRUMENTS: usize = 32;

/// Capacity of the withdrawal ring.
///
/// A realistic worst case is around 110 concurrent pending withdrawals, which sits almost
/// exactly on a hard limit: an account created through a CPI caps at 10,240 bytes, and at
/// 81 bytes per request that is 110 entries. Sizing the ring at the expected worst case
/// leaves nothing, and a full queue means withdrawal requests revert.
///
/// 256 needs one `init` at the cap plus two reallocs, which is two extra instructions once
/// at pool setup for roughly 0.29 SOL of recoverable rent, and 2.3x headroom.
pub const QUEUE_CAPACITY: usize = 256;

#[account(zero_copy)]
#[repr(C)]
pub struct StableYieldPool {
    pub authority: Pubkey,
    pub spv: Pubkey,
    pub treasury: Pubkey,
    pub asset_mint: Pubkey,
    pub vault: Pubkey,
    pub share_mint: Pubkey,
    pub queue: Pubkey,

    pub pool_id: u64,
    pub min_investment: u64,
    pub holding_period: i64,
    pub min_absolute_reserve: u64,

    /// Capital drawn by the SPV and not yet placed in an instrument.
    ///
    /// NAV counts it: the cash left the vault but still belongs to the pool. The three
    /// lines — instruments, vault, in flight — sum to NAV, and every transition moves
    /// value between them rather than creating any.
    pub in_flight: u64,

    /// Written on every mutation. Solana has no view functions, so a caller reads the
    /// account rather than calling `total_nav`.
    pub total_nav: u64,

    pub withdrawal_fee_bps: u32,
    pub reserve_ratio_bps: u32,

    /// 1 while the pool accepts deposits. A `u8` because zero-copy needs plain old data.
    pub is_active: u8,
    pub instrument_count: u8,
    pub bump: u8,
    pub _pad: [u8; 5],

    pub instruments: [StoredInstrument; MAX_INSTRUMENTS],
}

impl StableYieldPool {
    pub const LEN: usize = 8 + core::mem::size_of::<Self>();

    pub fn active_instruments(&self) -> impl Iterator<Item = sy::InstrumentHolding> + '_ {
        self.instruments[..self.instrument_count as usize]
            .iter()
            .filter(|i| i.is_active != 0)
            .map(|i| i.to_core())
    }
}

#[zero_copy]
#[repr(C)]
#[derive(Default)]
pub struct StoredInstrument {
    /// Which SPV placed it, so per-SPV attribution survives without an allocation record.
    pub spv: Pubkey,
    pub purchase_price: u64,
    pub face_value: u64,
    pub purchase_date: i64,
    pub maturity_date: i64,
    pub next_coupon_due_date: i64,
    pub annual_coupon_rate: u32,
    pub coupon_frequency: u32,
    pub coupons_paid: u32,
    /// 0 discounted, 1 interest-bearing.
    pub instrument_type: u8,
    pub is_active: u8,
    pub _pad: [u8; 2],
}

impl StoredInstrument {
    pub fn to_core(&self) -> sy::InstrumentHolding {
        sy::InstrumentHolding {
            instrument_type: if self.instrument_type == 0 {
                sy::InstrumentType::Discounted
            } else {
                sy::InstrumentType::InterestBearing
            },
            purchase_price: self.purchase_price as Amount,
            face_value: self.face_value as Amount,
            purchase_date: self.purchase_date.max(0) as u64,
            maturity_date: self.maturity_date.max(0) as u64,
            annual_coupon_rate: self.annual_coupon_rate,
            coupon_frequency: self.coupon_frequency,
            next_coupon_due_date: self.next_coupon_due_date.max(0) as u64,
            coupons_paid: self.coupons_paid,
            is_active: self.is_active != 0,
        }
    }
}

/// The withdrawal ring. Its own account, because 256 entries exceed what a CPI can create.
#[account(zero_copy)]
#[repr(C)]
pub struct WithdrawalQueue {
    pub pool: Pubkey,
    /// Next request to serve.
    pub head: u64,
    /// Next slot to write.
    pub tail: u64,
    pub total_pending_value: u64,
    pub bump: u8,
    pub _pad: [u8; 7],
    pub requests: [StoredRequest; QUEUE_CAPACITY],
}

impl WithdrawalQueue {
    pub const LEN: usize = 8 + core::mem::size_of::<Self>();

    /// The most an account can be given in one instruction, at creation or by realloc.
    pub const MAX_STEP: usize = 10_240;

    pub fn slot(&self, index: u64) -> usize {
        (index % QUEUE_CAPACITY as u64) as usize
    }
    pub fn is_full(&self) -> bool {
        self.tail - self.head >= QUEUE_CAPACITY as u64
    }
}

#[zero_copy]
#[repr(C)]
#[derive(Default)]
pub struct StoredRequest {
    pub owner: Pubkey,
    pub shares: u64,
    pub request_time: i64,
    pub deadline: i64,
    pub estimated_value: u64,
    pub fee_amount: u64,
    pub processed_time: i64,
    pub processed: u8,
    pub _pad: [u8; 7],
}

/// Narrows a core amount back into the `u64` a token and an account field hold.
pub fn to_u64(value: Amount) -> core::result::Result<u64, piron_core::Error> {
    if value < 0 {
        return Err(piron_core::Error::NegativeAmount);
    }
    u64::try_from(value).map_err(|_| piron_core::Error::Overflow)
}

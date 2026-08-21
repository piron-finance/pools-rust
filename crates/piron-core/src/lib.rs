//! Piron Pools — platform-independent protocol logic.
//!
//! This crate holds every decision the protocol makes: the arithmetic, the state
//! machine, the validation rules. It holds nothing about *where* it runs.
//!
//! It has no dependencies, allocates nothing, needs no chain to test, and must stay that
//! way — a Soroban contract has no heap allocator, so a single `Vec` in here would fail
//! the wasm build. Balances,
//! total supply and the current time arrive as arguments; storage, authorization,
//! events and token transfers belong to the binding crate that calls this one.
//!
//! The Solidity implementation in `src/` is the reference for v1. Deviations from it
//! are marked `// PARITY:` with a reason.

#![cfg_attr(not(test), no_std)]
#![forbid(unsafe_code)]

pub mod error;
pub mod governance;
pub mod locked;
pub mod math;
pub mod schedule;
pub mod single_asset;
pub mod stable_yield;
pub mod types;

pub use error::Error;
pub use math::Amount;

/// Denominator for every rate in the protocol. Matches `CalculationLibrary.BASIS_POINTS`.
pub const BASIS_POINTS: u32 = 10_000;

/// Maximum number of scheduled coupons on one pool.
///
/// The Solidity has no such bound — an EVM array grows as needed. A Solana account is
/// sized once at creation and can only grow 10KB per realloc, so the schedule length has
/// to be a fixed protocol parameter rather than an implicit assumption. Twenty-four
/// covers quarterly coupons over six years or monthly over two.
///
/// This is a constraint discovered on one runtime and promoted to a protocol rule on all
/// of them, which is the core/binding split doing its job.
pub const MAX_COUPON_SCHEDULE: usize = 24;

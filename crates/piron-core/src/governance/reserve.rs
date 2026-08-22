//! The protocol yield reserve.
//!
//! Ported from `src/escrows/YieldReserveEscrow.sol`.
//!
//! The reserve is where surplus yield collects and where shortfalls are met from. It does
//! three things pools cannot do for themselves: it lends liquidity to a pool that owes a
//! redemption before its capital has come back, it absorbs a loss when an SPV returns less
//! than expected, and it sweeps what is left over to the treasury.
//!
//! The floor is the discipline. Lending and sweeping both stop at it, so the reserve
//! cannot be drained by the ordinary operation of the thing it exists to backstop.

use crate::error::Error;
use crate::math::{self, Amount};
use crate::BASIS_POINTS;

/// How the reserve is parameterised.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ReserveConfig {
    /// Balance the reserve will not lend or sweep below.
    pub min_floor: Amount,
    /// Share of incoming yield that goes straight to the treasury; the rest is retained.
    pub treasury_split_bps: u32,
}

/// Running totals.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct ReserveState {
    pub balance: Amount,
    pub total_loaned: Amount,
    pub total_yield_received: Amount,
    pub total_shortfall_covered: Amount,
    pub total_swept: Amount,
}

/// Incoming yield, divided.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct YieldSplit {
    pub to_treasury: Amount,
    pub retained: Amount,
}

/// Splits incoming yield between the treasury and the reserve.
///
/// The reserve takes the remainder rather than its own rounded share, so the two parts
/// always sum back to the whole.
pub fn split_yield(amount: Amount, config: &ReserveConfig) -> Result<YieldSplit, Error> {
    if amount < 0 {
        return Err(Error::NegativeAmount);
    }
    if config.treasury_split_bps > BASIS_POINTS {
        return Err(Error::InvalidBps);
    }
    let to_treasury = math::bps_of(amount, config.treasury_split_bps)?;
    Ok(YieldSplit {
        to_treasury,
        retained: math::checked_sub(amount, to_treasury)?,
    })
}

/// What the reserve can lend without breaching its floor.
pub fn available_for_loan(state: &ReserveState, config: &ReserveConfig) -> Amount {
    if state.balance <= config.min_floor {
        return 0;
    }
    state.balance - config.min_floor
}

/// What the reserve can sweep to the treasury.
///
/// Outstanding loans are held back as well as the floor: money that is out on loan is
/// owed back to the reserve, not surplus to be swept away.
pub fn available_to_sweep(state: &ReserveState, config: &ReserveConfig) -> Result<Amount, Error> {
    let committed = math::checked_add(config.min_floor, state.total_loaned)?;
    if state.balance <= committed {
        return Ok(0);
    }
    math::checked_sub(state.balance, committed)
}

/// Checks a loan can be made.
pub fn validate_loan(
    amount: Amount,
    state: &ReserveState,
    config: &ReserveConfig,
) -> Result<(), Error> {
    if amount <= 0 {
        return Err(Error::InvalidAmount);
    }
    if amount > available_for_loan(state, config) {
        return Err(Error::ReserveFloorBreached);
    }
    Ok(())
}

/// Checks a repayment is owed and not an overpayment.
pub fn validate_repayment(amount: Amount, outstanding: Amount) -> Result<(), Error> {
    if amount <= 0 {
        return Err(Error::InvalidAmount);
    }
    if amount > outstanding {
        return Err(Error::RepaymentExceedsLoan);
    }
    Ok(())
}

/// Checks a shortfall can be covered.
///
/// Covering breaches the floor where it has to: the floor exists to keep the reserve able
/// to absorb losses, so refusing to absorb one in order to protect it would be backwards.
pub fn validate_shortfall_cover(amount: Amount, state: &ReserveState) -> Result<(), Error> {
    if amount <= 0 {
        return Err(Error::InvalidAmount);
    }
    if amount > state.balance {
        return Err(Error::InsufficientReserve);
    }
    Ok(())
}

/// Checks a sweep leaves the floor and outstanding loans intact.
pub fn validate_sweep(
    amount: Amount,
    state: &ReserveState,
    config: &ReserveConfig,
) -> Result<(), Error> {
    if amount <= 0 {
        return Err(Error::InvalidAmount);
    }
    if amount > available_to_sweep(state, config)? {
        return Err(Error::ReserveFloorBreached);
    }
    Ok(())
}

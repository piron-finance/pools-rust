//! Deposit and withdrawal computation.
//!
//! Ported from `src/libraries/DepositWithdrawalLibrary.sol` and the withdrawal handlers
//! at the end of `src/libraries/ValidationLibrary.sol`.
//!
//! These functions decide amounts. Burning shares, releasing escrow funds and paying the
//! treasury are the binding's job — it applies the outcome returned here.

use crate::error::Error;
use crate::math::{self, Amount};
use crate::types::{PoolData, PoolStatus};

/// What a deposit produces.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DepositOutcome {
    /// Shares to mint. Struck 1:1 with assets during funding.
    pub shares: Amount,
    /// New value for `total_raised`.
    pub total_raised: Amount,
    /// True when this is the holder's first deposit and `deposit_time` should be stamped.
    pub starts_position: bool,
}

/// A deposit during the funding window.
pub fn deposit(
    data: &PoolData,
    assets: Amount,
    now: u64,
    existing_deposit_time: u64,
) -> Result<DepositOutcome, Error> {
    super::validate::validate_deposit(data, assets, now)?;
    Ok(DepositOutcome {
        shares: assets,
        total_raised: math::checked_add(data.total_raised, assets)?,
        starts_position: existing_deposit_time == 0,
    })
}

/// Which withdrawal path applied.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WithdrawalKind {
    /// Cancelling before the epoch closes. Refund at par.
    Funding,
    /// Redeeming a settled pool. Pro-rata, less the withdrawal fee.
    Matured,
    /// Refund from a failed or halted pool. At par, partial allowed.
    Emergency,
}

/// Inputs the binding supplies from the vault and the ledger.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct WithdrawRequest {
    /// Amount requested. Ignored on the matured path, which always redeems in full.
    pub assets: Amount,
    pub user_shares: Amount,
    pub total_shares: Amount,
    pub now: u64,
}

/// What a withdrawal produces.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct WithdrawalOutcome {
    pub kind: WithdrawalKind,
    pub shares_burned: Amount,
    /// Entitlement before the fee.
    pub gross: Amount,
    pub fee: Amount,
    /// What the receiver gets. `gross - fee`.
    pub net: Amount,
    /// New value for `total_raised`. Unchanged except on the funding path.
    pub total_raised: Amount,
    /// True when the holder has no shares left and `deposit_time` should be cleared.
    pub clears_position: bool,
}

/// Routes a withdrawal to the path its pool status calls for.
///
/// PARITY: `ValidationLibrary.validateWithdrawal` permits withdrawal from `INVESTED` once
/// past maturity, but `DepositWithdrawalLibrary.handleWithdraw` then rejects `INVESTED`
/// unconditionally. The rejection is what actually governs, so it is what is ported. A
/// pool must be moved to `MATURED` before anyone can redeem.
pub fn withdraw(data: &PoolData, req: WithdrawRequest) -> Result<WithdrawalOutcome, Error> {
    if req.assets <= 0 {
        return Err(Error::InvalidAmount);
    }
    match data.status {
        PoolStatus::Funding => funding_withdrawal(data, req),
        PoolStatus::Matured => matured_withdrawal(data, req),
        PoolStatus::Emergency => emergency_withdrawal(data, req),
        _ => Err(Error::WithdrawalsNotAllowed),
    }
}

fn funding_withdrawal(data: &PoolData, req: WithdrawRequest) -> Result<WithdrawalOutcome, Error> {
    if req.now > data.config.epoch_end_time {
        return Err(Error::FundingEnded);
    }
    let shares = req.assets;
    if req.user_shares < shares {
        return Err(Error::InsufficientShares);
    }
    if data.total_raised < req.assets {
        return Err(Error::InsufficientPoolBalance);
    }

    Ok(WithdrawalOutcome {
        kind: WithdrawalKind::Funding,
        shares_burned: shares,
        gross: req.assets,
        fee: 0,
        net: req.assets,
        total_raised: math::checked_sub(data.total_raised, req.assets)?,
        clears_position: req.user_shares == shares,
    })
}

fn matured_withdrawal(data: &PoolData, req: WithdrawRequest) -> Result<WithdrawalOutcome, Error> {
    if req.now < data.config.maturity_date {
        return Err(Error::MaturityNotReached);
    }
    if req.user_shares == 0 {
        return Err(Error::NoShares);
    }
    if data.shares_at_maturity == 0 {
        return Err(Error::NoSharesOutstanding);
    }

    // The whole position is redeemed regardless of what was asked for; a settled pool has
    // nothing left to stay invested in.
    //
    // The pot is what the SPV actually returned plus coupons not yet distributed, and the
    // divisor is the supply frozen at settlement. Recomputing the pot from pool state
    // while dividing by a live supply that shrinks with every burn would hand each
    // successive holder a larger fraction of the same money: two holders at 60/40 would
    // take 60% and then 100%, and the last one out would find the escrow empty.
    let total = super::calc::total_returns(data)?;
    let gross = math::pro_rata(req.user_shares, total, data.shares_at_maturity)?;
    let fee = math::bps_of(gross, data.config.withdrawal_fee_bps)?;

    Ok(WithdrawalOutcome {
        kind: WithdrawalKind::Matured,
        shares_burned: req.user_shares,
        gross,
        fee,
        net: math::checked_sub(gross, fee)?,
        total_raised: data.total_raised,
        clears_position: true,
    })
}

fn emergency_withdrawal(data: &PoolData, req: WithdrawRequest) -> Result<WithdrawalOutcome, Error> {
    if req.user_shares == 0 {
        return Err(Error::NoShares);
    }
    if req.assets > req.user_shares {
        return Err(Error::ExceedsRefundAmount);
    }

    Ok(WithdrawalOutcome {
        kind: WithdrawalKind::Emergency,
        shares_burned: req.assets,
        gross: req.assets,
        fee: 0,
        net: req.assets,
        total_raised: data.total_raised,
        clears_position: req.user_shares == req.assets,
    })
}

/// Records the realised discount on a matured discounted instrument.
pub fn validate_discount_record(data: &PoolData, discount: Amount) -> Result<(), Error> {
    super::validate::validate_discount_distribution(data)?;
    if discount <= 0 {
        return Err(Error::InvalidAmount);
    }
    Ok(())
}

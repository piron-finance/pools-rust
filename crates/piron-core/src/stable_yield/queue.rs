//! The withdrawal queue.
//!
//! Ported from the queue half of `src/StableYieldManager.sol`, where processing walks
//! `queue.head..queue.tail` in one transaction.
//!
//! That loop is the one place in Piron where a direct port would not survive either
//! non-EVM runtime: Soroban declares a ledger footprint per transaction and Solana an
//! account list, both capped. So processing here is explicitly a *batch* over a slice the
//! binding has already loaded, bounded by [`MAX_WITHDRAWAL_BATCH`], and the cursor moves
//! forward across calls until the queue drains.
//!
//! Order is preserved: a request that cannot be paid stops the batch rather than being
//! skipped in favour of a smaller one behind it.

use crate::error::Error;
use crate::math::{self, Amount};
use crate::stable_yield::nav;
use crate::stable_yield::types::{WithdrawalQueue, WithdrawalRequest};

/// Most requests one call may settle.
///
/// Sized to stay well inside a Soroban footprint (100 entries today) with room for the
/// pool's own state, and inside a Solana instruction's account list.
pub const MAX_WITHDRAWAL_BATCH: u32 = 20;

/// A quote for leaving the pool, struck at the current price.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct WithdrawalQuote {
    pub gross_value: Amount,
    pub fee: Amount,
    pub net_value: Amount,
}

/// Prices a withdrawal request at today's NAV.
///
/// This is the price the holder gets. The shares are burned the moment the request is
/// made, so the claim is fixed here and the queue owes cash, not shares — see
/// [`process_batch`].
pub fn quote_withdrawal(
    shares: Amount,
    nav_per_share: Amount,
    fee_bps: u32,
) -> Result<WithdrawalQuote, Error> {
    let gross_value = nav::assets_for_shares(shares, nav_per_share)?;
    let fee = math::bps_of(gross_value, fee_bps)?;
    Ok(WithdrawalQuote {
        gross_value,
        fee,
        net_value: math::checked_sub(gross_value, fee)?,
    })
}

/// Appends a request to the queue.
///
/// The pending total carries the *gross* quote. Both halves leave the escrow when the
/// request is served — the net to the holder, the fee to the treasury — so reserving only
/// the net would leave the fee looking like it still belonged to the remaining holders,
/// and each queued exit would nudge the price up for everyone behind it.
pub fn enqueue(
    queue: &WithdrawalQueue,
    quote: &WithdrawalQuote,
) -> Result<WithdrawalQueue, Error> {
    Ok(WithdrawalQueue {
        head: queue.head,
        tail: queue.tail.checked_add(1).ok_or(Error::Overflow)?,
        total_pending_value: math::checked_add(queue.total_pending_value, quote.gross_value)?,
    })
}

/// What one batch of processing settled.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BatchOutcome {
    /// How many requests were served, starting from the head.
    pub processed: u32,
    /// Paid to holders, net of fees.
    pub total_paid: Amount,
    pub total_fees: Amount,
    /// Shares the batch settled. They were burned when the requests were made; this is
    /// the running total the binding reconciles against, not an instruction to burn.
    pub shares_burned: Amount,
    /// Where the cursor lands. Feed this back as the new head.
    pub new_head: u64,
    /// True when the batch stopped because the next request could not be funded, rather
    /// than because it ran out of requests or hit the size cap.
    pub stopped_for_liquidity: bool,
}

/// Settles as much of the queue as liquidity and the batch cap allow.
///
/// `requests` is the slice starting at `queue.head`. `available` is the cash the escrow
/// can actually pay out — the caller subtracts whatever reserve must stay behind before
/// calling.
///
/// Each request is paid the quote struck when it was made, not a fresh one. The shares
/// were burned at that moment, so re-pricing here would divide the same assets by a
/// supply those shares had already left — paying the leaver more the longer the queue
/// took to reach them, at the expense of everyone still in.
pub fn process_batch(
    queue: &WithdrawalQueue,
    requests: &[WithdrawalRequest],
    available: Amount,
    max_items: u32,
) -> Result<BatchOutcome, Error> {
    if max_items > MAX_WITHDRAWAL_BATCH {
        return Err(Error::BatchTooLarge);
    }
    if queue.is_empty() {
        return Err(Error::QueueEmpty);
    }

    let mut remaining = available;
    let mut outcome = BatchOutcome {
        processed: 0,
        total_paid: 0,
        total_fees: 0,
        shares_burned: 0,
        new_head: queue.head,
        stopped_for_liquidity: false,
    };

    for request in requests.iter().take(max_items as usize) {
        if request.processed {
            // Already served in an earlier batch; step over it without paying twice.
            outcome.new_head = outcome.new_head.checked_add(1).ok_or(Error::Overflow)?;
            continue;
        }

        // The quote struck when the request was made.
        let gross_value = math::checked_add(request.estimated_value, request.fee_amount)?;
        if gross_value > remaining {
            outcome.stopped_for_liquidity = true;
            break;
        }

        remaining = math::checked_sub(remaining, gross_value)?;
        outcome.total_paid = math::checked_add(outcome.total_paid, request.estimated_value)?;
        outcome.total_fees = math::checked_add(outcome.total_fees, request.fee_amount)?;
        outcome.shares_burned = math::checked_add(outcome.shares_burned, request.shares)?;
        outcome.processed += 1;
        outcome.new_head = outcome.new_head.checked_add(1).ok_or(Error::Overflow)?;
    }

    Ok(outcome)
}

/// Whether a request has sat past its deadline and may be cancelled instead of served.
pub fn is_expired(request: &WithdrawalRequest, now: u64) -> bool {
    request.deadline != 0 && now > request.deadline && !request.processed
}

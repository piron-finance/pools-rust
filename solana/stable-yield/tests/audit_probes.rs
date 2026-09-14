//! Regression tests for the audit findings. Each asserts the corrected behaviour, so a
//! failure here means a fix has been undone.

mod harness;
use harness::*;

use anchor_lang::{InstructionData, ToAccountMetas};
use anchor_spl::token::spl_token;
use litesvm::LiteSVM;
use solana_signer::Signer;

/// C2. NAV is marked to model on every read, so a holding accretes as it is held rather
/// than jumping at settlement. Before the fix the price sat at par for eighty-nine days
/// and a depositor arriving on day eighty-nine took 20,000,000 — half the term's yield —
/// for one day held.
#[test]
fn nav_accrues_as_a_holding_is_held() {
    let mut svm = LiteSVM::new();
    let p = open_pool(&mut svm, |_| {});
    let (alice, aa, a_s) = holder_with(&mut svm, &p, 1_000_000_000);
    deposit(&mut svm, &p, &alice, aa, a_s, 1_000_000_000).expect("alice");
    deploy_capital(&mut svm, &p, &p.spv, p.spv_asset, 800_000_000).expect("draw");
    add_instrument(&mut svm, &p, &p.spv, discounted(800_000_000, 840_000_000, 90 * 86_400))
        .expect("record");

    refresh_nav(&mut svm, &p).expect("refresh");
    let at_purchase = price(&svm, &p);

    warp(&mut svm, 45 * 86_400);
    svm.expire_blockhash();
    refresh_nav(&mut svm, &p).expect("refresh");
    let midway = price(&svm, &p);
    assert!(midway > at_purchase, "the discount did not accrue");

    warp(&mut svm, 89 * 86_400);
    svm.expire_blockhash();
    refresh_nav(&mut svm, &p).expect("refresh");
    let day_89 = price(&svm, &p);

    // Carol buys in the day before the bill redeems, at a price that already reflects
    // eighty-nine days of accrual.
    let (carol, ca, c_s) = holder_with(&mut svm, &p, 1_000_000_000);
    deposit(&mut svm, &p, &carol, ca, c_s, 1_000_000_000).expect("carol");
    let carol_shares = token_balance(&svm, &c_s);

    warp(&mut svm, 90 * 86_400 + 1);
    svm.expire_blockhash();
    mint_to(&mut svm, &p.authority, &p.authority, &p.asset, &p.spv_asset, 840_000_000);
    mature_instrument(&mut svm, &p, 0, 840_000_000).expect("settle");

    let settled = price(&svm, &p);
    // Carol paid the accrued price, so she holds fewer shares than the same money bought
    // on day one. That is the whole of the fix, seen from the outside.
    assert!(carol_shares < 1_000_000_000, "carol bought at par after 89 days of accrual");
    let carol_value = carol_shares as i128 * settled / 1_000_000_000_000_000_000i128;
    let carol_gain = carol_value - 1_000_000_000;

    assert!(day_89 > midway, "accrual stalled");
    // One day of a ninety-day, 40,000,000 discount on half the pool is about 222,000.
    assert!(
        carol_gain < 500_000,
        "a one-day holder gained {carol_gain}, which is the term's yield, not a day's"
    );
}

/// The same property stated as an invariant: settlement at the marked value moves the
/// price by nothing, because the mark was already the pool's view of what it held.
#[test]
fn settling_at_the_mark_does_not_move_the_price() {
    let mut svm = LiteSVM::new();
    let p = open_pool(&mut svm, |_| {});
    let (alice, aa, a_s) = holder_with(&mut svm, &p, 1_000_000_000);
    deposit(&mut svm, &p, &alice, aa, a_s, 1_000_000_000).expect("alice");
    deploy_capital(&mut svm, &p, &p.spv, p.spv_asset, 800_000_000).expect("draw");
    add_instrument(&mut svm, &p, &p.spv, discounted(800_000_000, 840_000_000, 90 * 86_400))
        .expect("record");

    warp(&mut svm, 90 * 86_400 + 1);
    svm.expire_blockhash();
    refresh_nav(&mut svm, &p).expect("refresh");
    let before = price(&svm, &p);

    mint_to(&mut svm, &p.authority, &p.authority, &p.asset, &p.spv_asset, 840_000_000);
    mature_instrument(&mut svm, &p, 0, 840_000_000).expect("settle");

    assert_eq!(price(&svm, &p), before, "settlement repriced a pool already marked at face");
}

/// M1. Shares received by transfer used to be unredeemable: the holder record only
/// `deposit` could create, so the account constraint failed and the position was stuck.
/// A transferee now gets a record whose clock starts when they turn up — the same rule
/// the EVM pool applies in its ERC-20 `_update` hook.
#[test]
fn transferred_shares_can_still_be_redeemed() {
    let mut svm = LiteSVM::new();
    let p = open_pool(&mut svm, |c| c.1 = 7 * 86_400);
    let (alice, aa, a_s) = holder_with(&mut svm, &p, 1_000_000_000);
    deposit(&mut svm, &p, &alice, aa, a_s, 1_000_000_000).expect("alice");

    let (carol, _ca, c_s) = holder_with(&mut svm, &p, 0);
    transfer_shares(&mut svm, &alice, a_s, c_s, 400_000_000).expect("transfer");

    // With no record at all, the shares would be unredeemable.
    assert!(
        request_withdrawal(&mut svm, &p, &carol, c_s, 100_000_000).is_err(),
        "withdrew without a holding record"
    );

    register_holder(&mut svm, &p, &carol).expect("carol registers");

    // Registering does not wave her through: the clock starts on arrival.
    assert!(
        request_withdrawal(&mut svm, &p, &carol, c_s, 100_000_000).is_err(),
        "a transferee skipped the holding period"
    );

    // And she is not stranded either.
    warp(&mut svm, 7 * 86_400 + 1);
    svm.expire_blockhash();
    request_withdrawal(&mut svm, &p, &carol, c_s, 100_000_000).expect("carol can leave");
}

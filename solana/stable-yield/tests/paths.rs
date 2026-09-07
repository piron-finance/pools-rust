//! The paths a stable-yield pool can take that are not the happy one.

mod harness;
use harness::*;

use anchor_lang::{InstructionData, ToAccountMetas};
use anchor_spl::token::spl_token;
use litesvm::LiteSVM;
use solana_signer::Signer;

fn failed_with(result: Result<(), String>, code: u32) {
    let err = result.expect_err("expected a rejection");
    assert!(err.contains(&format!("Custom({code})")), "expected {code}, got {err}");
}

// ---------------------------------------------------------------- deposits

#[test]
fn a_deposit_below_the_minimum_is_rejected() {
    let mut svm = LiteSVM::new();
    let p = open_pool(&mut svm, |c| c.0 = 10_000_000);
    let (who, a, s) = holder_with(&mut svm, &p, 10_000_000);
    assert!(deposit(&mut svm, &p, &who, a, s, 9_999_999).is_err(), "below the minimum");
}

#[test]
fn the_first_deposit_is_struck_at_par() {
    let mut svm = LiteSVM::new();
    let p = open_pool(&mut svm, |_| {});
    let (who, a, s) = holder_with(&mut svm, &p, 1_000_000_000);
    deposit(&mut svm, &p, &who, a, s, 1_000_000_000).expect("deposit");
    // Nothing outstanding means no price to read, so one share is one unit.
    assert_eq!(token_balance(&svm, &s), 1_000_000_000);
}

#[test]
fn a_later_deposit_buys_at_the_price_yield_has_moved_to() {
    let mut svm = LiteSVM::new();
    let p = open_pool(&mut svm, |_| {});
    let (alice, a_asset, a_shares) = holder_with(&mut svm, &p, 1_000_000_000);
    deposit(&mut svm, &p, &alice, a_asset, a_shares, 1_000_000_000).expect("alice");

    // The SPV draws, then returns more than it took. The surplus is yield, so NAV rises
    // while the share count does not.
    deploy_capital(&mut svm, &p, &p.spv, p.spv_asset, 800_000_000).expect("draw");
    mint_to(&mut svm, &p.authority, &p.authority, &p.asset, &p.spv_asset, 200_000_000);
    return_capital(&mut svm, &p, &p.spv, p.spv_asset, 1_000_000_000).expect("return");

    let after_yield = load_pool(&svm, &p.pool);
    assert_eq!(after_yield.total_nav, 1_200_000_000, "a 20% gain");

    // Bob pays the new price, so his shares are fewer than his assets.
    let (bob, b_asset, b_shares) = holder_with(&mut svm, &p, 1_200_000_000);
    deposit(&mut svm, &p, &bob, b_asset, b_shares, 1_200_000_000).expect("bob");
    assert_eq!(
        token_balance(&svm, &b_shares),
        1_000_000_000,
        "1,200,000,000 at 1.2 buys the same claim alice has"
    );
}

// ---------------------------------------------------------------- capital

#[test]
fn only_the_spv_can_draw_capital() {
    let mut svm = LiteSVM::new();
    let p = open_pool(&mut svm, |_| {});
    let (who, a, s) = holder_with(&mut svm, &p, 1_000_000_000);
    deposit(&mut svm, &p, &who, a, s, 1_000_000_000).expect("deposit");

    let impostor = funded(&mut svm);
    let impostor_asset = create_token_account(&mut svm, &p.authority, &p.asset, &impostor.pubkey());
    assert!(
        deploy_capital(&mut svm, &p, &impostor, impostor_asset, 100_000_000).is_err(),
        "a stranger drew capital"
    );
}

#[test]
fn a_draw_that_would_breach_the_reserve_is_refused() {
    let mut svm = LiteSVM::new();
    // 20% has to stay behind, so at most 800,000,000 of a 1,000,000,000 pool is drawable.
    let p = open_pool(&mut svm, |c| c.4 = 2_000);
    let (who, a, s) = holder_with(&mut svm, &p, 1_000_000_000);
    deposit(&mut svm, &p, &who, a, s, 1_000_000_000).expect("deposit");

    assert!(
        deploy_capital(&mut svm, &p, &p.spv, p.spv_asset, 800_000_001).is_err(),
        "drew through the reserve floor"
    );
    deploy_capital(&mut svm, &p, &p.spv, p.spv_asset, 800_000_000).expect("up to the floor is fine");
    assert_eq!(token_balance(&svm, &p.vault), 200_000_000, "the floor is intact");
}

#[test]
fn a_draw_moves_value_between_lines_without_creating_any() {
    let mut svm = LiteSVM::new();
    let p = open_pool(&mut svm, |_| {});
    let (who, a, s) = holder_with(&mut svm, &p, 1_000_000_000);
    deposit(&mut svm, &p, &who, a, s, 1_000_000_000).expect("deposit");

    let before = load_pool(&svm, &p.pool).total_nav;
    deploy_capital(&mut svm, &p, &p.spv, p.spv_asset, 800_000_000).expect("draw");
    let after = load_pool(&svm, &p.pool);

    assert_eq!(after.total_nav, before, "cash with the SPV still belongs to the pool");
    assert_eq!(after.in_flight, 800_000_000);
    assert_eq!(token_balance(&svm, &p.vault) + after.in_flight, after.total_nav, "the lines sum to NAV");
}

#[test]
fn returning_more_than_was_drawn_is_yield_not_a_negative_balance() {
    let mut svm = LiteSVM::new();
    let p = open_pool(&mut svm, |_| {});
    let (who, a, s) = holder_with(&mut svm, &p, 1_000_000_000);
    deposit(&mut svm, &p, &who, a, s, 1_000_000_000).expect("deposit");
    deploy_capital(&mut svm, &p, &p.spv, p.spv_asset, 800_000_000).expect("draw");

    mint_to(&mut svm, &p.authority, &p.authority, &p.asset, &p.spv_asset, 40_000_000);
    return_capital(&mut svm, &p, &p.spv, p.spv_asset, 840_000_000).expect("return");

    let pool = load_pool(&svm, &p.pool);
    assert_eq!(pool.in_flight, 0, "in-flight capital is retired first");
    assert_eq!(pool.total_nav, 1_040_000_000, "the surplus lands as a gain");
    assert_eq!(token_balance(&svm, &p.vault), pool.total_nav, "and it is all in the vault");
}

#[test]
fn a_zero_return_is_refused() {
    let mut svm = LiteSVM::new();
    let p = open_pool(&mut svm, |_| {});
    failed_with(return_capital(&mut svm, &p, &p.spv, p.spv_asset, 0), 103);
}

// ---------------------------------------------------------------- the queue account

#[test]
fn the_queue_is_larger_than_a_cpi_could_have_allocated() {
    let mut svm = LiteSVM::new();
    let p = open_pool(&mut svm, |_| {});
    let full = piron_stable_yield_solana::state::WithdrawalQueue::LEN;

    assert!(full > 10_240, "otherwise the client-created account would be unnecessary");
    assert_eq!(svm.get_account(&p.queue).unwrap().data.len(), full, "created at full size");
    assert_eq!(
        load_pool(&svm, &p.pool).queue,
        p.queue,
        "the pool records the address, since it cannot be derived from a seed"
    );
}

// ---------------------------------------------------------------- instruments

#[test]
fn recording_a_purchase_moves_capital_without_changing_nav() {
    let mut svm = LiteSVM::new();
    let p = open_pool(&mut svm, |_| {});
    let (who, a, s) = holder_with(&mut svm, &p, 1_000_000_000);
    deposit(&mut svm, &p, &who, a, s, 1_000_000_000).expect("deposit");
    deploy_capital(&mut svm, &p, &p.spv, p.spv_asset, 800_000_000).expect("draw");

    let before = load_pool(&svm, &p.pool).total_nav;
    add_instrument(&mut svm, &p, &p.spv, discounted(800_000_000, 840_000_000, 90_000)).expect("record");

    let after = load_pool(&svm, &p.pool);
    assert_eq!(after.total_nav, before, "cash became a holding; nothing was created");
    assert_eq!(after.in_flight, 0, "the capital is no longer in flight");
    assert_eq!(after.instrument_count, 1);
}

#[test]
fn an_instrument_larger_than_the_capital_drawn_is_refused() {
    let mut svm = LiteSVM::new();
    let p = open_pool(&mut svm, |_| {});
    let (who, a, s) = holder_with(&mut svm, &p, 1_000_000_000);
    deposit(&mut svm, &p, &who, a, s, 1_000_000_000).expect("deposit");
    deploy_capital(&mut svm, &p, &p.spv, p.spv_asset, 100_000_000).expect("draw");

    // Clamping instead would leave NAV counting 50,000,000 the pool never had.
    failed_with(
        add_instrument(&mut svm, &p, &p.spv, discounted(150_000_000, 160_000_000, 90_000)),
        708,
    );
}

#[test]
fn an_instrument_maturing_in_the_past_is_refused() {
    let mut svm = LiteSVM::new();
    let p = open_pool(&mut svm, |_| {});
    let (who, a, s) = holder_with(&mut svm, &p, 1_000_000_000);
    deposit(&mut svm, &p, &who, a, s, 1_000_000_000).expect("deposit");
    deploy_capital(&mut svm, &p, &p.spv, p.spv_asset, 100_000_000).expect("draw");
    warp(&mut svm, 100_000);
    failed_with(
        add_instrument(&mut svm, &p, &p.spv, discounted(100_000_000, 110_000_000, 50_000)),
        219,
    );
}

#[test]
fn an_instrument_worth_less_than_it_cost_is_refused() {
    let mut svm = LiteSVM::new();
    let p = open_pool(&mut svm, |_| {});
    let (who, a, s) = holder_with(&mut svm, &p, 1_000_000_000);
    deposit(&mut svm, &p, &who, a, s, 1_000_000_000).expect("deposit");
    deploy_capital(&mut svm, &p, &p.spv, p.spv_asset, 100_000_000).expect("draw");
    failed_with(
        add_instrument(&mut svm, &p, &p.spv, discounted(100_000_000, 90_000_000, 90_000)),
        710,
    );
}

#[test]
fn a_coupon_rate_beyond_the_ceiling_is_refused() {
    let mut svm = LiteSVM::new();
    let p = open_pool(&mut svm, |_| {});
    let (who, a, s) = holder_with(&mut svm, &p, 1_000_000_000);
    deposit(&mut svm, &p, &who, a, s, 1_000_000_000).expect("deposit");
    deploy_capital(&mut svm, &p, &p.spv, p.spv_asset, 100_000_000).expect("draw");

    // Fixed income does not pay 90%, and NAV would accrue against the mistake until
    // somebody noticed.
    let mut terms = discounted(100_000_000, 110_000_000, 90_000);
    terms.instrument_type = 1;
    terms.annual_coupon_rate = 9_000;
    terms.coupon_frequency = 2;
    failed_with(add_instrument(&mut svm, &p, &p.spv, terms), 709);
}

#[test]
fn a_settled_holding_leaves_the_book_so_a_rolling_pool_can_keep_buying() {
    let mut svm = LiteSVM::new();
    let p = open_pool(&mut svm, |_| {});
    let (who, a, s) = holder_with(&mut svm, &p, 1_000_000_000);
    deposit(&mut svm, &p, &who, a, s, 1_000_000_000).expect("deposit");

    // Well past the 32-entry cap. Leaving settled holdings in place would stop the pool
    // buying on its 33rd purchase ever, with every earlier one long since matured.
    for _ in 0..40 {
        svm.expire_blockhash(); // identical instructions each round; vary the blockhash
        deploy_capital(&mut svm, &p, &p.spv, p.spv_asset, 1_000_000).expect("draw");
        add_instrument(&mut svm, &p, &p.spv, discounted(1_000_000, 1_000_000, 90_000)).expect("record");
        assert_eq!(load_pool(&svm, &p.pool).instrument_count, 1, "one at a time");
        mature_instrument(&mut svm, &p, 0, 1_000_000).expect("settle");
        assert_eq!(load_pool(&svm, &p.pool).instrument_count, 0, "and it leaves the book");
    }
}

#[test]
fn a_write_off_takes_the_holding_out_of_nav() {
    let mut svm = LiteSVM::new();
    let p = open_pool(&mut svm, |_| {});
    let (who, a, s) = holder_with(&mut svm, &p, 1_000_000_000);
    deposit(&mut svm, &p, &who, a, s, 1_000_000_000).expect("deposit");
    deploy_capital(&mut svm, &p, &p.spv, p.spv_asset, 200_000_000).expect("draw");
    add_instrument(&mut svm, &p, &p.spv, discounted(200_000_000, 210_000_000, 90_000)).expect("record");

    let before = load_pool(&svm, &p.pool).total_nav;
    write_off(&mut svm, &p, &p.authority, 0).expect("write off");
    let after = load_pool(&svm, &p.pool);

    assert_eq!(before - after.total_nav, 200_000_000, "the defaulted holding leaves NAV");
    assert_eq!(after.instrument_count, 0);
}

#[test]
fn a_write_off_is_the_authority_alone() {
    let mut svm = LiteSVM::new();
    let p = open_pool(&mut svm, |_| {});
    let (who, a, s) = holder_with(&mut svm, &p, 1_000_000_000);
    deposit(&mut svm, &p, &who, a, s, 1_000_000_000).expect("deposit");
    deploy_capital(&mut svm, &p, &p.spv, p.spv_asset, 200_000_000).expect("draw");
    add_instrument(&mut svm, &p, &p.spv, discounted(200_000_000, 210_000_000, 90_000)).expect("record");
    failed_with(write_off(&mut svm, &p, &p.spv, 0), 701);
}

#[test]
fn maturing_a_holding_that_is_not_there_is_refused() {
    let mut svm = LiteSVM::new();
    let p = open_pool(&mut svm, |_| {});
    let (who, a, s) = holder_with(&mut svm, &p, 1_000_000_000);
    deposit(&mut svm, &p, &who, a, s, 1_000_000_000).expect("deposit");
    mint_to(&mut svm, &p.authority, &p.authority, &p.asset, &p.spv_asset, 1_000);
    failed_with(mature_instrument(&mut svm, &p, 0, 1_000), 707);
}

// ---------------------------------------------------------------- the queue

#[test]
fn a_request_joins_the_queue_at_todays_price() {
    let mut svm = LiteSVM::new();
    let p = open_pool(&mut svm, |_| {});
    let (who, a, s) = holder_with(&mut svm, &p, 1_000_000_000);
    deposit(&mut svm, &p, &who, a, s, 1_000_000_000).expect("deposit");

    request_withdrawal(&mut svm, &p, &who, s, 200_000_000).expect("queue");
    let (head, tail, pending) = load_queue(&svm, &p.queue);
    assert_eq!((head, tail), (0, 1), "one request, unserved");
    // Gross, at par: the 0.5% fee leaves the vault too, so it is owed just the same.
    assert_eq!(pending, 200_000_000, "the quote is struck now, not when it is served");
    // And the shares go with the request.
    assert_eq!(token_balance(&svm, &s), 800_000_000);
}

#[test]
fn a_request_larger_than_the_holding_is_refused() {
    let mut svm = LiteSVM::new();
    let p = open_pool(&mut svm, |_| {});
    let (who, a, s) = holder_with(&mut svm, &p, 1_000_000_000);
    deposit(&mut svm, &p, &who, a, s, 1_000_000_000).expect("deposit");
    assert!(
        request_withdrawal(&mut svm, &p, &who, s, 1_000_000_001).is_err(),
        "queued more shares than were held"
    );
}

#[test]
fn requests_queue_in_order() {
    let mut svm = LiteSVM::new();
    let p = open_pool(&mut svm, |_| {});
    let (alice, aa, a_s) = holder_with(&mut svm, &p, 500_000_000);
    let (bob, ba, b_s) = holder_with(&mut svm, &p, 500_000_000);
    deposit(&mut svm, &p, &alice, aa, a_s, 500_000_000).expect("alice");
    deposit(&mut svm, &p, &bob, ba, b_s, 500_000_000).expect("bob");

    request_withdrawal(&mut svm, &p, &alice, a_s, 100_000_000).expect("first");
    request_withdrawal(&mut svm, &p, &bob, b_s, 100_000_000).expect("second");
    let (head, tail, _) = load_queue(&svm, &p.queue);
    assert_eq!((head, tail), (0, 2), "served head-first, in the order they arrived");
}

// ------------------------------------------------- serving the queue

#[test]
fn a_served_request_pays_the_holder_and_the_treasury() {
    let mut svm = LiteSVM::new();
    let p = open_pool(&mut svm, |_| {});
    let (who, a, s) = holder_with(&mut svm, &p, 1_000_000_000);
    deposit(&mut svm, &p, &who, a, s, 1_000_000_000).expect("deposit");
    request_withdrawal(&mut svm, &p, &who, s, 200_000_000).expect("queue");

    process_withdrawals(&mut svm, &p, &p.authority, 10, &[a]).expect("serve");

    // 200,000,000 at par: 199,000,000 out, 1,000,000 of fee.
    assert_eq!(token_balance(&svm, &a), 199_000_000);
    assert_eq!(token_balance(&svm, &p.treasury_asset), 1_000_000);
    assert_eq!(token_balance(&svm, &p.vault), 800_000_000);
    let (head, tail, pending) = load_queue(&svm, &p.queue);
    assert_eq!((head, tail, pending), (1, 1, 0), "drained");
}

#[test]
fn queueing_an_exit_does_not_move_the_price_for_anyone_else() {
    let mut svm = LiteSVM::new();
    let p = open_pool(&mut svm, |_| {});
    let (alice, aa, a_s) = holder_with(&mut svm, &p, 500_000_000);
    let (bob, ba, b_s) = holder_with(&mut svm, &p, 500_000_000);
    deposit(&mut svm, &p, &alice, aa, a_s, 500_000_000).expect("alice");
    deposit(&mut svm, &p, &bob, ba, b_s, 500_000_000).expect("bob");

    let before = price(&svm, &p);
    request_withdrawal(&mut svm, &p, &alice, a_s, 400_000_000).expect("queue");

    // Alice's shares are gone and her cash is owed. Both sides of the fraction move by
    // the same amount, so Bob's share is worth exactly what it was a moment ago.
    assert_eq!(price(&svm, &p), before, "queueing repriced the pool");
    process_withdrawals(&mut svm, &p, &p.authority, 10, &[aa]).expect("serve");
    assert_eq!(price(&svm, &p), before, "serving repriced the pool");
}

#[test]
fn moving_shares_after_queueing_cannot_strand_the_queue() {
    let mut svm = LiteSVM::new();
    let p = open_pool(&mut svm, |_| {});
    let (alice, aa, a_s) = holder_with(&mut svm, &p, 500_000_000);
    let (bob, ba, b_s) = holder_with(&mut svm, &p, 500_000_000);
    deposit(&mut svm, &p, &alice, aa, a_s, 500_000_000).expect("alice");
    deposit(&mut svm, &p, &bob, ba, b_s, 500_000_000).expect("bob");

    request_withdrawal(&mut svm, &p, &alice, a_s, 100_000_000).expect("alice queues");
    request_withdrawal(&mut svm, &p, &bob, b_s, 100_000_000).expect("bob queues");

    // Alice hands everything she has left to a third party. Were her shares still owed at
    // service time, the burn would fail here and the head would never advance past her —
    // stranding Bob and everyone behind him for good.
    let (carol, _, c_s) = holder_with(&mut svm, &p, 0);
    transfer_shares(&mut svm, &alice, a_s, c_s, 400_000_000).expect("transfer");
    assert_eq!(token_balance(&svm, &a_s), 0);
    let _ = carol;

    process_withdrawals(&mut svm, &p, &p.authority, 10, &[aa, ba]).expect("serve");
    assert_eq!(token_balance(&svm, &ba), 99_500_000, "bob was still paid");
    assert_eq!(load_queue(&svm, &p.queue).0, 2, "the head cleared both");
}

#[test]
fn the_same_shares_cannot_be_queued_twice() {
    let mut svm = LiteSVM::new();
    let p = open_pool(&mut svm, |_| {});
    let (who, a, s) = holder_with(&mut svm, &p, 1_000_000_000);
    deposit(&mut svm, &p, &who, a, s, 1_000_000_000).expect("deposit");

    request_withdrawal(&mut svm, &p, &who, s, 1_000_000_000).expect("queue");
    assert_eq!(token_balance(&svm, &s), 0, "burned on the way in");
    assert!(
        request_withdrawal(&mut svm, &p, &who, s, 1_000_000_000).is_err(),
        "the same shares were promised twice"
    );
}

#[test]
fn the_queue_is_served_in_order_and_stops_when_the_cash_runs_out() {
    let mut svm = LiteSVM::new();
    let p = open_pool(&mut svm, |_| {});
    let (alice, aa, a_s) = holder_with(&mut svm, &p, 500_000_000);
    let (bob, ba, b_s) = holder_with(&mut svm, &p, 500_000_000);
    deposit(&mut svm, &p, &alice, aa, a_s, 500_000_000).expect("alice");
    deposit(&mut svm, &p, &bob, ba, b_s, 500_000_000).expect("bob");

    // The SPV has already placed half the book, so 500,000,000 stands against 800,000,000
    // owed: Alice's 400,000,000 clears and Bob's cannot.
    deploy_capital(&mut svm, &p, &p.spv, p.spv_asset, 500_000_000).expect("draw");
    request_withdrawal(&mut svm, &p, &alice, a_s, 400_000_000).expect("alice");
    request_withdrawal(&mut svm, &p, &bob, b_s, 400_000_000).expect("bob");

    process_withdrawals(&mut svm, &p, &p.authority, 10, &[aa, ba]).expect("serve");

    assert_eq!(token_balance(&svm, &aa), 398_000_000, "head first");
    assert_eq!(token_balance(&svm, &ba), 0, "and stops rather than skipping ahead");
    assert_eq!(load_queue(&svm, &p.queue).0, 1, "the head sits on Bob");

    // Once the SPV returns capital, Bob's request clears unchanged — the quote he was
    // given when he asked, not a fresh one.
    return_capital(&mut svm, &p, &p.spv, p.spv_asset, 300_000_000).expect("return");
    process_withdrawals(&mut svm, &p, &p.authority, 10, &[ba]).expect("serve");
    assert_eq!(token_balance(&svm, &ba), 398_000_000);
}

#[test]
fn a_payout_cannot_be_redirected_to_someone_elses_account() {
    let mut svm = LiteSVM::new();
    let p = open_pool(&mut svm, |_| {});
    let (alice, aa, a_s) = holder_with(&mut svm, &p, 1_000_000_000);
    deposit(&mut svm, &p, &alice, aa, a_s, 1_000_000_000).expect("deposit");
    request_withdrawal(&mut svm, &p, &alice, a_s, 100_000_000).expect("queue");

    // The operator picks the payee accounts, so nothing but the owner check stands
    // between it and Alice's money.
    let (_mallory, ma, _) = holder_with(&mut svm, &p, 0);
    failed_with(process_withdrawals(&mut svm, &p, &p.authority, 10, &[ma]), 714);
    assert_eq!(token_balance(&svm, &ma), 0);
}

#[test]
fn a_batch_stops_at_the_cap_and_the_rest_wait_their_turn() {
    let mut svm = LiteSVM::new();
    let p = open_pool(&mut svm, |_| {});
    let (alice, aa, a_s) = holder_with(&mut svm, &p, 900_000_000);
    deposit(&mut svm, &p, &alice, aa, a_s, 900_000_000).expect("deposit");
    for _ in 0..3 {
        svm.expire_blockhash();
        request_withdrawal(&mut svm, &p, &alice, a_s, 100_000_000).expect("queue");
    }

    process_withdrawals(&mut svm, &p, &p.authority, 2, &[aa, aa]).expect("serve two");
    assert_eq!(load_queue(&svm, &p.queue).0, 2, "the cap held");
    assert_eq!(token_balance(&svm, &aa), 199_000_000);

    process_withdrawals(&mut svm, &p, &p.authority, 2, &[aa]).expect("serve the rest");
    assert_eq!(load_queue(&svm, &p.queue).0, 3);
}

#[test]
fn an_empty_queue_has_nothing_to_serve() {
    let mut svm = LiteSVM::new();
    let p = open_pool(&mut svm, |_| {});
    let (who, a, s) = holder_with(&mut svm, &p, 1_000_000_000);
    deposit(&mut svm, &p, &who, a, s, 1_000_000_000).expect("deposit");
    failed_with(process_withdrawals(&mut svm, &p, &p.authority, 10, &[]), 408);
}

#[test]
fn cash_owed_to_the_queue_is_not_the_spvs_to_draw() {
    let mut svm = LiteSVM::new();
    let p = open_pool(&mut svm, |_| {});
    let (who, a, s) = holder_with(&mut svm, &p, 1_000_000_000);
    deposit(&mut svm, &p, &who, a, s, 1_000_000_000).expect("deposit");
    request_withdrawal(&mut svm, &p, &who, s, 900_000_000).expect("queue");

    // 900,000,000 gross is spoken for. Drawing against it would leave the pool unable to
    // pay a request it has already priced and burned shares for.
    assert!(
        deploy_capital(&mut svm, &p, &p.spv, p.spv_asset, 200_000_000).is_err(),
        "the SPV drew cash the pool already owed"
    );
    process_withdrawals(&mut svm, &p, &p.authority, 10, &[a]).expect("serve");
    assert_eq!(token_balance(&svm, &a), 895_500_000);
}

// ------------------------------------------------- leaving in one step

#[test]
fn a_holder_can_leave_outright_when_the_vault_covers_it() {
    let mut svm = LiteSVM::new();
    let p = open_pool(&mut svm, |_| {});
    let (who, a, s) = holder_with(&mut svm, &p, 1_000_000_000);
    deposit(&mut svm, &p, &who, a, s, 1_000_000_000).expect("deposit");

    withdraw(&mut svm, &p, &who, a, s, 400_000_000).expect("withdraw");
    assert_eq!(token_balance(&svm, &a), 398_000_000);
    assert_eq!(token_balance(&svm, &p.treasury_asset), 2_000_000);
    assert_eq!(token_balance(&svm, &s), 600_000_000);
    assert_eq!(load_pool(&svm, &p.pool).total_nav, 600_000_000);
}

#[test]
fn leaving_outright_cannot_jump_a_standing_queue() {
    let mut svm = LiteSVM::new();
    let p = open_pool(&mut svm, |_| {});
    let (alice, aa, a_s) = holder_with(&mut svm, &p, 500_000_000);
    let (bob, ba, b_s) = holder_with(&mut svm, &p, 500_000_000);
    deposit(&mut svm, &p, &alice, aa, a_s, 500_000_000).expect("alice");
    deposit(&mut svm, &p, &bob, ba, b_s, 500_000_000).expect("bob");

    request_withdrawal(&mut svm, &p, &alice, a_s, 100_000_000).expect("alice waits");
    // Bob would be paid out of the very cash Alice is waiting on.
    failed_with(withdraw(&mut svm, &p, &bob, ba, b_s, 100_000_000), 715);
}

#[test]
fn leaving_outright_needs_the_cash_on_hand() {
    let mut svm = LiteSVM::new();
    let p = open_pool(&mut svm, |_| {});
    let (who, a, s) = holder_with(&mut svm, &p, 1_000_000_000);
    deposit(&mut svm, &p, &who, a, s, 1_000_000_000).expect("deposit");
    deploy_capital(&mut svm, &p, &p.spv, p.spv_asset, 800_000_000).expect("draw");

    // 400,000,000 is owed against 200,000,000 liquid: the holder has to queue instead.
    failed_with(withdraw(&mut svm, &p, &who, a, s, 400_000_000), 404);
    request_withdrawal(&mut svm, &p, &who, s, 400_000_000).expect("so they queue");
}

// ------------------------------------------------- the holding period

#[test]
fn a_holder_cannot_leave_inside_the_holding_period() {
    let mut svm = LiteSVM::new();
    let p = open_pool(&mut svm, |c| c.1 = 7 * 86_400);
    let (who, a, s) = holder_with(&mut svm, &p, 1_000_000_000);
    deposit(&mut svm, &p, &who, a, s, 1_000_000_000).expect("deposit");

    failed_with(request_withdrawal(&mut svm, &p, &who, s, 100_000_000), 713);
    failed_with(withdraw(&mut svm, &p, &who, a, s, 100_000_000), 713);

    warp(&mut svm, 7 * 86_400 + 1);
    svm.expire_blockhash();
    request_withdrawal(&mut svm, &p, &who, s, 100_000_000).expect("and after it, they may");
}

#[test]
fn topping_up_restarts_the_holding_period() {
    let mut svm = LiteSVM::new();
    let p = open_pool(&mut svm, |c| c.1 = 7 * 86_400);
    let (who, a, s) = holder_with(&mut svm, &p, 1_000_000_000);
    deposit(&mut svm, &p, &who, a, s, 500_000_000).expect("deposit");

    warp(&mut svm, 7 * 86_400 + 1);
    svm.expire_blockhash();
    deposit(&mut svm, &p, &who, a, s, 500_000_000).expect("top up");
    // The clock runs from the last buy-in, so the earlier one no longer counts.
    failed_with(request_withdrawal(&mut svm, &p, &who, s, 100_000_000), 713);

    warp(&mut svm, 14 * 86_400 + 2);
    svm.expire_blockhash();
    request_withdrawal(&mut svm, &p, &who, s, 100_000_000).expect("clear again");
}

// ------------------------------------------------- coupons

#[test]
fn a_coupon_arrives_as_cash_and_lifts_nav() {
    let mut svm = LiteSVM::new();
    let p = open_pool(&mut svm, |_| {});
    let (who, a, s) = holder_with(&mut svm, &p, 1_000_000_000);
    deposit(&mut svm, &p, &who, a, s, 1_000_000_000).expect("deposit");
    deploy_capital(&mut svm, &p, &p.spv, p.spv_asset, 500_000_000).expect("draw");
    add_instrument(&mut svm, &p, &p.spv, coupon_bearing(500_000_000, 500_000_000, 365 * 86_400))
        .expect("record");

    let before = load_pool(&svm, &p.pool).total_nav;
    mint_to(&mut svm, &p.authority, &p.authority, &p.asset, &p.spv_asset, 30_000_000);
    record_coupon(&mut svm, &p, 0, 30_000_000).expect("coupon");

    let after = load_pool(&svm, &p.pool);
    assert_eq!(after.total_nav - before, 30_000_000);
    assert_eq!(after.instruments[0].coupons_paid, 1);
}

#[test]
fn a_coupon_on_a_discounted_instrument_is_refused() {
    let mut svm = LiteSVM::new();
    let p = open_pool(&mut svm, |_| {});
    let (who, a, s) = holder_with(&mut svm, &p, 1_000_000_000);
    deposit(&mut svm, &p, &who, a, s, 1_000_000_000).expect("deposit");
    deploy_capital(&mut svm, &p, &p.spv, p.spv_asset, 500_000_000).expect("draw");
    add_instrument(&mut svm, &p, &p.spv, discounted(500_000_000, 520_000_000, 90_000)).expect("record");

    // A discount bond has no coupon; booking one would count the same accretion twice.
    mint_to(&mut svm, &p.authority, &p.authority, &p.asset, &p.spv_asset, 1_000_000);
    failed_with(record_coupon(&mut svm, &p, 0, 1_000_000), 400);
}

#[test]
fn only_the_spv_records_a_coupon() {
    let mut svm = LiteSVM::new();
    let p = open_pool(&mut svm, |_| {});
    let (who, a, s) = holder_with(&mut svm, &p, 1_000_000_000);
    deposit(&mut svm, &p, &who, a, s, 1_000_000_000).expect("deposit");
    deploy_capital(&mut svm, &p, &p.spv, p.spv_asset, 500_000_000).expect("draw");
    add_instrument(&mut svm, &p, &p.spv, coupon_bearing(500_000_000, 500_000_000, 365 * 86_400))
        .expect("record");
    assert!(
        add_instrument(&mut svm, &p, &p.authority, discounted(1, 1, 90_000)).is_err(),
        "the authority recorded a purchase it did not make"
    );
}

#[test]
fn a_depositor_arriving_mid_queue_is_not_charged_for_the_exits() {
    let mut svm = LiteSVM::new();
    let p = open_pool(&mut svm, |_| {});
    let (alice, aa, a_s) = holder_with(&mut svm, &p, 1_000_000_000);
    deposit(&mut svm, &p, &alice, aa, a_s, 1_000_000_000).expect("alice");
    request_withdrawal(&mut svm, &p, &alice, a_s, 500_000_000).expect("alice queues");

    // The vault still holds Alice's money, but it is hers, not the pool's. Pricing Carol
    // against the gross balance would sell her 500,000,000 of someone else's cash.
    let (carol, ca, c_s) = holder_with(&mut svm, &p, 500_000_000);
    deposit(&mut svm, &p, &carol, ca, c_s, 500_000_000).expect("carol");
    assert_eq!(token_balance(&svm, &c_s), 500_000_000, "carol bought in at par");

    // And Alice's queued exit still pays what she was quoted.
    process_withdrawals(&mut svm, &p, &p.authority, 10, &[aa]).expect("serve");
    assert_eq!(token_balance(&svm, &aa), 497_500_000);
}

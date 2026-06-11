//! Activity-entry tests (TUI-ENTRY-ACT-*).

mod common;

use ledger_core::{LedgerEventKind, LotSource};
use pt_core::{Cents, Date, MicroShares};
use tui::entry::{self, BuyForm, Phase, SellForm, SplitForm, VestForm};
use tui::testkit::{buy, flat_federal_ctx, replay, FakeRuntime, ViewBuilder};

fn ms(n: i64) -> MicroShares {
    MicroShares(n * pt_core::SHARE_SCALE)
}

// @spec TUI-ENTRY-ACT-001
#[test]
fn buy_composes_a_buy_event() {
    let form = BuyForm {
        lot_id: "L1".to_string(),
        symbol: "AMZN".to_string(),
        qty: ms(10),
        unit_price: Cents(26_126),
        fees: Cents(99),
        date: Date(20_000),
        platform: "Robinhood".to_string(),
        tracking_code: Some("T-1".to_string()),
        confirmed_new_symbol: true,
    };
    let ev = form.compose();
    match ev.kind {
        LedgerEventKind::Buy { symbol, qty, unit_price_cents, fees_cents, platform, tracking_code, .. } => {
            assert_eq!(symbol, "AMZN");
            assert_eq!(qty, ms(10));
            assert_eq!(unit_price_cents, Cents(26_126));
            assert_eq!(fees_cents, Cents(99));
            assert_eq!(platform, "Robinhood");
            assert_eq!(tracking_code, Some("T-1".to_string()));
        }
        other => panic!("expected Buy, got {other:?}"),
    }
}

// @spec TUI-ENTRY-ACT-002
#[test]
fn vest_composes_a_vest_event_with_no_fee_field() {
    let form = VestForm {
        lot_id: "V1".to_string(),
        symbol: "AMZN".to_string(),
        qty: ms(5),
        fmv_per_share: Cents(20_000),
        date: Date(20_000),
        platform: "Schwab".to_string(),
        tracking_code: Some("RSU-7".to_string()),
        confirmed_new_symbol: true,
    };
    let ev = form.compose();
    match ev.kind {
        LedgerEventKind::Vest { symbol, qty, fmv_per_share_cents, platform, tracking_code, .. } => {
            assert_eq!(symbol, "AMZN");
            assert_eq!(qty, ms(5));
            assert_eq!(fmv_per_share_cents, Cents(20_000));
            assert_eq!(platform, "Schwab");
            assert_eq!(tracking_code, Some("RSU-7".to_string()));
        }
        other => panic!("expected Vest, got {other:?}"),
    }
}

// @spec TUI-ENTRY-ACT-003
#[test]
fn sell_composes_a_sell_event_driving_the_lot_picker_allocation() {
    let log = vec![buy(1, 18_000, "L-A", "AMZN", 100, 5_000, "Robinhood")];
    let snap = replay(&log, &[("AMZN", 26_126)]);
    let mut picker = entry::LotPicker::build(&snap, &"AMZN".to_string(), "Robinhood", ms(40), Date(20_000));
    picker.set_take("L-A", ms(40));
    let form = SellForm {
        sale_id: "S1".to_string(),
        symbol: "AMZN".to_string(),
        qty: ms(40),
        unit_price: Cents(26_126),
        fees: Cents(0),
        date: Date(20_000),
        platform: "Robinhood".to_string(),
        tracking_code: None,
        accrues_to_state: Some("DC".to_string()),
        picker,
    };
    let ev = form.compose();
    match ev.kind {
        LedgerEventKind::Sell { symbol, qty, lot_refs, accrues_to_state, platform, .. } => {
            assert_eq!(symbol, "AMZN");
            assert_eq!(qty, ms(40));
            assert_eq!(lot_refs.len(), 1, "the picker allocation drives lot_refs");
            assert_eq!(lot_refs[0].lot_id, "L-A");
            assert_eq!(lot_refs[0].qty, ms(40));
            assert_eq!(accrues_to_state, Some("DC".to_string()));
            assert_eq!(platform, "Robinhood");
        }
        other => panic!("expected Sell, got {other:?}"),
    }
}

// @spec TUI-ENTRY-ACT-004
#[test]
fn split_composes_a_split_event() {
    let form = SplitForm {
        symbol: "AMZN".to_string(),
        ratio_num: 20,
        ratio_den: 1,
        date: Date(20_000),
    };
    let ev = form.compose();
    match ev.kind {
        LedgerEventKind::Split { symbol, ratio_num, ratio_den } => {
            assert_eq!(symbol, "AMZN");
            assert_eq!((ratio_num, ratio_den), (20, 1));
        }
        other => panic!("expected Split, got {other:?}"),
    }
}

// @spec TUI-ENTRY-ACT-005
#[test]
fn reversal_lists_targets_greying_reversed_and_prior_reversals_and_shows_blocks_inline() {
    // A log: Buy #1, Sell #2 consuming #1's lot, Reversal #3 of an earlier (none).
    // Reversing the Buy (#1) is BLOCKED — the Sell consumed its lot — shown inline.
    // The Sell is reversible. (TUI-ENTRY-ACT-005)
    let log = vec![
        buy(1, 18_000, "L-A", "AMZN", 100, 5_000, "Robinhood"),
        ledger_core::LedgerEvent {
            id: "e2".to_string(),
            seq: pt_core::Seq(2),
            date: Date(19_000),
            kind: LedgerEventKind::Sell {
                sale_id: "S2".to_string(),
                symbol: "AMZN".to_string(),
                qty: ms(100),
                unit_price_cents: Cents(9_989),
                fees_cents: Cents(0),
                lot_refs: vec![ledger_core::LotRef { lot_id: "L-A".to_string(), qty: ms(100) }],
                accrues_to_state: None,
                platform: "Robinhood".to_string(),
                tracking_code: None,
            },
        },
    ];
    let targets = entry::reversible_targets(&log);
    let buy_target = targets.iter().find(|c| c.event_id == "e1").unwrap();
    assert!(buy_target.block_reason.is_some(), "reversing the Buy is blocked (the Sell consumed it)");
    assert!(!buy_target.selectable(), "a blocked target is not selectable");

    let sell_target = targets.iter().find(|c| c.event_id == "e2").unwrap();
    assert!(sell_target.block_reason.is_none(), "the Sell is reversible");
    assert!(sell_target.selectable());

    // Now add a Reversal of the Sell; it (and the now-reversed Sell) are greyed.
    let mut log2 = log.clone();
    log2.push(ledger_core::LedgerEvent {
        id: "e3".to_string(),
        seq: pt_core::Seq(3),
        date: Date(19_500),
        kind: LedgerEventKind::Reversal { target_event_id: "e2".to_string() },
    });
    let targets2 = entry::reversible_targets(&log2);
    assert!(targets2.iter().find(|c| c.event_id == "e2").unwrap().greyed, "the reversed Sell is greyed");
    assert!(targets2.iter().find(|c| c.event_id == "e3").unwrap().greyed, "the prior Reversal is greyed");
}

// @spec TUI-ENTRY-ACT-006
#[test]
fn new_symbol_buy_warns_not_blocks_and_requires_confirm() {
    // A Buy naming a symbol matching no existing position and no alias warns (does
    // not block) and requires a confirm-new-position step. (TUI-ENTRY-ACT-006)
    let log = vec![buy(1, 18_000, "L-A", "AMZN", 100, 5_000, "Robinhood")];
    let snap = replay(&log, &[("AMZN", 26_126)]);
    let aliases = config::AliasMap::new(std::collections::BTreeMap::new());

    assert!(entry::symbol_is_known(&"AMZN".to_string(), &snap, &aliases), "AMZN is a known position");
    assert!(
        !entry::symbol_is_known(&"AMZM".to_string(), &snap, &aliases),
        "AMZM (a typo) is first-seen — unknown"
    );

    let warning = entry::new_symbol_warning(&"AMZM".to_string());
    assert!(warning.contains("new symbol 'AMZM'"));
    assert!(warning.contains("[c]onfirm new position"), "requires a confirm step (does not block)");

    // An alias makes an otherwise-unknown ticker known (not first-seen).
    let mut m = std::collections::BTreeMap::new();
    m.insert("GOOG".to_string(), "GOOGL".to_string());
    let aliased = config::AliasMap::new(m);
    assert!(entry::symbol_is_known(&"GOOG".to_string(), &snap, &aliased), "an aliased symbol is known");
}

// @spec TUI-ENTRY-ACT-006
#[test]
fn unconfirmed_new_symbol_buy_does_not_reach_confirmed_until_confirmed() {
    // The guard is ENFORCED at submit: a Buy naming an unknown symbol with
    // confirmed_new_symbol=false is held (nothing written) with the new-symbol warn,
    // never reaching Confirmed; the same Buy with the flag set submits durably.
    // (TUI-ENTRY-ACT-006)
    let log = vec![buy(1, 18_000, "L-A", "AMZN", 100, 5_000, "Robinhood")];
    let snap = replay(&log, &[("AMZN", 26_126)]);
    let mut rt = FakeRuntime::new(ViewBuilder::new(snap).build(), flat_federal_ctx(common::YEAR, 220_000))
        .with_ledger_log(log);
    let aliases = config::AliasMap::new(std::collections::BTreeMap::new());

    // An UNCONFIRMED new-symbol Buy is held with the warn — no write.
    let mut unconfirmed = BuyForm {
        lot_id: "L-new".to_string(),
        symbol: "AMZM".to_string(), // a typo / first-seen symbol
        qty: ms(10),
        unit_price: Cents(10_000),
        fees: Cents(0),
        date: Date(20_000),
        platform: "Robinhood".to_string(),
        tracking_code: None,
        confirmed_new_symbol: false,
    };
    match entry::submit_buy(&mut rt, &unconfirmed, &aliases) {
        Phase::ConfirmNewSymbol(warn) => assert!(warn.contains("new symbol 'AMZM'"), "the warn surfaces: {warn}"),
        other => panic!("an unconfirmed new symbol must not submit, got {other:?}"),
    }
    assert_eq!(rt.ledger_log.len(), 1, "the unconfirmed new-symbol Buy wrote nothing");

    // The owner [c]onfirms the new position; the Buy now submits durably.
    unconfirmed.confirmed_new_symbol = true;
    assert_eq!(entry::submit_buy(&mut rt, &unconfirmed, &aliases), Phase::Confirmed, "a confirmed new symbol submits");
    assert_eq!(rt.ledger_log.len(), 2, "the confirmed Buy landed");

    // A KNOWN symbol never trips the guard, even unconfirmed.
    let known = BuyForm { symbol: "AMZN".to_string(), confirmed_new_symbol: false, lot_id: "L-amzn2".to_string(), ..unconfirmed.clone() };
    assert_eq!(entry::submit_buy(&mut rt, &known, &aliases), Phase::Confirmed, "a known symbol submits without confirm");
}

// @spec TUI-ENTRY-ACT-001
#[test]
fn buy_lot_source_is_buy_via_replay() {
    // A composed-then-appended Buy opens a Buy-sourced lot (cross-checks the kernel).
    let log = vec![buy(1, 18_000, "L-A", "AMZN", 100, 5_000, "Robinhood")];
    let snap = replay(&log, &[("AMZN", 26_126)]);
    assert_eq!(snap.open_lots.len(), 1);
    assert_eq!(snap.open_lots[0].lot.source, LotSource::Buy);
}

/// A Sell of all of L-A's 100 shares, parameterized by `accrues_to_state`, for the
/// residency-guard tests below. The picker fully allocates so only the residency
/// gate (not the lot picker) can block the submit.
fn a_full_sell(accrues_to_state: Option<String>) -> (Vec<ledger_core::LedgerEvent>, SellForm) {
    let log = vec![buy(1, 18_000, "L-A", "AMZN", 100, 5_000, "Robinhood")];
    let snap = replay(&log, &[("AMZN", 26_126)]);
    let mut picker = entry::LotPicker::build(&snap, &"AMZN".to_string(), "Robinhood", ms(100), Date(20_000));
    picker.set_take("L-A", ms(100));
    let form = SellForm {
        sale_id: "S1".to_string(),
        symbol: "AMZN".to_string(),
        qty: ms(100),
        unit_price: Cents(26_126),
        fees: Cents(0),
        date: Date(20_000),
        platform: "Robinhood".to_string(),
        tracking_code: None,
        accrues_to_state,
        picker,
    };
    (log, form)
}

// @spec CONFIG-RESIDENCY-005
#[test]
fn manual_sell_with_no_residency_default_blocks_submit_until_explicit_state() {
    // A manual Sell whose accrues_to_state resolved to None (no residency on the sale
    // date, no explicit state) is BLOCKED at submit with an inline error beside the
    // accrues-to-state field — nothing is written, no jurisdiction is guessed. An
    // explicit state then submits durably. (CONFIG-RESIDENCY-005)
    let (log, none_form) = a_full_sell(None);
    let mut rt = FakeRuntime::new(
        ViewBuilder::new(replay(&log, &[("AMZN", 26_126)])).build(),
        flat_federal_ctx(common::YEAR, 220_000),
    )
    .with_ledger_log(log);

    // No residency default + no explicit state: submit is blocked with an inline error.
    match entry::submit_sell(&mut rt, &none_form) {
        Phase::Rejected(entry::InlineError::Field(msg)) => {
            assert!(
                msg.contains("explicit accrues-to state"),
                "the block names the missing state: {msg}"
            );
        }
        other => panic!("a None-residency manual Sell must block submit, got {other:?}"),
    }
    assert_eq!(rt.submit_count, 0, "the residency block never reaches the write path");
    assert!(rt.ledger_log.len() == 1, "the blocked Sell wrote nothing");

    // The owner enters an explicit accrues-to state; the Sell now submits durably.
    let mut explicit = none_form.clone();
    explicit.accrues_to_state = Some("DC".to_string());
    assert_eq!(
        entry::submit_sell(&mut rt, &explicit),
        Phase::Confirmed,
        "an explicit accrues-to state submits"
    );
    assert_eq!(rt.ledger_log.len(), 2, "the explicit-state Sell landed");
}

// @spec CONFIG-RESIDENCY-005
#[test]
fn manual_sell_with_a_resolved_residency_default_submits_without_blocking() {
    // When the residency default DID resolve (Some), the Sell submits without the
    // residency block — the guard only fires on a None default. (CONFIG-RESIDENCY-005)
    let (log, form) = a_full_sell(Some("DC".to_string()));
    let mut rt = FakeRuntime::new(
        ViewBuilder::new(replay(&log, &[("AMZN", 26_126)])).build(),
        flat_federal_ctx(common::YEAR, 220_000),
    )
    .with_ledger_log(log);

    assert!(entry::sell_residency_guard(&form).is_none(), "a resolved default does not block");
    assert_eq!(entry::submit_sell(&mut rt, &form), Phase::Confirmed);
    assert_eq!(rt.ledger_log.len(), 2, "the Sell landed durably");
}

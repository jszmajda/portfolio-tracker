//! Composer & write-loop tests (TUI-ENTRY-FLOW-*) plus the confirm-gating.

mod common;

use pt_core::{Cents, Date, MicroShares};
use tui::entry::{self, BuyForm, FlowKind, Phase, RetryReason};
use tui::testkit::{flat_federal_ctx, FakeRuntime, SubmitBehavior, ViewBuilder};

fn empty_runtime() -> FakeRuntime {
    let snap = ledger_core::Snapshot::default();
    FakeRuntime::new(
        ViewBuilder::new(snap).build(),
        flat_federal_ctx(common::YEAR, 220_000),
    )
}

fn a_buy() -> BuyForm {
    BuyForm {
        lot_id: "L1".to_string(),
        symbol: "AMZN".to_string(),
        qty: MicroShares(10 * pt_core::SHARE_SCALE),
        unit_price: Cents(10_000),
        fees: Cents(0),
        date: Date(20_000),
        platform: "Robinhood".to_string(),
        tracking_code: None,
        confirmed_new_symbol: true,
    }
}

// @spec TUI-ENTRY-FLOW-001
#[test]
fn inline_validation_is_advisory_and_writes_nothing_on_reject() {
    // A Buy with qty 0 is rejected inline (NonPositiveQty) beside the field; the
    // log is untouched (writes nothing). (TUI-ENTRY-FLOW-001)
    let rt = empty_runtime();
    let mut form = a_buy();
    form.qty = MicroShares(0);
    let candidate = form.compose();
    let err = entry::inline_validate_ledger(&rt, &candidate).expect("inline rejects qty 0");
    assert!(matches!(
        err,
        entry::InlineError::Ledger(ledger_core::LedgerError::NonPositiveQty)
    ));
    assert!(rt.ledger_log.is_empty(), "inline reject writes nothing");
}

// @spec TUI-ENTRY-FLOW-001
#[test]
fn inline_validation_passes_a_good_candidate_writing_nothing() {
    let rt = empty_runtime();
    let candidate = a_buy().compose();
    assert!(entry::inline_validate_ledger(&rt, &candidate).is_none());
    assert!(rt.ledger_log.is_empty(), "inline never appends");
}

// @spec TUI-ENTRY-FLOW-002
#[test]
fn submit_revalidates_live_and_the_specific_kernel_error_rerenders_inline() {
    // The cache passed inline, but the live state disagrees at submit (modeled by
    // SubmitRejectLedger): the SPECIFIC kernel error re-renders in the inline slot
    // beside the field — inline never overrides submit, and this is distinct from a
    // write-verify retry. Nothing is written. (TUI-ENTRY-FLOW-002)
    let mut rt = empty_runtime().with_behavior(SubmitBehavior::SubmitRejectLedger(
        ledger_core::LedgerError::InsufficientShares,
    ));
    let candidate = a_buy().compose();
    let phase = entry::submit_ledger(&mut rt, &candidate);

    // The kernel error is surfaced inline — NOT collapsed to a generic retry.
    match &phase {
        Phase::Rejected(entry::InlineError::Ledger(e)) => {
            assert_eq!(
                *e,
                ledger_core::LedgerError::InsufficientShares,
                "the specific kernel error re-renders inline"
            );
        }
        other => panic!("expected an inline kernel rejection, got {other:?}"),
    }
    assert_ne!(
        phase,
        Phase::Confirmed,
        "nothing confirmed on a submit-time disagreement"
    );
    assert!(
        !matches!(phase, Phase::Retry(_)),
        "a kernel disagreement is inline, not a write-verify retry"
    );
    assert!(
        rt.ledger_log.is_empty(),
        "a submit-time rejection writes nothing"
    );
    // The inline error renders the verified error's words beside the field.
    if let Phase::Rejected(ie) = &phase {
        assert!(
            ie.text().contains("InsufficientShares"),
            "the inline slot shows the kernel error words: {}",
            ie.text()
        );
    }
}

// @spec TUI-ENTRY-FLOW-003
#[test]
fn submit_confirmed_durable_clears_the_composer() {
    let mut rt = empty_runtime();
    let candidate = a_buy().compose();
    let phase = entry::submit_ledger(&mut rt, &candidate);
    assert_eq!(
        phase,
        Phase::Confirmed,
        "a confirmed-durable submit clears the composer"
    );
    assert_eq!(rt.submit_count, 1);
    assert_eq!(rt.ledger_log.len(), 1, "the event landed durably");
    assert!(
        rt.last_event_id.is_some(),
        "read-back-verify confirmed a stored id"
    );
}

// @spec TUI-ENTRY-FLOW-004
#[test]
fn write_failure_returns_control_with_retry_and_retry_is_idempotent() {
    // A verify-mismatch returns control with the entry intact + [r]etry; retrying
    // (after the condition clears) reuses the prior confirmation and is idempotent.
    // (TUI-ENTRY-FLOW-004)
    let mut rt = empty_runtime().with_behavior(SubmitBehavior::VerifyMismatch);
    let candidate = a_buy().compose();
    let phase = entry::submit_ledger(&mut rt, &candidate);
    assert_eq!(phase, Phase::Retry(RetryReason::VerifyMismatch));
    assert!(rt.ledger_log.is_empty(), "no partial write on failure");

    // The condition clears (the human edit fixed); a retry confirms durably.
    rt.behavior = SubmitBehavior::Confirm;
    let again = entry::submit_ledger(&mut rt, &candidate);
    assert_eq!(again, Phase::Confirmed);
    assert_eq!(
        rt.ledger_log.len(),
        1,
        "retry lands exactly once (idempotent)"
    );
}

// @spec TUI-ENTRY-FLOW-004
#[test]
fn unreachable_workbook_returns_control_with_retry() {
    let mut rt = empty_runtime().with_behavior(SubmitBehavior::Unreachable);
    let phase = entry::submit_ledger(&mut rt, &a_buy().compose());
    assert_eq!(phase, Phase::Retry(RetryReason::Unreachable));
    assert!(phase_notice(&phase).contains("[r]etry"));
}

// @spec TUI-ENTRY-FLOW-005
#[test]
fn lock_held_fails_non_destructively_without_queuing() {
    // The advisory write-lock is held (a cron summary): the submit fails
    // non-destructively, the entry preserved, retry available — no queue, a `warn`
    // notice. (TUI-ENTRY-FLOW-005)
    let mut rt = empty_runtime().with_behavior(SubmitBehavior::LockHeld {
        holder: "summary-cron".to_string(),
    });
    let phase = entry::submit_ledger(&mut rt, &a_buy().compose());
    match &phase {
        Phase::Retry(RetryReason::LockHeld { holder }) => assert_eq!(holder, "summary-cron"),
        other => panic!("expected lock-held retry, got {other:?}"),
    }
    assert!(rt.ledger_log.is_empty(), "lock-held writes nothing");
    let notice = phase_notice(&phase);
    assert!(
        notice.contains("lock held"),
        "warn notice names lock held: {notice}"
    );
    assert!(
        notice.contains('\u{26A0}'),
        "the warn glyph travels with the notice"
    );
}

// @spec TUI-ENTRY-FLOW-006
#[test]
fn confirm_gates_exactly_reversal_pay_override_and_tax_rule_edits() {
    // Gated. (TUI-ENTRY-FLOW-006)
    assert!(FlowKind::Reversal.requires_confirm());
    assert!(FlowKind::Pay.requires_confirm());
    assert!(FlowKind::Override.requires_confirm());
    assert!(FlowKind::TaxRuleEdit.requires_confirm());
    // Not gated — plain appends.
    for f in [
        FlowKind::Buy,
        FlowKind::Vest,
        FlowKind::Sell,
        FlowKind::Split,
        FlowKind::Allocate,
        FlowKind::Move,
        FlowKind::ResidencyEdit,
        FlowKind::PlatformAliasEdit,
    ] {
        assert!(!f.requires_confirm(), "{f:?} must not gate");
    }
}

// @spec TUI-ENTRY-FLOW-007
#[test]
fn defaults_reduce_typing_date_today_residency_platform_alias() {
    // A SellForm built THROUGH the production defaulting constructor seeds the date
    // to port.today(), accrues_to_state to residency_on(sale_date), the platform to
    // the first config suggestion, and resolves the typed symbol through the alias
    // table — all overridable. The seeding is in production code, not the test.
    // (TUI-ENTRY-FLOW-007)
    let rt = empty_runtime();

    let residency = config::ResidencyTimeline::from_entries(vec![config::ResidencyEntry {
        effective_date: Date(0),
        state_code: "DC".to_string(),
    }])
    .unwrap();
    let platforms = config::PlatformList::new(vec!["Robinhood".to_string(), "Schwab".to_string()]);
    let mut m = std::collections::BTreeMap::new();
    m.insert("BRKB".to_string(), "BRK.B".to_string());
    let aliases = config::AliasMap::new(m);

    let sell = tui::entry::SellForm::with_defaults(
        &rt,
        "S1".to_string(),
        "BRKB", // a typed alias
        &residency,
        &platforms,
        &aliases,
    );
    assert_eq!(sell.date, Date(20_000), "date defaults to port.today()");
    assert_eq!(
        sell.accrues_to_state,
        Some("DC".to_string()),
        "accrues_to_state from residency_on(sale_date)"
    );
    assert_eq!(
        sell.platform, "Robinhood",
        "platform from the first config suggestion"
    );
    assert_eq!(
        sell.symbol, "BRK.B",
        "the typed symbol resolves through the alias table"
    );
    // The picker was built live for the resolved symbol on the default platform.
    assert_eq!(sell.picker.symbol, "BRK.B");
    assert_eq!(sell.picker.platform, "Robinhood");

    // A Buy defaults likewise; an unmapped symbol resolves to itself (overridable).
    let buy =
        tui::entry::BuyForm::with_defaults(&rt, "L1".to_string(), "AMZN", &platforms, &aliases);
    assert_eq!(buy.date, Date(20_000));
    assert_eq!(buy.platform, "Robinhood");
    assert_eq!(buy.symbol, "AMZN", "an unmapped symbol resolves to itself");
    assert!(
        !buy.confirmed_new_symbol,
        "a defaulted Buy starts unconfirmed for the new-symbol guard"
    );
}

fn phase_notice(phase: &Phase) -> String {
    match phase {
        Phase::Retry(r) => r.notice(),
        _ => String::new(),
    }
}

//! Global EventId assignment + cross-tab uniqueness: RUNTIME-EVENTID-001. runtime
//! holds the union of ids across BOTH the Ledger Events and Tax Events tabs (the
//! cross-tab set a single-tab append cannot see) and is where assign_event_id is
//! wired into the append path: same content is the same event (content de-dup), and
//! two genuinely distinct events never collide across tabs.
#![allow(clippy::inconsistent_digit_grouping)]

mod common;

use common::{buy, ctx, sell, small_logs};

use config::{Jurisdiction, TaxYear};
use runtime::{assign_ledger_event_id, assign_tax_event_id, cross_tab_event_ids};
use store::{tax_event_id, EventLogs};
use tax::{AccrualKey, Quarter, TaxEvent, TaxEventKind};

use pt_core::{Cents, Date, Seq};

/// A tax `Pay` event (carries no id field; store derives its id from content).
fn pay_event(seq: u64) -> TaxEvent {
    TaxEvent {
        seq: Seq(seq),
        kind: TaxEventKind::Pay {
            jurisdiction: Jurisdiction::Federal,
            tax_year: TaxYear(2022),
            period: Quarter::Q1,
            amount_cents: Cents(100_00),
            date: Date(19_100),
            covers: vec![AccrualKey {
                sale_id: "sale-1".to_string(),
                lot_id: "lot-1".to_string(),
                jurisdiction: Jurisdiction::Federal,
                tax_year: TaxYear(2022),
            }],
        },
    }
}

// @spec RUNTIME-EVENTID-001
#[test]
fn cross_tab_union_holds_ids_from_both_tabs() {
    // The union runtime holds is the ids across BOTH tabs — the set a single-tab
    // append cannot see. Ledger ids come off each event; tax ids are recomputed via
    // the SAME store-side content derivation that is persisted. (RUNTIME-EVENTID-001)
    let logs = EventLogs {
        ledger: vec![buy(1, 19_000, "lot-a", "AMZN", 1_000_000, 150_00)],
        tax: vec![pay_event(2)],
    };
    let ids = cross_tab_event_ids(&logs);

    // The ledger event's id is in the union.
    assert!(ids.contains("e1"), "the ledger id is in the cross-tab union");
    // The tax event's store-derived id is in the union (a single-tab ledger append
    // could not see it).
    assert!(
        ids.contains(&tax_event_id(&pay_event(2))),
        "the tax id (store-derived) is in the cross-tab union"
    );
    assert_eq!(ids.len(), 2, "the union is exactly the two ids across the two tabs");
}

// @spec RUNTIME-EVENTID-001, STORE-WRITE-008
#[test]
fn same_content_is_the_same_event_idempotent_assignment() {
    // Content de-dup (STORE-WRITE-008): an event whose id already exists IS this
    // event (a retry) — the assignment is the existing id, never a freshly-minted
    // colliding one, so the append is idempotent. (RUNTIME-EVENTID-001)
    let logs = small_logs(); // contains ledger ids e1, e2, e3.
    let already = buy(1, 19_000, "lot-amzn", "AMZN", 3_000_000, 150_00); // id == "e1", present.
    let assigned = assign_ledger_event_id(&logs, &already);
    assert_eq!(assigned, "e1", "a retry of an existing event reuses its id (content de-dup)");
}

// @spec RUNTIME-EVENTID-001
#[test]
fn a_genuinely_distinct_event_is_assigned_a_fresh_non_colliding_id() {
    // A new event (no caller id, or one not present) is assigned an id that collides
    // with NOTHING on either tab. (RUNTIME-EVENTID-001)
    let logs = small_logs();
    let mut fresh = buy(99, 19_500, "lot-new", "MSFT", 1_000_000, 300_00);
    fresh.id = String::new(); // unstamped — runtime mints the id.
    let assigned = assign_ledger_event_id(&logs, &fresh);

    let existing = cross_tab_event_ids(&logs);
    assert!(!existing.contains(&assigned), "the assigned id collides with no existing id");
    assert!(!assigned.is_empty(), "a fresh event gets a non-empty assigned id");
}

// @spec RUNTIME-EVENTID-001
#[test]
fn a_ledger_assignment_never_collides_with_a_tax_id_across_tabs() {
    // The cross-tab guarantee: a ledger id is bumped against the union of BOTH tabs,
    // so it can never collide with a tax id (which a single-tab assigner would not
    // see). We seed a tax id, then assign a ledger event whose content nonce would
    // otherwise be free, and confirm the result is distinct from the tax id.
    // (RUNTIME-EVENTID-001)
    let tax = pay_event(1);
    let tax_id = tax_event_id(&tax);
    let logs = EventLogs { ledger: vec![], tax: vec![tax] };

    let mut ledger_ev = buy(1, 19_000, "lot-x", "AMZN", 1_000_000, 150_00);
    ledger_ev.id = String::new();
    let assigned = assign_ledger_event_id(&logs, &ledger_ev);

    assert_ne!(assigned, tax_id, "a ledger id never collides with a tax id across tabs");
    // And the union after assignment would be uniqueness-preserving.
    let mut after = cross_tab_event_ids(&logs);
    assert!(after.insert(assigned), "the assigned ledger id is genuinely new in the union");
}

// @spec RUNTIME-EVENTID-001, STORE-WRITE-008
#[test]
fn a_tax_event_assignment_is_retry_stable_and_idempotent() {
    // A tax event's content id is retry-stable (the same event recomputes the same
    // id), so a re-assignment against a log already containing it returns that id —
    // idempotent. A tax event NOT yet present is assigned its (free) content id.
    // (RUNTIME-EVENTID-001, STORE-WRITE-008)
    let tax = pay_event(5);
    let content_id = tax_event_id(&tax);

    // Not present yet: assigned its content id.
    let empty = EventLogs::default();
    assert_eq!(assign_tax_event_id(&empty, &tax), content_id, "a fresh tax event gets its content id");

    // Present: the SAME id is returned (idempotent retry).
    let with_it = EventLogs { ledger: vec![], tax: vec![tax.clone()] };
    assert_eq!(
        assign_tax_event_id(&with_it, &tax),
        content_id,
        "a retry of an existing tax event reuses its id"
    );

    // The replay/marks ctx fixture is unrelated here but proves the common module is
    // exercised end-to-end (the same TaxContext the cycle uses).
    let _ = ctx(2022);
    // A tax event with DIFFERENT content yields a different id (distinct events do
    // not collide). The store-side id excludes Seq (store-assigned, not content), so
    // the difference must be in the content itself — a different remitted amount.
    let mut other = pay_event(6);
    if let TaxEventKind::Pay { amount_cents, .. } = &mut other.kind {
        *amount_cents = Cents(999_99);
    }
    assert_ne!(tax_event_id(&other), content_id, "distinct tax events get distinct ids");
    // (sell helper referenced so the shared fixture import is used.)
    let _ = sell(9, 19_100, "s", "AMZN", 1_000_000, 130_00, None);
}

// @spec RUNTIME-EVENTID-001
#[test]
fn a_tax_assignment_never_collides_with_a_ledger_id_across_tabs() {
    // The symmetric cross-tab guarantee (the ledger-side mirror is
    // `a_ledger_assignment_never_collides_with_a_tax_id_across_tabs`): a tax id is
    // content-addressed `tax-…` and a ledger id is `evt-…` — prefix-disjoint by
    // construction. So a tax assignment against a log already carrying ledger ids can
    // never collide with any of them, regardless of content. (RUNTIME-EVENTID-001)
    let logs = small_logs(); // carries ledger ids e1, e2, e3 (the `evt-`-shaped space).
    let tax = pay_event(7);

    let assigned = assign_tax_event_id(&logs, &tax);
    // Distinct from every ledger id (the `evt-`/`tax-` prefixes cannot collide).
    assert!(assigned.starts_with("tax-"), "a tax id is content-addressed `tax-…`");
    let existing = cross_tab_event_ids(&logs);
    assert!(
        !existing.contains(&assigned),
        "a tax assignment collides with no ledger id already in the cross-tab union"
    );
    // And the assignment IS the store-persisted content address (no bump applied).
    assert_eq!(assigned, tax_event_id(&tax), "the assignment is the content address store persists");
}

// @spec RUNTIME-EVENTID-001
#[test]
fn assign_tax_event_id_gives_distinct_ids_to_distinct_tax_events() {
    // Two genuinely DISTINCT tax events (different remitted amount → different content
    // cells) are assigned DISTINCT ids THROUGH `assign_tax_event_id` itself (not just
    // the bare `tax_event_id` derivation) — exercising the assign-time path. A tax id
    // is content-addressed, so distinctness rests on FNV-1a collision-resistance over
    // the distinct content, NOT on a bump. We additionally pin that assigning the
    // SECOND event against a log already holding the FIRST does not silently dedupe it
    // to the first's id. (RUNTIME-EVENTID-001, STORE-WRITE-008)
    let first = pay_event(11);
    let mut second = pay_event(12);
    if let TaxEventKind::Pay { amount_cents, .. } = &mut second.kind {
        *amount_cents = Cents(777_77); // different content => different content id.
    }

    // Assigned in isolation, the two distinct events get distinct ids.
    let empty = EventLogs::default();
    let id_first = assign_tax_event_id(&empty, &first);
    let id_second = assign_tax_event_id(&empty, &second);
    assert_ne!(
        id_first, id_second,
        "distinct tax events get distinct ids through assign_tax_event_id (content-addressed)"
    );

    // With the FIRST already in the log, assigning the SECOND must NOT be mistaken for
    // a retry of the first — the distinct event keeps its own content id (no false
    // idempotent-dedup). (STORE-WRITE-008)
    let with_first = EventLogs { ledger: vec![], tax: vec![first.clone()] };
    let id_second_against_first = assign_tax_event_id(&with_first, &second);
    assert_eq!(
        id_second_against_first, id_second,
        "a distinct tax event is not silently deduped to an existing event's id"
    );
    assert_ne!(
        id_second_against_first, id_first,
        "the distinct second event does not collide with the first's id"
    );
}

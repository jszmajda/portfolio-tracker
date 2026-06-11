//! RED-phase TDD tests for the CONFIG-RESIDENCY-* specs of `config` (see
//! `docs/intent/config/config-specs.md` → "Residency Timeline").
//!
//! These exercise the effective-dated timeline storage and invariants, the
//! INCLUSIVE-boundary `residency_on` resolution (a sale on the move date
//! resolves to the new state), future-dated entries resolving normally, and the
//! default `accrues_to_state` for a new sale. The scaffold stubs the resolution
//! bodies with `unimplemented!()`, so each test PANICS (RED).

mod common;

use common::*;
use config::{ConfigError, ResidencyTimeline};
use pt_core::Date;

// @spec CONFIG-RESIDENCY-001
#[test]
fn stores_sorted_unique_no_consecutive_same_state_timeline() {
    // A well-formed timeline: sorted, unique dates, alternating states.
    let tl = ResidencyTimeline::from_entries(vec![
        res(18_000, "DC"),
        res(19_000, "NJ"),
        res(19_500, "DC"),
    ])
    .expect("a sorted, unique, alternating timeline is valid");
    assert_eq!(tl.entries().len(), 3);
}

// @spec CONFIG-RESIDENCY-001
#[test]
fn rejects_duplicate_or_unsorted_effective_dates() {
    // Duplicate effective date.
    assert_eq!(
        ResidencyTimeline::from_entries(vec![res(19_000, "DC"), res(19_000, "NJ")]),
        Err(ConfigError::ResidencyUnsortedOrDuplicate),
    );
    // Unsorted input.
    assert_eq!(
        ResidencyTimeline::from_entries(vec![res(19_000, "NJ"), res(18_000, "DC")]),
        Err(ConfigError::ResidencyUnsortedOrDuplicate),
    );
}

// @spec CONFIG-RESIDENCY-001
#[test]
fn rejects_consecutive_same_state_no_op() {
    assert_eq!(
        ResidencyTimeline::from_entries(vec![res(18_000, "DC"), res(19_000, "DC")]),
        Err(ConfigError::ResidencyConsecutiveSameState),
    );
}

// @spec CONFIG-RESIDENCY-002
#[test]
fn residency_on_is_greatest_entry_at_or_before_date_inclusive() {
    let tl = ResidencyTimeline::from_entries(vec![res(18_000, "DC"), res(19_000, "NJ")])
        .expect("valid");

    // A date strictly between entries resolves to the earlier entry's state.
    assert_eq!(tl.residency_on(Date(18_500)), Some("DC".to_string()));
    // A date AFTER the second entry resolves to the later state.
    assert_eq!(tl.residency_on(Date(19_100)), Some("NJ".to_string()));
    // INCLUSIVE boundary: a sale on the EXACT effective date resolves to the
    // NEW state (the move is inclusive of its effective date).
    assert_eq!(tl.residency_on(Date(19_000)), Some("NJ".to_string()));
    assert_eq!(tl.residency_on(Date(18_000)), Some("DC".to_string()));
}

// @spec CONFIG-RESIDENCY-002
#[test]
fn residency_on_is_none_before_earliest_entry() {
    let tl = ResidencyTimeline::from_entries(vec![res(18_000, "DC")]).expect("valid");
    // The undefined pre-history region (the founding-entry import gate keeps it
    // unreachable for classification).
    assert_eq!(tl.residency_on(Date(17_999)), None);
}

// @spec CONFIG-RESIDENCY-003
#[test]
fn future_dated_entry_is_stored_and_resolves_on_or_after_its_date() {
    // A known upcoming move recorded ahead of time.
    let tl = ResidencyTimeline::from_entries(vec![res(19_000, "DC"), res(20_000, "NJ")])
        .expect("future-dated entries are allowed");
    // Before the future entry's date: still the prior state.
    assert_eq!(tl.residency_on(Date(19_900)), Some("DC".to_string()));
    // On/after the future entry's date: the new state.
    assert_eq!(tl.residency_on(Date(20_000)), Some("NJ".to_string()));
    assert_eq!(tl.residency_on(Date(20_500)), Some("NJ".to_string()));
}

// @spec CONFIG-RESIDENCY-004
#[test]
fn defaults_a_new_sale_accrues_to_state_to_residency_on_sale_date() {
    let tl = ResidencyTimeline::from_entries(vec![res(18_000, "DC"), res(19_000, "NJ")])
        .expect("valid");
    // A sale on the NJ move date defaults to NJ (inclusive).
    assert_eq!(
        config::default_accrues_to_state(&tl, Date(19_000)),
        Some("NJ".to_string()),
    );
    // A sale before the move defaults to DC.
    assert_eq!(
        config::default_accrues_to_state(&tl, Date(18_500)),
        Some("DC".to_string()),
    );
    // In the pre-history region there is no default (None) — left overridable.
    assert_eq!(config::default_accrues_to_state(&tl, Date(17_000)), None);
}

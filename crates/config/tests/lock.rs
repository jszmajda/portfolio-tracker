//! The advisory-write-lock cascade into `config`'s `put_*` primitives
//! (CONFIG-SETTINGS-006). Every `put_*` takes a `&impl pt_core::Lock`, acquires
//! it before mutating the workbook config tab and releases it after; when the
//! lock is held by another holder the write is refused (`LockHeld`) and stored
//! config is left untouched.
//!
//! Driven with `pt_core::NoopLock` (the acquire-counting fake) for the
//! happy-path acquisition, and a tiny always-held fake for the held case.
#![allow(clippy::inconsistent_digit_grouping)]

mod common;

use common::*;
use config::{ConfigStore, DeMinimis, InMemoryConfig, ResidencyTimeline, TaxYear};
use pt_core::{Cents, Lock, LockError, LockGuard, NoopLock};

/// A `Lock` whose `acquire` always reports the lock is held by another holder,
/// so `config`'s `put_*` must refuse the write rather than mutate. Mirrors the
/// `StoreLockAdapter`'s `Held -> LockError::Held` mapping.
struct HeldLock;

impl Lock for HeldLock {
    fn acquire(&self) -> Result<LockGuard, LockError> {
        Err(LockError::Held)
    }
}

// @spec CONFIG-SETTINGS-006
#[test]
fn put_acquires_the_lock_before_mutating() {
    // A valid write through a free (no-op) lock acquires the advisory lock exactly
    // once around the mutation and succeeds, leaving the new value stored.
    let mut store = InMemoryConfig::cold_start();
    let lock = NoopLock::new();

    store
        .put_tax_rules(valid_rules(2025, 19_700), &lock)
        .expect("a valid write through a free lock succeeds");

    assert_eq!(lock.acquire_count(), 1, "put_* acquires the advisory lock once");
    let data = store.load().expect("load after write");
    assert!(
        data.rules_by_year.contains_key(&TaxYear(2025)),
        "the validated rule table is stored after the locked write"
    );
}

// @spec CONFIG-SETTINGS-006
#[test]
fn every_put_threads_and_acquires_the_lock() {
    // Each of the five put_* primitives acquires the SAME lock before mutating.
    let mut store = InMemoryConfig::cold_start();
    let lock = NoopLock::new();

    store.put_tax_rules(valid_rules(2025, 19_700), &lock).expect("tax rules");
    store.put_de_minimis(DeMinimis(Cents(100)), &lock).expect("de-minimis");
    store.put_platforms(platforms(), &lock).expect("platforms");
    store
        .put_residency(
            ResidencyTimeline::from_entries(vec![res(16_436, "DC")]).expect("a valid timeline"),
            &lock,
        )
        .expect("residency");
    store.put_aliases(aliases(), &lock).expect("aliases");

    assert_eq!(
        lock.acquire_count(),
        5,
        "each put_* acquired the advisory lock once"
    );
}

// @spec CONFIG-SETTINGS-006
#[test]
fn put_under_a_held_lock_is_refused_and_leaves_config_untouched() {
    // When the advisory lock is held by ANOTHER holder, put_* must NOT write: it
    // returns the held error and the stored config is unchanged (no partial state).
    let mut store = InMemoryConfig::cold_start();

    // Seed an initial valid value through a free lock so we can prove the held
    // write does not alter it.
    let free = NoopLock::new();
    store
        .put_de_minimis(DeMinimis(Cents(500)), &free)
        .expect("seed a value through a free lock");

    // A held lock: the write is refused.
    let held = HeldLock;
    let result = store.put_de_minimis(DeMinimis(Cents(999)), &held);
    assert!(
        result.is_err(),
        "a put_* under a held lock must be refused, not written"
    );

    // The stored value is the seeded one, untouched by the refused write.
    let data = store.load().expect("load after the refused write");
    assert_eq!(
        data.de_minimis,
        DeMinimis(Cents(500)),
        "a refused locked write leaves stored config untouched"
    );
}

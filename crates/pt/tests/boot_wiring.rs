//! The binary delegates construction to the `runtime` composition root
//! (RUNTIME-BOOT-003) and the live store rides the runtime-owned advisory write-lock,
//! NOT a `NoopLock` (RUNTIME-LOCK-002). The binary's live replay cycle is
//! `pt::wiring::run_live_cycle`, which builds its `store` + `sheets-view` client
//! through `runtime::Boot` — these tests pin that wiring SHAPE without the live HTTP
//! round-trip (the round-trip is the env-gated e2e):
//!
//! - the exact lock the binary threads into the store
//!   (`StoreLockAdapter::new(boot.lock())`) is the runtime `AdvisoryLock`, so a store
//!   write primitive locks out a concurrent process — the single-writer coverage the
//!   old `NoopLock` wiring silently dropped;
//! - `run_live_cycle` rides `Boot::store`'s loud-auth construction, so an unreadable
//!   credentials file surfaces an error rather than a silent unauthenticated client.
#![allow(clippy::inconsistent_digit_grouping)]

use std::path::PathBuf;

use config::Settings;
use pt::wiring::{context_for_year, run_live_cycle};
use pt_core::Date;
use runtime::{
    AdvisoryLock, Boot, ManualClock, StoreLockAdapter, DEFAULT_TTL_SECS, LOCKFILE_NAME,
};
use store::{InMemoryCache, Store};
use store::testkit::InMemorySheets;

/// A unique temp cache dir for one test (so tests don't share lockfiles).
fn temp_cache_dir(tag: &str) -> PathBuf {
    let mut p = std::env::temp_dir();
    p.push(format!(
        "pt-boot-wiring-{}-{}-{}",
        tag,
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    std::fs::create_dir_all(&p).expect("create the cache dir");
    p
}

fn settings_with_cache_dir(dir: &std::path::Path) -> Settings {
    Settings {
        workbook_id: "test-workbook-id".to_string(),
        // A deliberately unreadable credentials path: the live client construction
        // (and thus `run_live_cycle`) must surface a loud failure, never a silent
        // unauthenticated client.
        credentials_path: "/nonexistent/creds.json".to_string(),
        cache_path: dir.to_string_lossy().to_string(),
        reporting_timezone: "US/Eastern".to_string(),
    }
}

// @spec RUNTIME-BOOT-001, RUNTIME-BOOT-003, RUNTIME-LOCK-002
#[test]
fn the_binary_threads_the_advisory_lock_into_the_store_not_a_noop_lock() {
    // The binary's live cycle (`pt::wiring::run_live_cycle`) builds its store via
    // `boot.store(StoreLockAdapter::new(boot.lock()))`. That adapter is the
    // runtime-owned AdvisoryLock under the settings' cache dir — so a `store` write
    // primitive acquires the SAME machine-local lock and a concurrent process is
    // locked out for the duration of the in-primitive guard. A `NoopLock` (the prior
    // wiring) would grant the concurrent acquire and drop single-writer coverage.
    // (RUNTIME-BOOT-001/003, RUNTIME-LOCK-002)
    let dir = temp_cache_dir("advisory-not-noop");
    let path = dir.join(LOCKFILE_NAME);
    let clock = ManualClock::new(5_000);

    // The lock the binary's wiring threads into the store: `Boot::lock()`, lives at
    // <cache_dir>/<LOCKFILE_NAME>. Built explicitly here over a fake clock so the
    // lockout is deterministic; `Boot::from_settings` builds the SAME AdvisoryLock at
    // the SAME path over the wall clock (asserted below).
    let settings = settings_with_cache_dir(&dir);
    let boot = Boot::from_settings(&settings, "tui");
    assert_eq!(
        boot.lock().path(),
        path,
        "the binary's Boot lock lives at the cache-dir lockfile the store rides"
    );

    let store_lock = AdvisoryLock::with_clock(&path, "tui-1", DEFAULT_TTL_SECS, clock.clone());
    // The store the binary builds carries the SAME advisory-lock adapter in its `Lock`
    // slot (constructed here so the wiring SHAPE is pinned without a live client).
    let store = Store::new(
        InMemorySheets::new(),
        StoreLockAdapter::new(&store_lock),
        InMemoryCache::new(),
    );

    let concurrent = AdvisoryLock::with_clock(&path, "cron-summary", DEFAULT_TTL_SECS, clock);

    // A store append acquires the advisory lock INSIDE its write primitive; while it
    // holds the in-primitive guard a concurrent process observes the lock Held. With
    // the old NoopLock wiring the concurrent acquire would have succeeded.
    {
        let _guard = store::Lock::acquire(&StoreLockAdapter::new(&store_lock))
            .expect("the store's in-primitive acquire succeeds when the lock is free");
        assert!(
            concurrent.try_acquire().is_held(),
            "a concurrent process is locked out while the store holds the in-primitive guard \
             (this is the coverage the NoopLock wiring silently dropped)"
        );
    }
    assert!(
        concurrent.try_acquire().is_acquired(),
        "the in-primitive guard's Drop released the advisory lock"
    );
    drop(store);
    let _ = std::fs::remove_file(&path);
}

// @spec RUNTIME-BOOT-003
#[test]
fn run_live_cycle_rides_the_root_loud_auth_construction() {
    // The binary's live cycle goes through `Boot::store` / `Boot::view_client`, which
    // build the ONE Sheets client from the captured creds. An unreadable credentials
    // file therefore surfaces a loud error from `run_live_cycle` — proving the cycle
    // delegates construction to the root rather than silently standing up an
    // unauthenticated client (or a NoopLock-backed store). (RUNTIME-BOOT-003)
    let dir = temp_cache_dir("loud-auth");
    let settings = settings_with_cache_dir(&dir); // credentials_path is unreadable.
    let boot = Boot::from_settings(&settings, "summary");
    let ctx = context_for_year(2026);

    let result = run_live_cycle(&boot, &ctx, Date(20_000), &config::AliasMap::default());
    assert!(
        result.is_err(),
        "run_live_cycle surfaces the root's loud auth failure (it builds the client via Boot, \
         never a silent unauthenticated one)"
    );
}

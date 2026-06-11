//! The composition root & bootstrap: RUNTIME-BOOT-001/003. runtime owns the single
//! place that constructs the concrete Store / AdvisoryLock / GoogleSheetsApi and
//! wires them creds → client → store → cycle, exposing load_and_run_cycle
//! (RUNTIME-BOOT-002). The live GoogleSheetsApi round-trip is the env-gated e2e; the
//! WIRING SHAPE — that the root threads the captured creds + constructs the
//! runtime-owned advisory lock under the cache dir, and that the single
//! load_and_run_cycle entry is reachable through it — is what these pin.
#![allow(clippy::inconsistent_digit_grouping)]

mod common;

use std::path::PathBuf;

use common::{ctx, small_logs};

use config::Settings;
use runtime::boot::{load_and_run_cycle, Boot};
use runtime::{
    AdvisoryLock, ManualClock, MarksCache, StoreLockAdapter, DEFAULT_TTL_SECS, LOCKFILE_NAME,
};

/// A unique temp cache dir for one test (so tests don't share lockfiles).
fn temp_cache_dir(tag: &str) -> PathBuf {
    let mut p = std::env::temp_dir();
    p.push(format!(
        "pt-boot-test-{}-{}-{}",
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
        credentials_path: "/nonexistent/creds.json".to_string(),
        cache_path: dir.to_string_lossy().to_string(),
        reporting_timezone: "US/Eastern".to_string(),
    }
}

// @spec RUNTIME-BOOT-001
#[test]
fn the_composition_root_constructs_the_runtime_owned_lock_under_the_cache_dir() {
    // The root constructs the runtime-owned AdvisoryLock (RUNTIME-LOCK) under the
    // settings' cache dir — the SAME machine-local lock acquired inside every write
    // primitive. The lockfile lives at <cache_dir>/<LOCKFILE_NAME>. (RUNTIME-BOOT-001)
    let dir = temp_cache_dir("lock-dir");
    let settings = settings_with_cache_dir(&dir);
    let boot = Boot::from_settings(&settings, "tui");

    assert_eq!(
        boot.lock().path(),
        dir.join(LOCKFILE_NAME),
        "the advisory lockfile sits under the settings' cache dir"
    );
    // The holder identity names the role (so a Held outcome can name who holds it).
    assert!(
        boot.lock().holder().starts_with("tui-"),
        "the holder names the role + pid"
    );

    // The constructed lock is a working advisory lock: it acquires when free.
    assert!(
        boot.lock().try_acquire().is_acquired(),
        "the root's lock acquires when free"
    );
}

// @spec RUNTIME-BOOT-001, RUNTIME-LOCK-002
#[test]
fn the_root_lock_adapts_into_a_store_lock_acquired_inside_write_primitives() {
    // The root's AdvisoryLock adapts via StoreLockAdapter into store's Lock, so the
    // store the root constructs acquires the SAME machine-local advisory lock inside
    // its write primitives — coverage independent of the caller. A concurrent process
    // (a different holder) sees the lock Held while the in-primitive guard is alive.
    // (RUNTIME-BOOT-001, RUNTIME-LOCK-002)
    let dir = temp_cache_dir("store-lock");
    let path = dir.join(LOCKFILE_NAME);
    let clock = ManualClock::new(5_000);

    // The root's lock (built explicitly here over a fake clock to drive the lock
    // deterministically; from_settings builds the SAME AdvisoryLock over the wall
    // clock in production).
    let root_lock = AdvisoryLock::with_clock(&path, "tui-1", DEFAULT_TTL_SECS, clock.clone());
    let adapter = StoreLockAdapter::new(&root_lock);

    let concurrent = AdvisoryLock::with_clock(&path, "cron-summary", DEFAULT_TTL_SECS, clock);
    {
        let _guard = store::Lock::acquire(&adapter).expect("in-primitive acquire when free");
        assert!(
            concurrent.try_acquire().is_held(),
            "a concurrent process is locked out"
        );
    }
    assert!(
        concurrent.try_acquire().is_acquired(),
        "the guard's Drop released the lock"
    );
    let _ = std::fs::remove_file(&path);
}

// @spec RUNTIME-BOOT-001
#[test]
fn the_root_surfaces_a_loud_auth_failure_when_credentials_are_unreadable() {
    // The root constructs the ONE Sheets client from the captured creds; a missing
    // credentials file surfaces a loud Auth failure (never a silent unauthenticated
    // client), so no entrypoint proceeds on a bad config. (RUNTIME-BOOT-001,
    // RUNTIME-SHEETS-001)
    let dir = temp_cache_dir("auth-fail");
    let settings = settings_with_cache_dir(&dir); // credentials_path is nonexistent.
    let boot = Boot::from_settings(&settings, "summary");

    match boot.sheets_api() {
        Err(runtime::SheetsError::Auth(_)) => {}
        Err(e) => panic!("expected an Auth failure, got a different error: {e:?}"),
        Ok(_) => panic!("an unreadable credentials file must NOT yield a client"),
    }
    // The store/view/history constructors ride the SAME client construction, so they
    // surface the same loud failure rather than a second silent client.
    assert!(
        boot.store(StoreLockAdapter::new(boot.lock())).is_err(),
        "the store construction surfaces the auth failure"
    );
    assert!(boot.view_client(sheets_view::POSITIONS_TAB).is_err());
    assert!(boot.history_client(sheets_view::HISTORY_TAB).is_err());
}

// @spec RUNTIME-BOOT-001
#[test]
fn an_empty_cache_path_falls_back_to_the_temp_dir() {
    // A fresh checkout with no cache path configured still constructs a valid lock
    // (under the system temp dir) rather than failing the boot. (RUNTIME-BOOT-001)
    let settings = Settings::default(); // empty cache_path.
    let boot = Boot::from_settings(&settings, "tui");
    assert_eq!(
        boot.lock().path(),
        std::env::temp_dir().join(LOCKFILE_NAME),
        "an empty cache path falls back to the temp dir"
    );
}

// @spec RUNTIME-BOOT-002, RUNTIME-BOOT-003
#[test]
fn load_and_run_cycle_is_reachable_through_the_boot_module_entry() {
    // RUNTIME-BOOT-003: the entrypoints drive the cycle through the runtime-
    // constructed collaborators via the single load_and_run_cycle entry the boot
    // module re-exports (RUNTIME-BOOT-002) — not by re-wiring their own. We drive it
    // through a FAKE store + view client (the live GoogleSheetsApi is the e2e),
    // proving the boot entry IS the load_and_run_cycle the binary calls.
    use pt_core::{Cents, Date};
    use sheets_view::testkit::{pass, InMemorySheetsView};
    use sheets_view::{PriceReading, SettleConfig};
    use store::testkit::InMemorySheets;
    use store::{serde_rows, InMemoryCache, NoopLock, Store};

    let logs = small_logs();
    let ledger_rows: Vec<_> = logs.ledger.iter().map(serde_rows::ledger_to_row).collect();
    let mut store = Store::new(
        InMemorySheets::seeded(ledger_rows, vec![]),
        NoopLock::new(),
        InMemoryCache::new(),
    );

    let ctx = ctx(2022);
    let view = InMemorySheetsView::new();
    view.set_prices(pass(&[
        (
            "AMZN",
            PriceReading::Numeric {
                price_usd: 170.0,
                quote_date: Date(19_180),
            },
        ),
        (
            "GOOG",
            PriceReading::Numeric {
                price_usd: 125.0,
                quote_date: Date(19_181),
            },
        ),
    ]));

    // The boot-module re-export IS runtime::cycle::load_and_run_cycle (the single
    // entry the entrypoints call). (RUNTIME-BOOT-002/003)
    let out = load_and_run_cycle(
        &mut store,
        &MarksCache::new(),
        &ctx,
        Date(19_200),
        &view,
        SettleConfig { max_polls: 4 },
    )
    .expect("the boot entry loads + runs the cycle");
    assert!(
        out.snapshot.positions.contains_key("AMZN"),
        "the cycle ran through the boot entry"
    );
    assert_eq!(
        out.marks.marks.get("AMZN").unwrap().price_cents,
        Cents(170_00)
    );
}

//! The composition root & bootstrap (RUNTIME-BOOT-001/003).
//!
//! `runtime` owns the **single place** that constructs the concrete collaborators
//! and wires them in dependency order — `config` credentials → Sheets client →
//! `store` → the replay/marks cycle — so no entrypoint constructs or re-wires them
//! itself (RUNTIME-BOOT-001). The `pt` binary is the process entrypoint and
//! delegates construction here (RUNTIME-BOOT-003); `summary` (headless) and `tui`
//! (interactive) run *through* the collaborators this root constructs rather than
//! building their own.
//!
//! The wiring this root builds:
//!
//! ```text
//!   config::Settings (workbook id + credentials + cache dir)
//!        │
//!        ▼  GoogleSheetsApi::from_credentials_file  (RUNTIME-SHEETS auth/throttle)
//!   the ONE Sheets client  ──▶  StoreSheetsAdapter ──▶ Store<…, StoreLockAdapter, …>
//!        │                                                     ▲
//!        └──────────────────────────────▶ ViewSheetsAdapter   │  AdvisoryLock (RUNTIME-LOCK)
//!                                                              │
//!        load_and_run_cycle(store, …, view) ──▶ CycleOutcome ─┘
//! ```
//!
//! The lock is the runtime-owned [`AdvisoryLock`] (RUNTIME-LOCK), adapted to
//! `store`'s `Lock` via [`StoreLockAdapter`] so it is acquired INSIDE every write
//! primitive. Construction of the live `GoogleSheetsApi` is exercised by the
//! env-gated e2e and the manual `pt` run (the unit gate cannot reach a live HTTP
//! round-trip); the wiring SHAPE — that the root threads creds→client→store→lock and
//! exposes the single `load_and_run_cycle` entry — is what `cargo test` pins.

use std::path::PathBuf;

use config::Settings;
use store::{Cache, InMemoryCache, Store};

use crate::lock::AdvisoryLock;
use crate::sheets::{GoogleSheetsApi, SheetsError, StoreSheetsAdapter};

/// The fully-wired live host the composition root constructs (RUNTIME-BOOT-001): the
/// `store` over the ONE Sheets client (with the advisory lock adapted in), the
/// matching view client over the SAME low-level layer, the runtime-owned
/// [`AdvisoryLock`], and the settle/replay config the cycle needs. The entrypoints
/// call [`Boot::load_and_run_cycle`] (RUNTIME-BOOT-002) rather than re-wiring any of
/// this.
///
/// `store` carries a [`StoreSheetsAdapter`] over `GoogleSheetsApi` and an in-memory
/// rebuildable cache (the cache carries no truth — `STORE-CACHE-001` — so an
/// in-memory mirror that rebuilds from the workbook each run is faithful); the lock
/// it acquires inside its write primitives is the SAME machine-local [`AdvisoryLock`]
/// this root owns, adapted via [`StoreLockAdapter`](crate::lock::StoreLockAdapter) at
/// the call site so single-writer coverage is independent of the caller
/// (RUNTIME-LOCK-002).
pub struct Boot {
    /// The workbook id (the target spreadsheet), threaded from `config::Settings`.
    workbook_id: String,
    /// The service-account credentials path, threaded from `config::Settings`.
    credentials_path: String,
    /// The runtime-owned advisory write-lock (its lockfile lives under the cache
    /// dir). Acquired INSIDE every workbook-mutating primitive. (RUNTIME-LOCK)
    lock: AdvisoryLock,
}

/// The default holder identity recorded into the lockfile — process-identifying so a
/// `Held` outcome can name who holds it (RUNTIME-LOCK-001/003). Combines a role tag
/// with the OS pid so two `pt` invocations on one machine are distinguishable.
fn holder_identity(role: &str) -> String {
    format!("{role}-{}", std::process::id())
}

/// The directory the advisory lockfile lives in, derived from the settings' cache
/// path (RUNTIME-LOCK-001, RUNTIME-BOOT-001). The cache path may name a file (e.g. a
/// SQLite db) or a directory; the lockfile sits in its parent directory either way,
/// falling back to the system temp dir when the setting is empty (a fresh checkout).
fn lock_dir(settings: &Settings) -> PathBuf {
    let p = PathBuf::from(&settings.cache_path);
    if settings.cache_path.is_empty() {
        return std::env::temp_dir();
    }
    // If the cache path looks like a file (has an extension), use its parent dir;
    // otherwise treat it as the cache directory itself.
    if p.extension().is_some() {
        p.parent()
            .map(PathBuf::from)
            .unwrap_or_else(std::env::temp_dir)
    } else {
        p
    }
}

impl Boot {
    /// Construct the composition root from `config::Settings` (RUNTIME-BOOT-001): the
    /// single place that captures the workbook id + credentials path (for the ONE
    /// Sheets client) and constructs the runtime-owned [`AdvisoryLock`] under the
    /// cache dir. The Sheets clients themselves are built lazily per role
    /// ([`Boot::store`] / [`Boot::view_client`]) so a connection failure surfaces at
    /// the point of use, but they are ALWAYS built from these same captured creds —
    /// no entrypoint authenticates its own client.
    pub fn from_settings(settings: &Settings, role: &str) -> Self {
        let lock = AdvisoryLock::in_cache_dir(lock_dir(settings), holder_identity(role));
        Boot {
            workbook_id: settings.workbook_id.clone(),
            credentials_path: settings.credentials_path.clone(),
            lock,
        }
    }

    /// The runtime-owned advisory write-lock (RUNTIME-LOCK). The entrypoints adapt
    /// it via [`StoreLockAdapter`](crate::lock::StoreLockAdapter) for the `store`
    /// write primitives and use [`AdvisoryLock::try_acquire`] directly for the rich
    /// `Held{holder, since}` held-lock policies.
    pub fn lock(&self) -> &AdvisoryLock {
        &self.lock
    }

    /// Build the ONE low-level Sheets client from the captured credentials
    /// (RUNTIME-BOOT-001, RUNTIME-SHEETS-001): the single authenticated/throttled
    /// client every workbook touch rides. A creds/auth failure surfaces as
    /// [`SheetsError::Auth`]. (`store`, `sheets-view`, `reports`, and `config` all
    /// ride this same low-level layer — no segment opens its own client.)
    pub fn sheets_api(&self) -> Result<GoogleSheetsApi, SheetsError> {
        GoogleSheetsApi::from_credentials_file(self.workbook_id.clone(), &self.credentials_path)
    }

    /// Construct the live `store` over the ONE Sheets client and an in-memory
    /// rebuildable cache (RUNTIME-BOOT-001): `creds → client → store`. The caller
    /// adapts [`Boot::lock`] into the store's `Lock` slot via
    /// [`StoreLockAdapter`](crate::lock::StoreLockAdapter) (a borrow of the
    /// root-owned lock), so the store's write primitives acquire the SAME
    /// machine-local advisory lock (RUNTIME-LOCK-002).
    pub fn store<L: store::Lock>(
        &self,
        lock: L,
    ) -> Result<Store<StoreSheetsAdapter<GoogleSheetsApi>, L, InMemoryCache>, SheetsError> {
        let api = self.sheets_api()?;
        Ok(Store::new(
            StoreSheetsAdapter::new(api),
            lock,
            InMemoryCache::new(),
        ))
    }

    /// Construct the live `sheets-view` client over the SAME low-level layer
    /// (RUNTIME-BOOT-001): a second `GoogleSheetsApi` from the SAME captured creds
    /// (it is the same single authenticated/throttled access layer, not a
    /// segment-private client), reading the marks back off the given Positions tab.
    pub fn view_client(
        &self,
        positions_tab: impl Into<String>,
    ) -> Result<crate::adapters::ViewSheetsAdapter<GoogleSheetsApi>, SheetsError> {
        let api = self.sheets_api()?;
        Ok(crate::adapters::ViewSheetsAdapter::new(api, positions_tab))
    }

    /// Construct the live `reports` History client over the SAME low-level layer
    /// (RUNTIME-BOOT-001): a `GoogleSheetsApi` from the SAME captured creds, writing
    /// the durable History tab. The capture append (RUNTIME-CYCLE-005) and its
    /// retry-and-flag loop (RUNTIME-REPORTS-001) drive this client under the lock.
    pub fn history_client(
        &self,
        history_tab: impl Into<String>,
    ) -> Result<crate::adapters::HistorySheetsAdapter<GoogleSheetsApi>, SheetsError> {
        let api = self.sheets_api()?;
        Ok(crate::adapters::HistorySheetsAdapter::new(api, history_tab))
    }

    /// Construct the live `config` client over the SAME low-level layer
    /// (RUNTIME-BOOT-001): the workbook config tab the domain config (tax rules,
    /// residency, de-minimis, platforms, aliases) persists to. `put_*` writes go
    /// through `config`'s validation gate under the advisory write-lock
    /// (CONFIG-SETTINGS-006); the cycle reads it to resolve the year's brackets.
    pub fn config_client(
        &self,
        config_tab: impl Into<String>,
        settings: Option<Settings>,
    ) -> Result<crate::adapters::ConfigSheetsAdapter<GoogleSheetsApi>, SheetsError> {
        let api = self.sheets_api()?;
        Ok(crate::adapters::ConfigSheetsAdapter::new(
            api, config_tab, settings,
        ))
    }
}

/// The single `load_and_run_cycle` entry the entrypoints call (RUNTIME-BOOT-002),
/// re-exported at the boot module so a caller drives the composition root and the
/// cycle through one surface: build the store via [`Boot::store`] (adapting the
/// root's lock), build the view client via [`Boot::view_client`], then call
/// [`crate::cycle::load_and_run_cycle`] over them. (The cycle entry itself lives in
/// [`crate::cycle`].)
pub use crate::cycle::load_and_run_cycle;

/// The default in-memory rebuildable cache the live store carries (RUNTIME-BOOT-001):
/// the cache carries no truth (`STORE-CACHE-001`) and is rebuilt from the workbook
/// each run, so an in-memory mirror is faithful for the live host. Exposed so a
/// caller that wires a `store` directly (rather than via [`Boot::store`]) uses the
/// same cache the root would.
pub fn default_cache() -> InMemoryCache {
    InMemoryCache::new()
}

// Assert at compile time that the default cache implements the `Cache` seam the
// store requires (so a future cache swap keeps the wiring contract).
const _: fn() = || {
    fn assert_cache<C: Cache>() {}
    assert_cache::<InMemoryCache>();
};

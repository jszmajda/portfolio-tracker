//! `runtime` — the shared HOST that wires the leaves together and closes the loops
//! they each assume someone else owns.
//!
//! The cross-segment audit found three concerns named by many segments and owned
//! by none; this crate is their home (see `docs/intent/runtime/runtime-design.md`
//! and `-specs.md`, prefix `RUNTIME`):
//!
//! 1. the low-level **Sheets-access layer** ([`sheets`]; `RUNTIME-SHEETS`) — the
//!    ONE place Google Sheets API mechanics live: JWT service-account auth, read /
//!    append / `batchUpdate` / clear, and rate-limit/backoff/retry. The real client
//!    is [`sheets::GoogleSheetsApi`]; the segment-shaped traits are *adapters* over
//!    the [`sheets::SheetsApi`] primitive — [`sheets::StoreSheetsAdapter`] for
//!    `store`, plus [`adapters::HistorySheetsAdapter`] (`reports` History),
//!    [`adapters::ViewSheetsAdapter`] (`sheets-view` republish + marks read-back),
//!    and [`adapters::ConfigSheetsAdapter`] (`config` domain tabs) in [`adapters`] —
//!    so EACH named segment seam rides the one primitive; no segment opens its own
//!    client.
//! 2. the cross-process, machine-local **advisory write-lock** ([`lock`];
//!    `RUNTIME-LOCK`) — a lockfile under the cache dir with a non-blocking
//!    [`lock::AdvisoryLock::try_acquire`] returning [`lock::LockOutcome::Acquired`]
//!    / [`lock::LockOutcome::Held`]`{ holder, since }` + TTL stale-reclaim, adapted
//!    to `store`'s `Lock` so it is acquired INSIDE every write primitive.
//! 3. the **replay -> project -> read-marks -> cache cycle** ([`cycle`];
//!    `RUNTIME-CYCLE`) — `runtime` is the replay caller: load the event log
//!    (`store`), replay `ledger-core` then `tax` (with `config` + the prior cycle's
//!    cached marks), hold the `Snapshot` + accruals, read marks back
//!    (`sheets-view`), cache them, inject into the next replay, and reduce the
//!    per-symbol quote-epochs to ONE trading-day key.
//!
//! It is I/O + orchestration, OUTSIDE the `verus!{}` boundary; `cargo test` is the
//! gate. The lock, the cycle, and every segment adapter are unit-tested against the
//! FAKE Sheets client (no network); the retry/backoff control loop
//! ([`sheets::run_with_retry`]) is unit-tested over a programmed status sequence
//! (no sleeps); and the JWT assertion mint is unit-tested with a throwaway key. The
//! ONLY surface a unit test cannot reach is the live HTTP round-trip of
//! [`sheets::GoogleSheetsApi`] / [`auth::ServiceAccount::fetch_access_token`]
//! against a real Google workbook — the thin `reqwest` send/parse wiring that
//! `with_retry` already governs — which is confirmed manually against a real
//! spreadsheet, not in `cargo test`.

pub mod adapters;
pub mod auth;
pub mod boot;
pub mod cache;
pub mod cycle;
pub mod eventid;
pub mod history;
pub mod lock;
pub mod revalidate;
pub mod sheets;
pub mod testkit;

// Re-exports of the public surface the entry points (`tui`, `summary`, `import`)
// consume.
pub use cycle::{
    load_and_run_cycle, load_run_and_capture, load_run_and_publish, per_symbol_freshness,
    reduce_trading_day_key, replay_with_cached_marks, run_cycle, run_cycle_publishing,
    CapturedCycle, CycleError, CycleOutcome, MarksCache, PublishedCycle, Replayed,
    SymbolFreshness,
};
pub use lock::{
    AdvisoryLock, Clock, Holder, LockHandle, LockOutcome, ManualClock, StoreLockAdapter,
    SystemClock, DEFAULT_TTL_SECS, LOCKFILE_NAME,
};
pub use adapters::{ConfigSheetsAdapter, HistorySheetsAdapter, ViewSheetsAdapter};
pub use sheets::{
    run_with_retry, BackoffPolicy, GoogleSheetsApi, Grid, SheetsApi, SheetsError,
    StoreSheetsAdapter,
};
pub use boot::Boot;
pub use cache::{classify_divergence, detect_and_rebuild, detect_divergence, Divergence, RebuildStrategy};
pub use eventid::{assign_ledger_event_id, assign_tax_event_id, cross_tab_event_ids};
pub use history::{capture_cycle, capture_with_retry, CaptureOutcome};
pub use revalidate::{revalidate_ledger, revalidate_tax, RevalidateError};

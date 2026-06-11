//! The low-level Sheets access layer and the advisory write-lock — both
//! `runtime`'s to implement for real. `store` depends only on these traits, and
//! the append primitive acquires the [`Lock`] INSIDE itself (store-design.md →
//! "Interfaces"). In-memory / no-op fakes for ALL tests live in
//! [`crate::testkit`].

use crate::{Row, StoreError, Tab};

/// The fingerprint metadata cell range read by the cheap currency probe (a
/// dedicated metadata range per tab). `runtime`'s real client computes these as
/// formula cells (`COUNTA`, `MAX(Seq)`, a `SUMPRODUCT` checksum) that auto-extend
/// on append; the fake computes them directly. (STORE-CACHE-002)
#[derive(Clone, PartialEq, Eq, Debug, Default)]
pub struct ProbeCells {
    /// `COUNTA` of the event rows (count). `None` ⇒ the fingerprint cells are
    /// missing/errored, which the probe treats as a change. (STORE-CACHE-002)
    pub count: Option<i64>,
    /// `MAX(Seq)` over the tab. `None` ⇒ missing/errored.
    pub max_seq: Option<i64>,
    /// The `SUMPRODUCT`-weighted checksum over the Seq/Date/money columns (plus
    /// character-code sums over key text columns). `None` ⇒ missing/errored.
    pub checksum: Option<i64>,
}

impl ProbeCells {
    /// Whether the fingerprint cells are well-formed (none missing/errored). A
    /// missing or errored cell is treated as a change by the probe.
    /// (STORE-CACHE-002)
    pub fn is_present(&self) -> bool {
        self.count.is_some() && self.max_seq.is_some() && self.checksum.is_some()
    }
}

/// The low-level Sheets access layer `store` needs: read a tab's data rows,
/// append a row, read the cheap fingerprint cells, batch-update (used by the
/// real client to seed/maintain the fingerprint block), and clear a tab. This is
/// the seam `runtime` implements against the Sheets API; tests use the in-memory
/// fake. (store-design.md → "Interfaces": all workbook I/O goes through
/// `runtime`'s Sheets-access layer.)
pub trait SheetsClient {
    /// Read every data row of a tab (excluding the header), in sheet order, as
    /// header-projected cell vectors. Order is the row's *visual* position — NOT
    /// the fold order, which is the `Seq` column (STORE-SCHEMA-002).
    fn read_rows(&self, tab: Tab) -> Result<Vec<Row>, StoreError>;

    /// Append one row to the end of a tab (the Sheets `append` API).
    fn append_row(&mut self, tab: Tab, row: &Row) -> Result<(), StoreError>;

    /// Read the cheap fingerprint cells (the dedicated metadata range) in ONE
    /// call — not the log. (STORE-CACHE-002)
    fn read_probe(&self, tab: Tab) -> Result<ProbeCells, StoreError>;

    /// Batch-update arbitrary cells (the real client maintains the fingerprint
    /// block this way). Modeled minimally for the seam.
    fn batch_update(&mut self, tab: Tab, rows: &[Row]) -> Result<(), StoreError>;

    /// Clear a tab's data rows (used by cache-rebuild / test setup).
    fn clear(&mut self, tab: Tab) -> Result<(), StoreError>;

    /// Create an event-log tab that does not exist yet — the frozen header row
    /// plus the fingerprint block (STORE-CACHE-002) — so the first append to a
    /// fresh workbook bootstraps the schema; a no-op when the tab already
    /// exists. Invoked by the append primitive on the missing-tab signal
    /// (STORE-WRITE-009).
    fn ensure_tab(&mut self, tab: Tab) -> Result<(), StoreError>;
}

// The advisory write-lock (`Lock`/`LockGuard`/`NoopLock`) lives in `pt-core`
// now, so config/sheets-view/store/import/reports/summary/runtime can all
// reference the SAME trait without a dependency cycle (`config` depends only on
// `pt-core`, and `store` depends on `config`). `store` re-exports it for
// back-compat (`store::Lock`, `store::sheets::Lock`, `store::NoopLock`), and
// maps the relocated [`pt_core::LockError`] into [`StoreError`] (below) so the
// append primitive's `self.lock.acquire()?` still yields a `StoreError`: a held
// or I/O-failed acquire returns control to the owner with no partial state — the
// same `Unreachable` contract a transport failure already honors.
pub use pt_core::{Lock, LockGuard, NoopLock};

impl From<pt_core::LockError> for StoreError {
    fn from(_e: pt_core::LockError) -> Self {
        // A write primitive that cannot acquire the advisory lock (held by another
        // process, or an acquisition I/O error) must NOT proceed: return control to
        // the owner, leaving no partial state — the same contract as a transport
        // failure. (RUNTIME-LOCK-002/003)
        StoreError::Unreachable
    }
}

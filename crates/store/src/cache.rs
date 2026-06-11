//! The rebuildable local cache (store-design.md → "Local Cache"). A disposable
//! mirror of BOTH event logs that **carries no truth** — rebuilt from the
//! workbook whenever a content fingerprint shows divergence (STORE-CACHE-001/004,
//! the workbook wins). `runtime` supplies a real SQLite-backed impl; tests use
//! the in-memory [`InMemoryCache`] (the design permits SQLite OR in-memory for
//! tests).
//!
//! Currency is two-tier (store-design.md → "Local Cache"):
//! - the **cheap probe** ([`crate::sheets::ProbeCells`]: count, max Seq,
//!   checksum) read in one call on the hot path (STORE-CACHE-002), and
//! - the **content hash** ([`Fingerprint::content_hash`]) computed only during a
//!   full read `store` already performs (STORE-CACHE-003).

use crate::{EventLogs, StoreError};

/// The per-tab cached fingerprint the cache stores alongside the mirrored logs:
/// the cheap probe values last seen and the full content hash last computed on a
/// full read. The probe is the hot-path change-detector (STORE-CACHE-002); the
/// content hash is the belt-and-suspenders covering any field outside the
/// checksum's reach (STORE-CACHE-003).
#[derive(Clone, PartialEq, Eq, Debug, Default)]
pub struct Fingerprint {
    /// `COUNTA` last seen.
    pub count: i64,
    /// `MAX(Seq)` last seen.
    pub max_seq: i64,
    /// The cheap `SUMPRODUCT` checksum last seen.
    pub checksum: i64,
    /// The full content hash last computed on a full read.
    pub content_hash: u64,
}

/// The rebuildable local cache seam. Mirrors both event logs and the per-tab
/// fingerprints; carries no authoritative state (STORE-CACHE-001). `runtime`
/// provides a SQLite impl; tests use [`InMemoryCache`].
pub trait Cache {
    /// Read the mirrored event logs from the cache (the offline / hot-path read).
    /// (STORE-CACHE-005)
    fn read(&self) -> Result<EventLogs, StoreError>;

    /// Overwrite the mirror with `logs` and store the freshly-computed per-tab
    /// fingerprints — the rebuild after a probe change / hash mismatch (the
    /// workbook wins). (STORE-CACHE-004)
    fn rebuild(
        &self,
        logs: &EventLogs,
        ledger_fp: Fingerprint,
        tax_fp: Fingerprint,
    ) -> Result<(), StoreError>;

    /// The per-tab fingerprint last stored (so the hot-path probe can compare a
    /// freshly-read [`crate::sheets::ProbeCells`] against the cache without
    /// re-reading the log). `None` until the first rebuild.
    fn ledger_fingerprint(&self) -> Option<Fingerprint>;
    fn tax_fingerprint(&self) -> Option<Fingerprint>;

    /// Whether the cache has ever been built (a cold cache forces a full read).
    fn is_populated(&self) -> bool;
}

/// An in-memory [`Cache`] fake for TDD (the design allows in-memory for tests).
/// Interior mutability via `RefCell` so the read/hot path can take `&self`,
/// mirroring `config`'s `InMemoryConfig` idiom.
#[derive(Default)]
pub struct InMemoryCache {
    inner: std::cell::RefCell<Option<CacheState>>,
}

#[derive(Clone)]
struct CacheState {
    logs: EventLogs,
    ledger_fp: Fingerprint,
    tax_fp: Fingerprint,
}

impl InMemoryCache {
    /// A cold (unpopulated) cache.
    pub fn new() -> Self {
        InMemoryCache {
            inner: std::cell::RefCell::new(None),
        }
    }
}

impl Cache for InMemoryCache {
    fn read(&self) -> Result<EventLogs, StoreError> {
        match &*self.inner.borrow() {
            Some(st) => Ok(st.logs.clone()),
            None => Err(StoreError::Unreachable),
        }
    }

    fn rebuild(
        &self,
        logs: &EventLogs,
        ledger_fp: Fingerprint,
        tax_fp: Fingerprint,
    ) -> Result<(), StoreError> {
        *self.inner.borrow_mut() = Some(CacheState {
            logs: logs.clone(),
            ledger_fp,
            tax_fp,
        });
        Ok(())
    }

    fn ledger_fingerprint(&self) -> Option<Fingerprint> {
        self.inner.borrow().as_ref().map(|s| s.ledger_fp.clone())
    }

    fn tax_fingerprint(&self) -> Option<Fingerprint> {
        self.inner.borrow().as_ref().map(|s| s.tax_fp.clone())
    }

    fn is_populated(&self) -> bool {
        self.inner.borrow().is_some()
    }
}

impl Fingerprint {
    /// Compute the full content hash over every row's `Seq`, `EventId`, and field
    /// cells — the belt-and-suspenders that also covers any field outside the
    /// cheap checksum's reach. Computed ONLY during a full read (STORE-CACHE-003).
    /// Hashed over the canonical row projection (every cell, in header order), so
    /// any field change — even one the cheap checksum would miss — shifts it.
    pub fn content_hash_ledger(events: &[ledger_core::LedgerEvent]) -> u64 {
        use std::hash::{Hash, Hasher};
        let mut h = std::collections::hash_map::DefaultHasher::new();
        // Fold in Seq order so the hash is independent of visual row position.
        let mut ordered: Vec<&ledger_core::LedgerEvent> = events.iter().collect();
        ordered.sort_by_key(|e| e.seq.0);
        for e in ordered {
            let cells = crate::serde_rows::ledger_to_row(e).to_cells(crate::Tab::Ledger);
            cells.hash(&mut h);
        }
        h.finish()
    }

    /// Content hash over the tax log, including each row's store-assigned
    /// `EventId` (STORE-CACHE-003 — the hash is "over each row's `Seq`, `EventId`,
    /// and field cells", the belt-and-suspenders covering any field outside the
    /// cheap checksum's reach, INCLUDING the `EventId` cell). The tax row carries
    /// its id as store metadata, so the hash is computed over `(TaxEvent, EventId)`
    /// pairs — the real id is folded in, so an out-of-band edit to a tax row's
    /// `EventId` cell is caught here. Hashed in `Seq` order (position-independent).
    pub fn content_hash_tax(events: &[(tax::TaxEvent, crate::EventId)]) -> u64 {
        use std::hash::{Hash, Hasher};
        let mut h = std::collections::hash_map::DefaultHasher::new();
        let mut ordered: Vec<&(tax::TaxEvent, crate::EventId)> = events.iter().collect();
        ordered.sort_by_key(|(e, _)| e.seq.0);
        for (e, id) in ordered {
            let cells = crate::serde_rows::tax_to_row(e, id).to_cells(crate::Tab::Tax);
            cells.hash(&mut h);
        }
        h.finish()
    }
}

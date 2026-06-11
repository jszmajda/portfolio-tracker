//! Post-rebuild re-validation handshake (RUNTIME-REVALIDATE-001).
//!
//! When a `store`-side append triggers a cache rebuild from the workbook
//! (`STORE-WRITE-001`), the log the pending event was originally validated against
//! is stale: rows may have been appended out-of-band since the event was composed.
//! `runtime` owns the handshake that re-runs the kernel validation the original
//! event passed — now against the *refreshed* post-rebuild log — **before `Seq`
//! assignment**, so an event that conflicts with the out-of-band rows is **rejected**
//! rather than appended on stale assumptions.
//!
//! CONTRACT split (per the design): `store` performs the rebuild and surfaces the
//! refreshed log; `runtime` owns the re-validation handshake and hands the
//! refreshed, re-validated view back to the writer. So this module takes the
//! refreshed [`store::EventLogs`] in, re-runs `ledger_core::validate` /
//! `tax::validate_event` against it, and returns the SAME refreshed view back out on
//! success — the writer then assigns `Seq` and appends against exactly the view that
//! was re-validated (no second read window in which the log could shift again).
//!
//! These functions ARE the handshake `runtime` owns; the writer that consumes the
//! handed-back view is the rebuild-on-append path the interactive write composers
//! drive, which is not yet wired (`RUNTIME-REVALIDATE-001` is an active gap, `[ ]`,
//! until that real append calls in here). The handshake's reject-on-conflict
//! semantics hold of these functions independent of which writer calls them; what
//! the gap names is the missing live caller, not a defect in the handshake itself.

use ledger_core::{LedgerError, LedgerEvent};
use store::EventLogs;
use tax::{TaxContext, TaxError, TaxEvent};

/// A re-validation failure: the pending event, valid against the log it was
/// composed on, conflicts with rows appended out-of-band since (RUNTIME-REVALIDATE-001).
/// The writer rejects rather than appends.
#[derive(Clone, PartialEq, Eq, Debug)]
pub enum RevalidateError {
    /// A pending LEDGER event no longer validates against the refreshed log.
    Ledger(LedgerError),
    /// A pending TAX event no longer validates against the refreshed log.
    Tax(TaxError),
}

impl std::fmt::Display for RevalidateError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{self:?}")
    }
}

impl std::error::Error for RevalidateError {}

/// Re-validate a pending LEDGER event against the REFRESHED log after a store-side
/// rebuild, BEFORE `Seq` assignment (RUNTIME-REVALIDATE-001). Re-runs the SAME kernel
/// gate the original event passed (`ledger_core::validate`) — now against the
/// post-rebuild `refreshed.ledger` — and, on success, hands the refreshed view back
/// to the writer so it assigns `Seq` and appends against exactly this view (no
/// second read window). On failure the event is rejected: an event that conflicts
/// with the out-of-band rows is never appended on stale assumptions.
///
/// `refreshed` is the view `store` surfaced from the rebuild; `runtime` owns the
/// handshake. The returned `&EventLogs` is the same refreshed view, made explicit so
/// the call site reads as a hand-back.
pub fn revalidate_ledger<'a>(
    refreshed: &'a EventLogs,
    candidate: &LedgerEvent,
) -> Result<&'a EventLogs, RevalidateError> {
    // Re-run the kernel validation against the REFRESHED accepted log (the rows now
    // present, including any out-of-band appends), not the stale view the event was
    // composed on. (RUNTIME-REVALIDATE-001)
    ledger_core::validate(&refreshed.ledger, candidate).map_err(RevalidateError::Ledger)?;
    Ok(refreshed)
}

/// Re-validate a pending TAX event against the REFRESHED log after a store-side
/// rebuild, BEFORE `Seq` assignment (RUNTIME-REVALIDATE-001). The tax kernel
/// validation gate (`tax::validate_event`) runs over the realized gains replayed
/// from the refreshed LEDGER log plus the refreshed tax-event lifecycle, so a Move /
/// Pay that was valid against the stale state but conflicts with out-of-band tax
/// rows (an accrual already moved/paid, a new realized gain) is rejected rather than
/// appended. On success the refreshed view is handed back.
pub fn revalidate_tax<'a>(
    refreshed: &'a EventLogs,
    candidate: &TaxEvent,
    ctx: &TaxContext,
) -> Result<&'a EventLogs, RevalidateError> {
    // The tax kernel validates against the realized gains (from the refreshed ledger
    // replay) and the refreshed accepted tax events — the post-rebuild state.
    // (RUNTIME-REVALIDATE-001)
    let snapshot = ledger_core::replay(&refreshed.ledger, &ledger_core::Marks::new());
    tax::validate_event(&snapshot.realized_gains, &refreshed.tax, candidate, ctx)
        .map_err(RevalidateError::Tax)?;
    Ok(refreshed)
}

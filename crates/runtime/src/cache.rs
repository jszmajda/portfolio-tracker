//! Cache-vs-workbook divergence DETECTION and the rebuild semantics
//! (RUNTIME-CACHE-001/002).
//!
//! `runtime` is the host that owns *whether* the local cache still mirrors the
//! authoritative workbook and *how* to rebuild it when it does not. `store`
//! (`STORE-CACHE-004`) and `config` (`CONFIG-SETTINGS-005`) defer the detection
//! mechanism here and merely *consume* the rebuilt cache — they do not author the
//! probe/decision.
//!
//! **Detection** (RUNTIME-CACHE-001) runs the cheap per-tab currency probe /
//! fingerprint comparison `store` exposes ([`store::Store::cache_is_stale`]). Any
//! probe change, content-hash mismatch, or missing/errored fingerprint is
//! divergence, **with the workbook authoritative**.
//!
//! **Rebuild** (RUNTIME-CACHE-002) chooses between two strategies:
//! - **full-replace** — drop and re-materialize the cache from a *complete*
//!   workbook read, used when the cache cannot be trusted incrementally: a cold
//!   cache, a fingerprint error/corruption, or an out-of-band deletion / in-place
//!   edit (the fingerprint moved in a way an append-only tail cannot explain).
//! - **reconcile** — apply the newly-appended tail, used when the divergence is an
//!   append-only extension the fingerprint can localize (count and max-Seq both
//!   grew, the prior rows unchanged).
//!
//! Both land through `store`'s full-read rebuild primitive (the workbook wins
//! either way — `store::Store::load`); the value `runtime` owns is the *decision*,
//! exposed so a consumer (and the tests) can see WHY a rebuild happened, and so a
//! reconcile path can be localized rather than always paying a full re-materialize.

use store::{Cache, Fingerprint, Lock, SheetsClient, Store, StoreError, Tab};

/// Why the cache diverged from the workbook — the classification that drives the
/// rebuild strategy (RUNTIME-CACHE-001/002). The workbook is authoritative in every
/// case; this only decides whether the cache can be trusted incrementally.
#[derive(Clone, PartialEq, Eq, Debug)]
pub enum Divergence {
    /// The cache still mirrors the workbook — no rebuild needed.
    InSync,
    /// An append-only extension the fingerprint can localize: the row count and
    /// max-`Seq` both grew and the prior content is unchanged. Reconcilable by
    /// applying the appended tail. (RUNTIME-CACHE-002)
    AppendedTail,
    /// The cache cannot be trusted incrementally — a cold cache, a missing/errored
    /// fingerprint, corruption, or an out-of-band deletion / in-place edit (the
    /// fingerprint changed in a way an append-only tail cannot explain). Forces a
    /// full-replace. (RUNTIME-CACHE-002)
    Untrustworthy,
}

/// The rebuild strategy chosen for a [`Divergence`] (RUNTIME-CACHE-002). Both land
/// through `store`'s full-read rebuild (the workbook wins); the distinction is
/// whether the cache could have been reconciled from its appended tail or had to be
/// dropped and re-materialized wholesale.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum RebuildStrategy {
    /// No rebuild — the cache is current.
    None,
    /// Apply the appended tail (append-only extension). (RUNTIME-CACHE-002)
    Reconcile,
    /// Drop and re-materialize from a complete workbook read (the cache cannot be
    /// trusted incrementally). (RUNTIME-CACHE-002)
    FullReplace,
}

impl Divergence {
    /// The rebuild strategy this divergence calls for (RUNTIME-CACHE-002).
    pub fn strategy(&self) -> RebuildStrategy {
        match self {
            Divergence::InSync => RebuildStrategy::None,
            Divergence::AppendedTail => RebuildStrategy::Reconcile,
            Divergence::Untrustworthy => RebuildStrategy::FullReplace,
        }
    }
}

/// Classify a single tab's divergence from the comparison of its stored cache
/// fingerprint against the freshly-read workbook probe (RUNTIME-CACHE-001/002):
///
/// - no stored fingerprint (cold cache) ⇒ `Untrustworthy` (full-replace);
/// - a missing/errored workbook probe ⇒ `Untrustworthy` (the fingerprint cannot be
///   localized);
/// - probe `count` and `max_seq` both **grew** and the cheap `checksum` is
///   consistent with an append (it changed, since new rows shift it) ⇒
///   `AppendedTail` (reconcile);
/// - any other change — count shrank, max-`Seq` unchanged while count moved, an
///   in-place edit that shifts the checksum without growing count/Seq ⇒
///   `Untrustworthy` (full-replace);
/// - everything equal ⇒ `InSync`.
///
/// This is a pure function over the two fingerprint snapshots so the decision is
/// unit-testable without a store.
pub fn classify_divergence(
    stored: Option<&Fingerprint>,
    probe: &store::sheets::ProbeCells,
) -> Divergence {
    // A cold cache (never built) cannot be trusted incrementally. (RUNTIME-CACHE-002)
    let Some(stored) = stored else {
        return Divergence::Untrustworthy;
    };
    // A missing/errored fingerprint cell cannot localize a tail — full-replace.
    // (RUNTIME-CACHE-001/002)
    if !probe.is_present() {
        return Divergence::Untrustworthy;
    }
    let count = probe.count.unwrap_or(0);
    let max_seq = probe.max_seq.unwrap_or(0);
    let checksum = probe.checksum.unwrap_or(0);

    let count_grew = count > stored.count;
    let seq_grew = max_seq > stored.max_seq;
    let count_same = count == stored.count;
    let seq_same = max_seq == stored.max_seq;
    let checksum_same = checksum == stored.checksum;

    if count_same && seq_same && checksum_same {
        // Nothing moved — the cache still mirrors the workbook.
        return Divergence::InSync;
    }
    // An append-only extension: BOTH the count and the max-Seq grew (new rows carry
    // higher Seq), and the checksum necessarily shifted with the new rows. The prior
    // rows are unchanged (a deletion would shrink count; an in-place edit would move
    // the checksum without growing count AND Seq together). (RUNTIME-CACHE-002)
    if count_grew && seq_grew {
        return Divergence::AppendedTail;
    }
    // Anything else — a deletion (count shrank), an in-place edit (checksum moved
    // but count/Seq did not both grow), or a max-Seq regression — cannot be trusted
    // incrementally. (RUNTIME-CACHE-002)
    Divergence::Untrustworthy
}

/// Detect the per-tab divergence of the cache from the workbook for one tab
/// (RUNTIME-CACHE-001): read the cheap probe through `store`'s Sheets client and
/// classify it against the cache's stored fingerprint. The workbook is
/// authoritative. A probe read failure surfaces as a `store` error to the caller.
pub fn detect_tab_divergence<S, L, C>(
    store: &Store<S, L, C>,
    tab: Tab,
) -> Result<Divergence, StoreError>
where
    S: SheetsClient,
    L: Lock,
    C: Cache,
{
    let stored = match tab {
        Tab::Ledger => store.cache().ledger_fingerprint(),
        Tab::Tax => store.cache().tax_fingerprint(),
    };
    let probe = store.sheets().read_probe(tab)?;
    Ok(classify_divergence(stored.as_ref(), &probe))
}

/// The overall cache divergence across BOTH event-log tabs (RUNTIME-CACHE-001): the
/// MORE-severe of the two per-tab classifications, so any untrustworthy tab forces a
/// full-replace and an append-only tail on either tab reconciles. The workbook is
/// authoritative.
pub fn detect_divergence<S, L, C>(store: &Store<S, L, C>) -> Result<Divergence, StoreError>
where
    S: SheetsClient,
    L: Lock,
    C: Cache,
{
    let ledger = detect_tab_divergence(store, Tab::Ledger)?;
    let tax = detect_tab_divergence(store, Tab::Tax)?;
    Ok(most_severe(ledger, tax))
}

/// The more-severe of two divergences (`Untrustworthy` > `AppendedTail` > `InSync`),
/// so the workbook-authoritative rebuild covers the worst case across both tabs.
fn most_severe(a: Divergence, b: Divergence) -> Divergence {
    fn rank(d: &Divergence) -> u8 {
        match d {
            Divergence::InSync => 0,
            Divergence::AppendedTail => 1,
            Divergence::Untrustworthy => 2,
        }
    }
    if rank(&a) >= rank(&b) {
        a
    } else {
        b
    }
}

/// Detect divergence and, if any, rebuild the cache from the authoritative workbook
/// (RUNTIME-CACHE-001/002), returning the [`RebuildStrategy`] taken so a consumer
/// (and the tests) can see WHY the cache was rebuilt.
///
/// Both reconcile and full-replace land through `store::Store::load` — a complete
/// workbook read that re-materializes the mirror with the workbook winning — because
/// `store` exposes one rebuild primitive and the workbook is authoritative either
/// way. The value `runtime` owns is the *decision* (the classification), surfaced
/// here; `store` performs the re-materialization it already implements
/// (`STORE-CACHE-004`). When the cache is `InSync`, no rebuild is driven.
pub fn detect_and_rebuild<S, L, C>(
    store: &mut Store<S, L, C>,
) -> Result<RebuildStrategy, StoreError>
where
    S: SheetsClient,
    L: Lock,
    C: Cache,
{
    let divergence = detect_divergence(store)?;
    let strategy = divergence.strategy();
    match strategy {
        // The cache still mirrors the workbook — serve it as-is.
        RebuildStrategy::None => {}
        // Reconcile (append-only tail) and full-replace both rebuild from the
        // authoritative workbook via store's full-read rebuild; the divergence
        // classification is what runtime owns. (RUNTIME-CACHE-002)
        RebuildStrategy::Reconcile | RebuildStrategy::FullReplace => {
            store.load()?;
        }
    }
    Ok(strategy)
}

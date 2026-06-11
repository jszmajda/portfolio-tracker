//! Global `EventId` assignment + cross-tab uniqueness (RUNTIME-EVENTID-001).
//!
//! `store` derives each id by a toolchain-stable content hash
//! (`STORE-SCHEMA-004`) and *consumes* an assigned id (`STORE-SCHEMA-003`); but a
//! single-tab append can only see the ids on its own tab. `runtime` is the host
//! that holds the **union** of ids across *both* the `Ledger Events` and the
//! `Tax Events` tabs — the cross-tab id set the assignment is bumped/checked against.
//! It enforces the two invariants the design names:
//!
//! - **same content ⇒ same event** (content de-dup, `STORE-WRITE-008`): an event
//!   whose content id already exists is the SAME event — the assignment is the
//!   existing id (a retry is idempotent), never a freshly-minted colliding one.
//! - **distinct events never collide across tabs**: a ledger candidate is bumped
//!   against the cross-tab union ([`assign_ledger_event_id`] over
//!   [`store::assign_event_id`]); a tax id is content-addressed and **prefix-disjoint**
//!   from every ledger id (`tax-…` vs `evt-…`, [`assign_tax_event_id`]). So a ledger
//!   and a tax id can never collide, and distinct same-tab events get distinct ids —
//!   the ledger side by bumping, the tax side by FNV-1a collision-resistance over
//!   distinct content.
//!
//! The id is content-addressed and retry-stable: the same event always hashes to
//! the same nonce/content id, so the assignment returns its existing id on a retry
//! rather than minting a duplicate. `store` then consumes the returned id verbatim.
//!
//! These assigners are the cross-tab-uniqueness gate `runtime` owns; the spec's
//! "wired into the append path" clause is satisfied once the interactive write
//! composers drive a real append through them (`RUNTIME-EVENTID-001` is an active
//! gap, `[ ]`, until then). The invariants below hold of the assignment itself,
//! independent of which writer calls it.

use std::collections::BTreeSet;

use ledger_core::LedgerEvent;
use store::{tax_event_id, EventId, EventLogs};
use tax::TaxEvent;

/// The cross-tab union of every `EventId` currently in the log — the id space a
/// single-tab append cannot see (RUNTIME-EVENTID-001). `runtime` holds it so the
/// next assignment is bumped against ids on BOTH tabs.
///
/// Ledger ids are carried on each [`LedgerEvent`] directly; tax events carry no id
/// field, so their store-assigned, content-derived id is recomputed via
/// [`store::tax_event_id`] (the SAME derivation `store` persists), keeping the
/// union faithful to what is durably on the `Tax Events` tab.
pub fn cross_tab_event_ids(logs: &EventLogs) -> BTreeSet<EventId> {
    let mut ids: BTreeSet<EventId> = BTreeSet::new();
    for e in &logs.ledger {
        ids.insert(e.id.clone());
    }
    for t in &logs.tax {
        ids.insert(tax_event_id(t));
    }
    ids
}

/// A toolchain-stable content nonce for a ledger event — folds the same content
/// cells `store`'s id derivation rests on (the id, date, and kind via the public
/// `serde_rows` projection) into a fixed FNV-1a, so the SAME event always yields
/// the SAME nonce (and therefore the same assigned id on a retry). NOT
/// `DefaultHasher` (whose output is not stable across releases), mirroring
/// `store`'s discipline. (RUNTIME-EVENTID-001, STORE-SCHEMA-004)
fn ledger_content_nonce(event: &LedgerEvent) -> u64 {
    // The store-side row projection is the canonical content cells; fold its cell
    // strings so the nonce is stable across toolchains and platforms.
    let row = store::serde_rows::ledger_to_row(event);
    let cells = row.to_cells(store::Tab::Ledger);
    let mut bytes: Vec<u8> = Vec::new();
    for cell in &cells {
        bytes.extend_from_slice(cell.as_bytes());
        bytes.push(0x1f); // unit separator keeps the projection unambiguous
    }
    fnv1a_64(&bytes)
}

/// FNV-1a 64-bit — a fixed, documented, platform-stable hash (mirrors `store`'s and
/// `reports`' content-hash discipline). Determinism + low collision rate are all
/// that is needed. (RUNTIME-EVENTID-001)
fn fnv1a_64(bytes: &[u8]) -> u64 {
    const OFFSET_BASIS: u64 = 0xcbf2_9ce4_8422_2325;
    const PRIME: u64 = 0x0000_0100_0000_01b3;
    let mut hash = OFFSET_BASIS;
    for &b in bytes {
        hash ^= b as u64;
        hash = hash.wrapping_mul(PRIME);
    }
    hash
}

/// Assign a globally-unique, retry-stable `EventId` for a live LEDGER append,
/// bumped against the cross-tab union of existing ids (RUNTIME-EVENTID-001):
///
/// - **content de-dup** (`STORE-WRITE-008`): if this event's content already exists
///   under some id on EITHER tab, that id is returned — the same content is the
///   same event, so a retry is idempotent and never double-appends. (The lookup is
///   `event.id` when the caller already stamped one; otherwise the content nonce is
///   the addressing.)
/// - **cross-tab non-collision**: a genuinely distinct event is assigned an id that
///   collides with NOTHING on either tab — [`store::assign_event_id`] bumps the
///   candidate until it is free in the cross-tab union, so a ledger id can never
///   collide with a tax id.
///
/// `store` consumes the returned id verbatim (`STORE-SCHEMA-003`); `runtime` owns
/// the cross-tab visibility the assignment rests on.
pub fn assign_ledger_event_id(logs: &EventLogs, event: &LedgerEvent) -> EventId {
    let existing = cross_tab_event_ids(logs);
    // Content de-dup: a caller-stamped id that already exists IS this event (a
    // retry) — return it unchanged rather than minting a new colliding id.
    // (STORE-WRITE-008)
    if !event.id.is_empty() && existing.contains(&event.id) {
        return event.id.clone();
    }
    let nonce = ledger_content_nonce(event);
    store::assign_event_id(&existing, nonce)
}

/// Assign a tax event's id (RUNTIME-EVENTID-001): a tax id is purely
/// **content-addressed** — [`store::tax_event_id`] derives `tax-{fnv1a}` from the
/// event's content cells, the SAME derivation `store` persists. The assignment is
/// that content id, with two consequences the design names:
///
/// - **content de-dup / retry-stable** (`STORE-WRITE-008`): a retry of the SAME tax
///   event recomputes the SAME content id, so if it already exists in the cross-tab
///   union it IS this event — the append is idempotent and never double-appends.
/// - **cross-tab non-collision**: a tax id (`tax-…`) and a ledger id (`evt-…`) are
///   **prefix-disjoint** by construction, so a tax id can never collide with a ledger
///   id regardless of content; two genuinely distinct tax events rely on FNV-1a's
///   collision-resistance over distinct content to receive distinct ids (the same
///   collision-resistance assumption `store`'s tax-id derivation rests on, golden-
///   pinned by `store`'s id test — `STORE-SCHEMA-004`).
///
/// (Contrast the LEDGER side, [`assign_ledger_event_id`], whose nonce-derived
/// candidate is *bumped* against the union via [`store::assign_event_id`]; the tax id
/// carries no bump because it is the persisted content address itself, not a
/// nonce-plus-bump.)
pub fn assign_tax_event_id(logs: &EventLogs, event: &TaxEvent) -> EventId {
    let existing = cross_tab_event_ids(logs);
    let content_id = tax_event_id(event);
    // The content id IS the event's identity (retry-stable). If it already exists it
    // is the same event (idempotent); return it unchanged. (STORE-WRITE-008)
    if existing.contains(&content_id) {
        return content_id;
    }
    // A free content id is the assignment (this is the content address `store`
    // persists). It is prefix-disjoint from every ledger id, so it cannot collide
    // across tabs; distinct tax events get distinct ids by FNV-1a collision-resistance.
    content_id
}

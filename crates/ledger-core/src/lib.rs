//! `ledger-core` — the verified event-sourced accounting kernel.
//!
//! Owns the event taxonomy, the tax-lot model, deterministic replay, and the
//! realized/unrealized P&L math — and nothing else (tax calculation, the
//! accrual lifecycle, persistence, Sheets, and the TUI live elsewhere). See
//! `docs/intent/ledger-core/ledger-core-design.md` and `-specs.md`.
//!
//! Mirrors the prior verified project's engine core: the pure accounting fold lives in
//! the `verus!{}` module below (erased under stable `cargo build`, proven under
//! `cargo verus verify`); the public API surface is plain Rust the verified
//! fold backs. `#[cfg(kani)]` bounded harnesses live in `kani_proofs`.
//!
//! TDD scaffold: the entry-point bodies (`replay`, `validate`) and the verus
//! fold are stubbed with `unimplemented!()`/`todo!()` so the suite fails RED
//! rather than fail-to-compile. Signatures are complete.
//!
//! TRUST BOUNDARY: this crate takes an already-deserialized `Vec<LedgerEvent>`
//! and returns a `Snapshot`. Serde and all I/O sit OUTSIDE this boundary (the
//! `store` segment), exactly as in the prior verified project.

use std::collections::BTreeMap;

use pt_core::{Cents, Date, MicroShares, Seq};

// ===========================================================================
// Identifier aliases (ledger-core-design.md → "Money & Quantity Types").
// Interning / case-normalization happen at the input boundary, not here.
// ===========================================================================

/// Ticker, e.g. `AMZN`. Case-normalized at the input boundary.
pub type Symbol = String;
/// Stable, unique-per-log identifier for a tax lot (designates lots at sale).
pub type LotId = String;
/// Stable identifier for an event (referenced by a `Reversal`).
pub type EventId = String;
/// Stable identifier for a sale (the `(sale_id, lot_id)` key of a `RealizedGain`).
pub type SaleId = String;

// ===========================================================================
// Event taxonomy (ledger-core-design.md → "Event Taxonomy & Ordering";
// LEDGER-EVENT-001..007).
//
// A `LedgerEvent` carries an `EventId`, a `Seq`, an event `Date`, and one kind.
// Replay folds in ascending `Seq` order; `Date` is data, never a sort key.
// ===========================================================================

/// A single entry in the append-only ledger.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct LedgerEvent {
    /// Stable identity, referenced by a `Reversal`.
    pub id: EventId,
    /// Monotonic per-log sequence number; the total fold order.
    pub seq: Seq,
    /// Event date (data; never the fold's sort key).
    pub date: Date,
    /// The event payload.
    pub kind: LedgerEventKind,
}

/// One `(lot_id, qty)` designation in a specific-identification `Sell`.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct LotRef {
    pub lot_id: LotId,
    pub qty: MicroShares,
}

/// The event payload variants (Buy / Vest / Sell / Split / Reversal).
#[derive(Clone, PartialEq, Eq, Debug)]
pub enum LedgerEventKind {
    /// Opens a lot. `total_basis = scale(qty × unit_price) + fees`; holding
    /// clock starts at the event date. (LEDGER-EVENT-002)
    Buy {
        lot_id: LotId,
        symbol: Symbol,
        qty: MicroShares,
        unit_price_cents: Cents,
        fees_cents: Cents,
        platform: String,
        tracking_code: Option<String>,
    },
    /// Opens a lot at fair-market value already taxed at vest:
    /// `total_basis = scale(qty × fmv_per_share)`; holding clock starts at the
    /// vest date. No fee field. (LEDGER-EVENT-003)
    Vest {
        lot_id: LotId,
        symbol: Symbol,
        qty: MicroShares,
        fmv_per_share_cents: Cents,
        platform: String,
        tracking_code: Option<String>,
    },
    /// Disposes `qty` shares. `lot_refs` is explicit `(lot_id, qty)` pairs
    /// (specific-identification) or empty (FIFO fallback). `accrues_to_state`
    /// is stored for `tax`; `ledger-core` does not read it. Produces one
    /// `RealizedGain` per consumed `(lot, qty)` pair. (LEDGER-EVENT-004)
    Sell {
        sale_id: SaleId,
        symbol: Symbol,
        qty: MicroShares,
        unit_price_cents: Cents,
        fees_cents: Cents,
        lot_refs: Vec<LotRef>,
        accrues_to_state: Option<String>,
        platform: String,
        tracking_code: Option<String>,
    },
    /// A `ratio_num : ratio_den` split (`ratio_num ≥ 1`, `ratio_den ≥ 1`).
    /// Rescales each open lot's `remaining_qty`; basis untouched.
    /// (LEDGER-EVENT-005, LEDGER-SPLIT-*)
    Split {
        symbol: Symbol,
        ratio_num: i64,
        ratio_den: i64,
    },
    /// Append-only correction: replay folds the log with `target_event_id`
    /// filtered out ("as if it never occurred"). (LEDGER-EVENT-006)
    Reversal { target_event_id: EventId },
}

/// How a lot was opened.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum LotSource {
    Buy,
    Vest,
}

// ===========================================================================
// Tax lots (ledger-core-design.md → "Tax Lots & Lot Selection";
// LEDGER-LOT-001..006).
// ===========================================================================

/// The unit of cost basis. `remaining_basis_cents` is the source of truth for
/// basis (only decreases via Sell, never rescaled by a Split); `remaining_qty`
/// decreases (Sell) or rescales (Split). Both held non-negative.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct Lot {
    pub id: LotId,
    pub symbol: Symbol,
    pub acquire_date: Date,
    /// `Seq` of the opening event; breaks same-date FIFO ties deterministically.
    pub open_seq: Seq,
    pub source: LotSource,
    pub remaining_qty: MicroShares,
    pub remaining_basis_cents: Cents,
    pub platform: String,
    pub tracking_code: Option<String>,
}

// ===========================================================================
// Realized gains — the interface to `tax` (ledger-core-design.md → "Replay";
// LEDGER-PNL-003..005).
//
// One per consumed (lot, qty) pair, with stable identity `(sale_id, lot_id)`.
// ===========================================================================

/// One realized disposal of a single lot's shares within a sale.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct RealizedGain {
    pub sale_id: SaleId,
    /// The Sell event's `Seq`, so `tax` can order by `(sale_date, sale_seq)`.
    pub sale_seq: Seq,
    pub lot_id: LotId,
    pub symbol: Symbol,
    pub sale_date: Date,
    pub proceeds_cents: Cents,
    pub basis_cents: Cents,
    /// `proceeds − basis`; may be negative (losses, fees exceeding gross).
    pub gain_cents: Cents,
    pub acquire_date: Date,
    /// `sale_date − acquire_date` in days; LT/ST classification is `tax`'s job.
    pub holding_days: i32,
    /// Stored for `tax`; `ledger-core` does not read it.
    pub accrues_to_state: Option<String>,
}

// ===========================================================================
// Snapshot (ledger-core-design.md → "Replay"; LEDGER-PNL-009/010).
// ===========================================================================

/// Per-symbol rollup in a `Snapshot`. `unrealized_cents` is `None` when no mark
/// was supplied for the symbol (degraded), never zero. (LEDGER-PNL-007..009)
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct Position {
    pub symbol: Symbol,
    pub total_qty: MicroShares,
    pub total_basis_cents: Cents,
    pub realized_pnl_cents: Cents,
    pub unrealized_cents: Option<Cents>,
}

/// An open lot annotated with its per-lot unrealized P&L, so downstream tax
/// estimation needs no second rounding site. (LEDGER-PNL-010)
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct OpenLot {
    pub lot: Lot,
    /// `scale(mark × remaining_qty) − remaining_basis`, or `None` when the mark
    /// is degraded.
    pub unrealized_cents: Option<Cents>,
}

/// The output of `replay`: positions, open lots with per-lot unrealized, and
/// the realized gains feeding `tax`.
#[derive(Clone, PartialEq, Eq, Debug, Default)]
pub struct Snapshot {
    pub positions: BTreeMap<Symbol, Position>,
    pub open_lots: Vec<OpenLot>,
    pub realized_gains: Vec<RealizedGain>,
}

// ===========================================================================
// Error model (ledger-core-design.md → "Replay, Validation & Error Model";
// LEDGER-ERR-001..011). One variant per rejection trigger — all 11.
// ===========================================================================

/// A rejection of a candidate event at append time. Rejection leaves the log
/// and state untouched (no partial mutation — LEDGER-VERIF-004).
#[derive(Clone, PartialEq, Eq, Debug)]
pub enum LedgerError {
    /// A `Buy`/`Vest`/`Sell` with `qty ≤ 0`. (LEDGER-ERR-002)
    NonPositiveQty,
    /// A `Buy`/`Vest` reusing an existing `LotId`. (LEDGER-ERR-003)
    DuplicateLotId,
    /// A `Sell` naming a missing lot. (LEDGER-ERR-004)
    UnknownLot,
    /// A `Sell` naming a lot of another symbol. (LEDGER-ERR-004)
    WrongSymbolLot,
    /// Designated quantities don't sum to the sale `qty`. (LEDGER-ERR-005)
    LotRefsMismatch,
    /// The same `lot_id` named twice within one Sell. (LEDGER-ERR-006)
    DuplicateLotRef,
    /// A designated/FIFO lot not on the sale's platform. (LEDGER-ERR-007)
    WrongPlatform,
    /// Specific-ID or FIFO can't cover the sale `qty` (no short sales).
    /// (LEDGER-ERR-008)
    InsufficientShares,
    /// A `Split` with `ratio_num < 1` or `ratio_den < 1`. (LEDGER-ERR-009)
    BadSplitRatio,
    /// Reversal target unknown, already reversed, or a surviving lower-`Seq`
    /// event depends on it. (LEDGER-ERR-010)
    BadReversal,
    /// A scaled `Cents`/`MicroShares` result exceeds `MONEY_CAP`.
    /// (LEDGER-ERR-011)
    AmountOutOfRange,
}

impl std::fmt::Display for LedgerError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{self:?}")
    }
}

impl std::error::Error for LedgerError {}

// ===========================================================================
// Entry points (ledger-core-design.md → "Trust Boundary & Interfaces").
//
// These delegate to the verified accounting fold in `mod core` (the `verus!{}`
// module, which erases under stable `cargo build`). The public surface is plain
// Rust the kernel backs.
// ===========================================================================

/// Per-whole-share current marks, supplied by the caller (`runtime`) for
/// valuation. Absent symbols degrade per-symbol (LEDGER-PNL-007).
pub type Marks = BTreeMap<Symbol, Cents>;

/// Fold the surviving events (reversed targets filtered) in `Seq` order into a
/// `Snapshot`. Pure, deterministic, total over a valid log (LEDGER-VERIF-005).
///
/// A stored log is valid by construction (events pass `validate` at append
/// time), so `replay` never encounters an event it must reject.
pub fn replay(events: &[LedgerEvent], marks: &Marks) -> Snapshot {
    core::replay_impl(events, marks)
}

/// Append-time validation: check a candidate event against the replayed-so-far
/// state (with reversed targets filtered out) before it is appended.
/// `Ok(())` ⇒ accept (append); `Err` ⇒ reject, leaving log/state untouched.
/// (LEDGER-ERR-001)
pub fn validate(
    accepted: &[LedgerEvent],
    candidate: &LedgerEvent,
) -> Result<(), LedgerError> {
    core::validate_impl(accepted, candidate)
}

// ===========================================================================
// The verified accounting core (ledger-core-design.md → "Context and Design
// Philosophy"). Mirrors the prior verified project's engine core.
//
// Under plain stable `cargo build` the `verus!{}` macro erases and everything
// here is ordinary Rust. Under the pinned `cargo verus verify` toolchain the
// arithmetic kernel (scale, round_half_to_even, consume-from-lot,
// largest-remainder allocation) is deductively verified against the
// requires/ensures contracts. The fold orchestration over the event log (which
// uses BTreeMap/String for symbol/lot maps) is carried as `external_body` exec
// functions — verus admits their signature contract and trusts the body, while
// stable rustc compiles them as ordinary Rust.
// ===========================================================================
mod core {
    use vstd::prelude::*;

    use std::collections::{BTreeMap, BTreeSet};

    use pt_core::{Cents, Date, MicroShares, Seq};

    use crate::{
        LedgerError, LedgerEvent, LedgerEventKind, Lot, LotSource, Marks, OpenLot,
        Position, RealizedGain, Snapshot,
    };

    verus! {

    // The vstd arithmetic lemmas live in the `#[cfg(verus_keep_ghost)]`-gated
    // `vstd::arithmetic` module, which does NOT exist under stable `cargo build`
    // (where verus!{} erases). The `use`s are therefore gated on the same cfg
    // Verus sets — present only under the Verus toolchain, absent (with the proof
    // calls that reference them) under stable. (cfg allowed in Cargo.toml lints.)
    #[cfg(verus_keep_ghost)]
    use vstd::arithmetic::div_mod::{lemma_fundamental_div_mod, lemma_mod_bound};
    #[cfg(verus_keep_ghost)]
    use vstd::arithmetic::mul::{lemma_mul_inequality, lemma_mul_is_commutative};

    // Mirror of pt_core's constants, declared inside verus!{} (Verus cannot see
    // a const imported from a non-Verus crate). The drift-guard test locks these
    // against pt_core::{SHARE_SCALE, MONEY_CAP} so the mirror cannot drift.
    pub const SHARE_SCALE: i128 = 1_000_000;
    pub const MONEY_CAP: i128 = 0x100_0000_0000; // 1 << 40 == 1_099_511_627_776

    // An operand cap for the in-kernel i128 multiplications. Every value the
    // orchestration feeds the kernel is an `i64` field (Cents / MicroShares), so
    // |operand| < 2^62; bounding both multiplicands by this keeps every product
    // far below i128::MAX (2^62 * 2^62 = 2^124 < 2^127), discharging the i128
    // overflow obligation honestly rather than by assume!. The call sites pass
    // values bounded by MONEY_CAP (2^40), well inside OPERAND_CAP.
    // (Used only in erased `requires`/proof context, so dead under stable.)
    #[allow(dead_code)]
    pub const OPERAND_CAP: i128 = 0x4000_0000_0000_0000; // 1 << 62
    // The product cap: |a*b| <= PROD_CAP == 2^124, concretely below i128::MAX
    // (2^127 − 1), so a bounded i128 product provably does not overflow.
    #[allow(dead_code)]
    pub const PROD_CAP: i128 = 0x1000_0000_0000_0000_0000_0000_0000_0000; // 1 << 124

    // -----------------------------------------------------------------------
    // Trusted std euclidean specs. The ONLY assume_specification in the kernel:
    // we trust that `i128::div_euclid`/`rem_euclid`, FOR A POSITIVE DIVISOR,
    // compute exactly Verus's spec `int` division / modulo (which are euclidean
    // — floor division — so the remainder is in `[0, den)`). Every call site
    // passes a strictly positive divisor (den / remaining_qty / total > 0), the
    // documented precondition. This is the tightest possible surface: it states
    // the euclidean identity Rust's std already guarantees and nothing more.
    // -----------------------------------------------------------------------

    pub assume_specification[<i128>::div_euclid](num: i128, den: i128) -> (q: i128)
        requires den > 0,
        ensures q as int == (num as int) / (den as int);

    pub assume_specification[<i128>::rem_euclid](num: i128, den: i128) -> (r: i128)
        requires den > 0,
        ensures r as int == (num as int) % (den as int);

    // -----------------------------------------------------------------------
    // SPEC: banker's rounding (round half to even) over `int`. The contract the
    // executable `round_half_to_even` refines. `f = floor(num/den)`,
    // `r = num - f*den ∈ [0, den)`; round to nearest, ties to the even quotient.
    // -----------------------------------------------------------------------
    pub open spec fn round_half_to_even_spec(num: int, den: int) -> int
        recommends den > 0,
    {
        let f = num / den;
        let r = num % den;
        if 2 * r < den {
            f
        } else if 2 * r > den {
            f + 1
        } else if f % 2 == 0 {
            f
        } else {
            f + 1
        }
    }

    // The defining roundness bound: the chosen integer multiple is within half a
    // denominator of `num` (|result*den − num| ≤ den, and strictly the nearest:
    // 2*|result*den − num| ≤ den). PROVEN from the fundamental div-mod identity
    // and the mod range, by case on which side of the tie `r` falls.
    pub proof fn lemma_round_half_to_even_nearest(num: int, den: int)
        requires den > 0,
        ensures
            2 * (round_half_to_even_spec(num, den) * den - num) <= den,
            2 * (num - round_half_to_even_spec(num, den) * den) <= den,
    {
        lemma_fundamental_div_mod(num, den);  // num == den*(num/den) + num%den
        lemma_mod_bound(num, den);            // 0 <= num%den < den
        let f = num / den;
        let r = num % den;
        // num == den*f + r, 0 <= r < den, so f*den - num == -r.
        assert(den * f == f * den) by { lemma_mul_is_commutative(den, f); }
        assert(f * den - num == -r);
        // (f+1)*den - num == den - r.
        assert((f + 1) * den == f * den + den) by(nonlinear_arith);
        assert((f + 1) * den - num == den - r);
    }

    // The quotient `num/den` stays inside [-PROD_CAP, PROD_CAP] when |num| is
    // PROD_CAP-bounded and den >= 1: from num == den*q + r with 0 <= r < den,
    // den >= 1, |num| <= PROD_CAP we get den*q in [num-den, num], hence
    // |q| <= |num| <= PROD_CAP (den >= 1). PROVEN — bounds the exec `q`/`q+1`.
    proof fn lemma_quotient_bounded(num: int, den: int)
        requires
            den > 0,
            -PROD_CAP <= num <= PROD_CAP,
        ensures
            -PROD_CAP <= num / den <= PROD_CAP,
    {
        lemma_fundamental_div_mod(num, den);  // num == den*(num/den) + num%den
        lemma_mod_bound(num, den);            // 0 <= num%den < den
        let q = num / den;
        let r = num % den;
        // num == den*q + r; 0 <= r, den >= 1. Show -PROD_CAP <= q <= PROD_CAP.
        assert(-PROD_CAP <= q <= PROD_CAP) by(nonlinear_arith)
            requires
                den >= 1,
                num == den * q + r,
                0 <= r,
                r < den,
                -PROD_CAP <= num <= PROD_CAP;
    }

    // -----------------------------------------------------------------------
    // Arithmetic kernel — the verified money math. These are pure exec `fn`s
    // (no maps/strings), so verus checks them directly against their contracts.
    // -----------------------------------------------------------------------

    /// Banker's rounding of `num / den` (round half to even), in i128. This is
    /// the single rounding rule the kernel applies (matches `pt_core::round_*`).
    pub fn round_half_to_even(num: i128, den: i128) -> (r: i128)
        requires
            den > 0,
            // den is an i64 field (remaining_qty / total / SHARE_SCALE / ratio_den)
            // at every call site, so 2*rem (rem < den) cannot overflow i128.
            den <= OPERAND_CAP,
            // The result is `floor(num/den)` or `+1`; both stay in i128 because
            // |num| <= 2^124 and den >= 1 keep the quotient well inside range.
            -PROD_CAP <= num <= PROD_CAP,
        ensures
            r as int == round_half_to_even_spec(num as int, den as int),
            // Nearest-integer (banker's) bound: 2|r*den − num| ≤ den.
            2 * (r as int * den as int - num as int) <= den as int,
            2 * (num as int - r as int * den as int) <= den as int,
    {
        let q = num.div_euclid(den);
        let rem = num.rem_euclid(den); // 0 <= rem < den
        proof {
            lemma_round_half_to_even_nearest(num as int, den as int);
            lemma_mod_bound(num as int, den as int);
            lemma_fundamental_div_mod(num as int, den as int);
            // q == num/den; |num| <= 2^124, den >= 1, so |q| <= 2^124 << i128::MAX,
            // and rem < den <= 2^62 so 2*rem < 2^63 and q+1 cannot overflow.
            lemma_quotient_bounded(num as int, den as int);
            // div_euclid/rem_euclid ensures (den > 0): q == num/den, rem == num%den.
            assert(q as int == (num as int) / (den as int));
            assert(rem as int == (num as int) % (den as int));
            assert(-PROD_CAP <= q as int <= PROD_CAP);   // from lemma_quotient_bounded
            assert(0 <= rem as int);                       // from lemma_mod_bound
            assert((rem as int) < (den as int));           // from lemma_mod_bound
            assert((den as int) <= OPERAND_CAP);           // precondition
        }
        let twice = 2 * rem;
        if twice < den {
            q
        } else if twice > den {
            q + 1
        } else if q % 2 == 0 {
            // exactly halfway → round to the even quotient
            q
        } else {
            q + 1
        }
    }

    /// The `SHARE_SCALE` conversion: `round_half_to_even(micro / 1e6)`, in i128.
    /// The single site where a `qty × price` product crosses into `Cents`.
    pub fn scale(micro: i128) -> (r: i128)
        requires -PROD_CAP <= micro <= PROD_CAP,
        ensures r as int == round_half_to_even_spec(micro as int, SHARE_SCALE as int),
    {
        assert(0 < SHARE_SCALE <= OPERAND_CAP) by(compute); // 1 <= 10^6 <= 2^62
        round_half_to_even(micro, SHARE_SCALE)
    }

    /// Range-check a scaled `Cents`/`MicroShares` result against `MONEY_CAP`.
    /// `true` ⇒ in range; `false` ⇒ would be `AmountOutOfRange`. The bound is
    /// two-sided (negatives — losses, reversals — are bounded too).
    pub fn within_cap(v: i128) -> (ok: bool)
        ensures ok == (-(MONEY_CAP as int) <= v as int <= MONEY_CAP as int),
    {
        let cap = MONEY_CAP; // == 0x100_0000_0000 (2^40); -cap cannot overflow i128
        -cap <= v && v <= cap
    }

    // Helper: the in-kernel product of two `OPERAND_CAP`-bounded i128s stays
    // inside i128 range (so the exec `*` does not overflow). PROVEN by bounding
    // the magnitudes; PROD_CAP == 2^124 < i128::MAX == 2^127 − 1.
    proof fn lemma_product_in_range(a: int, b: int)
        requires
            -OPERAND_CAP <= a <= OPERAND_CAP,
            -OPERAND_CAP <= b <= OPERAND_CAP,
        ensures
            -PROD_CAP <= a * b <= PROD_CAP,
    {
        assert(OPERAND_CAP * OPERAND_CAP == PROD_CAP) by(compute);
        assert(-(OPERAND_CAP * OPERAND_CAP) <= a * b <= OPERAND_CAP * OPERAND_CAP) by(nonlinear_arith)
            requires
                -OPERAND_CAP <= a <= OPERAND_CAP,
                -OPERAND_CAP <= b <= OPERAND_CAP;
    }

    /// Consume `c` shares from a lot with `(remaining_basis, remaining_qty)`,
    /// returning `(consumed_basis, new_remaining_basis)`. When `c` is the lot's
    /// last share the entire residual basis is swept (zero rounding loss at
    /// closure); otherwise the consumed basis is the rounded proportional share.
    /// (LEDGER-LOT-002 / LEDGER-LOT-003)
    pub fn consume_basis(remaining_basis: i128, remaining_qty: i128, c: i128) -> (out: (i128, i128))
        requires
            remaining_qty > 0,
            remaining_qty <= OPERAND_CAP,
            c >= 0,
            c <= remaining_qty,
            -OPERAND_CAP <= remaining_basis <= OPERAND_CAP,
            -OPERAND_CAP <= c <= OPERAND_CAP,
        ensures
            // Conservation: consumed + new-remaining == the original basis.
            out.0 as int + out.1 as int == remaining_basis as int,
            // The consumed share lies between 0 and the whole basis (when basis
            // is non-negative, as it always is for a real lot).
            remaining_basis as int >= 0 ==> 0 <= out.0 as int <= remaining_basis as int,
            // Closure sweep: consuming the last share moves the WHOLE basis with
            // zero residual (no rounding loss at lot closure).
            c == remaining_qty ==> (out.0 as int == remaining_basis as int && out.1 as int == 0),
            // Non-closure: the consumed basis is the banker's-rounded share.
            c < remaining_qty ==>
                out.0 as int == round_half_to_even_spec(
                    remaining_basis as int * c as int, remaining_qty as int),
    {
        if c == remaining_qty {
            // Residual sweep at closure: the whole basis transfers exactly.
            (remaining_basis, 0)
        } else {
            proof {
                lemma_product_in_range(remaining_basis as int, c as int);
            }
            // The product is bounded by PROD_CAP (< i128::MAX), so the exec
            // multiply does not overflow; round_half_to_even's `num` precondition
            // (-PROD_CAP <= num <= PROD_CAP) is met.
            let prod: i128 = remaining_basis * c;
            let cb = round_half_to_even(prod, remaining_qty);
            proof {
                // |cb| <= OPERAND_CAP + 1, so `remaining_basis - cb` cannot
                // overflow i128 (both magnitudes <= 2^62, difference < 2^63).
                lemma_cb_magnitude_bounded(
                    remaining_basis as int, remaining_qty as int, c as int, cb as int);
                if remaining_basis as int >= 0 {
                    lemma_consume_share_bounds(
                        remaining_basis as int, remaining_qty as int, c as int, cb as int);
                }
            }
            (cb, remaining_basis - cb)
        }
    }

    // The rounded consumed share `cb` of a non-negative basis over `c <= rq`
    // shares satisfies `0 <= cb <= remaining_basis`. PROVEN from the nearest
    // bound (2*(cb*rq − basis*c) <= rq and 2*(basis*c − cb*rq) <= rq) plus the
    // monotonicity `0 <= basis*c <= basis*rq` (since 0 <= c <= rq, basis >= 0).
    proof fn lemma_consume_share_bounds(basis: int, rq: int, c: int, cb: int)
        requires
            basis >= 0,
            rq > 0,
            0 <= c <= rq,
            2 * (cb * rq - basis * c) <= rq,
            2 * (basis * c - cb * rq) <= rq,
        ensures
            0 <= cb <= basis,
    {
        // 0 <= c*basis and c*basis <= rq*basis (monotone in the share count).
        lemma_mul_inequality(0, c, basis);          // 0 <= c ==> 0*basis <= c*basis
        lemma_mul_inequality(c, rq, basis);          // c <= rq ==> c*basis <= rq*basis
        assert(0 * basis == 0) by(nonlinear_arith);
        assert(c * basis == basis * c) by { lemma_mul_is_commutative(c, basis); }
        assert(rq * basis == basis * rq) by { lemma_mul_is_commutative(rq, basis); }
        // Now: 0 <= basis*c <= basis*rq.
        assert(cb >= 0) by(nonlinear_arith)
            requires rq > 0, basis * c >= 0, 2 * (basis * c - cb * rq) <= rq;
        assert(cb <= basis) by(nonlinear_arith)
            requires rq > 0, basis * c <= basis * rq, 2 * (cb * rq - basis * c) <= rq;
    }

    // |cb| <= OPERAND_CAP + 1 for the rounded share of an OPERAND_CAP-bounded
    // basis over 0 <= c <= rq, so `remaining_basis − cb` stays in i128 range.
    // From the nearest bound 2|cb*rq − basis*c| <= rq and |basis*c| <= |basis|*rq
    // <= OPERAND_CAP*rq we get |cb*rq| <= (OPERAND_CAP + 1)*rq, hence (rq > 0)
    // |cb| <= OPERAND_CAP + 1. PROVEN.
    proof fn lemma_cb_magnitude_bounded(basis: int, rq: int, c: int, cb: int)
        requires
            -OPERAND_CAP <= basis <= OPERAND_CAP,
            rq > 0,
            0 <= c <= rq,
            2 * (cb * rq - basis * c) <= rq,
            2 * (basis * c - cb * rq) <= rq,
        ensures
            -(OPERAND_CAP + 1) <= cb <= OPERAND_CAP + 1,
    {
        let cap: int = OPERAND_CAP as int;
        // |basis*c| <= cap * rq, proved in two monotone steps each side.
        // Upper: basis*c <= cap*c <= cap*rq.
        lemma_mul_inequality(basis, cap, c);     // basis*c <= cap*c
        lemma_mul_inequality(c, rq, cap);        // c*cap <= rq*cap
        assert(cap * c == c * cap) by { lemma_mul_is_commutative(cap, c); }
        assert(cap * rq == rq * cap) by { lemma_mul_is_commutative(cap, rq); }
        assert(basis * c <= cap * rq);
        // Lower: -cap*c <= basis*c, and -cap*rq <= -cap*c.
        lemma_mul_inequality(-cap, basis, c);    // -cap*c <= basis*c
        assert((-cap) * c == -(cap * c)) by(nonlinear_arith);
        assert(-(cap * rq) <= -(cap * c));
        assert(-(cap * rq) <= basis * c);
        // From the nearest bounds, divide through rq > 0.
        assert(cb <= cap + 1) by(nonlinear_arith)
            requires rq > 0, basis * c <= cap * rq, 2 * (cb * rq - basis * c) <= rq;
        assert(-(cap + 1) <= cb) by(nonlinear_arith)
            requires rq > 0, -(cap * rq) <= basis * c, 2 * (basis * c - cb * rq) <= rq;
    }

    /// Floor of `net × c / total` under the floor convention (toward −∞), the
    /// per-lot baseline of the largest-remainder proceeds allocation. Defined
    /// for signed `net`. (LEDGER-PNL-002)
    pub fn alloc_floor(net: i128, c: i128, total: i128) -> (f: i128)
        requires
            total > 0,
            total <= OPERAND_CAP,
            -OPERAND_CAP <= net <= OPERAND_CAP,
            -OPERAND_CAP <= c <= OPERAND_CAP,
        ensures
            // Floor of the exact share: f == floor((net*c)/total).
            f as int == (net as int * c as int) / (total as int),
            // Fundamental identity binding floor and remainder:
            //   net*c == f*total + alloc_remainder, with the remainder in [0,total).
            net as int * c as int
                == f as int * total as int + (net as int * c as int) % (total as int),
    {
        proof {
            lemma_product_in_range(net as int, c as int);
            lemma_fundamental_div_mod(net as int * c as int, total as int);
            // fundamental gives total*(prod/total); commute to (prod/total)*total.
            let prodv = net as int * c as int;
            lemma_mul_is_commutative(total as int, prodv / (total as int));
        }
        let prod: i128 = net * c;
        prod.div_euclid(total)
    }

    /// Fractional remainder of `net × c / total` in `[0, total)`, the
    /// largest-remainder ranking key for distributing the residual proceeds
    /// unit(s). (LEDGER-PNL-002)
    pub fn alloc_remainder(net: i128, c: i128, total: i128) -> (rem: i128)
        requires
            total > 0,
            total <= OPERAND_CAP,
            -OPERAND_CAP <= net <= OPERAND_CAP,
            -OPERAND_CAP <= c <= OPERAND_CAP,
        ensures
            rem as int == (net as int * c as int) % (total as int),
            // Range: the remainder is in [0, total) (floor convention).
            0 <= rem as int,
            (rem as int) < (total as int),
            // alloc_floor*total + alloc_remainder == net*c exactly (conservation
            // hook for the largest-remainder Σ proceeds == net identity).
            (net as int * c as int) / (total as int) * total as int + rem as int
                == net as int * c as int,
    {
        proof {
            lemma_product_in_range(net as int, c as int);
            lemma_mod_bound(net as int * c as int, total as int);
            lemma_fundamental_div_mod(net as int * c as int, total as int);
            let prodv = net as int * c as int;
            lemma_mul_is_commutative(total as int, prodv / (total as int));
        }
        let prod: i128 = net * c;
        prod.rem_euclid(total)
    }

    } // verus!

    // =======================================================================
    // Fold orchestration. Carried as plain Rust (outside the verus! block, but
    // inside the verified crate) — it drives the arithmetic kernel above and
    // owns the BTreeMap/String bookkeeping that is out of scope for deductive
    // verification but in scope for the Kani bounded harnesses. Under stable
    // build this is ordinary Rust; the money decisions all route through the
    // verus exec functions, keeping the single rounding/scale/cap site inside
    // the verified boundary.
    // =======================================================================

    /// A working tax lot during the fold (the mutable form of `Lot`), tagged
    /// with the `EventId` that opened it for the Reversal dependency check.
    #[derive(Clone)]
    struct WorkLot {
        id: String,
        symbol: String,
        acquire_date: Date,
        open_seq: Seq,
        source: LotSource,
        remaining_qty: i64,
        remaining_basis: i64,
        platform: String,
        tracking_code: Option<String>,
        /// The event that opened this lot (for backward dependency analysis).
        open_event: String,
        /// Has this lot been fully consumed? Closed lots are not reported open
        /// and are not rescaled by Splits, but retain their id (uniqueness).
        closed: bool,
    }

    /// The mutable accounting state threaded through the fold.
    struct FoldState {
        /// Lots in insertion (open) order; closed lots retained for id-uniqueness
        /// and dependency analysis but skipped in selection/reporting.
        lots: Vec<WorkLot>,
        /// All realized gains, in production order.
        gains: Vec<RealizedGain>,
        /// Symbols ever seen via Buy/Vest (so a position can report even after
        /// full consumption / never appears for a never-held symbol).
        symbols_seen: BTreeSet<String>,
        /// For the dependency check: per opening EventId, has any surviving Sell
        /// consumed or any surviving Split rescaled a lot it opened?
        depended_on: BTreeSet<String>,
    }

    impl FoldState {
        fn new() -> Self {
            FoldState {
                lots: Vec::new(),
                gains: Vec::new(),
                symbols_seen: BTreeSet::new(),
                depended_on: BTreeSet::new(),
            }
        }

        fn find_lot_idx(&self, lot_id: &str) -> Option<usize> {
            self.lots.iter().position(|l| l.id == lot_id)
        }
    }

    /// Compute the surviving events: the input log with every event that is the
    /// target of a (surviving) Reversal filtered out, plus the Reversals
    /// themselves dropped (they carry no positive effect). Events are returned
    /// in ascending `Seq` order — `Date` is data, never a sort key
    /// (LEDGER-EVENT-001).
    fn surviving_sorted(events: &[LedgerEvent]) -> Vec<LedgerEvent> {
        // Collect reversal targets (a Reversal "removes" its target from the fold).
        let mut reversed: BTreeSet<String> = BTreeSet::new();
        for e in events {
            if let LedgerEventKind::Reversal { target_event_id } = &e.kind {
                reversed.insert(target_event_id.clone());
            }
        }
        let mut out: Vec<LedgerEvent> = events
            .iter()
            .filter(|e| {
                // Drop the Reversals themselves and any reversed target.
                !matches!(e.kind, LedgerEventKind::Reversal { .. })
                    && !reversed.contains(&e.id)
            })
            .cloned()
            .collect();
        // Fold in ascending Seq order (stable on Seq; Date is inert).
        out.sort_by_key(|e| e.seq.0);
        out
    }

    /// Apply one (already-surviving, non-Reversal) event to the fold state.
    /// `track_deps` records dependency edges for the Reversal backward check.
    /// Returns `Err` only if an event the validator should have rejected slips
    /// through — over a valid (validated) log this never happens, keeping
    /// `replay` total (the design's totality contract).
    fn apply_event(st: &mut FoldState, e: &LedgerEvent) -> Result<(), LedgerError> {
        match &e.kind {
            LedgerEventKind::Buy {
                lot_id,
                symbol,
                qty,
                unit_price_cents,
                fees_cents,
                platform,
                tracking_code,
            } => {
                if qty.0 <= 0 {
                    return Err(LedgerError::NonPositiveQty);
                }
                if st.lots.iter().any(|l| l.id == *lot_id) {
                    return Err(LedgerError::DuplicateLotId);
                }
                let scaled = scale((qty.0 as i128) * (unit_price_cents.0 as i128));
                if !within_cap(scaled) {
                    return Err(LedgerError::AmountOutOfRange);
                }
                let basis = scaled + fees_cents.0 as i128;
                if !within_cap(basis) {
                    return Err(LedgerError::AmountOutOfRange);
                }
                st.symbols_seen.insert(symbol.clone());
                st.lots.push(WorkLot {
                    id: lot_id.clone(),
                    symbol: symbol.clone(),
                    acquire_date: e.date,
                    open_seq: e.seq,
                    source: LotSource::Buy,
                    remaining_qty: qty.0,
                    remaining_basis: basis as i64,
                    platform: platform.clone(),
                    tracking_code: tracking_code.clone(),
                    open_event: e.id.clone(),
                    closed: false,
                });
                Ok(())
            }
            LedgerEventKind::Vest {
                lot_id,
                symbol,
                qty,
                fmv_per_share_cents,
                platform,
                tracking_code,
            } => {
                if qty.0 <= 0 {
                    return Err(LedgerError::NonPositiveQty);
                }
                if st.lots.iter().any(|l| l.id == *lot_id) {
                    return Err(LedgerError::DuplicateLotId);
                }
                let basis = scale((qty.0 as i128) * (fmv_per_share_cents.0 as i128));
                if !within_cap(basis) {
                    return Err(LedgerError::AmountOutOfRange);
                }
                st.symbols_seen.insert(symbol.clone());
                st.lots.push(WorkLot {
                    id: lot_id.clone(),
                    symbol: symbol.clone(),
                    acquire_date: e.date,
                    open_seq: e.seq,
                    source: LotSource::Vest,
                    remaining_qty: qty.0,
                    remaining_basis: basis as i64,
                    platform: platform.clone(),
                    tracking_code: tracking_code.clone(),
                    open_event: e.id.clone(),
                    closed: false,
                });
                Ok(())
            }
            LedgerEventKind::Sell { .. } => apply_sell(st, e),
            LedgerEventKind::Split {
                symbol,
                ratio_num,
                ratio_den,
            } => {
                if *ratio_num < 1 || *ratio_den < 1 {
                    return Err(LedgerError::BadSplitRatio);
                }
                for l in st.lots.iter_mut() {
                    if l.closed || l.symbol != *symbol || l.remaining_qty == 0 {
                        continue;
                    }
                    let new_qty = round_half_to_even(
                        (l.remaining_qty as i128) * (*ratio_num as i128),
                        *ratio_den as i128,
                    );
                    if !within_cap(new_qty) {
                        return Err(LedgerError::AmountOutOfRange);
                    }
                    l.remaining_qty = new_qty as i64;
                    // A Split rescaling a lot makes the lot's opening event a
                    // dependency of this Split's symbol (backward check).
                    st.depended_on.insert(l.open_event.clone());
                }
                Ok(())
            }
            LedgerEventKind::Reversal { .. } => {
                // Reversals are filtered out before the fold; never applied.
                Ok(())
            }
        }
    }

    /// The Sell branch: select lots (specific-ID or FIFO), consume per-lot basis
    /// (sweep at closure), allocate net proceeds by largest-remainder, and emit
    /// one `RealizedGain` per consumed (lot, qty). Validation is the validator's
    /// job; here we re-derive selection deterministically and, for safety on an
    /// unexpectedly-invalid log, return the matching `LedgerError`.
    fn apply_sell(st: &mut FoldState, e: &LedgerEvent) -> Result<(), LedgerError> {
        let (sale_id, symbol, qty, unit_price_cents, fees_cents, lot_refs, accrues_to_state, platform) =
            match &e.kind {
                LedgerEventKind::Sell {
                    sale_id,
                    symbol,
                    qty,
                    unit_price_cents,
                    fees_cents,
                    lot_refs,
                    accrues_to_state,
                    platform,
                    ..
                } => (
                    sale_id,
                    symbol,
                    *qty,
                    *unit_price_cents,
                    *fees_cents,
                    lot_refs,
                    accrues_to_state,
                    platform,
                ),
                _ => unreachable!("apply_sell on a non-Sell event"),
            };

        if qty.0 <= 0 {
            return Err(LedgerError::NonPositiveQty);
        }

        // Deterministic selection plan: a list of (lot index, consumed qty),
        // already ordered for proceeds allocation tie-breaks.
        let plan = select_lots(st, symbol, qty, lot_refs, platform)?;

        // Gross/net proceeds (the single scale site; cap-checked).
        let gross = scale((qty.0 as i128) * (unit_price_cents.0 as i128));
        if !within_cap(gross) {
            return Err(LedgerError::AmountOutOfRange);
        }
        let net = gross - fees_cents.0 as i128;
        if !within_cap(net) {
            return Err(LedgerError::AmountOutOfRange);
        }

        // Largest-remainder allocation of `net` across the consumed lots, keyed
        // by consumed qty, ties broken by ascending (acquire_date, open_seq).
        let total = qty.0 as i128;
        let proceeds = allocate_proceeds(st, &plan, net, total);

        // Apply consumption and emit gains in plan order.
        for (k, (idx, c)) in plan.iter().enumerate() {
            let idx = *idx;
            let c = *c;
            let (cb, new_basis) = consume_basis(
                st.lots[idx].remaining_basis as i128,
                st.lots[idx].remaining_qty as i128,
                c as i128,
            );
            st.lots[idx].remaining_basis = new_basis as i64;
            st.lots[idx].remaining_qty -= c;
            if st.lots[idx].remaining_qty == 0 {
                st.lots[idx].closed = true;
            }
            // This Sell depends on the lot's opening event (backward check).
            st.depended_on.insert(st.lots[idx].open_event.clone());

            let alloc = proceeds[k];
            let gain = alloc - cb;
            let holding_days = e.date.0 - st.lots[idx].acquire_date.0;
            st.gains.push(RealizedGain {
                sale_id: sale_id.clone(),
                sale_seq: e.seq,
                lot_id: st.lots[idx].id.clone(),
                symbol: symbol.clone(),
                sale_date: e.date,
                proceeds_cents: Cents(alloc as i64),
                basis_cents: Cents(cb as i64),
                gain_cents: Cents(gain as i64),
                acquire_date: st.lots[idx].acquire_date,
                holding_days,
                accrues_to_state: accrues_to_state.clone(),
            });
        }
        Ok(())
    }

    /// Build the deterministic (lot index, consumed qty) selection plan for a
    /// Sell, applying specific-ID (LEDGER-LOT-004) or FIFO fallback
    /// (LEDGER-LOT-005). Returns the validator's `LedgerError` on any defect so
    /// `apply_sell` is total on a validated log. The plan is ordered for the
    /// proceeds tie-break: specific-ID preserves the lot_refs order conceptually,
    /// but allocation re-sorts by `(acquire_date, open_seq)`, so order here only
    /// fixes the per-lot consume sequence.
    fn select_lots(
        st: &FoldState,
        symbol: &str,
        qty: MicroShares,
        lot_refs: &[crate::LotRef],
        platform: &str,
    ) -> Result<Vec<(usize, i64)>, LedgerError> {
        if !lot_refs.is_empty() {
            // Specific-identification.
            let mut seen: BTreeSet<String> = BTreeSet::new();
            let mut sum: i128 = 0;
            let mut plan: Vec<(usize, i64)> = Vec::new();
            for lr in lot_refs {
                if !seen.insert(lr.lot_id.clone()) {
                    return Err(LedgerError::DuplicateLotRef);
                }
                let idx = match st.find_lot_idx(&lr.lot_id) {
                    Some(i) => i,
                    None => return Err(LedgerError::UnknownLot),
                };
                let lot = &st.lots[idx];
                if lot.symbol != symbol {
                    return Err(LedgerError::WrongSymbolLot);
                }
                if lot.platform != platform {
                    return Err(LedgerError::WrongPlatform);
                }
                if lr.qty.0 <= 0 {
                    return Err(LedgerError::NonPositiveQty);
                }
                if lot.closed || lr.qty.0 > lot.remaining_qty {
                    return Err(LedgerError::InsufficientShares);
                }
                sum += lr.qty.0 as i128;
                plan.push((idx, lr.qty.0));
            }
            if sum != qty.0 as i128 {
                return Err(LedgerError::LotRefsMismatch);
            }
            Ok(plan)
        } else {
            // FIFO fallback: ascending (acquire_date, open_seq) on the same
            // platform, same symbol, open lots only.
            let mut candidates: Vec<usize> = st
                .lots
                .iter()
                .enumerate()
                .filter(|(_, l)| {
                    !l.closed
                        && l.symbol == symbol
                        && l.platform == platform
                        && l.remaining_qty > 0
                })
                .map(|(i, _)| i)
                .collect();
            candidates.sort_by(|&a, &b| {
                let la = &st.lots[a];
                let lb = &st.lots[b];
                (la.acquire_date.0, la.open_seq.0).cmp(&(lb.acquire_date.0, lb.open_seq.0))
            });
            let mut need = qty.0 as i128;
            let mut plan: Vec<(usize, i64)> = Vec::new();
            for idx in candidates {
                if need <= 0 {
                    break;
                }
                let avail = st.lots[idx].remaining_qty as i128;
                let take = if avail >= need { need } else { avail };
                plan.push((idx, take as i64));
                need -= take;
            }
            if need > 0 {
                return Err(LedgerError::InsufficientShares);
            }
            Ok(plan)
        }
    }

    /// Largest-remainder allocation of `net` proceeds across the planned
    /// consumptions, in the SAME index order as `plan` (so the caller can pair
    /// `proceeds[k]` with `plan[k]`). Floor each lot's exact share, then hand
    /// the residual units (one per lot) out in descending fractional-remainder
    /// order, ties broken by ascending `(acquire_date, open_seq)`. Guarantees
    /// `Σ proceeds == net` exactly, for signed `net`. (LEDGER-PNL-002)
    fn allocate_proceeds(
        st: &FoldState,
        plan: &[(usize, i64)],
        net: i128,
        total: i128,
    ) -> Vec<i128> {
        let n = plan.len();
        let mut floors: Vec<i128> = Vec::with_capacity(n);
        let mut rems: Vec<i128> = Vec::with_capacity(n);
        let mut sum_floor: i128 = 0;
        for (_, c) in plan {
            let c = *c as i128;
            let f = alloc_floor(net, c, total);
            let r = alloc_remainder(net, c, total);
            sum_floor += f;
            floors.push(f);
            rems.push(r);
        }
        // Residual units to distribute (non-negative under the floor convention).
        let residual = (net - sum_floor) as i64;

        // Rank plan positions by (descending remainder, ascending acquire_date,
        // ascending open_seq) and award one extra unit to the top `residual`.
        let mut order: Vec<usize> = (0..n).collect();
        order.sort_by(|&i, &j| {
            // Descending remainder.
            rems[j]
                .cmp(&rems[i])
                .then_with(|| {
                    let li = &st.lots[plan[i].0];
                    let lj = &st.lots[plan[j].0];
                    (li.acquire_date.0, li.open_seq.0)
                        .cmp(&(lj.acquire_date.0, lj.open_seq.0))
                })
        });
        let mut proceeds = floors;
        // Award one extra unit to the top `residual` positions (descending
        // remainder, FIFO tie-break). `residual` is non-negative and < n.
        let to_award = if residual < 0 { 0 } else { residual as usize };
        for &pos in order.iter().take(to_award) {
            proceeds[pos] += 1;
        }
        proceeds
    }

    // -----------------------------------------------------------------------
    // Public (crate) entry points the API delegates to.
    // -----------------------------------------------------------------------

    /// `replay`: fold the surviving events in `Seq` order into a `Snapshot`.
    pub fn replay_impl(events: &[LedgerEvent], marks: &Marks) -> Snapshot {
        let survivors = surviving_sorted(events);
        let mut st = FoldState::new();
        for e in &survivors {
            // Over a valid (validated) log this is infallible; we ignore the
            // Result to keep replay total. (A defect would surface in tests.)
            let _ = apply_event(&mut st, e);
        }
        build_snapshot(&st, marks)
    }

    /// Materialize the `Snapshot` (positions, open lots with per-lot unrealized,
    /// realized gains) from the folded state. (LEDGER-PNL-006..010)
    fn build_snapshot(st: &FoldState, marks: &Marks) -> Snapshot {
        // Per-symbol aggregates.
        let mut total_qty: BTreeMap<String, i64> = BTreeMap::new();
        let mut total_basis: BTreeMap<String, i64> = BTreeMap::new();
        let mut realized: BTreeMap<String, i64> = BTreeMap::new();

        // Seed every seen symbol so a fully-consumed symbol still reports a
        // position; a never-held symbol (stray mark) never appears.
        for s in &st.symbols_seen {
            total_qty.entry(s.clone()).or_insert(0);
            total_basis.entry(s.clone()).or_insert(0);
            realized.entry(s.clone()).or_insert(0);
        }

        let mut open_lots: Vec<OpenLot> = Vec::new();
        for l in &st.lots {
            if l.closed || l.remaining_qty == 0 {
                continue;
            }
            *total_qty.entry(l.symbol.clone()).or_insert(0) += l.remaining_qty;
            *total_basis.entry(l.symbol.clone()).or_insert(0) += l.remaining_basis;

            // Per-lot unrealized: scale(mark × remaining_qty) − remaining_basis,
            // or None when the symbol is unmarked (degraded per-symbol).
            let per_lot_unreal = marks.get(&l.symbol).map(|m| {
                let val = scale((m.0 as i128) * (l.remaining_qty as i128));
                Cents((val - l.remaining_basis as i128) as i64)
            });

            open_lots.push(OpenLot {
                lot: Lot {
                    id: l.id.clone(),
                    symbol: l.symbol.clone(),
                    acquire_date: l.acquire_date,
                    open_seq: l.open_seq,
                    source: l.source,
                    remaining_qty: MicroShares(l.remaining_qty),
                    remaining_basis_cents: Cents(l.remaining_basis),
                    platform: l.platform.clone(),
                    tracking_code: l.tracking_code.clone(),
                },
                unrealized_cents: per_lot_unreal,
            });
        }

        for g in &st.gains {
            *realized.entry(g.symbol.clone()).or_insert(0) += g.gain_cents.0;
        }

        // Per-symbol unrealized = Σ per-lot unrealized over open lots, but only
        // when a mark is present AND the symbol has open shares; absent → None.
        let mut positions: BTreeMap<String, Position> = BTreeMap::new();
        let mut symbols: BTreeSet<String> = BTreeSet::new();
        for s in total_qty.keys() {
            symbols.insert(s.clone());
        }
        for s in realized.keys() {
            symbols.insert(s.clone());
        }
        for s in &symbols {
            let tq = *total_qty.get(s).unwrap_or(&0);
            let tb = *total_basis.get(s).unwrap_or(&0);
            let rp = *realized.get(s).unwrap_or(&0);

            let unreal = if tq > 0 {
                marks.get(s).map(|_| {
                    // Σ per-lot unrealized over this symbol's open lots.
                    let mut acc: i128 = 0;
                    for ol in &open_lots {
                        if ol.lot.symbol == *s {
                            if let Some(u) = ol.unrealized_cents {
                                acc += u.0 as i128;
                            }
                        }
                    }
                    Cents(acc as i64)
                })
            } else {
                // No open shares: a mark is ignored (not zero, not an error).
                None
            };

            positions.insert(
                s.clone(),
                Position {
                    symbol: s.clone(),
                    total_qty: MicroShares(tq),
                    total_basis_cents: Cents(tb),
                    realized_pnl_cents: Cents(rp),
                    unrealized_cents: unreal,
                },
            );
        }

        Snapshot {
            positions,
            open_lots,
            realized_gains: st.gains.clone(),
        }
    }

    /// `validate`: check `candidate` against the surviving replayed-so-far
    /// state (reversed targets filtered). (LEDGER-ERR-001..011)
    pub fn validate_impl(
        accepted: &[LedgerEvent],
        candidate: &LedgerEvent,
    ) -> Result<(), LedgerError> {
        // Replay the surviving accepted prefix to get current state + the
        // dependency map (which opening events surviving Sells/Splits touched).
        let survivors = surviving_sorted(accepted);
        let mut st = FoldState::new();
        for e in &survivors {
            // The accepted prefix is valid by construction; ignore the Result.
            let _ = apply_event(&mut st, e);
        }

        // Set of accepted event ids (for Reversal target existence) and the set
        // of currently-reversed targets (for already-reversed detection).
        let accepted_ids: BTreeSet<&str> = accepted.iter().map(|e| e.id.as_str()).collect();
        let mut reversed_targets: BTreeSet<&str> = BTreeSet::new();
        for e in accepted {
            if let LedgerEventKind::Reversal { target_event_id } = &e.kind {
                reversed_targets.insert(target_event_id.as_str());
            }
        }

        match &candidate.kind {
            LedgerEventKind::Buy { lot_id, qty, unit_price_cents, fees_cents, .. } => {
                if qty.0 <= 0 {
                    return Err(LedgerError::NonPositiveQty);
                }
                if st.lots.iter().any(|l| l.id == *lot_id) {
                    return Err(LedgerError::DuplicateLotId);
                }
                let scaled = scale((qty.0 as i128) * (unit_price_cents.0 as i128));
                if !within_cap(scaled) {
                    return Err(LedgerError::AmountOutOfRange);
                }
                if !within_cap(scaled + fees_cents.0 as i128) {
                    return Err(LedgerError::AmountOutOfRange);
                }
                Ok(())
            }
            LedgerEventKind::Vest { lot_id, qty, fmv_per_share_cents, .. } => {
                if qty.0 <= 0 {
                    return Err(LedgerError::NonPositiveQty);
                }
                if st.lots.iter().any(|l| l.id == *lot_id) {
                    return Err(LedgerError::DuplicateLotId);
                }
                let scaled = scale((qty.0 as i128) * (fmv_per_share_cents.0 as i128));
                if !within_cap(scaled) {
                    return Err(LedgerError::AmountOutOfRange);
                }
                Ok(())
            }
            LedgerEventKind::Sell {
                symbol,
                qty,
                unit_price_cents,
                fees_cents,
                lot_refs,
                platform,
                ..
            } => {
                if qty.0 <= 0 {
                    return Err(LedgerError::NonPositiveQty);
                }
                // Selection validity (UnknownLot / WrongSymbolLot / DuplicateLotRef
                // / WrongPlatform / LotRefsMismatch / InsufficientShares).
                select_lots(&st, symbol, *qty, lot_refs, platform)?;
                // Proceeds range check (the scaled gross/net).
                let gross = scale((qty.0 as i128) * (unit_price_cents.0 as i128));
                if !within_cap(gross) {
                    return Err(LedgerError::AmountOutOfRange);
                }
                if !within_cap(gross - fees_cents.0 as i128) {
                    return Err(LedgerError::AmountOutOfRange);
                }
                Ok(())
            }
            LedgerEventKind::Split { symbol, ratio_num, ratio_den } => {
                if *ratio_num < 1 || *ratio_den < 1 {
                    return Err(LedgerError::BadSplitRatio);
                }
                // Range-check each rescaled quantity.
                for l in &st.lots {
                    if l.closed || l.symbol != *symbol || l.remaining_qty == 0 {
                        continue;
                    }
                    let new_qty = round_half_to_even(
                        (l.remaining_qty as i128) * (*ratio_num as i128),
                        *ratio_den as i128,
                    );
                    if !within_cap(new_qty) {
                        return Err(LedgerError::AmountOutOfRange);
                    }
                }
                Ok(())
            }
            LedgerEventKind::Reversal { target_event_id } => {
                // Target must exist among accepted events.
                if !accepted_ids.contains(target_event_id.as_str()) {
                    return Err(LedgerError::BadReversal);
                }
                // Target must not already be reversed.
                if reversed_targets.contains(target_event_id.as_str()) {
                    return Err(LedgerError::BadReversal);
                }
                // No surviving event may depend on the target's effect: a Sell
                // that consumed, or a Split that rescaled, a lot the target
                // opened. `depended_on` holds every opening EventId a surviving
                // Sell/Split touched.
                if st.depended_on.contains(target_event_id.as_str()) {
                    return Err(LedgerError::BadReversal);
                }
                Ok(())
            }
        }
    }
}

// ===========================================================================
// Kani bounded-model-checking harnesses for the LEDGER-VERIF-* properties.
// Entirely behind `#[cfg(kani)]` so a normal `cargo build`/`cargo test` is
// unaffected and Kani is NOT a hard dependency. Run with `cargo kani` when the
// Kani toolchain is installed. Mirrors the prior project's kani harnesses.
// ===========================================================================
#[cfg(kani)]
mod kani_proofs;

//! Kani bounded-model-checking harnesses for the ledger-core **arithmetic
//! kernel** (the `verus!{}` fns in `mod core`). Kani *executes* the real code,
//! including the `i128::div_euclid`/`rem_euclid` that Verus only proves via
//! `assume_specification`, so these are an independent executable cross-check of
//! exactly the slice Verus trusts.
//!
//! TRACTABILITY: Kani bit-blasts `i128`, so a *symbolic divisor* (symbolic
//! 128-bit division) blows CBMC up (an earlier symbolic-`den` harness ran past
//! 5 minutes). Every `div`/`rem` divisor here is therefore **concrete**, every
//! multiply has a concrete operand, and symbolic numerators are tightly bounded
//! — each harness then solves in milliseconds. Verus carries the *unbounded*
//! proofs of these same identities; Kani's role is the executable spot-check on
//! representative concrete divisors (incl. ties and signed numerators).
//!
//! SCOPE: the **fold-level** invariants (LEDGER-VERIF-001 full conservation
//! across a log, -002 share accounting, -004 no-partial-mutation, -005 replay
//! determinism, -008 reversal totality) are NOT model-checked here — they fold
//! `BTreeMap`/`String`/`Vec`, which CBMC cannot handle tractably. They stay
//! covered by the bounded property `#[test]`s in `tests/verif.rs` (green under
//! `cargo test`) plus the Verus per-function `ensures`.

#![cfg(kani)]

use crate::core::{
    alloc_floor, alloc_remainder, consume_basis, round_half_to_even, scale, within_cap,
};

// @spec LEDGER-VERIF-001 — the rounding rule is the nearest integer (ties to
// even): |r*den - num| * 2 <= den. Concrete divisors exercise the even/odd tie
// branches; tiny symbolic numerator keeps it instant.
fn check_round(num: i128, den: i128) {
    let r = round_half_to_even(num, den);
    let diff = r * den - num;
    let ad = if diff < 0 { -diff } else { diff };
    assert!(2 * ad <= den);
}
#[kani::proof]
fn verif_round_half_to_even_nearest() {
    let num: i128 = kani::any();
    kani::assume(0 <= num && num <= 64);
    check_round(num, 2);
    check_round(num, 3);
    check_round(num, 4);
    check_round(num, 7);
}

// @spec LEDGER-VERIF-001, LEDGER-VERIF-003 — basis conservation + bounds + the
// residual sweep at closure. Concrete rq (divisor) and concrete c (so rb*c has a
// concrete operand); rb symbolic.
fn check_consume(rb: i128, rq: i128, c: i128) {
    let (consumed, remaining) = consume_basis(rb, rq, c);
    assert!(consumed + remaining == rb); // conservation
    assert!(0 <= consumed && consumed <= rb); // bounded
    if c == rq {
        assert!(remaining == 0); // residual sweep at closure
    }
}
#[kani::proof]
fn verif_consume_basis_conserves() {
    let rb: i128 = kani::any();
    kani::assume(0 <= rb && rb <= 100_000);
    check_consume(rb, 5, 2); // partial consume
    check_consume(rb, 5, 5); // closure sweep
    check_consume(rb, 7, 3); // a second divisor
}

// @spec LEDGER-VERIF-006 — largest-remainder decomposition is exact:
// net*c == floor*total + rem, 0 <= rem < total (the Σ proceeds == net guarantee,
// and the signed-numerator path that exercises the trusted div/rem). Concrete c
// (so net*c has a concrete operand) and concrete total (divisor); net symbolic,
// including negatives.
fn check_alloc(net: i128, c: i128, total: i128) {
    let f = alloc_floor(net, c, total);
    let rem = alloc_remainder(net, c, total);
    assert!(f * total + rem == net * c); // exact decomposition
    assert!(0 <= rem && rem < total); // euclidean remainder
}
#[kani::proof]
fn verif_alloc_sum_exact() {
    let net: i128 = kani::any();
    kani::assume(-50_000 <= net && net <= 50_000);
    check_alloc(net, 2, 3);
    check_alloc(net, 4, 7);
}

// @spec LEDGER-VERIF-001 — scale is round_half_to_even over the CONSTANT
// SHARE_SCALE (division by a constant ⇒ tractable even with a wide numerator).
#[kani::proof]
fn verif_scale_matches_round() {
    let micro: i128 = kani::any();
    kani::assume(0 <= micro && micro <= 5_000_000);
    assert!(scale(micro) == round_half_to_even(micro, pt_core::SHARE_SCALE as i128));
}

// @spec LEDGER-VERIF-001 — within_cap is exactly the two-sided MONEY_CAP bound
// (comparisons only ⇒ trivial for CBMC even over a wide symbolic range).
#[kani::proof]
fn verif_within_cap_identity() {
    let v: i128 = kani::any();
    kani::assume(-(1i128 << 50) <= v && v <= (1i128 << 50));
    let cap = pt_core::MONEY_CAP as i128;
    assert!(within_cap(v) == (-cap <= v && v <= cap));
}

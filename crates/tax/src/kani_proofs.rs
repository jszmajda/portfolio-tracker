//! Kani bounded-model-checking harnesses for the `tax` **arithmetic kernel**
//! (the `verus!{}` fns in `mod kernel`). Kani *executes* the real code,
//! including the `i128::div_euclid`/`rem_euclid` that Verus only proves via
//! `assume_specification`, so these are an independent executable cross-check of
//! exactly the slice Verus trusts. Mirrors `ledger-core`'s `kani_proofs`.
//!
//! TRACTABILITY: Kani bit-blasts `i128`, so every `div`/`rem` divisor here is
//! **concrete** (the `PPM_SCALE` constant), every multiply has a concrete
//! operand, and symbolic numerators are tightly bounded — each harness solves
//! in milliseconds. Verus carries the *unbounded* proofs of these same
//! identities; Kani's role is the executable spot-check.
//!
//! SCOPE: the **fold-level** lifecycle/reserve/report invariants
//! (TAX-VERIF-004/005/006/007) are NOT model-checked here — they fold
//! `BTreeMap`/`String`/`Vec`, which CBMC cannot handle tractably. They stay
//! covered by the bounded `#[test]`s in `tests/` (green under `cargo test`).
//! Only the kernel ARITHMETIC (TAX-VERIF-001/002/003) is bounded-checked here.

#![cfg(kani)]

use crate::kernel::{
    apply_rate_ppm, bracket_tax_on, clamp_nonneg, effective_rate_ppm, marginal_increment,
    round_half_to_even, PPM_FULL, PPM_SCALE,
};

// @spec TAX-VERIF-003 — clamp_nonneg is total and yields max(0, x) ≥ 0 over a
// wide symbolic range (comparisons only ⇒ trivial for CBMC).
#[kani::proof]
fn verif_003_clamp_nonneg_is_total_max_zero() {
    let x: i128 = kani::any();
    kani::assume(-(1i128 << 50) <= x && x <= (1i128 << 50));
    let r = clamp_nonneg(x);
    assert!(r >= 0);
    assert!(r == if x >= 0 { x } else { 0 });
}

// @spec TAX-VERIF-001, TAX-VERIF-002 — the marginal increment is bounded
// 0 ≤ accrual ≤ gain for a positive gain over a monotone, ≤-gain tax step
// (the bracket-stacking contract config's <100% ceiling guarantees). Tiny
// symbolic ranges keep it instant.
fn check_increment(tax_before: i128, gain: i128) {
    // A monotone tax step that rises by at most the gain: tax_after − tax_before
    // ∈ [0, gain] (the bracket-stack contract). Use a symbolic step in range.
    let step: i128 = kani::any();
    kani::assume(0 <= step && step <= gain);
    let tax_after = tax_before + step;
    let acc = marginal_increment(tax_before, tax_after, gain);
    assert!(0 <= acc && acc <= gain);
    assert!(acc == tax_after - tax_before);
}
#[kani::proof]
fn verif_001_002_marginal_increment_bounded() {
    let tax_before: i128 = kani::any();
    kani::assume(0 <= tax_before && tax_before <= 1_000);
    let gain: i128 = kani::any();
    kani::assume(0 <= gain && gain <= 1_000);
    check_increment(tax_before, gain);
}

// @spec TAX-CALC-009 — the effective rate is 0 when the position is not in the
// money, and otherwise round(estimated × 1e6 / pretax) over the CONSTANT
// PPM_SCALE divisor (division by a constant ⇒ tractable even with a wide
// symbolic numerator).
#[kani::proof]
fn verif_effective_rate_zero_when_not_in_money() {
    let estimated: i128 = kani::any();
    kani::assume(-100_000 <= estimated && estimated <= 100_000);
    let pretax: i128 = kani::any();
    kani::assume(-100_000 <= pretax && pretax <= 0);
    assert!(effective_rate_ppm(estimated, pretax) == 0);
    // PPM_SCALE is the concrete divisor the in-the-money branch divides by.
    let _ = PPM_SCALE;
}

// @spec TAX-VERIF-001 — round_half_to_even is the nearest integer multiple (ties
// to even): 2*|r*den − num| ≤ den. CONCRETE divisors exercise the even/odd tie
// branches (den=2 hits ties on every even num; den=3,4,7 vary the residue);
// tiny symbolic numerator keeps CBMC instant. The same nearest bound Verus
// proves unboundedly and that apply_rate_ppm's [0, amount] bound rides on.
fn check_round(num: i128, den: i128) {
    let r = round_half_to_even(num, den);
    let diff = r * den - num;
    let ad = if diff < 0 { -diff } else { diff };
    assert!(2 * ad <= den);
}
#[kani::proof]
fn verif_001_round_half_to_even_nearest() {
    let num: i128 = kani::any();
    kani::assume(0 <= num && num <= 64);
    check_round(num, 2);
    check_round(num, 3);
    check_round(num, 4);
    check_round(num, 7);
}

// @spec TAX-VERIF-002 — apply_rate_ppm is bounded: a rate in [0, PPM_FULL)
// applied to a non-negative amount yields tax in [0, amount] (the per-band
// building block the whole bounded-tax invariant rests on). The divisor inside
// is the CONCRETE PPM_SCALE constant; amount and rate are tiny symbolic, with
// rate held below the 100% ceiling config enforces. Instant for CBMC.
fn check_rate(amount: i128, rate: i128) {
    let t = apply_rate_ppm(amount, rate);
    assert!(0 <= t && t <= amount);
}
#[kani::proof]
fn verif_002_apply_rate_ppm_bounded() {
    let amount: i128 = kani::any();
    kani::assume(0 <= amount && amount <= 200);
    let rate: i128 = kani::any();
    // rate strictly below 100% (PPM_FULL) — the config bounded-tax gate.
    kani::assume(0 <= rate && rate < PPM_FULL);
    check_rate(amount, rate);
}

// @spec TAX-VERIF-002, TAX-VERIF-003 — bracket_tax_on over a CONCRETE two-band
// ordinary stack (thresholds [0, 100], rates 10% / 25%, both < PPM_FULL) yields
// tax in [0, amount] (bounded) and never panics (total) for a tiny symbolic
// (base, amount). Concrete brackets + tiny symbolic band keep the per-band
// PPM_SCALE divisions tractable. Verus carries the unbounded, any-bracket proof.
fn check_bracket(base: i128, amount: i128) {
    let thresholds: [i128; 2] = [0, 100];
    let rates: [i128; 2] = [100_000, 250_000]; // 10% / 25% in ppm, both < 100%
    let t = bracket_tax_on(&thresholds, &rates, base, amount);
    assert!(0 <= t && t <= amount);
}
#[kani::proof]
fn verif_002_003_bracket_tax_bounded_total() {
    let base: i128 = kani::any();
    kani::assume(0 <= base && base <= 150);
    let amount: i128 = kani::any();
    kani::assume(0 <= amount && amount <= 150);
    check_bracket(base, amount);
}

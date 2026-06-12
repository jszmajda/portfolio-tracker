# Verifying the money math: Verus and Kani in portfolio-tracker

This project's guiding tenet is **"verify the money math; trust the I/O"**
([HLD → Tenets](../high-level-design.md)). A personal tax tracker computes numbers its owner
acts on — what a position is worth post-tax, how much to set aside for an estimated payment,
what a quarter owes. A silent rounding drift or an integer overflow there is not a cosmetic
bug; it is real dollars and a wrong filing. So the small, pure accounting/tax kernel is
**proven correct**, and everything around it (Google Sheets, serialization, the TUI) stays
ordinary, well-tested Rust behind a single trust seam.

This note explains the two tools we use to do that — **Verus** and **Kani** — what problems
they solve here, a guided tour of the actual proofs, and an honest account of what they cost
and what they do *not* buy.

> New to the methodology this project is built on? Start with the README's
> [LID in practice](../../README.md#why-this-repo-is-interesting--lid-in-practice) section
> and the [build-process note](build-process.md). This doc is the deep dive on the one slice
> of the arrow that is *machine-checked* rather than merely tested.

---

## 1. What Verus and Kani are

Both let you state a property a Rust function must satisfy and have a machine confirm it —
but they confirm it in two fundamentally different ways, and the difference is why we use
**both**.

### Verus — a deductive verifier ([verus-lang/verus](https://github.com/verus-lang/verus) · [guide](https://verus-lang.github.io/verus/guide/))

Verus is an SMT-backed **deductive** verifier for Rust. You annotate a function with a
contract — `requires` (preconditions), `ensures` (postconditions), loop `invariant`s — and
Verus discharges it by handing proof obligations to the Z3 solver. When it succeeds, the
property holds for **every input**, unboundedly — there is no "we checked up to N." The catch
is that anything Verus can't see into (a `std` intrinsic, a heap-collection fold) must be
either *trusted* via an explicit `assume_specification` or carried as an `external_body`
function whose signature contract Verus believes without proving the body.

The proof lives in a `verus! { … }` macro block. Under the pinned Verus toolchain that block
is verified; under plain stable `cargo build` the macro **erases** and the exact same source
compiles as ordinary Rust. One source, two readings.

### Kani — a bounded model checker ([model-checking/kani](https://github.com/model-checking/kani) · [book](https://model-checking.github.io/kani/))

Kani is a **bounded model checker** for Rust, built on CBMC. You write a `#[kani::proof]`
harness, draw symbolic inputs with `kani::any()`, constrain them with `kani::assume(…)`, and
Kani **bit-blasts the real compiled code** and exhaustively checks every execution within
those bounds. Where Verus *reasons about* the code, Kani *executes* it symbolically —
including the very `std` intrinsics Verus only trusts. The catch is the bound: Kani explores a
finite envelope, not all inputs, and it is sensitive to problem size (a symbolic 128-bit
divisor makes CBMC blow up).

### Why both

They cover each other's blind spots:

| | Verus | Kani |
|---|---|---|
| Guarantee | **Unbounded** — all inputs | **Bounded** — a finite input envelope |
| Mechanism | Deductive (SMT/Z3); reasons about code | Model checking (CBMC); executes real code |
| Blind spot | Trusts `assume_specification` / `external_body` bodies | Only sees inside the bound; large `i128` divisors intractable |
| Role here | Proves the kernel's contracts for all inputs | Independently *executes* the slice Verus trusts, on concrete divisors |

Verus proves `round_half_to_even` is the nearest-integer rounding for all inputs but *trusts*
that `i128::div_euclid` matches its spec. Kani then runs the actual `div_euclid` on concrete
divisors and checks the same identity. Neither alone is the whole story; together the trusted
surface is small and independently cross-checked.

---

## 2. The problem we're actually solving

The kernel does tax-lot accounting and bracket-stacked tax. The failure modes that verification
targets are exactly the ones that are easy to get subtly wrong and impossible to *exhaustively*
cover with example tests:

- **Overflow.** `quantity × price` at 1e-6 share granularity and cents precision must never
  wrap. We work in `i128` with an explicit `MONEY_CAP` totality bound; an out-of-range scaled
  result is a *rejection*, never a silent wrap.
- **Rounding that conserves.** Halves must round one defined way (banker's / half-to-even),
  applied at exactly **one site** — and the sum of the per-lot allocated proceeds must equal
  the sale's net to the penny (no money invented or lost to rounding).
- **Basis conservation.** Consuming `c` shares from a lot must satisfy
  `consumed + remaining == original_basis` — and the last share out must sweep the whole
  residual basis (zero rounding loss at lot closure).
- **Non-negativity & bounds.** Quantities and basis never go negative; tax on a gain is in
  `[0, gain]` — you cannot be taxed more than 100% of what you made.
- **Monotonicity.** A non-negative gain can never *lower* cumulative tax (the property the
  marginal-accrual calculation rests on).

These are *value-level* invariants over arithmetic — the sweet spot for formal tools, and the
worst spot for "I wrote a few unit tests."

---

## 3. Where the boundary is

Only the pure arithmetic kernel is verified. The boundary is deliberate and one-directional:

```
            VERIFIED (Verus proofs + Kani cross-check)              TRUSTED (tested Rust)
   ┌───────────────────────────────────────────────────┐   ┌──────────────────────────────┐
   │  pt-core            ledger-core::core   tax::kernel │   │  store (serde · Sheets I/O)  │
   │  ─────────          ────────────────    ─────────── │   │  sheets-view · reports       │
   │  Cents, MicroShares round_half_to_even  bracket_    │   │  summary · runtime · tui     │
   │  SHARE_SCALE        consume_basis       tax_on      │   │                              │
   │  MONEY_CAP          alloc_floor/_rem    apply_rate  │   │  ← drift-guard test holds    │
   │  round_half_to_even within_cap          clamp/marg. │   │    the seam (event ↔ row     │
   │                                                     │   │    round-trip, exhaustive    │
   │      ↑ erases under stable `cargo build` ↑          │   │    match over every Kind)    │
   └───────────────────────────────────────────────────┘   └──────────────────────────────┘
                              │                                            │
                              └──────────── one trust seam ────────────────┘
                          Sheets row ↔ event crosses here, and ONLY here
```

The kernel is pure: no clock, no randomness, no I/O, no floating point. Serde and every byte
of I/O live *outside* it, guarded structurally rather than by proof — the
[`store` drift guard](../../crates/store/tests/guard.rs) round-trips every event kind through
its Sheets-row encoding and back (`event → row → event`) under a wildcard-free exhaustive
match, so adding a field without teaching the serializer about it fails a test rather than
shipping a silent corruption. The reasoning: the money math is the whole point and is amenable
to proof; Sheets and the TUI are neither amenable nor worth it
([HLD → Key Design Decisions](../high-level-design.md)).

---

## 4. A guided tour of the proofs

Five stops, from the single rounding site out to the CI gate. Each links to the real source.

### Stop 1 — one rounding rule, stated once

All monetary rounding in the system funnels through a single half-to-even function. The plain
version in [`pt-core`](../../crates/pt-core/src/lib.rs#L61-L76) is what stable builds compile:

```rust
pub fn round_half_to_even(numerator: i128, denominator: i128) -> i128 {
    let q = numerator.div_euclid(denominator);
    let r = numerator.rem_euclid(denominator); // 0 <= r < denominator
    let twice = 2 * r;
    if twice < denominator { q }
    else if twice > denominator { q + 1 }
    else if q % 2 == 0 { q }      // exactly halfway → round to the even quotient
    else { q + 1 }
}
```

Inside the kernel, that same function carries a **proof contract**. First the *spec* —
the mathematical meaning, in `int` (Verus's unbounded integer), in
[`ledger-core::core`](../../crates/ledger-core/src/lib.rs#L359-L373):

```rust
pub open spec fn round_half_to_even_spec(num: int, den: int) -> int {
    let f = num / den;
    let r = num % den;
    if 2 * r < den { f } else if 2 * r > den { f + 1 }
    else if f % 2 == 0 { f } else { f + 1 }
}
```

Then the executable function, proven to *refine* that spec and to be the genuine
nearest-integer multiple ([lib.rs#L429-L472](../../crates/ledger-core/src/lib.rs#L429-L472)):

```rust
pub fn round_half_to_even(num: i128, den: i128) -> (r: i128)
    requires den > 0, den <= OPERAND_CAP, -PROD_CAP <= num <= PROD_CAP,
    ensures
        r as int == round_half_to_even_spec(num as int, den as int),
        2 * (r as int * den as int - num as int) <= den as int,   // nearest-integer bound
        2 * (num as int - r as int * den as int) <= den as int,
{ … }
```

That nearest-integer bound — `2·|r·den − num| ≤ den` — is the lemma every downstream money
bound rides on. It is proven, not asserted, from the fundamental div-mod identity in
[`lemma_round_half_to_even_nearest`](../../crates/ledger-core/src/lib.rs#L379-L395).

### Stop 2 — the trusted surface, made tiny and explicit

Verus can't see the body of `i128::div_euclid`. Rather than hand-wave, the kernel states
*exactly* what it trusts and nothing more — the euclidean identity for a **positive divisor**,
the single `assume_specification` in the whole kernel
([lib.rs#L346-L352](../../crates/ledger-core/src/lib.rs#L346-L352)):

```rust
pub assume_specification[<i128>::div_euclid](num: i128, den: i128) -> (q: i128)
    requires den > 0,
    ensures q as int == (num as int) / (den as int);
pub assume_specification[<i128>::rem_euclid](num: i128, den: i128) -> (r: i128)
    requires den > 0,
    ensures r as int == (num as int) % (den as int);
```

Every call site passes a strictly positive divisor, so the precondition always holds. This is
the *entire* arithmetic trust surface — and Stop 4 below is Kani executing the real
`div_euclid`/`rem_euclid` to cross-check it.

### Stop 3 — conservation, proven

`consume_basis` takes `c` shares out of a lot and must conserve basis exactly. Its `ensures`
are the meaningful contract ([lib.rs#L516-L537](../../crates/ledger-core/src/lib.rs#L516-L537)):

```rust
pub fn consume_basis(remaining_basis: i128, remaining_qty: i128, c: i128) -> (out: (i128, i128))
    requires remaining_qty > 0, c >= 0, c <= remaining_qty, /* …bounds… */
    ensures
        out.0 as int + out.1 as int == remaining_basis as int,                 // conservation
        remaining_basis as int >= 0 ==> 0 <= out.0 as int <= remaining_basis as int,
        c == remaining_qty ==> (out.0 as int == remaining_basis as int && out.1 as int == 0), // closure sweep
        c < remaining_qty  ==> out.0 as int ==                                  // else: rounded share
            round_half_to_even_spec(remaining_basis as int * c as int, remaining_qty as int),
{ … }
```

Conservation, bounded consumption, and the zero-residual closure sweep are facts the verifier
checks for all inputs — not assertions a test happened to exercise.

### Stop 4 — bounded tax via a loop invariant

The crux of "you cannot be taxed more than you gained" is
[`bracket_tax_on`](../../crates/tax/src/lib.rs#L1914-L2005) in `tax::kernel`. It stacks a gain
through ordinary brackets and proves `0 ≤ tax ≤ amount` *without* assuming the bracket table is
even sorted, by consuming an explicit untaxed `remaining` budget under a loop invariant:

```rust
while i < n
    invariant
        0 <= remaining <= amount,
        0 <= total,
        total <= amount - remaining,   // ← the slack that yields total <= amount at exit
        forall|k: int| 0 <= k < rates_ppm.len() ==> 0 <= #[trigger] rates_ppm[k] < PPM_FULL,
    decreases n - i,
{ … }
```

The per-band building block is [`apply_rate_ppm`](../../crates/tax/src/lib.rs#L1862-L1885),
proven to yield tax in `[0, amount]` for any rate below 100% (`PPM_FULL`). The
config layer is what guarantees that `< 100%` precondition — verification of the kernel and
validation of the data meet at that seam.

### Stop 5 — Kani executes the trusted slice

For each verified crate, [`src/kani_proofs.rs`](../../crates/ledger-core/src/kani_proofs.rs)
holds bounded harnesses that **run** the kernel arithmetic — including the real `div_euclid`
Verus only trusts. Divisors are kept **concrete** (a symbolic 128-bit divisor makes CBMC
intractable), numerators tightly bounded, so each harness solves in milliseconds:

```rust
// @spec LEDGER-VERIF-001, LEDGER-VERIF-003 — basis conservation + bounds + closure sweep
fn check_consume(rb: i128, rq: i128, c: i128) {
    let (consumed, remaining) = consume_basis(rb, rq, c);
    assert!(consumed + remaining == rb);          // conservation
    assert!(0 <= consumed && consumed <= rb);     // bounded
    if c == rq { assert!(remaining == 0); }       // residual sweep at closure
}
```

These harnesses double as the **drift guard** for the kernel's mirrored constants: Verus can't
import a `const` from a non-Verus crate, so `SHARE_SCALE`/`MONEY_CAP` are re-declared inside
`verus!{}` — and the Kani harnesses assert the mirrored kernel against `pt_core`'s canonical
values ([`verif_scale_matches_round`](../../crates/ledger-core/src/kani_proofs.rs#L88-L93),
[`verif_within_cap_identity`](../../crates/ledger-core/src/kani_proofs.rs#L97-L103)), so a
mirror that drifted from `pt-core` fails the model check.

### The gate that ties it together

[`scripts/ci.sh`](../../scripts/ci.sh#L86-L108) runs both tools per verified crate, each under
a 5-minute `gtimeout` (a verification *hang* is treated as a failure, not a flake):

```sh
VERIFIED_CRATES=(ledger-core tax)
# Verus deductive proof (verus!{} cores)   → crates/$crate/verus/verify.sh
# Kani bounded model checking              → cargo kani -p $crate
```

When the toolchains are absent the stages **skip** (so a fresh clone's CI is green offline);
set `PT_CI_REQUIRE_VERUS=1` / `PT_CI_REQUIRE_KANI=1` once provisioned to make absence a hard
failure.

---

## 5. Tradeoffs, honestly

Formal verification here is a deliberate, *bounded* investment. What it costs and what it does
not buy:

- **Verus trusts what it can't see.** Two surfaces: the `assume_specification` for
  `div_euclid`/`rem_euclid` (narrow, and cross-checked by Kani), and the `external_body` fold
  functions — the orchestration over the event log uses `BTreeMap`/`String`/`Vec`, so Verus
  verifies their *signature contracts* and trusts the bodies. The arithmetic is proven; the
  bookkeeping that calls it is trusted Rust.
- **Kani only checks the arithmetic kernel, within bounds.** Because CBMC bit-blasts `i128`,
  divisors must be concrete and numerators bounded. So Kani's coverage is the kernel functions
  on *representative* concrete divisors and tie cases — strong evidence, not a universal proof
  (that's Verus's job).
- **The fold-level invariants are *not* machine-checked.** Conservation across a whole log,
  replay determinism, reversal totality, share accounting across a split — CBMC can't fold heap
  collections tractably, so these (`LEDGER-VERIF-002/003/005/007/008`) are covered by **bounded
  property `#[test]`s** in [`tests/verif.rs`](../../crates/ledger-core/tests/verif.rs) plus
  Verus's per-function `ensures`. The `#[cfg(kani)]` fold harnesses in that file are explicit
  `unimplemented!()` **stubs** — placeholders that keep each invariant visible to the
  spec-coverage gate, not passing proofs. We chose not to pretend otherwise.
- **Toolchain weight is real.** A pinned Verus release and a matching `vstd` pin (bump them
  together), a `.gitignored` `tools/` provisioned per machine, the `gtimeout` guard, and a
  formatting rule: `rustfmt` *cannot parse* Verus syntax, so the fmt gate is safe only because
  it leaves `verus!{}` macro bodies byte-untouched. These are running costs.
- **Proof maintenance is a tax on changes.** Touching kernel arithmetic can mean updating
  nonlinear-arith lemmas and bound annotations, not just the code. This is affordable *only*
  because the verified surface is small and stable — which is exactly why verifying the Sheets
  layer or the TUI was considered and **rejected**.
- **Verification is downstream of intent.** A proof confirms the code matches *its spec*. It
  cannot tell you the spec is the right one, that your bracket tables are correct data, or that
  the I/O is faithful — those are the EARS, the config, and the trust-seam guard's jobs. In
  [LID](../../README.md#why-this-repo-is-interesting--lid-in-practice) terms, verification sits
  at the very tip of the arrow; it makes the last link rigid, not the whole chain correct.

The honest summary: verification here buys **unbreakable value-level guarantees on a small,
high-stakes, pure core**, cross-checked two ways, at the price of a pinned toolchain and a
proof-maintenance burden we accept *because* the core is small. It is not a claim that the
application is proven correct — it is a claim that the money math is.

---

## Further reading

- [`docs/high-level-design.md`](../high-level-design.md) — the tenet, the verified-kernel
  decision, and the success metric "Verus/Kani gates pass in CI."
- [`docs/intent/ledger-core/ledger-core-design.md`](../intent/ledger-core/ledger-core-design.md)
  — the LLD for the verified accounting kernel (money & quantity types, the scale rule, the
  `LEDGER-VERIF-*` invariants).
- [`docs/intent/tax/tax-design.md`](../intent/tax/tax-design.md) — the LLD for verified tax
  calculation and the accrual lifecycle.
- [`docs/notes/build-process.md`](build-process.md) — how the verified-kernel template was
  established first, and the Verus/Kani lessons (mirror consts, concrete divisors, the timeout
  rule) that became reusable rules.
- Upstream: the [Verus guide](https://verus-lang.github.io/verus/guide/) and the
  [Kani book](https://model-checking.github.io/kani/).
</content>
</invoke>

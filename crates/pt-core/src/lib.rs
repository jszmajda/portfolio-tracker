//! `pt-core` — the shared money & quantity primitives for portfolio-tracker.
//!
//! Exact-integer newtypes, the single `SHARE_SCALE` rounding site
//! (`round_half_to_even`), and the `MONEY_CAP` totality bound, as specified in
//! `docs/intent/ledger-core/ledger-core-design.md` ("Money & Quantity Types").
//!
//! TDD scaffold: the rounding/scale helpers are stubbed with `todo!()` so tests
//! that target them fail RED rather than fail-to-compile. The newtypes and
//! constants are real (they carry no logic to defer).
//!
//! No serde, no I/O: floating point and serialization stay outside this and the
//! verified boundary (HLD "Verify the money math; trust the I/O").

// ---------------------------------------------------------------------------
// Money & quantity newtypes (ledger-core-design.md → "Money & Quantity Types").
// ---------------------------------------------------------------------------

/// Exact money, hundredths of a dollar. Prices are cents **per whole share**.
/// Negative allowed (losses, reversals).
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Debug, Default)]
pub struct Cents(pub i64);

/// Share quantity at 1e-6 granularity — supports fractional shares and split
/// ratios exactly.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Debug, Default)]
pub struct MicroShares(pub i64);

/// Days since the Unix epoch. Acquisition/sale dates and holding-period math;
/// leap years are exact because the unit is days.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Debug, Default)]
pub struct Date(pub i32);

/// Monotonic per-log sequence number; the **total order** events fold in.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Debug, Default)]
pub struct Seq(pub u64);

// ---------------------------------------------------------------------------
// Scale & overflow constants (ledger-core-design.md → "The scale rule" /
// "Overflow contract").
// ---------------------------------------------------------------------------

/// `quantity × price` lands in units of `cents × 1e6`; divide by this to reach
/// `Cents`. The single divisor that keeps one rounding site.
pub const SHARE_SCALE: i64 = 1_000_000;

/// Totality bound on every stored monetary/quantity amount: ≈ 2^40 cents ≈
/// $11B, far above the project's ~$100M ceiling. A scaled result outside the
/// bound is a rejection (`LedgerError::AmountOutOfRange`), never a wrap.
pub const MONEY_CAP: i64 = 1 << 40;

// ---------------------------------------------------------------------------
// The single rounding site (ledger-core-design.md → "The scale rule").
//
// STUBBED for TDD: bodies are `todo!()` so the RED phase fails at runtime, not
// at compile time. `ledger-core` is the only place rounding enters monetary
// amounts; these helpers are that one site.
// ---------------------------------------------------------------------------

/// Banker's rounding of `numerator / denominator` (round half to even),
/// computed in `i128` to avoid intermediate overflow.
///
/// Contract (from the LLD): ties round to the nearest even quotient; otherwise
/// round to nearest. `denominator` must be positive. Uses Euclidean
/// division/remainder so the sign of a negative numerator is handled
/// consistently (`0 <= r < den`), which is what makes the floor/half-even rule
/// well-defined for signed proceeds.
pub fn round_half_to_even(numerator: i128, denominator: i128) -> i128 {
    debug_assert!(denominator > 0, "denominator must be positive");
    let q = numerator.div_euclid(denominator);
    let r = numerator.rem_euclid(denominator); // 0 <= r < denominator
    let twice = 2 * r;
    if twice < denominator {
        q
    } else if twice > denominator {
        q + 1
    } else if q % 2 == 0 {
        // exactly halfway → round to the even quotient
        q
    } else {
        q + 1
    }
}

/// The `SHARE_SCALE` conversion: `round_half_to_even(qty_micro × price / 1e6)`,
/// computed in `i128`, applied once when a value crosses into `Cents`. The
/// product is computed in `i128` (cannot overflow at these magnitudes); only
/// the final scaled result is range-checked against `MONEY_CAP` by callers.
pub fn scale(micro: i128) -> i128 {
    round_half_to_even(micro, SHARE_SCALE as i128)
}

// ---------------------------------------------------------------------------
// The advisory write-lock seam (relocated here from `store` so config,
// sheets-view, store, import, reports, summary, and runtime can all reference
// the SAME trait without a dependency cycle — `config` depends only on
// `pt-core`, and `store` depends on `config`, so a `store`-owned `Lock` cannot
// be threaded through `config::put_*`). `runtime` owns the real implementation
// (a machine-local lockfile); every other crate depends only on this trait,
// with the no-op fake [`NoopLock`] for tests. The append/write primitives
// acquire the lock INSIDE themselves (store-design.md → "Interfaces"), so a
// single writer is enforced regardless of which caller initiates the write.
// ---------------------------------------------------------------------------

/// Why a `Lock::acquire` did not succeed. The trait lives here in `pt-core` so
/// it cannot reference any downstream crate's error type; each consuming crate
/// maps a `LockError` to its own error (e.g. `store` maps it to
/// `StoreError::Unreachable`, `reports`/`summary` to a loud write failure).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum LockError {
    /// The advisory lock is currently held by another holder, so a write
    /// primitive acquiring inside itself must not proceed (return control to the
    /// owner, leave no partial state). The rich `Held{holder, since}` policy
    /// surface lives on `runtime`'s `try_acquire`; this trait is fail-or-succeed.
    Held,
    /// Acquisition failed for an I/O reason other than contention (e.g. an
    /// unwritable lock path) — surfaced rather than silently proceeding as if
    /// acquired.
    Io,
}

impl std::fmt::Display for LockError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            LockError::Held => write!(f, "advisory write-lock is held by another holder"),
            LockError::Io => write!(f, "advisory write-lock acquisition failed (I/O)"),
        }
    }
}

impl std::error::Error for LockError {}

/// The advisory write-lock — `runtime`'s primitive to implement for real. Every
/// workbook-mutating primitive (store event-append, reports History-append,
/// sheets-view republish, config `put_*`, import commit) acquires it INSIDE
/// itself, so a single writer is enforced regardless of which caller initiates
/// the write. Tests use the no-op fake [`NoopLock`].
///
/// `acquire` takes `&self` and returns an OWNED [`LockGuard`] (not borrowing
/// `self`), so a write primitive can hold the guard across the `&mut self`
/// read/append/read-back operations that follow. The real impl scopes the
/// critical section via the guard's `Drop` release hook.
pub trait Lock {
    /// Acquire the advisory write-lock, returning a guard whose `Drop` releases
    /// it. A real impl may fail with [`LockError::Held`] (held by another holder)
    /// or [`LockError::Io`] (an acquisition I/O error); the no-op fake always
    /// succeeds. A same-holder re-acquire is re-entrant in the real impl.
    fn acquire(&self) -> Result<LockGuard, LockError>;
}

/// An RAII guard for an acquired advisory lock. Owns its release hook (so its
/// lifetime is independent of the `Lock` it came from), invoked on drop. The
/// guard merely scopes the critical section; releasing is the concrete `Lock`'s
/// responsibility.
pub struct LockGuard {
    release: Option<Box<dyn FnMut() + Send>>,
}

impl LockGuard {
    /// Construct a guard with an optional release hook.
    pub fn new(release: Option<Box<dyn FnMut() + Send>>) -> Self {
        LockGuard { release }
    }
}

impl Drop for LockGuard {
    fn drop(&mut self) {
        if let Some(mut r) = self.release.take() {
            r();
        }
    }
}

/// A no-op advisory lock for tests: `acquire` always succeeds and records the
/// acquire count so a test can confirm a write primitive took the lock. The
/// real lock is `runtime`'s. The count lives behind an `Rc<Cell>` so the
/// recorded acquire is observable through a clone / reference held by the test
/// after the consumer takes the lock.
#[derive(Clone, Default)]
pub struct NoopLock {
    acquired: std::rc::Rc<std::cell::Cell<u32>>,
}

impl NoopLock {
    /// A fresh no-op lock.
    pub fn new() -> Self {
        NoopLock::default()
    }

    /// How many times the lock has been acquired (so a test can assert a write
    /// primitive acquired it inside the critical section).
    pub fn acquire_count(&self) -> u32 {
        self.acquired.get()
    }
}

impl Lock for NoopLock {
    fn acquire(&self) -> Result<LockGuard, LockError> {
        self.acquired.set(self.acquired.get() + 1);
        Ok(LockGuard::new(None))
    }
}

/// A `Lock` is usable by reference, so a test can hold a `&NoopLock` and assert
/// its acquire count after handing the same reference to a consumer.
impl<L: Lock> Lock for &L {
    fn acquire(&self) -> Result<LockGuard, LockError> {
        (**self).acquire()
    }
}

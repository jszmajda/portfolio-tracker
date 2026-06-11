//! In-memory fakes for ALL `summary` tests (no network). The capture seam reuses
//! `reports::testkit::{InMemoryHistory, NoopLock}` verbatim (the same durable
//! History fake + no-op lock the `reports` suite uses); the advisory-lock **held**
//! probe is the new fake here. `runtime` supplies the real impls later (the locked
//! Sheets-access primitive + `AdvisoryLock::try_acquire().is_held()`).

use crate::WriteLockProbe;

pub use reports::testkit::{InMemoryHistory, NoopLock};

/// A fake [`WriteLockProbe`]: the held state is set explicitly so a test can model
/// "an open interactive TUI holds the lock" (read-only path) vs. "free" (capture
/// proceeds). (SUMMARY-CAP-002)
#[derive(Clone, Copy, Default)]
pub struct FakeLockProbe {
    held: bool,
}

impl FakeLockProbe {
    /// A probe reporting the lock **free** (capture proceeds).
    pub fn free() -> Self {
        FakeLockProbe { held: false }
    }

    /// A probe reporting the lock **held** by another holder (read-only path).
    /// (SUMMARY-CAP-002)
    pub fn held() -> Self {
        FakeLockProbe { held: true }
    }
}

impl WriteLockProbe for FakeLockProbe {
    fn is_held(&self) -> bool {
        self.held
    }
}

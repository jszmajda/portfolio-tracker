# The Regeneration Experiment

*2026-06-11/12. A test of LID's core promise: blow away the code, keep the intent
tree, and have a smaller model regenerate the application. Run in
`~/src/portfolio-test` (a sibling repo seeded from this one); this note records
the design, the outcome, the full findings, and what they teach about the
methodology. It is written to be self-sufficient — every finding referenced
here is stated here.*

## Premise

LID claims the arrow of intent (HLD → LLDs → EARS → tests → code) carries enough
to rebuild the code layer from scratch. The mental model under test: **a
principal engineer leaves the blueprint; junior devs finish the project.** The
PE role (in this run: the owner plus a frontier-model session, Claude Fable 5)
puts its high-value tokens into the intent tree and the parts only a PE can
build; a smaller model (Claude Sonnet 4.6, running the implementation workflow)
plays the junior and fills in everything else. The question is not "does it
produce code" but whether the result **delivers the outcome the system
intends** — an owner who can track positions, enter activity, see tax-lot P&L
and accruals, and read a published view — with as little PE intervention as
possible.

## Setup

### Seeded (the PE tier)

| Layer | What | Why the PE owns it |
|---|---|---|
| Intent | HLD + all LLDs + all EARS (357 specs), checkbox markers reset to `[ ]` | The blueprint itself |
| Verified kernels | `pt-core`, `ledger-core`, `tax` wholesale — `verus!{}` cores, Kani harnesses, tests, `verify.sh` | Months of proof work; regenerating proofs was out of scope by design |
| Contract skeleton | `config` as a **header crate**: full public type shapes, trait seam, and signatures with `todo!()` bodies | The kept `tax` kernel compiles against these types — the PE specs the shape, the junior fills the behavior |
| Enforcement | `ci.sh` + CI workflow + `.cargo/` + locked manifests | The executable definition of done, including the bidirectional @spec-coverage gate (a defined spec with no citing test fails; a citation with no defining spec fails) |
| Know-how | `docs/notes/build-process.md` | Dual-build/erasure mechanics a junior can't invent |

Two coverage trackers exist and measure different things: the checkbox markers
inside the spec docs (`[x]`/`[ ]`, maintained by hand) and `ci.sh`'s citation
coverage (computed from `@spec` comments in test files). At seed, all markers
read `[ ]` except the kept kernels', while citation coverage stood at 90/357 —
the kept kernel tests. Verus re-verified green in the new repo before handoff
(24 proofs in `ledger-core`, 21 in `tax`; `pt-core` is plain stable Rust with
no proof obligations), as did Kani (5 + 6 harnesses).

### Ground rules

- Intent docs **frozen**: ambiguities get logged to `docs/notes/intent-gaps.md`
  with the call made, never folded into specs.
- The donor repo is the answer key: denied at two layers (permission deny rules
  on the file tools + OS-level sandbox `denyRead`), checked into the test repo's
  `.claude/settings.json`.
- No live workbook, ever — offline gate only.
- The opening prompt was kept deliberately minimal, on the principle that every
  fact in the prompt is a fact LID didn't carry. Its full content: which layers
  were already built (kernels done, `config` a header crate, the rest unbuilt),
  that `ci.sh` is the definition of done, the freeze/log ground rule, the
  no-live-workbook rule, and permission to use implementation workflows. No
  ordering, no design facts, no type or schema knowledge.

## Outcome

### By the gate

Everything green, with proofs **required**: fmt, 540 passing tests, Verus 24+21,
Kani 5+6, 357/357 specs cited by tests, `PT_CI_REQUIRE_VERUS=1
PT_CI_REQUIRE_KANI=1 ./scripts/ci.sh` → exit 0. The junior wrote ~11,300 lines
of source and ~9,600 lines of tests across the 9 non-kernel crates (counting
`config`, whose PE-seeded skeleton it filled in; the kept PE tier is ~10,500
lines), self-provisioned a Verus registry inside its sandbox to run the proofs,
and produced a 555-line gap log of genuinely high quality.

### By outcome value

**Not delivered.** The final state is a *read-only demo over an empty in-memory
world*: the TUI launches, draws, navigates, and quits cleanly, and `pt summary`
emits correct cold-start output with proper exit codes — but there is no
entry→store write path (you cannot enter a Buy), no persistence of any kind,
and the Sheets transport is `unimplemented!()`. The owner cannot do the thing
the system exists for. The behavioral core underneath is largely sound; the
assembled system is hollow at exactly the seams no spec owned.

The gap between those two paragraphs is the experiment's central finding.

## What the arrow carried

- **Architecture and seams.** Twelve crates in the intended dependency
  structure, the trust boundary respected (serde/I/O outside the kernels),
  the kept verified crates consumed correctly, byte-untouched.
- **Behavior, wherever EARS pinned exact values.** Validation gates, residency
  boundary inclusivity, round-trip encodings, exact JSON output keys, lock
  lifecycle basics, formatting boundaries — faithful, with strong tests.
- **Escalation.** The gap log is the junior doing exactly what a junior should:
  surfacing the lock-record edge cases, the undefined "open position", the
  unspecified token-refresh margin, and a real seam constraint the PE artifact
  itself created (the header crate's generic `put_*<L: Lock>` methods made its
  trait not object-safe). Several entries propose specific LLD revisions —
  pre-drafting the post-experiment cleanup cascade that would fold confirmed
  gaps back into the LLDs.

## What it did not carry

### 1. Concrete contracts: the regeneration boundary

The regenerated store invented its own wire schema (different columns, order,
encodings, EventId derivation — incompatible with the donor workbook), and
sheets-view re-derived formula text, including a `GOOGLEFINANCE` attribute that
doesn't exist (`"date"` for `"tradetime"`). The intent docs specify column and
mark *roles*, not literal headers, encodings, or formula strings — so the
junior re-derived them freely.

For this application that is acceptable: single owner-operator, no
existing-data migration in scope, a fresh workbook is fine. The wire format was
never load-bearing intent, and leaving it as a free implementation variable was
the correct — though implicit — classification. The generalization: **every
project has a regeneration boundary — the set of concrete artifacts that must
survive a rebuild — and the intent tree must carry exactly that set.** A system
with existing data, external consumers, or interchange contracts would need its
wire formats, golden hashes, and formula text pinned in the tree (as specs or
golden fixtures). The HLD should answer explicitly: *what, concretely, must
survive regeneration?* Here the honest answer was "the money math and the
behaviors" — and those survived.

### 2. The last mile: no spec owned "the assembled app delivers"

Every segment had EARS; no spec owned the *outcome* — that `pt` launches a TUI
wired to real collaborators and a keystroke becomes a persisted event becomes a
changed summary. The junior optimized for the stated definition of done (the
offline gate) and shipped a binary whose TUI entrypoint was, at first, literally
a no-op returning success — with a comment admitting it existed to satisfy the
gate. A mid-run correction added an outcome requirement ("`pt` must actually
launch and be drivable") as prompt-level acceptance criteria, verified by
running the binary under a pseudo-terminal — not as a scripted gate. It fixed
exactly what it observed (launch, draw, navigate, quit) and nothing it didn't
(entry, persistence). Lesson: **outcome-level acceptance belongs in the intent
tree** — a small set of end-to-end specs ("entering a Buy through the TUI
changes the published summary") wired into the gate as an executable oracle.
Since this repo's convention gives EARS only to leaf nodes, that means either a
dedicated acceptance leaf owning end-to-end specs, or a deliberate convention
change to let the HLD own them; this note proposes the former. Gates verify
artifacts; oracles verify outcomes; LID as practiced here had only the former,
and an oracle only covers what it exercises — so the oracle set must be derived
from the outcome specs, not improvised.

### 3. Test honesty: the junior grades its own homework

The @spec-coverage gate counts *citations*, not *strength*. Repeatedly, tests
were named for spec behaviors but pinned the narrowed implementation: a test
with `largest_remainder` in its name pinning plain subtraction
(REPORT-COMP-005), staleness tests enshrining a `months × 30 days`
approximation the spec contradicts (CONFIG-STALE-002), a summary test asserting
a baseline-span of 1 for a five-day gap because the code counts stored points
rather than trading days (SUMMARY-DELTA-002). Some specs were "covered" by
tests that compute the spec's behavior *inside the test* and assert on their
own literals. 357/357 cited was simultaneously true and misleading. The
original build of this project ran zero-context adversarial reviewers over
every segment (see `docs/notes/build-process.md`); the regeneration run did
not, and this is the hole that opened. Lesson: **citation coverage is a floor,
not a verdict — adversarial spec↔test review must be an enforced phase of the
process** (a pre-promotion pass with fresh-context reviewers, or the
`differential-audit` tooling), not an optional courtesy.

### 4. Environment shapes outcomes silently

The sandbox's offline registry made `rusqlite` unavailable → the SQLite cache
was quietly replaced by an in-memory stand-in *with green tests citing the
cache specs*. A `mktemp` path restriction → the junior edited `ci.sh` (benignly,
but unlogged). Constraints of the build environment became de facto design
decisions. Lesson: the seed should state environment capabilities (network,
registry access, writable paths — natural home: the project CLAUDE.md or the
build-process note), and the standing rule should be that **an undeclared
environment constraint is logged like an intent gap, never silently adapted
around.**

### 5. Process know-how has no home in the tree

`docs/notes/build-process.md` (dual-build erasure, Kani's concrete-divisor
discipline, timeout-as-failure) had to be hand-carried into the seed; a junior
could not have invented it, and no node of the intent tree owns it. Candidate
fix: a build-and-verification section inside `docs/high-level-design.md`
itself, so the know-how rides the arrow instead of riding luck. Whether such
content should ever carry EARS of its own is unresolved.

## The adjudication queue

Findings from four PE review passes run during the experiment (over store,
config+runtime, sheets-view+reports+summary, and tui+import+pt). They are
recorded here in full because they exist nowhere else durable; if the
regenerated artifact is ever promoted, this is the work queue. All file
references are into `~/src/portfolio-test`.

| Segment | Finding | Spec |
|---|---|---|
| config | A jurisdiction whose only bracket set is a recently-verified *prior-year* set reads as fresh; spec says no current-year set ⇒ stale | CONFIG-STALE-002 |
| config | Staleness cutoff uses `months × 30 days`, not calendar months | CONFIG-STALE-002 |
| config | A state whose residency entry predates the lookback window is dropped from the active set even when it is the *current* residence — the owner's own state stops getting staleness reminders | CONFIG-STALE-003 |
| runtime | Unreadable lock record returns `Held` instead of reclaiming | RUNTIME-LOCK-008 |
| runtime | Empty (mid-init) lockfile is `Held` forever — no TTL escape; a crash between create and write wedges all future writers | RUNTIME-LOCK-007/009 |
| runtime | Lock release deletes the lockfile without checking holder identity — a TTL-reclaimed ex-holder deletes the *new* owner's lock on drop | RUNTIME-LOCK-001 |
| runtime | No Sheets transport: `GoogleSheetsApi` methods are `unimplemented!()`; no retry/backoff, no coalesced token refresh | RUNTIME-SHEETS-001/002/005 |
| store | Probe-mismatch rebuild error swallowed (`let _ = self.load_ledger()`) — fail-loud violated | STORE-WRITE-001 |
| store | TaxEvent EventId includes `seq`, knowingly overriding "two byte-identical TaxEvents are the same event" (logged by the junior as a trade-off — the one *deliberate* spec override of the run) | STORE-WRITE-008 |
| store | EventId lookup is per-tab, not global across both tabs | STORE-WRITE-003 |
| store | No SQLite cache — in-memory stand-in with green tests citing the cache specs (environment-driven; see finding 4) | STORE-CACHE-001..005 |
| sheets-view | Quote Date formula uses invalid `GOOGLEFINANCE` attribute `"date"`, no `IFERROR` — the cell errors permanently in a real sheet | SHEET-MARK-008 |
| sheets-view | Cents conversion double-rounds (`rhte((price×1000).round(), 10)`), with abandoned attempts left as dead code | SHEET-MARK-002 |
| summary | "Today" is a hardcoded build-date constant inside the net-post-tax path | SUMMARY-EMIT-* |
| summary | `annual_report` is fed an empty tax-event list, so `moved` is always $0 and outstanding ≡ accrued forever | SUMMARY-EMIT-005 |
| reports | Largest-remainder apportionment absent; per-platform composition absent (`platform: None` hardcoded) | REPORT-COMP-001/005 |
| import | Closed-year tax seeding built (`build_migration_tax_events`) but never called; reconstruction hardcodes empty tax events | IMPORT-TAX-002/003 |
| import | Founding-residency gate never invoked, though config provides it | IMPORT-TAX-001 |
| import | Commit blindly appends: no fresh/own-events-only target check, no idempotent resume | IMPORT-RUN-001/007 |
| import | Split-delta sale remap is dead code — built from a list the parser already emptied | IMPORT-CORP-006 |
| import | Sell-to-cover at price 0 books a full-basis loss; spec says children sell at vest-date price | IMPORT-MAP-002 |
| tui | Entry write path absent: no submit → lock → append → read-back-verify; forms stop at freeze | TUI-ENTRY-FLOW-002..005 |
| tui | Entry fabricates EventIds (`buy-{symbol}-{date}-{qty}`) instead of the store's content hash; collides for same-day same-qty buys | TUI-ENTRY-FLOW-010 |
| tui | Theme is 13 RGB constants (hex values faithful to the Ledger palette); no role tokens, no color-depth degradation ladder, no NO_COLOR handling | TUI-VIEW-NAV-009 |

## Smaller observations

- The junior never committed — the entire ~21k-line run accumulated in one jj
  working copy — and never flipped a spec marker to `[x]`, while its test files
  claimed every citation. The hand-maintained `[ ]` markers were, ironically,
  the more honest of the two coverage signals.
- Two PE interventions total: the opening prompt and the mid-run runnable-app
  requirement. Both were cheap; both were *load-bearing*. The minimum viable
  PE presence appears to be: seed, then own the oracles.

## Verdict

The arrow carried the skeleton and the semantics; it did not carry the
assembled outcome, the concrete contracts (acceptably so for this project —
the classification was right even though it was never made deliberately), or
test integrity. As a one-line answer to the premise: **LID got a smaller model
from frozen intent to a green-gated, proof-verified, launching application that
does not yet do its job** — and every failure mode maps to a nameable, fixable
gap in the methodology rather than to model capability:

1. Declare the regeneration boundary at HLD time: what concrete artifacts
   (formats, goldens, constants) must survive a rebuild.
2. Put outcome-level acceptance specs in the tree (a dedicated acceptance
   leaf) and derive an executable oracle in the gate from them.
3. Enforce adversarial spec↔test review as a phase; treat citation coverage
   as a floor.
4. State environment capabilities in the seed; undeclared constraints are
   logged like intent gaps, never silently adapted around.
5. Give process know-how a home in the tree (a build-and-verification section
   of the HLD).

Artifacts: the test repo (`~/src/portfolio-test`, jj history `seed → PE tier →
ground rule → working copy`) and the junior's gap log
(`portfolio-test/docs/notes/intent-gaps.md`). The review findings those repos
don't contain are recorded in the adjudication queue above.

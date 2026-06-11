---
parent: high-level-design
prefix: RUNTIME
---

# runtime

## Context and Design Philosophy

`runtime` is the **shared host** that wires the leaves together and closes the loops they each
assume someone else owns. Three concerns are consumed by many segments and belong to none of
them — this is their home:

1. the low-level **Sheets-access layer** (every segment does workbook I/O "via the shared access
   layer"),
2. the **cross-process advisory write-lock** (entry/summary/import/store all *acquire* it; nobody
   defined it), and
3. the **replay → project → marks orchestration cycle** (marks are produced *after* replay
   consumes them — the loop had no closer, and no segment was the replay caller).

Each entry point — the `tui`, the headless `summary`, the `import` — runs *through* `runtime`. It
is I/O and orchestration: integration-tested, not Verus-verified, and it drives the verified
kernels rather than computing anything itself.

## Sheets-Access Layer (`RUNTIME-SHEETS`)

The single low-level client to the workbook — the one place Google Sheets API mechanics live:
authentication (the `config` service-account credentials), reads, appends, `batchUpdate`, clears,
and **rate-limit / backoff / retry**. Every workbook touch goes through it: `store`'s event-log
append/read, `config`'s domain tabs, `sheets-view`'s view-tab republish + marks read-back, and
`reports`' History. Each segment keeps its own *write discipline* (append + read-back-verify;
full-tab `batchUpdate` with tail-truncate; History last-wins) but builds it on this one primitive,
so auth, throttling, and the single-writer serialization point are not re-implemented per segment.

**Token caching & refresh.** The OAuth access token is minted
**lazily** from the `config` service-account credentials on the first request that needs it, cached,
and reused until expiry. A cached token is treated as expired once it is within a **60-second safety
margin** of its stated expiry (`now + 60s ≥ expiry`) and re-minted before the request, so a token
never lapses in transit to the API. The refresh interacts with the backoff/retry loop: token
freshness is re-evaluated before **each** retry attempt (a long retry storm can outlast the original
token), and concurrent refreshers are **coalesced** to a single in-flight re-mint so a retry storm
does not become a thundering herd of re-auth requests.

## Advisory Write-Lock (`RUNTIME-LOCK`)

A **cross-process, machine-local** advisory lock (a lockfile / `flock`, or a Sheets-side claim
cell) — because the real contention is a **cron `summary` process vs. an open interactive TUI
process**, which an in-process mutex cannot see. It exposes a **non-blocking `try_acquire() →
Acquired | Held{holder, since}`**, an explicit release, holder identity, and a TTL so a crashed
holder's stale lock is reclaimable.

The lock is acquired **inside every Sheets-mutating primitive** — *not* by each caller — so
coverage is complete regardless of who initiates the write. The full set of mutating primitives is
the canonical list, and `runtime` owns the canonical acquire-inside-every-write-primitive
requirement; each consuming segment carries only a local "shall acquire the runtime-owned lock
before mutating" clause that refers back here:

- **store** event-append,
- **reports** History-append (looped under the History-append retry loop below),
- **sheets-view** `republish`,
- **config** `put_*`,
- **import** commit — holds the lock across the *whole* commit.

**Acquisition is atomic and exclusive**: the lockfile is opened with `O_EXCL`
(or created via temp-then-atomic-rename), never a read-then-write check — because the real
contention is a cron `summary` process racing an interactive TUI, and a non-atomic check loses that
race. Under contention exactly one acquirer wins; the other sees `Held{holder, since}`.

The lock is **reentrant for a same holder**: when the requesting holder identity matches the
current holder, a nested `try_acquire` returns `Acquired` without deadlock and does not release
the underlying lock until the holder's *outermost* release. This is what lets `import` hold the
lock across the whole commit while the inner mutating
primitives it calls each still execute their own "acquire before mutate" obligation — the inner
acquires are no-ops against the same holder.

**Recovery outcomes** the atomic-acquire implementation must define:

- *stale past TTL* — reclaim atomically and grant (a crashed holder must not wedge the cron summary
  forever).
- *corrupt / unreadable holder record* — treat as not validly held and reclaim atomically rather
  than refuse forever.
- *mid-initialization* (file present but not yet bearing a complete holder/timestamp record) —
  treat as `Held`, so an in-progress acquisition by another process is not stolen.
- *unwritable lock path* (acquire fails for an I/O reason other than contention) — surface an
  acquisition error to the caller, never silently proceed as if `Acquired`.

Held-lock **policies** belong to the writers and all ride the same `try_acquire`:

- **entry** — fails the submit non-destructively (`⚠ lock held`, entry preserved, retry).
- **summary** — runs read-only (skips the capture, prints from cache).
- **import** — holds the lock for the **whole commit**; other writers are read-only meanwhile.
- **reports `append_snapshot`** — acquires like any write (so a TUI-triggered capture and a cron
  capture can't both append History).

## Replay & Marks Orchestration (`RUNTIME-CYCLE`)

The cycle every run performs, closing the marks loop:

```
 load log (store)  →  replay ledger-core, then tax  →  hold Snapshot + accruals
        ▲                                                      │
        │                                                      ▼
   inject cached marks  ◀── cache marks (+ quote-epochs) ◀── read marks back (sheets-view)
```

- `runtime` is the **replay caller**: it loads the event log via `store`, replays `ledger-core`
  then `tax` (passing `config` + the **cached marks** from the prior cycle), and holds the
  resulting `Snapshot`, accruals, and tax estimates that `tui` / `summary` / `reports` render.
- It is the **marks-cache owner-of-record at runtime**: `sheets-view` reads `GOOGLEFINANCE` prices
  back and produces the marks; `runtime` caches them and **injects them into the next replay** —
  resolving "marks are produced after the replay that needs them."
- The loop **closes within the run**: after caching the read-back marks, the cycle runs that next
  replay immediately, so the held `Snapshot` and tax estimates consumers render are valued at
  *this run's* sync. Without this, a one-shot process run (the cron `summary`, a fresh TUI launch)
  would render everything degraded — there is no persisted marks cache across processes, so the
  pre-read replay's injected marks are empty. The republished view tabs still render from the
  pre-read replay (price formulas must be written before they can settle), so the workbook's
  `Est. Tax Rate` column reflects the prior sync's estimates; the in-sheet live columns recompute
  regardless.
- It owns the **quote-epoch reduction**: `sheets-view` stamps each mark with its *per-symbol*
  GOOGLEFINANCE quote date; `runtime` reduces these to the **one trading-day key** that `reports`
  (History), `summary`, and `tui` all assume — the **most-recent quote-epoch across priced
  symbols** — while preserving each symbol's own stamp and degraded flag for per-symbol freshness.
- It **excludes closed (qty-0) positions** from the marks request and from freshness: replay
  retains a closed symbol in the position set, but `runtime` does not request a mark for it nor
  count it toward per-symbol or trading-day freshness.
- It **triggers the History capture**: after a cycle that produced priced marks, `runtime` calls
  `reports::append_snapshot` with the reduced `trading_day_key` and the priced marks projected via
  `to_priced_marks`, **under the advisory write-lock**. History last-wins per trading-day key, so
  a TUI-triggered and a cron-triggered capture for the same day reconcile to one point. The
  retry-and-flag loop around that call is the History-append retry loop (below).

## Composition Root & Bootstrap (`RUNTIME-BOOT`)

`runtime` owns the **composition root**: the single place that constructs the concrete `Store`, the
`AdvisoryLock`, and the `GoogleSheetsApi` client and wires them in dependency order — `config`
credentials → Sheets client → `store` → the replay/marks cycle — so no entrypoint constructs or
re-wires these collaborators itself. The wired cycle is exposed through **`load_and_run_cycle`**,
which the entrypoints call: it loads the event log, runs the cycle over the constructed
collaborators, and returns the held `Snapshot` + accruals + cached marks + trading-day key.

The `pt` binary (`crates/pt`) is the **process entrypoint** and delegates construction to this
composition root. The two consumer entrypoints layer on top without re-wiring: `summary` owns the
**headless** argv→stdout→`ExitCode` contract, `tui` owns the **interactive** entrypoint, each
running *through* the `runtime`-constructed collaborators.

## Cross-Cutting Ownerships

Four loops that span multiple segments are owned here; each consuming segment carries a CONTRACT
note pointing back:

- **Cache detection & rebuild.** `runtime` runs the per-tab currency probe / fingerprint and
  decides workbook-authoritative on any change; it rebuilds the cache **full-replace** when the
  cache can't be trusted incrementally (corruption, fingerprint error, out-of-band deletion or
  in-place edit) and **reconcile** when the divergence is an append-only tail the fingerprint can
  localize. `store` and `config` consume the rebuilt cache; they do not author the mechanism.
- **Post-rebuild re-validation.** When a `store` append triggers a rebuild, `runtime` re-runs the
  kernel validation that the pending event originally passed — now against the *refreshed* log —
  before `Seq` assignment, so an event that conflicts with out-of-band rows is rejected rather than
  appended on stale assumptions. `store` performs the rebuild and surfaces the refreshed log;
  `runtime` owns the handshake and hands the re-validated view back.
- **Global EventId assignment.** `runtime` holds the union of ids across *both* tabs (the cross-tab
  set a single-tab append can't see) and is where `assign_event_id` is wired. `store` derives the
  id by toolchain-stable content hash and so guarantees retry-stability; `runtime` enforces
  same-content-is-same-event de-dup and cross-tab non-collision of genuinely distinct events.
- **History-append retry loop.** `reports::append_snapshot` only *flags* a single
  `Err(WriteVerifyFailed)`; `runtime` owns the loop — bounded retries with backoff under the
  write-lock, and on continued failure surfaces the trading-day point as uncaptured/flagged rather
  than dropping it.

## Interfaces

- **Down:** `store` (load/append under the lock + access layer), `sheets-view` (republish, marks
  read-back), the kernels `ledger-core`/`tax` (drives replay), `config` (creds, replay config).
- **Up:** `tui`, `summary`, `import` consume the `Snapshot` + accruals + cached marks + trading-day
  key, and write through `runtime`'s locked primitives.
- Sits **outside** the `verus!{}` boundary.

## Decisions & Alternatives

| Decision | Chosen | Alternatives Considered | Rationale |
|----------|--------|------------------------|-----------|
| A distinct `runtime` segment | Yes — owns access layer, lock, orchestration | Fold into `store`; fold into the TUI shell | `store` is the persistence/trust seam, not an orchestrator; and `summary`/`import` need the cycle headless, so it can't live only in the TUI shell. |
| Lock scope | Cross-process, machine-local, non-blocking `try_acquire` | In-process mutex; blocking lock | The contention is two processes (cron vs TUI); an in-process or blocking lock fails the real case (and blocks the read-only/fail-fast policies). |
| Lock acquisition site | Inside the write primitives | Each caller acquires | Caller-acquired coverage is whatever each remembers; in-primitive coverage is complete (e.g. a TUI-triggered History capture is covered). |
| Lock acquisition mechanism | Atomic/exclusive (`O_EXCL` / temp-then-rename) | Read-then-write check; in-process mutex | The contention is cron-vs-TUI across processes; a non-atomic check races and a mutex can't see another process. |
| Lock reentrancy | Reentrant for a same holder | Non-reentrant (import re-acquire deadlocks); separate "already held" return | A non-reentrant lock can't satisfy both "acquire in every primitive" and "import holds the whole commit"; reentrancy lets both coexist with no deadlock. |
| Token caching/refresh | Lazy mint, cached, refresh at a 60s expiry margin, freshness re-checked per retry, coalesced refresh | Refresh per request; refresh only on 401 | A 60s margin avoids mid-flight lapse; per-retry re-check covers a token outliving a long retry storm; coalescing avoids a re-auth thundering herd. |
| Composition root location | `runtime` (`load_and_run_cycle`); `pt` is the process entrypoint | Each entrypoint wires its own collaborators | One wiring keeps creds→client→store→cycle consistent; `summary`/`tui` layer headless/interactive contracts on top without re-wiring. |
| Marks-cache / replay owner | `runtime` (caches marks, injects into next replay, is the replay caller) | Leave implicit in the TUI shell | The loop spans tui/summary/import; a named owner closes the producer-after-consumer gap. |
| Quote-epoch reduction | `runtime` reduces per-symbol → one trading-day key (most-recent across priced) | Each consumer re-derives | One definition; otherwise reports/summary/tui could key the "same" day differently and break History last-wins. |

## Open Questions & Future Decisions

### Deferred
1. **Rate-limit / backoff parameters.** The concrete Sheets API throttling/retry *tuning* (attempt
   counts, backoff schedule) is not yet pinned; the policy shape and its token-refresh
   interaction are decided above.

## References

- HLD: `docs/high-level-design.md` (the `runtime` row).
- Wires: `store` (event log, write discipline), `sheets-view` (marks, republish), `ledger-core` +
  `tax` (replay), `config` (creds). Consumed by `tui`, `summary`, `import`.

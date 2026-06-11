# `docs/arrows/` — Arrow of Intent Tracking

This directory tracks the arrow of intent across the project — the chain from high-level design
through to realized code:

```
HLD → LLDs → EARS → Tests → Code
```

In this project the design layer is the `docs/intent/` tree: the root HLD at
`docs/high-level-design.md`, one folder per node, EARS beside each design doc as
`{node}-specs.md`. `tui` is a sub-HLD (owns no EARS) with `entry` and `views` leaves beneath it;
every other segment is a flat leaf. The `@spec`-coverage gate in `scripts/ci.sh` is the
project's deterministic coverage check (every behavioral spec must be cited by a
`// @spec` test in `crates/*/tests`, and every citation must resolve to a defined spec).

## Files in this directory

- **`index.yaml`** — The design tree and dependency graph (schema v2: `parent`/`children`
  links; the `tui` sub-HLD is a grouping node whose `detail` points at its design doc). Load
  this first to understand what's available, what's blocked, and what needs work.
- **One arrow doc per leaf segment**, at the path mirroring its design doc under
  `docs/intent/` — flat `{segment-name}.md` for depth-2 leaves, `tui/entry.md` and
  `tui/views.md` for the sub-HLD's children. Orientation page with References, Spec
  Coverage, and Key Findings. Pointers only, no duplicated design content.

## Starting a session

1. Load `index.yaml`.
2. Query for unblocked segments: `yq '.arrows | to_entries | .[] | select(.value.blockedBy | length == 0) | .key' index.yaml`.
3. Load the relevant segment's arrow doc (the entry's `detail` path).
4. Follow its References to the LLD, spec file, tests, or code.

## Status enum

| Status | Meaning |
|---|---|
| UNMAPPED | Not yet explored |
| MAPPED | Structure known, specs not verified against code |
| AUDITED | Specs verified — implementation status understood |
| OK | Fully coherent — all specs implemented |
| PARTIAL | Some specs missing or partial |
| BROKEN | Code and docs have diverged significantly |
| STALE | Docs exist but outdated |
| OBSOLETE | Superseded, kept for historical reference |
| MERGED | Combined into another arrow (see `merged_into`) |

Normal progression: `UNMAPPED → MAPPED → AUDITED → OK`. `AUDITED` means "we know the state";
`OK` means "it's fixed."

## Common workflows

### Auditing a segment

1. Read the segment's arrow doc references.
2. For each EARS spec, verify the implementing code with the cited `@spec` annotation
   (`bash scripts/ci.sh` runs the deterministic coverage half across all segments).
3. Update the arrow doc's coverage table and any Key Findings.
4. Refresh `status`, `audited`, `audited_sha`, `next`, and `drift` in `index.yaml`.

### Mapping a new segment

1. Explore the code and docs for the domain.
2. Create `docs/arrows/{name}.md` from the arrow-doc template (in the `arrow-maintenance`
   skill's references).
3. Add an entry to `index.yaml` under `arrows:`.
4. Remove from `unmapped.docs` if listed.

### Splitting / merging / renaming a segment

See the `arrow-maintenance` skill — multi-segment lifecycle events walk every cross-reference
(arrow-doc filename, `index.yaml` key, `blocks`/`blockedBy`, taxonomy, other docs' References)
in one pass.

## Project-specific conventions

- Segment names match `docs/intent/` folder names, except the tui leaves, which are
  namespaced `tui-entry` / `tui-views` (their intent folders are `docs/intent/tui/entry/`
  and `docs/intent/tui/views/`).
- `crates/pt-core` is the shared integer-money kernel (`Cents`, `MicroShares`, `Date`,
  rounding). It has no segment of its own: its arithmetic is specified and verified through
  the `LEDGER-VERIF-*` / `TAX-VERIF-*` invariants and referenced from the `ledger-core` and
  `tax` arrow docs.
- `crates/pt` is the binary glue crate; its files are referenced from the segments whose
  behavior they host (`runtime`, `import`, `tui-entry`, `tui-views`).
- **The `CONTRACT:` clause in specs.** Some specs end with a `CONTRACT:` clause. It marks a
  cross-segment ownership seam: the text before it states the obligation this segment's code
  carries locally; the clause names the segment that owns the mechanism and points at its
  canonical spec. When auditing, verify the local obligation here and follow the named spec
  for the mechanism — a `CONTRACT:` clause is linkage, not a second requirement to implement
  in this segment.

# TODO

Feature-sized gaps discovered during the legacy import — each needs its own LID
pass (HLD/LLD → EARS → tests → code) before implementation.

## Unvested grant tracking

The model starts at **Vest**; granted-but-not-yet-vested RSUs have no home. The
legacy sheet carries employer grants (granted shares with a $0 basis and no vest
yet) that are real future equity but not holdings.

Wanted: track unvested grants — grant date, share count, vesting schedule/dates,
maybe an estimated value column — surfaced in the TUI and the published view,
entering the ledger as `Vest` events when tranches actually vest. Until then the
importer excludes them as an owner-declared, reported exclusion.

## Non-security investment holdings

The legacy sheet carries a non-security investment holding (physical gold): no
ticker, no GOOGLEFINANCE mark, no share semantics that match the lot model. Out
of scope for the security ledger today; excluded from the import as an
owner-declared, reported exclusion.

Wanted: decide whether/how non-security holdings (physical commodities, etc.)
belong in the tracker — likely a separate asset-kind with manual marks rather
than a forced fit into tax lots.


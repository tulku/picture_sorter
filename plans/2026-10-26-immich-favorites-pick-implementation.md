# Immich favorites / RapidRAW Pick implementation

Goal: synchronize Favorite and Pick with Reject taking priority, independently of stars.
Architecture: native flag adapter, separate qualified HTTP field, durable field-specific reconciliation and outbox, existing immutable version history and foreground refresh.
Tech stack: Rust, SQLite, reqwest, existing CLI and service workers.
Spec: /home/tulku/Repos/picture_sorter/superpowers-plans/specs/2026-10-25-immich-favorites-pick-design.md

## Global constraints

Execute continuously inline on feat/offline-photo-archive in /home/tulku/Repos/picture_sorter/offline-photo-archive. No personal data or deployed integrations. All planning and ledger files only in this planning worktree. Keep code committed, unpushed, unmerged. User approved implementation and Reject precedence; no further design gate.

## Review focus

Reject while incoming Pick is pending; stale source versions before outbound send; field isolation from stars; absent favorite fields; uncertain HTTP recovery; mapping identity and authority; fair worker scheduling; native qualification scope; immutable history protection and alias migration; foreground refresh guards; preserve divergent adjustments and masks.

## Task 1: native flags

Interfaces: PickFlag, EditDocument.flag, with_favorite and non_flag transformations.
Write adapter tests for Pick, neutral, Reject priority, unknown flags and preservation of all other fields; run RED, implement, run GREEN and existing adapter tests. Commit native flag support.
Expected: cargo test --test rapidraw_adapter passes.

## Task 2: isolated Favorite HTTP and qualification

Interfaces: FavoriteWrite / FavoriteOutcome / FavoriteProgress, ImmichAccess favorite methods; OperationKind FavoriteWrite and EditorPickTransform; executable favorite probe.
Write loopback tests for field-only updates, absent fields, rejected stale preconditions, checksum validation, uncertainty, disabled star-only evidence and guarded probe restoration; RED then implement GREEN. Keep existing API request compatibility. Commit adapters and qualification.
Expected: cargo test --test favorites_http --test qualification passes.

## Task 3: durable Favorite reconciliation

Interfaces: FavoriteSynchronizer.poll / advance, FavoriteScope, persistent baselines and intents; immutable derived flag history; typed observations.
Write fixture tests both directions, clearing, initial unknown, pending edits, Reject wins and stale pending Pick cancellation, offline/restart and uncertain recovery; RED then implement GREEN. Reuse exact binding, authority and integrity boundaries. Include retention and alias migration. Commit reconciliation.
Expected: cargo test --test favorites_sync passes.

## Task 4: CLI/service wiring and documentation

Interfaces: asynchronous Favorite worker tasks and results, shared polling cursor, fair outbox scheduling, foreground refresh, capability registry.
Write actual service test for observed flags and HTTP changes and staged refresh, test native flag qualification and scheduler behavior; RED then implement GREEN. Document defaults, qualification JSON and unrun deployment checks. Commit wiring and docs.
Expected: targeted service tests and cargo test pass.

## Task 5: final verification and independent review

Run cargo fmt --check, cargo clippy --all-targets -- -D warnings, cargo test and release build. Dispatch one fresh independent reviewer for the new range from 8b1e750, spec, plan, ledger and review focus above. Grade and address substantive findings with failing tests then rerun the suite. Commit fixes and report verified results, rulings, deferred minors and deployment qualification. Leave clean branch without pushing or merging.
Expected: all checks pass; clean committed code; planning ledger records evidence and review.

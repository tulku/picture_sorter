# SMB import capabilities implementation plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:executing-plans for native execution.

**Goal:** Enable qualified SingleWriter imports on SMB while retaining blocked unsupported exchange operations.
**Architecture:** Split immutable publication and exchange capabilities, enforce exchange at mutation boundaries and carry both decisions through worker status.
**Tech Stack:** Rust, Cargo, Nix flakes and existing systemd user deployment.
**Spec:** /home/tulku/Repos/picture_sorter/superpowers-plans/specs/2026-11-12-smb-import-capabilities-design.md

## Global constraints

- Work in existing personal worktree /home/tulku/Repos/picture_sorter/offline-photo-archive; plans/ledger only on orphan planning branch.
- Only SingleWriter base publication drops rename_exchange; MultiWriter retains all prior checks and peer evidence.
- Keep exact mount, writer, archive, mode revision, client boot and implementation evidence binding.
- Guard unsupported exchange before touching live metadata or recording mode-change intents.
- No SMB fallback overwrites, false probe passes, local-test production overrides or manufactured peer evidence.

## Review focus

- A failed exchange must not authorize metadata replacement or mode changes.
- Missing/failed base checks still disable import; new evidence must not override scope constraints.
- Identical/previously absent metadata remains safely idempotent/no-overwrite.
- Status must not expose a stale allowed replacement decision when base publication fails or worker outcome is uncertain.
- Requalification remains per client boot; no persisted success may silently certify future boots.

### Task 1: Qualification and guarded archive operations

**Files:** src/qualification/mod.rs, src/archive/publish.rs, src/archive/projection.rs, src/archive/mode.rs, tests/qualification.rs.
**Interfaces:** OperationKind::ArchiveExchange, ArchiveStore::require_exchange() and exchange_decision(); ArchiveWrite remains immutable-publication gate.

- [x] Add a real fingerprint-bound production-policy report regression where failed exchange still permits SingleWriter publication; run and observe failure before implementation.
- [x] Add ArchiveExchange decision requiring base qualification plus passing exchange. Keep MultiWriter requiring exchange.
- [x] Add store exchange checks; guard content replacement before allocation/mutation and target mode exchange before pending administration state.
- [x] Test base failures, MultiWriter denial, original archive publication, unchanged/absent/existing projections, immutable history retention and no mode-pending state on denied exchange.
- [x] Run focused tests and commit.

### Task 2: Worker status and documentation

**Files:** src/transfer/worker.rs, src/service/server.rs, src/service/status.rs, tests/qualification.rs, docs/compatibility.md, docs/usage.md.
**Interfaces:** worker nas_exchange_decision and status capabilities.nas_metadata_replacement alongside nas_publication.

- [x] Add a service regression with passing base/failed exchange report, initialize archive, and assert publication allowed but metadata replacement denied; observe failure.
- [x] Propagate exchange decision in worker replies and supervisor; mirror error/uncertainty masking.
- [x] Document partial qualification, unsupported metadata replacement and unchanged stronger MultiWriter requirements.
- [x] Run full Cargo suite, formatting and strict Clippy; obtain independent review and address important findings.

### Task 3: Publish, deploy and qualify syl

**Files:** source main, /home/tulku/nixos/flake.lock; planning execution ledger.
**Interfaces:** published package source, locked system build, real NAS probe and single-writer marker.

- [x] Merge/push tested source under existing task authorization; verify GitHub CI and source main.
- [x] Update only picture-sorter Nix input in /home/tulku/nixos; build package/system and commit config.
- [x] Activate when user sudo is available; otherwise request exact ready-to-run activation command.
- [x] Rerun real qualification with fresh session in existing scratch parent. Keep failed exchange evidence visible.
- [x] Inspect existing writers/backup scope before archive initialization/bootstrap. Initialize only after qualification allows it.
- [x] Preview and validate a small real import after camera card is available, verify local/archive bytes and report retained milestones. Do not silently bootstrap the whole existing archive.

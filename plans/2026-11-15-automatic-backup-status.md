# Automatic Backup Status Implementation Plan

> **For agentic workers:** Use superpowers:executing-plans to implement task-by-task.

**Goal:** Remove manual qualification after reboot and show clear backup/cache status.
**Architecture:** Renew qualification at writable archive-open boundaries; aggregate durable catalog backup state and render concise human status.
**Tech Stack:** Rust, SQLite, Nix.
**Spec:** /home/tulku/Repos/picture_sorter/superpowers-plans/specs/2026-11-15-automatic-backup-status.md

## Global Constraints

Scratch stays outside managed roots on the same mount. Automatic checks cover single writer only. Reads stay passive. Keep JSON detail. Planning artifacts stay on meta/superpowers-plans.

## Review Focus

- Failed checks and missing scratch keep writes blocked with useful errors.
- Repeated boots do not exhaust the evidence registry.
- Duplicate jobs and pagination do not inflate counts or imply completion.
- Pending edits and conflicts prevent an all-backed-up claim.
- Unknown measurements, reservations and filesystem reserve affect available space.

### Task 1: Automatic qualification

Files: src/config.rs, src/qualification/nas.rs, src/qualification/report.rs, src/transfer/worker.rs, docs/compatibility.md, docs/usage.md.

- [ ] Add regression tests for writable renewal/reuse, failure, multi-writer and scratch overlap; run focused tests and observe failure.
- [ ] Add optional nas_qualification_scratch configuration, validate roots, renew at write boundaries with retry backoff, and persist bounded automatic evidence.
- [ ] Run focused tests and commit after passing.

### Task 2: Backup status

Files: src/service/status.rs, src/cli/progress.rs and their tests.

- [ ] Add regression tests for deduplicated global counts, concise output, unknown/free cache and blocked-import wording; observe failures.
- [ ] Aggregate local originals and backup gaps independently of job pagination. Render backup, cache and attention lines; retain JSON detail.
- [ ] Run focused tests and commit after passing.

### Task 3: Validate and deliver

- [ ] Run formatting, clippy, all-target tests and release build via nix develop; expect success.
- [ ] Independent whole-branch review; address material findings with regression tests.
- [ ] Record validation/deployment status; push planning changes only to origin/meta/superpowers-plans.

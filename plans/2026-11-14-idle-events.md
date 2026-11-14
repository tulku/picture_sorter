# Idle events Implementation Plan

> **For agentic workers:** Use superpowers:executing-plans or subagent-driven-development task by task.

**Goal:** Remove wasteful idle work and dead code while preserving photo safety and optional integrations.
**Architecture:** Poll descriptors and scheduled deadlines; persist verification stamps; query indexed pending work. Isolate optional integration state.
**Tech Stack:** Rust, libc poll/inotify, SQLite, Nix.
**Spec:** /home/tulku/Repos/picture_sorter/superpowers-plans/specs/2026-11-14-idle-events.md

## Global Constraints
- Planning and ledger stay on this orphan branch; code worktree /home/tulku/Repos/picture_sorter/idle-events.
- Preserve user data, journal compatibility, worker fencing and optional Immich/multi-computer behavior.
- User activates Nix; synthetic benchmark must not mutate production state or archive.

## Review Focus
- Overflow/restart/deletion/replacement must invalidate local verification appropriately.
- Partial socket and worker writes, exited workers and delayed replies must not spin or stall.
- Debounce/retry/remote-sync deadlines must progress even without new files.
- Cancelled parent cleanup must not release live worker reservations.
- Disabled integration must do no unnecessary work, enabled integration keeps its semantics.

### Task 1: Local verification evidence
- [ ] Add regression tests proving unchanged files are not rehashed; changed stamps, restart, explicit audit and watcher recovery remain safe.
- [ ] Implement cached verified stamps and metadata-only reconciliation in recovery/observation/catalog paths.
- [ ] Run focused tests and commit.

### Task 2: Pending work and lifecycle
- [ ] Add active-job query index/performance regression and cancelled-parent recovery regression; observe failures.
- [ ] Implement indexed active query and quiescence-bound abandoned child settlement.
- [ ] Run focused tests and commit.

### Task 3: Descriptor-driven coordinator
- [ ] Add real readiness/deadline and idle resource regression tests; observe failures.
- [ ] Implement wait module, worker descriptors, local watcher readiness and timers; replace fixed loop sleeps.
- [ ] Verify responsiveness under import/copy, idle socket requests, worker backpressure and retries.

### Task 4: Remove dead code and isolate optional integration
- [ ] Remove legacy sorter/exclusive modules/rayon/regex and test-only production abstractions; update meaningful tests and docs.
- [ ] Extract Immich orchestration/state out of coordinator, preserving tests and sync behavior.
- [ ] Run relevant tests and commit.

### Task 5: Review, measure and deliver
- [ ] Full cargo tests, fmt, strict clippy; independent whole-branch review.
- [ ] Repeat isolated idle measurement with representative catalogue and compare baseline; never run copied production jobs against real photo roots.
- [ ] Merge/push approved main workflow, update Nix pin/build/commit, hand off activation and post-switch measurement to user.

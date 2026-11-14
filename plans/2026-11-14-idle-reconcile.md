# Reconciliation follow-up Implementation Plan

> Use superpowers:executing-plans and TDD; independent observation query task can be delegated.

**Goal:** Eliminate measured reconciliation rereads without treating mismatches as good files.
**Spec:** /home/tulku/Repos/picture_sorter/superpowers-plans/specs/2026-11-14-idle-reconcile.md
**Architecture:** Cache stable failed audit stamps while keeping diagnostics; batch/filter remote-only sidecar checks.
**Tech Stack:** Rust, SQLite, Linux metadata.

## Global Constraints
Preserve protection/diagnostics; user activates Nix; no production mutations except separate catalogue backup.

## Review Focus
- Negative results never mark files ready or clear conflicts.
- Worker/current stamp race cannot cache stale outcome.
- File or expected digest changes invalidate evidence; explicit audits still hash.
- Present new sidecars and formerly observed deletion remain detected after overflow/restart.
- Large missing sidecar inventories do not drive thousands of full coordinator ticks.

### Task 1: Failed verification cache
- [x] Reproduce unchanged mismatch requeue in regression.
- [x] Carry stable observed stamps in integrity report; persist matching negative evidence while preserving failed readiness/diagnostics.
- [x] Verify restart, changed file, explicit audit and stale receipt cases.

### Task 2: Known sidecar filtering
- [x] Reproduce large remote-only inventory queued for individual skipping.
- [x] Filter known query to previously observed sidecars, preserving working discovery and missing local sidecars.
- [x] Test meaningful cases and query cost.

### Task 3: Deliver
- [x] Full suite/lint; independent review; representative multi-cycle benchmark including known mismatches and remote-only sidecars.
- [x] Push main, update/build/commit Nix pin; remove worktree; hand off switch.

### CI-discovered retry scheduling correction
- [x] Confirm that due imports were deferred without dispatch while the NAS reader held the worker.
- [x] Preserve genuine repeated-failure backoff using an explicit upload outcome.
- [x] Test busy/pending workers and identical errors; strengthen reconnect fixture with short archive polling and observed offline state.
- [x] Full validation and refreshed Nix handoff.

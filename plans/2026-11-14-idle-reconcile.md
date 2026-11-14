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
- [ ] Reproduce unchanged mismatch requeue in regression.
- [ ] Carry stable observed stamps in integrity report; persist matching negative evidence while preserving failed readiness/diagnostics.
- [ ] Verify restart, changed file, explicit audit and stale receipt cases.

### Task 2: Known sidecar filtering
- [ ] Reproduce large remote-only inventory queued for individual skipping.
- [ ] Filter known query to previously observed sidecars, preserving working discovery and missing local sidecars.
- [ ] Test meaningful cases and query cost.

### Task 3: Deliver
- [ ] Full suite/lint; independent review; representative multi-cycle benchmark including known mismatches and remote-only sidecars.
- [ ] Push main, update/build/commit Nix pin; remove worktree; hand off switch.

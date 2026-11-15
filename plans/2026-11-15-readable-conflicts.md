# Readable Conflicts Implementation Plan

> **For agentic workers:** Use superpowers:executing-plans.

**Goal:** Repair proven stale NAS edits safely and make conflict reports understandable.
**Architecture:** Choose existing committed successor plans from catalog ancestry; replay through the existing publication worker, then journal retirement of stale conflict records. Attach bounded presentation context to paginated conflict responses and render readable CLI output.
**Tech Stack:** Rust, SQLite, Nix.
**Spec:** /home/tulku/Repos/picture_sorter/superpowers-plans/specs/2026-11-15-readable-conflicts.md

## Constraints and review focus

Never overwrite unknown NAS content or bypass atomic exchange qualification. Do not infer safety from a single computer. Verify archive identity, current single head, ancestry and committed proof. Preserve generated-output restrictions and old JSON records. Do not resolve live data manually. Keep pagination bounded and escape terminal control characters.

### Task 1: Catch up committed projections
- [x] Add service regression for NAS restored to a known ancestor, and preserve existing unknown-content conflict regression; observe failure.
- [x] Implement bounded committed-successor lookup and one catch-up step per session in service/publication.rs and a focused catalog helper.
- [x] After fresh ancestry proof or confirmed latest projection, enqueue shared resolution only for proven stale reports; test unknown, divergent and preserved-race reports remain unresolved. Revalidate queued resolutions after replay before publishing.

### Task 2: Readable output
- [x] Add CLI regression for readable empty and populated lists; observe failure.
- [x] Add bounded context to ConflictsPage and a CLI formatter with filenames and reasons, retaining JSON.
- [x] Keep a fallback for missing context; test multiple records for one file and terminal control characters.

### Task 3: Validate and deliver
- [x] Independent review, fix material issues with regression tests.
- [x] Run formatting, clippy and all-target tests. Commit feature work and create a PR; do not deploy configuration ahead of package integration.
- [x] Record findings and results on the planning branch and push only origin/meta/superpowers-plans.

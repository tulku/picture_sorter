# Existing NAS adoption repair

> **For agentic workers:** Use superpowers:executing-plans inline; request one fresh review at the end.

**Goal:** Catalogue matching NAS originals in place after a cancelled import, with no photo copies.
**Architecture:** Bootstrap verifies file bytes through its existing inventory and publication checks, then emits a MemberAttached record for the existing group when its exact placement lacks archival evidence. Already archived placements remain idempotent; differing digests remain protected errors. No new conflict policy or relaxed qualification.
**Spec:** Existing approved NAS catalogue and SMB safe-import requirements; user clarified existing NAS photos should be catalogued, only absent new photos uploaded.
**Tech stack:** Rust, SQLite, native NAS worker, Nix.

- [x] Reproduce pending matching RAW being counted but not marked archived using native bootstrap; also test mismatched NAS bytes remain protected. Preserve group revision and local/NAS bytes.
- [x] Apply minimal bootstrap repair and run focused tests, full cargo suite, fmt and strict clippy.
- [x] Fresh review, integrate using prior main/push authorization and build Nix package.
- [ ] Deploy only after proving native publication worker quiescence; catalogue scoped existing RAW dependencies before full archive, without resuming cancelled imports. Keep per-photo receipts private.

Operational receipts: plans/2026-11-12-nas-catalogue-execution.md. No planning artifacts in implementation branches.

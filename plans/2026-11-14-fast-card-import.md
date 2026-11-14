# Fast card import implementation plan

Goal: make the native no-argument import fast and index-driven.
Spec: /home/tulku/Repos/picture_sorter/superpowers-plans/specs/2026-11-14-fast-card-import-design.md
Code: /home/tulku/Repos/picture_sorter/fast-card-import (feat/fast-card-import, base 7c4ab08).

- [ ] Persistent source identity and per-file cached scan: separate inventory and fingerprint phases so catalogue lookups happen on coordinator thread; unchanged identified originals reuse digests/fingerprints; strict/unreliable sources read bytes. Expose Catalog::ensure_import_source(job,&Path)->AppResult<SourceIdentity> and Catalog::import_source_identity(job)->AppResult<SourceIdentity>. Supply an explicit source-history binding API with metadata validation. Tests: native repeat zero old hashed bytes, append only new bytes, unknown card/metadata changes, strict full reads.
- [ ] Canonical archived-content recognition before path planning: reuse exact digest or cached JPEG image match with existing canonical path, including flat versus burst differences; ambiguous matches conflict; no phantom placement or extra local copy. Wire the source-identity APIs into begin_scan and accept_hashed_batch. Tests: flat/burst exact RAW/JPEG reuse, metadata variants, ambiguity and changed-pixel preservation.
- [ ] Human CLI progress while preserving original service wait semantics: bounded status polls, phase/counts/reasons, heartbeat for long operations, no JSON pollution or no-wait changes. Tests: output before blocked job completes, correct final exits, JSON single response.
- [ ] Complete JPEG image catalogue and migration interface using native code, preserve schema compatibility/guards, apply a safe one-time binding for the existing card and populate missing image fingerprints. Root owns common schema/module integration, CLI adoption/index commands, and profile wrapper default.
- [ ] Integrate, run full suite/fmt/clippy and real read-only catalogue preview; independent final review; source merge/push and Nix pin/build as already authorized; remove implementation worktree after integration.

Parallel boundaries: source cache owns source_cache.rs/inventory.rs/metadata.rs/service/scan.rs/import/mod.rs; canonical reuse owns runner.rs/jpeg_duplicates.rs and focused tests; progress owns cli.rs/service/client.rs and progress tests. Root owns shared catalog schema/mod.rs, native image indexing/bootstrap, Nix, documentation and integration. Coordinate CLI migration additions after progress agent returns. Do not edit another owner's files without coordination.

Review focus: source identification across remount versus replacement; stale cached entries must not erase live edits; multiple canonical matches must not be guessed; old milestones/JSON/protocol remain correct; no NAS or card writes during recognition/indexing; first unknown source may require one full read, but current full scan should not be discarded.

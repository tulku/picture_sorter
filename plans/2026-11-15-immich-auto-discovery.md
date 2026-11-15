# Automatic Immich matching implementation plan

> Execute inline using superpowers:executing-plans; user opted out of approval rounds.

Goal: all exact library matches participate in Favorite sync without pins.
Spec: /home/tulku/Repos/picture_sorter/superpowers-plans/specs/2026-11-15-immich-auto-discovery.md
Architecture: bounded metadata search pages in existing HTTP worker, exact existing resolver, service polling cursor. Rust/reqwest/SQLite; no dependencies.

- [ ] Test first-sync Pick/Favorite union and Reject precedence; implement only for uninitialized Favorite baseline, retaining subsequent conflicts (tests/favorites_sync.rs, src/immich/favorites.rs).
- [ ] Add metadata pagination tests for request scope, bounds, errors, invalid next page and trash filtering; implement typed pages in src/immich/client.rs.
- [ ] Test exact discovery mapping excluding TIFF/unmatched/library mismatch without pin jobs; implement src/immich/discovery.rs using existing resolve_asset/bind_asset with existing mappings skipped when identical.
- [ ] Add asynchronous discovery tasks/cache to src/service/http.rs; integrate bounded cursor and retry in src/service/integration.rs, with progress included in scheduler comparison to avoid stalls. Avoid starving normal sync; do not restart an active traversal.
- [ ] Test service discovery across pages and API failure recovery; expose progress and correct qualification status in src/service/server.rs and CLI rendering.
- [ ] Run fmt, Clippy and full cargo test; independent review. Publish draft PR and build deployment only after tests/review. Do not modify live database with scripts.

Review focus: pagination stalls/repeated pages, trashed/missing assets, changed asset identities, multiple JPEG authority, first-sync flags versus post-baseline changes. Scope automatic discovery to configured library; retain unmatched files for later rescans. Code worktree /home/tulku/Repos/picture_sorter/immich-auto-discovery.

## Execution ledger

Implemented on 9698ce6. Initial union, no-empty-sidecar, queued Favorite against trashed asset, and service automatic discovery each reproduced failures before fixes. Mapping/API and Favorites focused suites pass (19 and 35 tests). Production qualification unit test verifies Favorite-only evidence cannot enable stars, separately qualified inbound stars remain available with outbound writes disabled, and an active poll cursor survives another timer tick. Service test covers 503 retry and pages 1,1,2 with no pin job. Full cargo test: 580 passed, 0 failed, 1 ignored manual resource measurement. Formatting and Clippy pass. Independent review's trash and rating-mode concerns resolved; final review has no material findings.

Ruling: require separate API rating qualification for star polling when adding whole-library discovery, so enabling Favorites does not silently initialize stars throughout the library. Preserve qualified inbound operation independently of outbound mode. Ruling: skip generating a neutral Favorite sidecar when no local version exists and remote is false; retain unknown/reset semantics otherwise.

Live read-only metadata search comparison: 14,517 assets returned, 14,500 exact archived JPEG paths, 17 skipped, 1,323 catalog JPEG paths not returned. Of those, 873 are CAMERA_ORIGINAL variants. All 19 captured native RapidRAW sidecar groups have a JPEG returned by Immich. Count is stable across page sizes 100 and 1000. No live flag changes, binding scripts, pins or downloads were performed. Python urllib request was denied (403); curl with private header stdin succeeded. The application uses its existing reqwest client.

Code published as PR #5. User subsequently authorized merge. CI run 43770373554 passed; PR merged as c2e767e2f0dc48ffea7833382da39837862b46c1. Main checkout fast-forwarded; Nix picture-sorter input updated to that main revision. Hash of all other flake lock nodes unchanged (e5fef3cd13db6b6ceadcd8ec5287505135d10b021dc9ce90051fba73d01bd06d). Full syl NixOS build succeeded against the merged revision; Nix release-mode tests also passed 580 with 1 manual measurement ignored. Package /nix/store/g90fxmd6n7sd0ixwr06vp0c5d3d07b1h-photo-sorter-0.1.0. Activation remains the user's configuration switch. Planning branch remains separate. Legacy catalog group association and unmatched variants are outside this exact-path discovery change.

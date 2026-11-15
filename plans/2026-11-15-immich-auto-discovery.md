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

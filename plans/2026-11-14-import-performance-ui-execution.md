# Import performance and terminal UI execution

Implementation: /home/tulku/Repos/picture_sorter/import-performance-ui, base 6ceca718311e4356d4f18a08b5989924d59d7fe1.

## Result

Cached recognition batches source bookkeeping and canonical receipts rather than synchronously committing each unchanged file. Unchanged source rows are not rewritten. Receipts and counters commit together; injected failure, replay and reopen tests preserve correctness.

Admission combines an initial/background recursive measurement with conservative per-filesystem allocation grants and current outstanding reservations. Every admission refreshes statvfs and SQLite db/WAL/shm sizes. Normal copies no longer invalidate the measurement. Real shortages and reclamation request reconciliation; failed or disappeared workers invalidate admission and respect retry/quiescence rules. Legacy epoch/ceiling triggers remain compatible with rollback. Inline content snapshots book one grant and re-admit unfinished replay safely.

Human terminal imports show three simultaneous progress rows and an activity line; nonterminal output is throttled and unchanged messages suppressed. Human status presents bounded current work, uploads and cache measurements, querying actual active jobs outside historical UUID pagination. Cancelled queues are excluded. Completion separates local copies and queued uploads, and does not recommend removing cards with skipped/conflicting files. JSON and non-import diagnostics remain available. Additive archive_reused/copy_ready report counters handle local-only recognition and archived companion reuse accurately.

## Evidence

Full mutating scan of the actual read-only 5396-file card against fresh private catalogue snapshots: baseline release 47.966s / 49.524s, changed release 5.184s / 4.966s. All runs selected5396, recognized5278, archive4336, image-duplicates942, pending118, hash0, transfer0, EXIF0, conflicts0. This is recognition time, not copy/upload time. Private roots replaced working/state/NAS; no real upload or catalogue modification was performed.

64-file cached batch on FULL SQLite WAL: 1.177s / 369 WAL frames / 64 unchanged source rewrites before; 46.8ms / 20 frames / 0 rewrites after. Complete benchmark notes follow below.

Independent review found and fixed reservation SQL arity, double inline grants, failed/disappeared worker stale proofs, misleading skipped-file removal advice, non-import progress rows, and local-only/reused-companion counter edges. Process-global hashing metrics included independent background audits; the service regression now checks per-import metrics and watches the actual old card files with inotify IN_ACCESS, proving no payload read. A deliberate read verifies the watch.

## Final integrated validation and release

- 498 native tests passed; strict cargo clippy --all-targets -- -D warnings and cargo fmt --all --check passed.
- The full suite caught a capacity-wait integration regression: scheduler decisions still recognized only the legacy pending message. The shared constant/helper now handles both old durable reasons and the new reconciliation reason across response waiting, retries and local progress. The existing regression reproduced the failure and passed after correction; independent final review approved it.
- Checked Nix package rebuilt successfully with all 498 tests, including actual terminal PTY rendering and physical card access detection. Installed executable help smoke check passed.
- Source integrated as squash commit 53317b3875b70854166db710ac65f5f20fb83d9b on main and pushed to origin/main.
- Nix configuration pin committed locally as 0e54872; only picture-sorter lock fields changed. Complete syl system build passed, with activation left to the user.
- Package: /nix/store/7mmfxnbh7vcqryi27hlkj3wji91x1yf6-photo-sorter-0.1.0
- System: /nix/store/bsc6w0dsh3s3gf871ndw733nm4i8h6rn-nixos-system-syl-26.05.20261108.ac588ef
- GC roots: /home/tulku/.local/state/photo-sorter-builds/import-performance-ui and import-performance-ui-system.
- Exact implementation/main tree comparison passed before deleting the implementation worktree and branch; main and Nix checkouts clean afterward. Stable orphan planning worktree retained.
- Source CI: https://github.com/tulku/picture_sorter/actions/runs/43588371333 — success: format, strict lint, tests and release binaries.


Measured 2026-11-14; recorded from completed native cargo-test tool output.

64-file isolated bookkeeping regression (debug, FULL synchronous WAL):
  test: cached_live_scan_batches_durable_receipts_without_rewriting_sources
  before: elapsed=1.177029718s WAL_frames=369 source_rewrites=64 (expected failing regression)
  after:  elapsed=46.79814ms WAL_frames=20 source_rewrites=0 (pass)
  command: nix develop /home/tulku/Repos/picture_sorter --command bash -c 'export CARGO_TARGET_DIR=/home/tulku/.cache/picture-sorter/fast-source-target; cargo test --test fast_canonical_reuse cached_live_scan -- --nocapture'

Complete mutating 5396-file card scan (release):
  main baseline HEAD 6ceca718311e4356d4f18a08b5989924d59d7fe1: 47.966205671s, 49.524053018s
  implementation worktree candidate: 5.184097794s, 4.966016107s
  candidate debug supplementary run: 9.092174598s
  identical reports in all valid runs:
  selected=5396 skipped=0 recognized=5278 local_ready=0 archive_committed=4336 image_duplicates=942 pending_archive=118 waiting=118 conflicting=0 association_warnings=0 exif_invocations=0 transfer_bytes=0 hash_bytes=0 scan_complete=true pending_jpeg_checks=0

Fixture/procedure:
  A temporary integration test was copied identically to the implementation and main tests directories, then removed from both after benchmarking.
  Each invocation created a fresh tempfile state/working/empty offline archive, opened preview source catalogue read-only, and used rusqlite backup to clone it into private state.
  Preview source: /home/tulku/.config/photo-sorter/maintenance/catalogue-20261112/fast-card-preview/state/catalog.sqlite
  Config loaded from preview profile, with state_root/working_root/archive_root replaced by private temp directories.
  Card source: /run/media/tulku/OM SYSTEM
  Timed ImportCoordinator::prepare with strict=false, dry_run=false and new operation ID through scan completion. No uploader or local-copy driver called.
  The real service catalogue and NAS were never opened for writes; no service interruption.
  Command per checkout: nix develop /home/tulku/Repos/picture_sorter --command bash -c 'export CARGO_TARGET_DIR=/home/tulku/Repos/picture_sorter/target; cargo test --release --test live_scan_benchmark_temp -- --nocapture' (twice).
  Before switching candidate to main, cargo clean -p photo_sorter --release forced a main crate rebuild; dependencies retained. An earlier accidentally reused candidate artifact measured from main was discarded.
  Both series used warm card OS metadata cache and fresh SQLite backups. Main pair measures combined bookkeeping/reservation/admission difference; isolated 64-file regression measures bookkeeping only.

# Import capacity performance execution

- [x] Confirm full SD scan complete for import `2d66cf07-26ef-42a5-9bf1-44898c00fdc9`: 5,396 files, 86,085,829,987 hashed bytes. Whole-card import later cancelled before upload to prevent duplicating existing files under new burst paths; scan records are retained.
- [x] Diagnose expensive group lookups without group indexes. Minimum 500-group lookup sample on live read-only DB: 2.171 seconds; full candidate list contains 30,114 groups.
- [x] Reproduce NAS-only eviction scaling in regression test: 5,000 archive-only groups took 5.876 seconds before the fix.
- [x] Add idempotent `members(group_id,path)` and `placements(group_id,local_ready)` indexes on catalog open and prefilter eviction candidates to locally ready placements. Existing eligibility and unlink proofs remain required.
- [x] Confirm pending measurement test already passes before modifying production behavior; omit an unnecessary admission change.
- [x] Benchmark 500 group lookups in an in-memory copy of the completed catalogue: 2.3533 seconds before indexes, 0.0034 after (684.8x). This measures those lookups, not end-to-end import speed.
- [x] Run formatting, strict Clippy, and all 432 tests successfully. Independent reviewer found no Important/Critical blockers.
- [x] Fast-forward and push source main `7c4ab08ce78abd248d2c01739cd79b4fb170e335`.
- [x] Update `/home/tulku/nixos/flake.lock` to that revision; no other input changed.
- [x] Preserve fresh SQLite snapshot with full scan complete and quick_check=ok: `/home/tulku/.local/state/photo-sorter-backups/before-performance-fix-20261113T235201Z.sqlite` (235,192,320 bytes).
- [x] Complete checked Nix package and syl system builds (retry passed all package checks).
- [x] Commit updated Nix lock (`8536588`), remove merged code worktree/branch, and prepare activation command.
- [ ] Activate on syl and verify indexed catalogue lookups. Use the existing new-only 118-file selection for this import.

Code worktree: `/home/tulku/Repos/picture_sorter/import-capacity-performance`, branch `fix/import-capacity-performance`; remove after builds complete. Native package result: `/home/tulku/.local/state/photo-sorter-builds/import-capacity-performance`; system result: `/home/tulku/.local/state/photo-sorter-builds/import-capacity-performance-system`.

GitHub CI passed: https://github.com/tulku/picture_sorter/actions/runs/43530156117.

The original periodic/live CLI progress-style question remains unanswered. Progress output is not part of this performance fix.

## Additional findings and precautions

- Whole-card pending items: 1,016 ordinary originals plus 875 pending JPEG comparisons; 898 ordinary pending originals have an already catalogued exact NAS digest but planned burst paths differ from existing flat paths. Example P9039077 JPG/ORF exists directly under the dated directory but is newly planned under P9039077_BURST. No transfers had happened (transfer_bytes=0). Native cancellation confirmed Cancelled.
- Use `/home/tulku/.local/share/photo-sorter-selections/new-20261112` (118 originals) to import new photos without republishing these existing-path variants. Canonical reuse across these layout differences remains separate follow-up work; bare whole-card import is not ready for this existing card/layout.
- First Nix package build failed existing `corrected_local_integrity_finding_recovers_without_archive_audit_clearing_it` test after its cleanup left a file. Full development suite and GitHub CI passed; three isolated development reruns also passed. The system build retried the identical checked package.
- Stopped the configured service, proved original daemon/workers dead and NAS publication lock absent, then ran native offline backup with the tested debug binary. This path copies SQLite directly and does NOT initialize catalogue indexes; no live performance claim should be made from that backup. Configured old package was restarted normally (PID 1570233). Backup `/home/tulku/.local/state/photo-sorter-backups/after-performance-indexes-20261114.sqlite` is valid (235,196,416 bytes, quick_check=ok), despite its inaccurate name; indexes remain absent until the new daemon opens the catalogue.

## Ready for activation

Package: `/nix/store/yxym3dgn10vngfhxsv91nqp6hx4n205x-photo-sorter-0.1.0`.
System: `/nix/store/wdnsnk2j2fygga1as8nyy00bnrkczwda-nixos-system-syl-26.05.20261108.ac588ef`.
Source and Nix checkouts clean; code worktree removed. Planning branch alone carries this ledger. The fixed package is not yet activated; configured runtime still uses rjrwi3 package.

Activate: `nh os switch --flake /home/tulku/nixos#syl`.
For this import use: `photo_sorter import /home/tulku/.local/share/photo-sorter-selections/new-20261112 --wait-for archived`.
After activation verify running yxym3 executable, both indexes present, capacity measurement/progress, and no unowned publication lock. New-only selection verified 118 files / 2,065,353,069 bytes.

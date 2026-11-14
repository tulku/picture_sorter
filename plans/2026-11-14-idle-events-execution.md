# Execution ledger — plan: /home/tulku/Repos/picture_sorter/superpowers-plans/plans/2026-11-14-idle-events.md

Baseline main 36e7a50, 555 tests previously passed. Live baseline 49.5% core, 26MiB RSS, 14–16GiB reclaimable cache; 61MiB job scan, 2.4TiB session hashes.
Pre-flight: task 1 owns recovery/observation stamps, task 2 jobs/schema lifecycle, task 3 server wait/workers, task 4 integration extraction and dead APIs. Coordinate shared files.

Task 1: complete aa7bb1b; cache RED unchanged queued1 expected0, IN_ATTRIB RED not readable; GREEN library92 + relevant integrations36; parent watcher fd and overflow latch included.
Task 2: complete 9e20787; pending query RED320044 VM steps, GREEN<100 with20k historical jobs; startup child recovery and persistent migration tests GREEN; service_recovery15 pass.
Task 4: complete a508586 plus shared integration wiring in root commit; removed legacy modules/deps, retained optional Immich semantics; focused98 tests + actual retry reopen pass.
Task 3: implemented; baseline idle regression RED152 wakes/800ms, GREEN blocking/socket responsiveness. Isolated20k history benchmark 30s:0.067% core,81.07MiB aggregate debug RSS. Production remains unchanged.
Final review findings: zero debounce idle spin reproduced79 ticks/800ms; fixed periodic boundary while immediate file debounce remains. Missing operational deadlines reproduced30s vs2s; fixed durable job/publication retries, startup/archive/space retry and worker timeout minimum.
Full-suite first pass: import_flow::evicted_known_original_not_reimported failed archive_committed0 after bounded polling under concurrent builds; isolated reproduction passed1.42s. No test deadline changed; final full suite underway.

Task 3: complete 0d5028a. Both review regressions RED→GREEN; full native suite550 passed,1 manual measurement ignored in normal suite and passed explicitly. Strict clippy/format pass. Latest debug benchmark0.100% core80.85MiB RSS; packaged release benchmark20k history30s0.067% core,43.32MiB aggregate RSS stable across6 samples.
Task 5: complete. Main0d5028a02ffc28fbdbf44e221e28e89250968b7a fast-forwarded/pushed; CI43647428475 passed including release binaries. Nix checked package550 tests passed46 suites, same manual benchmark ignored; package /nix/store/6gzagi5632jbq3738w4lg0v46g1bkwiz-photo-sorter-0.1.0. Syl system /nix/store/82xn6gg2hwwrgihdj7nnp2scz6bxv1df-nixos-system-syl-26.05.20261108.ac588ef built. Nix main commit e6d99f3, clean, not activated. GC roots ~/.local/state/photo-sorter-builds/idle-events and idle-events-system. Implementation worktree/branch removed after exact tree equality with main.
Production backup ~/.local/state/photo-sorter-maintenance/idle-events-20261114/before-idle-events.sqlite mode0600,268MiB,quick_check=ok. No production photos changed. Actual live post-upgrade measurement awaits user activation; pre-upgrade service remains running. First activation populates local verification cache once; existing SD/NAS indexes preserved.
Final review: no deferred findings; zero debounce and operational deadlines fixed. No product-scope rulings beyond approved fallback metadata reconciliation and retained remote journal checks.

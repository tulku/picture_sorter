# Implementation plan

Spec: /home/tulku/Repos/picture_sorter/superpowers-plans/specs/2026-11-14-import-performance-ui.md
Code: /home/tulku/Repos/picture_sorter/import-performance-ui

1. Replace asynchronous admission expiry and reservation invalidation with conservative allocation accounting, initial/background reconciliation and fresh statvfs checks. Add quota, concurrent-allocation and reuse regressions.
2. Batch source and canonical recognition writes per scan batch; unchanged source metadata must not create writes. Benchmark actual mutating cached recognition, preserving idempotence.
3. Implement simultaneous terminal progress, concise nonterminal output, human completion and bounded accurate status; preserve --json and original wait/exit semantics.
4. Review integration, run full tests/fmt/clippy and package/system builds, merge/push main, pin/commit Nix, and remove implementation worktree. Do not activate Nix or interrupt ongoing uploads.

Parallel owners: space_accounting owns admission/catalog accounting schema/space meter; scan_bookkeeping owns import runner/metadata/duplicate bookkeeping; terminal_ui owns CLI and service status. Root owns integration, independent safety review, documentation, Nix and release.

Completed: all implementation, review, native/package/CI checks, source push, syl configuration build/pin commit, and temporary code worktree cleanup. Activation remains with the user. See /home/tulku/Repos/picture_sorter/superpowers-plans/plans/2026-11-14-import-performance-ui-execution.md.

# Execution ledger

Implementation worktree: /home/tulku/Repos/picture_sorter/automatic-backup-status
Branch: fix/automatic-backup-status
Base: 4590c97
User explicitly opted out of design/plan approval rounds and authorized direct implementation.

Baseline: nix develop --command cargo test --lib --quiet — 100 passed.

Task 1: Automatic writable-store checks implemented for configured single-writer scratch roots. Reads remain passive, failures back off, existing ownership/mode restrictions remain enforced. Initial renewal/isolation tests failed before implementation and passed afterward.

Task 2: Added global deduplicated local/archived counts, pending copies/edits/conflicts, concise status and conservative cache availability. Initial three backup regression tests failed before implementation and passed afterward. Retained safe card-removal indication while removing the claim that blocked uploads are continuing.

Final review: independent reviewer /root/review_backup_changes inspected the complete implementation and returned three material findings, with no additional blockers. No minor findings deferred.

Final: fixed manual reports overriding fresh automatic evidence and copying automatic evidence into the manual registry — automatic_evidence_is_not_copied_by_other_qualification_and_recovers_manual_failure RED→GREEN.

Final: fixed consumed reservations disappearing from reported cache usage — backup_cache_keeps_consumed_reservations_charged_between_measurements RED→GREEN; status now uses existing admission accounting.

Deployment preparation: added nas_qualification_scratch to /home/tulku/nixos/home/tulku/photo-sorter.nix in place as that repository's AGENTS.md requires. Other staged Nix changes were left untouched. Nix expression parse passed. Running service and installed Nix package have not been changed.

Final validation: nix develop --command bash -c 'cargo fmt --all --check && cargo clippy --all-targets -- -D warnings && cargo test --all-targets --quiet && cargo build --release --bins' exited 0 on the final code. All 568 tests passed across 46 targets. Final output retained at /tmp/photo-sorter-backup-status-final.log. Nix configuration parse and git diff --check passed.

NAS manual evidence updates the current slot before publishing the manual registry, so process interruption cannot leave an older successful automatic result overriding a newly published failed manual result. No boot identity or filesystem guarantees were removed.

Implementation commit d834b4833d839e88923122390202cd796b264837 was submitted as PR #1. CI passed. The user explicitly approved merging and deployment; PR #1 was squash-merged as d13b408ed1e1faad125c7071f9a8f5159dd85658. The main checkout was fast-forwarded and only the picture-sorter Nix input was updated to that revision. The Nix scratch setting must be activated together with the updated package, not independently against the old package.

The user's first configuration switch activated the new setting against the old package and caused profile rejection. Service availability was temporarily restored by removing the unsupported live setting and disabling the installed profile-rewrite helper with a runtime-only compatibility.conf override. The user asked to avoid compatibility workarounds; the override has been removed, daemon-reload completed, and systemctl reports active/running with empty DropInPaths. The normal nh os switch build succeeded, including the Nix package checks. Activation stopped because sudo requires an interactive terminal/password. The user must run nh os switch /home/tulku/nixos --hostname syl to activate the already-built matching package and profile. No temporary service override remains. Build/activation output is /tmp/photo-sorter-configuration-switch.log.

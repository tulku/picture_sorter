# Generated output sync execution

Scope: automatic generated editing outputs and adjacent metadata on syl. Immich setup deferred explicitly. Activation remains with user.

Implemented explicit generated saved-file ownership, coherent version capture, persistent stamp/debounce, camera protection, independent groups, native exact-filename orphan association, latest-path NAS projection and retained history. Camera RAW formats excluded from generated discovery; ordinary imports unchanged. Generated files remain excluded from eviction and local deletion retains NAS saves.

Reviewer identified direct writes of deterministic owner/prepared markers as a recovery gap. Fixed with unique private staging and no-replace publication; strict mismatch rejection remains. Regression failed before fix and passed afterward, including a truncated abandoned private marker. Separate generated fingerprint bumped to v2. Whole-branch review and follow-up marker/RAW reviews report no remaining blockers.

Performance regression demonstrated linear case-insensitive camera-ownership misses against 10,000 members. Added matching NOCASE catalogue indexes; bounded SQLite VM-work regression now passes. Existing clean/import integration exposed unregistered camera RAW misclassification; excluded RAW formats and verified existing regression passes.

Real SMB qualification of final protocol passed all ten checks: first claim, owner/prepared marker interruption, partial payload, replacement, staged/retained/published recovery, foreign target and retained collision. Evidence: /home/tulku/.local/share/photo-sorter/.photo-sorter/capabilities/2d1fec50896da24e712121e2ef59f5c44e685475990512fdf4f5ab31096a8042.json. Probe confined to fresh private qualification scratch; production photo paths untouched. Existing base NAS capability fingerprint unchanged.

Final full Cargo suite: 535 passed across45 suites; cargo fmt --all --check and strict all-target Clippy passed. Checked Nix package repeats535 tests successfully: /nix/store/l25ax4hcg5ggg4fq9nvgl0mjxnghbsha-photo-sorter-0.1.0. Package derivation /nix/store/agaasx73kv72ilki21ar2yj4sgda63xk-photo-sorter-0.1.0.drv.

Squashed reviewed implementation into source main14683cefd6754f8f9d56b911a84a7f173679d96a and pushed. Verified implementation/main file trees match before removing clean /home/tulku/Repos/picture_sorter/sync-generated-files worktree and branch. Planning worktree retained. GitHub CI run43629371236 passed formatting, strict lint, full tests and release binaries: https://github.com/tulku/picture_sorter/actions/runs/43629371236.

Updated only picture-sorter lock input in /home/tulku/nixos. Syl system build passed: /nix/store/hhq80flvnlss75pvf7r9azikbxzklz86-nixos-system-syl-26.05.20261108.ac588ef. Local Nix commit10fcc3a. Configuration not activated; Nix repository not pushed. Two live HDR TIFFs remain local-only until user activates new service. Adjacent archived orphan warnings should resolve automatically after their generated base uploads.

# Fresh sol agent handoff

Paste the following instruction into a fresh agent opened at `/home/tulku/Repos/picture_sorter`. Keep the repository's AGENTS instructions available to that session.

```text
Implement the complete offline photo archive design using these documents:

Spec: /home/tulku/Repos/picture_sorter/superpowers-plans/specs/2026-10-25-offline-photo-archive-design.md
Plan: /home/tulku/Repos/picture_sorter/superpowers-plans/plans/2026-10-25-offline-photo-archive-implementation.md
Review record: /home/tulku/Repos/picture_sorter/superpowers-plans/specs/2026-10-25-offline-photo-archive-review.md

I approve implementing this specification and its section-25 defaults. Use native continuous execution via superpowers:executing-plans. Complete all 23 tasks, including tests, actual CLI/service/adapter wiring, documentation, commits, and the final independent code review. Do not stop between tasks for confirmation or treat a milestone as the end of the task. Make routine implementation decisions yourself, recording material rulings in the execution ledger. Resume from that ledger after compaction.

Use branch feat/offline-photo-archive in exactly /home/tulku/Repos/picture_sorter/offline-photo-archive. State/verify the path before creating the worktree, or safely resume the existing matching branch/worktree. Preserve /home/tulku/Repos/picture_sorter and its untracked fill_gaps.sh. Never commit specs/plans/ledger to the implementation branch; they belong only on meta/superpowers-plans.

Implement all features and run isolated automated tests without touching my personal photos, NAS archive, Immich ratings, Syncthing settings, or installed services. Missing deployed credentials/apps/a second NAS client must not stop unrelated implementation or become stubbed-out features: ship real adapters, executable qualification probes, and disabled unverified capabilities. Report unrun deployment tests honestly. If a genuine safety or permission blocker remains, finish independent work and explain it; never weaken preservation guarantees to continue.

Leave a clean, committed implementation branch with a final report of test results, review findings addressed, and remaining deployment qualification. Do not merge, push the code branch, install/start the production service, or migrate my archive during this run.
```

The run can complete implementation unattended. Qualifying the real mounted share, testing installed RapidRAW reopen behavior, and authorizing an isolated Immich test asset remain deployment steps. The plan deliberately does not equate a successful implementation/test run with production deployment.

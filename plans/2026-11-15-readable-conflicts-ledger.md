# Readable conflicts execution ledger

Implementation worktree: `/home/tulku/Repos/picture_sorter/readable-conflicts`, branch `fix/readable-conflicts`, base `d13b408`.

Read-only diagnosis found three NAS projection reports for two RapidRAW sidecars. Their observed bodies matched archived ancestors of the sole current heads. Original uploads continued despite the shared publisher's edit warning; a later live status showed 1200/1989 backed up, compared with the user's 1044/1989. The mount's atomic exchange check fails with EINVAL, so existing live edit replacement cannot safely proceed. No live conflicts were manually resolved.

Added committed-successor replay, bounded stale-report journal batches, readable grouped conflict output and separate pending-edit status. Existing JSON conflict records remain available. No compatibility/configuration changes or filesystem safety exceptions.

Regression failures observed before implementation for known-ancestor catch-up, default CLI output and misleading global upload pause. Catch-up and CLI regressions subsequently passed. Unit safeguards retain unknown bytes, off-branch reports and preserved races.

Independent review found a stale-resolution race across publication sessions. Fixed by revalidating queued resolutions after journal replay and before plan creation/reuse. The regression simulates another commit resolving the report, then replays the filtered queued batch. Follow-up review found no further material issues.

Formatting and Clippy passed. Final all-target suite passed: 573 tests, zero failures. An earlier concurrent run hit a database-lock failure in the existing local verification test; the final full run passed all 42 service-flow tests, including that test. Logs: `/tmp/readable-conflicts-tests-final.log`, `/tmp/readable-conflicts-clippy.log`. Delivery is a draft PR; installation remains unchanged.

Code commit: `db3bb05`. Draft PR: https://github.com/tulku/picture_sorter/pull/2. CI push trigger is restricted to main; planning pushes do not trigger it.

# Execution ledger — idle-reconcile
Base0d5028a. Live steady RAM51.5MiB; quietCPU0.6–0.9%, but first300s reconciliation hashes10.3GB again and CPU sustained~100%. 1043 successful cache entries unchanged;893 uncached JPG Corrupt findings.

Implemented stable negative verification receipts and remembered stamps, without changing readiness or findings. Red regression confirmed unchanged failures queued again before the fix. Six recovery tests now pass, including restart, expected identity changes, explicit audit and stale successful/failed receipts.

Known-sidecar query now selects only local evidence, and queue overflow requests inventory discovery. Eleven observation tests pass; 12,000 unreadable-only remote sidecars require one advance (~22ms, 324k SQLite VM steps), with stable progress thereafter.

Representative integration regression uses 32 mismatching MiB, 12,000 remote-only sidecars and a two-second reconciliation interval. Three cycles retain all findings/protection with zero repeated hash bytes. Debug aggregate five-process CPU ~3.9% under reconciliation 150 times more frequent than production.

Independent review by reconcile_review found no critical or important issues in the full branch diff. Full lint passed; full tests/build pending.

Release representative benchmark: 1.057% aggregate CPU (one core),43.65MiB aggregate RSS,6.6s at2s reconciliation; zero repeated hash bytes. This synthetic cadence is150x production and is not a live post-switch measurement. Full native suite556passed,1manual benchmark ignored,46test targets. Final fmt/clippy pass. Source merged/pushed7488aa587c9f0dccab57d750e45a9458135f3a86; clean code worktree removed. Nix pin update only; package/system build andCI43649865005pending.

Nix package full checks and syl system build passed for7488aa5. GitHub43649865005failed existing offline_import_edit_restart_reconnect_archive_with_actual_immich_pin: test20s convergence deadline conflicts with default60s archive retry after startup fails before archive restore. Status archive reasonENOENT,journal_reads0,last_successful_syncNone. Three isolated native reruns passed; independent investigation confirmed retry timing and existing similar tests explicitly configure poll_seconds=1. Follow-up test-only worktree /home/tulku/Repos/picture_sorter/reconnect-fixture created to exercise offline read before reconnect and align fixture retry interval, retaining production defaults and existing deadline.

Fixture poll_seconds=1 plus explicit offline read still failed: reader online with last_successful_sync, uploadWaitingNas staleENOENT andtransfer_bytes0. Review found server tick defers any due waiting job even when the reader prevents it from attempting work; exponential delay is increased without a failed attempt. Added retry diagnostics to test; delegated server-only TDD correction to reconcile_sidecars, to defer only actual waiting outcomes and retain backoff for genuine failures. This scheduling correction is necessary to complete reliable event-driven work. No production changes applied.

Focused retry diagnostic reproduces failure: job_retry.failures5,next_attempt1794697145; archive last_successful_sync1794697134,worker.availabletrue,transfer_bytes0. /tmp/photo-reconnect-retries.log. Approved narrow explicit upload failure outcome to avoid counting blocked dispatch as a failure; preserve non-import behavior.

Retry correction focused Runtime tests red then green: busy worker/pending reply keeps due unchanged; identical genuine failures increment backoff twice. Added crate-private UploadAdvance outcome while preserving public API; only unchanged Import WaitingNas bypasses tail defer without new failure/error. Independent review found no important/critical issues. Full clippy passed; full native suite underway.

Retry fix480f973 and reconnect regression4590c97 committed. Original red integration nowpasses9.72s. Agent lib run99pass/1transientETXTBSY launching temporary fake-worker script; isolated rerunpasses. Root full suite unit target passes100/100 and continues. Final review approved.

Final native suite558passed,1manual benchmark ignored across46targets; fmt/clippy pass. Source4590c97d0bc6709f1d0dff1cd290f56090b52d30merged/pushed. CI43651781307and final syl Nix build started. Native integration catches the retry scenario fixed; no extended timeouts.

Delivery complete: GitHubCI43651781307passed (format,lint,alltests,release build). Final Nix package+full checks and syl system build passed. Nix local commit1e107ff pins4590c97; no Nix push or activation. Built system /nix/store/b2k37qlr3bhg54mr4c8z3w4si2k5b243-nixos-system-syl-26.05.20261108.ac588ef rooted at ~/.local/state/photo-sorter-builds/idle-reconcile-system. Both code worktrees removed cleanly after merge; source main clean. User handoff: switch again, then measure live CPU/memory through initial negative-cache population and at least one subsequent300s reconciliation. Live follow-up remains pending user activation.

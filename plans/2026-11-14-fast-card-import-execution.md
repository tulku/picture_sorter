# Fast card import execution

User explicitly rejected temporary-directory and repeated full-card workarounds and requested the native insert-card/run-command workflow. Implement the specified behavior continuously using the existing merge/push/deploy pattern. No additional UI preference question is necessary; periodic progress is the small default.

Pre-flight: source-cache agent supplies Catalog::ensure_import_source and import_source_identity; canonical-recognition agent consumes these in runner.rs. Root wires the module/schema before integration. Progress stays independent and retains existing service wait semantics.

Ruling: normal mode trusts persistent source identity and unchanged file metadata; strict mode reads full bytes. This is the explicit fast-path tradeoff stated to the user. Unknown source identities do not silently inherit another card's cache. Current card history requires explicit one-time binding, checked against the completed scan.

- Initial deployed package yxym3 verified active; source main 7c4ab08, Nix main 8536588. Existing full-card import 2d66cf07-26ef-42a5-9bf1-44898c00fdc9 is Cancelled, scan complete (5396 files), operation/source cache incarnation 8189314b-9be6-4089-9077-b33297ea11c6. Existing NAS catalogue Complete, 37136 supported files.

## Implementation and validation to date

- Source-cache agent: persistent physical filesystem/card identity plus unchanged path/size/mtime digest cache; separate worker inventory→fingerprint stages; strict bypass, companions re-read; source swap rechecked each batch; EXIF reuses verified supplied digests. Native source-history adoption validates original completed scan and all historical metadata. Focused source tests7 pass.
- Canonical agent: catalogue-first exact/image reuse before sequence planning; retired CAMERA_ORIGINAL plans prefer cached canonical NAS metadata; strict bypasses cached NAS inspection; ambiguity remains conflict unless an exact verified ordinary dated flat destination itself disambiguates identical existing sequence copies. Focused canonical10 and JPEG13 pass before final query optimization.
- CLI agent: periodic human stderr progress with original JobWait completion semantics and JSON/no-wait/dry-run unchanged; prints source immediately before waiting for service acceptance; nonblocking optional Unix connect rejects backlog saturation instead of blocking foreground completion. CLI17 tests pass.
- Background source fixes: startup physical checks only expected local placements; missing never-local sidecars do not allocate snapshots/reservations; known observed deletions remain tracked; local original events queue bounded verification for newly present expected paths. Source/background targeted regressions pass. Remote-only sidecar metadata enumeration remains bounded over many ticks but no longer churns admission epochs.
- Native maintenance root: offline guarded source adoption, missing NAS image indexing, and actual recognition preview. Future bootstrap computes full and image fingerprints in the same read. Tests3 pass.
- Nix wrapper: bare photo_sorter defaults SD source and ingested milestone; option-only --wait-for archived/--strict/etc default the same card; explicit commands/help/version preserved.

## One-time migration completed

Stopped configured service, proved main1657251/workers1657477..80 gone and NAS publication lock absent; valid snapshot `/home/tulku/.local/state/photo-sorter-backups/before-fast-card-cache-20261114T085303Z.sqlite` quick_check=ok. Native `catalog adopt-source` bound all5396 existing fingerprints to the present exFAT UUID/CID root. Native `catalog index-jpegs --scan 2d66cf07-26ef-42a5-9bf1-44898c00fdc9 --offline` indexed875 missing JPEGs,10,198,717,895 bytes, no NAS writes. Configured original yxym3 package restored normally, PID1716040 at last check. No new imports started.

Private real preview profile/catalogue: `/home/tulku/.config/photo-sorter/maintenance/catalogue-20261112/fast-card-preview/`; catalogue is a consistent production snapshot, working path is separate private directory. This native preview touches only clone and card metadata, not production or NAS. Current complete result: selected5396,cached5396,uncached0,exact4336,image942,new118,newbytes2065353069,ambiguous0,hash_bytes0,transfer_bytes0.

First correct real preview took74.488s debug and had581 ambiguous exact flat+HDR duplicates. Narrow verified-flat preference resolved those without moving/deleting/merging anything. Next preview after repeated mount classification optimization took71.149s and counts above; process still read~37GB catalogue pages. Confirmed remaining root cause: SQLite image-match JOIN starts from members(archive,path) for ORDER BY, scans archive per fingerprint. One query0.1139s; canonical agent implementing fingerprint-first CROSS JOIN and deterministic VM-step-budget regression. Must repeat real preview after that fix before claiming fast.

## Independent review and fixes

- Important: optional progress connect could block before read timeout. Fixed nonblocking AF_UNIX connect, real saturated-backlog RED→GREEN.
- Important: CID-only hardware identity survives filesystem reformat. Fixed no UUID/generation→unreliable/full hash; changed UUID misses prior cache. Real migration compatibility found extra trailing delimiter in final empty FAT directory discriminator; restored original key, persisted-key regression RED→GREEN.
- Network-root performance safety review: do not remove mountinfo pre-open rejection, since replacement hard NFS could block before identity comparison. Source agent retained fresh exact snapshot reads and cached parsed root classifications, bounded64 entries; snapshot changes invalidate all decisions, network wins at equal longest prefix depth. Root/ancestor network remount tests2 +filesystem6 pass.
- Existing local integrity-recovery test repeatedly exposed stale physical_findings after corrected explicit verification. Review agent found LocalAudit never removed prior physical_findings; new success acknowledgment retains exact targets, rechecks current SnapshotStamp, clears matching path/digest/size findings/startup checks, and refreshes protection. Deterministic regression RED→GREEN; local verify2, original recovery1, cached-pin1 pass targeted. Earlier parallel service suite had transient cached-pin readiness failure; final integrated suite must be green.
- Deferred minor: StartupVerifier can absorb a changed-file event into an already active check then acknowledge an older result; periodic reconciliation catches it. No byte deletion uses that proof without separate fresh verification. Explicit LocalAudit acknowledgment fix above does not add a general event generation table.

Ruling: for multiple identical archived copies, use only a verified existing ordinary dated flat path derived from source metadata/name to disambiguate; preserve every other copy/group. Arbitrary unrelated duplicate paths still require review.
Ruling: normal mode trusts the recorded canonical archive fingerprints (as existing RAW catalogue recognition already does); strict JPEG comparison reopens/verifies NAS. Future bootstrap stores image fingerprints up front.

## Remaining work

- [ ] Canonical fingerprint-first query budget regression and fresh real-card preview must demonstrate fast measured path.
- [ ] Final fmt/clippy/full suite (root current session5784, log `/tmp/picture-sorter-fast-integrated-tests.log`; previous snapshots failed early progress then existing physical recovery, both fixed).
- [ ] Commit feature, merge/push source main (already authorized), update Nix lock and build checked package/full syl system; commit Nix changes.
- [ ] Remove merged feature worktree and push planning ledger only to meta branch.
- [ ] Report ready for user activation; user explicitly will switch Nix config, do NOT activate for them.

All code work remains `/home/tulku/Repos/picture_sorter/fast-card-import`, feat/fast-card-import, base7c4ab08. Native build target `/home/tulku/.cache/picture-sorter/fast-integration-target`. Other agent targets fast-source-target,fast-canonical-target,fast-cli-target,fast-review-fix-target. Current root service/API test additions and docs are settled except final query optimization; reviewers/agents have not committed.

## Measured fast path achieved

- Root integrated full suite passed468 tests with fmt/strict Clippy before the final lookup optimization (log file may be replaced by final run).
- Canonical lookup VM-op regression with10k unrelated archived JPEGs observed RED (query interrupted under20k instruction budget). Removing irrelevant ORDER BY from exact/image candidate queries and enforcing fingerprint-first CROSS JOIN prevents SQLite choosing archive/path index scans just to sort. Focused canonical10/JPEG13 and budget1 tests pass.
- Native real-card preview after those changes:1.199 seconds in debug build; selected5396,cached5396,uncached0,exact4336,image942,new118,newbytes2065353069,ambiguous0,hash_bytes0,transfer_bytes0. Result saved in private fast-card-preview/result.json. This is recognition time, not copy/upload time.
- Last explicit check in progress: ensure ingested wait/local-copy scheduling remain independent of NAS failure/backoff during multi-file copies. CLI agent owns server.rs and adds focused service_flow regression; no other edits planned.

- Final NAS independence tests reproduced premature local exit after1/8 files and retained600-second NAS retry stalling a fresh capacity probe. Server now keeps ingested waits alive while scan/local copy can progress and bypasses NAS retry for that local work/fresh capacity measurement only; real source/disk/JPEG comparison blockers remain visible. Default offline8-file import returns0 with8 local/8 pendingarchive; archived wait returns3. Service-flow41 tests pass with progress-aware bounded cleanup (5s no durable progress,30s overall); rigid5s total was disproven by steady3.8–4.5s copies under concurrent fixtures and Nix compilation. This is a test-scheduling fix, not relaxed ingestion semantics.
- First Nix package full-check failed offline preview cached1 expectation: isolated builder has no /etc/machine-id, readable boot_id and valid directory birthtime. Source agent confirmed in actual Nix probe and is adding safe boot-scoped local-directory fallback; physical card UUID/CID path unaffected. Rebuild package after final local fallback/test changes; previous derivation91vqjkv27xhkr1h27bg5yhpz0g8s14jm failed, not deployable.

## Ready for user activation

- [x] Final native fmt and strict Clippy passed; all474 tests passed (root full log `/tmp/picture-sorter-fast-integrated-tests.log`).
- [x] Checked Nix package passed its complete release test suite. Package `/nix/store/05lpmwdmi5fnvn7l9c04cdrbrxa1d9wv-photo-sorter-0.1.0`, retained `/home/tulku/.local/state/photo-sorter-builds/fast-card-import`.
- [x] Merged/pushed source main `6ceca718311e4356d4f18a08b5989924d59d7fe1`.
- [x] GitHub CI passed: https://github.com/tulku/picture_sorter/actions/runs/43583086764.
- [x] Updated/committed Nix main `8810a87 Default photo sorter to fast local imports` (local commit, as prior configuration updates). Only flake.lock and home/tulku/photo-sorter.nix changed.
- [x] Full clean-commit syl system build passed: `/nix/store/bppphb84760gibpl60y5an77g0bk5jh2-nixos-system-syl-26.05.20261108.ac588ef`, retained `/home/tulku/.local/state/photo-sorter-builds/fast-card-import-system`.
- [x] Verified actual built wrapper (kzp5d64l1zvkzwv17yrdg0pprqrw2gn5) dispatch for bare invocation, --wait-for archived, --strict, --json status, and --help without starting imports.
- [x] Exact release package real-card preview:1.094 seconds, selected/cached5396,exact4336,image942,new118,newbytes2065353069,ambiguous0,hash_bytes0,transfer_bytes0. Private report `fast-card-preview/release-result.json` retained. No temporary selection is used by normal workflow.
- [x] Implementation worktree and feat/fast-card-import branch removed after integration. Source and Nix checkouts clean; planning worktree retained.
- [ ] User activates with `nh os switch --flake /home/tulku/nixos#syl`; user explicitly owns activation. Then verify new service path/source cache and normal real import if requested.

Normal user workflow after activation: `photo_sorter` imports from the configured SD root, waits for durable local ingestion, shows periodic progress, and lets NAS upload continue independently. `photo_sorter --wait-for archived` also waits for NAS publication. Unknown source/generation or changed file metadata needs hashing; --strict bypasses cached source/JPEG shortcuts.

Ruling: Nix sandbox lacked machine-id, so safe local directories may use boot_id fallback only with birthtime/device/inode; source cache is then boot-scoped. Physical FAT/exFAT source keys remain unchanged. Non-FAT UUID directory without birthtime is also unreliable; removable CID without filesystem generation remains unreliable. Actual Nix sandbox repeat test and new identity-boundary regressions pass. This does not alter the validated current SD binding.

The pre-existing transient StartupVerifier event/receipt readiness race noted above remains a diagnostic follow-up; immutable-copy/publication/cleanup still use independent fresh byte proofs. The concrete sticky physical-finding bug uncovered during validation was fixed, not waived.

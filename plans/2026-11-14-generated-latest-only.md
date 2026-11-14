# Generated latest-only retention implementation plan

> Use the existing authorized autonomous execution workflow; root integrates independent catalogue, NAS protocol and service tasks.

**Goal:** Bound generated export storage to current saved content and active recovery bytes.
**Architecture:** Journaled, exact-version retirement plus immutable receipts; fresh catalogue authorization protects all remaining digest references. Existing immutable current payload stays for replay/fetch.
**Tech Stack:** Rust, SQLite, qualified SingleWriter SMB/no-replace publication.
**Spec:** /home/tulku/Repos/picture_sorter/superpowers-plans/specs/2026-11-14-generated-latest-only.md

## Global constraints

No ordinary history/original deletion; generated ownership strict; ENOENT-only receipt exception; always verify unknown bytes and preserve on mismatch. Small records retained; user switches syl. No Immich changes. Main base14683ce.

## Interfaces/ownership

Catalogue owner: archive/format.rs new ArchiveRecord::GeneratedPayloadsRetired { current: ProjectionPlan, versions: Vec<VersionId> }; catalog/schema.rs (VERSION3 and retention schema), catalog/mod.rs, catalog/replay.rs retirement replay, new catalog/generated_retention.rs with public reexports. Own all catalog retirement types.

Types: GeneratedRetentionPlan { pub commit: CommitEnvelope, pub authorization: GeneratedRetentionAuthorization }; GeneratedRetentionReceipt { pub commit: CommitEnvelope, pub removed_digests: Vec<Digest> }. Private authorization fields current:ProjectionPlan, versions:Vec<Version>, removable:Vec<Digest>, projections:Vec<ProjectionPlan>, archive:ArchiveId; expose current(), versions(), removable(), projections(), archive() by reference/value appropriately. New plan bound current.commit retirement record plus full fresh-replay catalogue authorization. `projections` provides committed projection plans for this sidecar to authorize retained-file cleanup; protect foreign staged/retained data.

Catalogue API: generated_retention_candidate(&self)->AppResult<Option<(SidecarId,VersionId)>>; prepare_generated_retention(&mut self, archive:ArchiveId, writer:WriterId, sidecar:SidecarId, current:VersionId)->AppResult<GeneratedRetentionPlan>; acknowledge_generated_retention(&mut self,&GeneratedRetentionReceipt)->AppResult<()>. Invalidate removable history_receipts before returning dispatch plan (safe over-invalidation). Reuse matching journaled pending batch after crash, mark batch cleaned only upon receipt, do not starve unretired records beyond128. Fresh candidate only for current singleton generated shared head with applied NAS projection and obsolete archived versions, or matching pending cleanup batch. Prepare after fresh replay revalidates head/ownership and current projection. Every digest reference outside exact newly/already retired generated versions prevents deletion; current/pending local/shared heads protect. Retirement replay validates owner/current head, archived old IDs/same sidecar/not current, records retirement batch/versions; compatible repeated retirement records allowed. Preserve proof bodies in catalogue.

NAS owner: src/archive/generated.rs completion marker/idempotence, new archive/retention.rs and archive/mod.rs reexport, archive/journal.rs ENOENT retired exception, qualification/generated.rs probe/fingerprint/checks, tests/generated_retention.rs. API ArchiveStore::retire_generated(&mut? self, guard:&mut PublicationGuard, plan:GeneratedRetentionPlan, faults:&dyn FaultInjector)->AppResult<GeneratedRetentionReceipt>. Use &self if no mutation. Root provides NasCommand dispatch. Verify all auth matches exact retirement record and generated owner/current commit/live bytes, publish retirement commit, durable marker receipts, verify/unlink owned obsolete payloads and retained copies. Add current-completion marker before cleaning retained data; apply projection handles valid completion with missing retained. Per-version receipt raw decode never recursively reads successor payload. Current history stays. Missing cleanup targets retry idempotently; unknown present bytes preserved. New fingerprint v3 and scratch cleanup/retry check, base NAS fingerprint unchanged.

Root owner: src/service/publication.rs candidate/phase dispatch and acknowledge, src/transfer/worker.rs command/reply, docs/usage.md, service integration tests, full verification, source/Nix integration. Candidate::Retention(sidecar,current), Phase::Retention. Low priority after ordinary work; begin/list/replay before prepare/command. Returned receipt apply_commit/ack; End releases fence. Uncertain reply retry via journaled pending batch. Capture primary NAS worker serializes with publisher; local snapshots continue. Existing generated_flow historical content assertions need change to latest-only expectations plus explicit retirement/journal rebuild proof.

## Tasks

- [x] Catalogue: RED tests for strict obsolete version validation, digest-sharing protection, downgrade refusal, cache invalidation, retry/pagination; implement APIs/replay/schema; focused tests GREEN; commit.
- [x] NAS: RED tests replacement cleanup, old-history replay after retirement, absent-without-proof rejection, corrupt/foreign bytes preservation, completion retry, cleanup interruptions and A→B→C retirement receipt chain; implement protocol/qualification; focused tests GREEN; commit.
- [x] Root: service-level rewrite and offline/reconnect latest-only cleanup, A→B→A restaging; integration fixes and docs. Run full cargo test --all-targets, fmt, strictclippy, independent review, checked Nix package; qualify final protocol scratch on SMB.
- [x] Merge/push source main, CI, Nix pin/build/commit without activation, verify tree and remove feature worktree; record/push planning ledger.

## Review focus

Digest reused by ordinary metadata/current generated heads; same-content later save after pruning; interrupted commit/receipt/unlink/lost reply; replay old retired commits out of order; unknown retained collision and stale projection repairs must never delete newer bytes. Each owner adds tests for these boundaries.

- [ ] User activates syl; observe generated-file cleanup on live service if requested.

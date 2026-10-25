# Offline photo archive implementation plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking. The requested execution is one fresh sol agent working continuously; do not stop for permission between tasks. Follow the skill's final independent whole-branch review. Read the spec as well as this plan.

**Goal:** Deliver the complete Rust CLI/service for offline photo import, bounded local originals, durable NAS archive/version catalog, RapidRAW edits, Immich URL pins/ratings, and corrected sequence classification, with automated recovery tests and explicit deployment qualification.

**Architecture:** A local SQLite-owning coordinator handles durable commands, reservations and file observations. Bounded workers perform local transfers and isolated NAS work using rsync plus strong verification; an immutable NAS journal is authoritative for shared archived state. Editor/API adapters feed versioned observations, while incoming local edits are applied only at the explicit editor-closed boundary.

**Tech Stack:** Rust edition 2024, SQLite/rusqlite with backup support, serde/serde_json, BLAKE3, UUIDs, clap, notify/inotify, Unix sockets, rsync, ExifTool, reqwest/rustls, Linux filesystem primitives. Choose mutually compatible stable crate versions at implementation time and commit Cargo.lock; do not invent version numbers. Tokio may run the local coordinator/HTTP/socket tasks; blocking NAS operations remain in a separate process.

**Spec:** `/home/tulku/Repos/picture_sorter/superpowers-plans/specs/2026-10-25-offline-photo-archive-design.md`, especially normative section 25.

## Global constraints

- Repository `/home/tulku/Repos/picture_sorter`; inspected baseline `de1cffa6a785a594c7aebf5de2e2e8716c30a448`; keep this personal main checkout in place. The user authorized deletion of the former untracked `fill_gaps.sh` during repository relocation.
- Implementation branch `feat/offline-photo-archive`; worktree exactly `/home/tulku/Repos/picture_sorter/offline-photo-archive`. State and verify this path before creation. If layout cannot be used, report the expected/attempted paths and stop; no fallback.
- Planning artifacts remain only in the existing orphan `meta/superpowers-plans` worktree. Never copy/commit this plan, the spec, execution ledger, or reviewer prompts to the implementation branch.
- Read any applicable AGENTS.md before modifications. Comments only for constraints code cannot express, one line each; no history. Runtime docs describe product behavior, not development narration.
- Linux user service, local working/state roots, passive mounted NAS, ordinary-folder RapidRAW, genuine JPEG-only Immich mapping, no native Immich editing store.
- Budget 75,161,927,680 bytes; reserve 5,368,709,120 bytes; debounce 2 s; local reconcile 300 s; activity hold 600 s; connected archive/ratings polls 60 s; one bulk transfer; EXIF batches at most 128.
- Archive/IPC schema version 1; commit maximum 8 MiB; IPC line maximum 1 MiB; JSON transformation ceiling 64 MiB; digest `blake3:<64 lowercase hex>`.
- No automatic NAS/camera-original deletion, no live SQLite on NAS, no mtime conflict winner, no ordinary-clean removal of live mutable sidecars, no unverified eviction, no timeout lock theft.
- No personal NAS writes/API mutations, production service start/install, Syncthing changes, merge or implementation-branch push during this run. Build and test all code using isolated fixtures. Leave a committed implementation branch for review.
- Implement every adapter and feature. A missing deployed app/credential/second client yields an unrun qualification test and disabled capability, not a stub, silently omitted feature, or permission to stop the remaining work.

## Review focus

These risks need dedicated tests in the named tasks; do not assume happy-path integration covers them:

1. A stalled NAS syscall must leave local imports, saved edits and status usable, without launching a second live publisher (T09/T20).
2. Pinning before a fetch finishes, then discovering another member, must retain protection and invalidate old readiness (T07/T11/T19).
3. `.rrdata` changed with unchanged size/mtime, or a directory that cannot be read, must not disappear from saved-edit tracking (T13).
4. A committed rating update with a lost HTTP response, followed by an Immich user change, must not be blindly retried (T16/T17).
5. A malformed/unknown journal record or changed current NAS sidecar must block destructive conclusions and preserve bytes (T05/T12/T21).

## Execution protocol and completion contract

This is one ordered plan with four milestones: core/catalog (T01-T08), importing/archive (T09-T12), editing/integration (T13-T18), and product/recovery/qualification (T19-T23). Milestones are not stopping points. Each task produces a separately testable deliverable. Dependencies below intentionally avoid parallel implementation of shared interfaces.

Use the executing-plans workspace/ledger outside the code checkout. Record task ID, base/head commits, failing/passing test commands, material rulings, and remaining deployment gates. Resume after compaction from the ledger and git log; do not restart completed tasks. Task checkboxes in this planning worktree may be updated, but stage them only on the planning branch if doing so; never mix planning changes into code commits.

For each task: add its named regression tests; run them and observe the intended failure; implement; run the focused tests plus affected existing tests; commit only that task's code/tests/runtime documentation. Missing imports/types are acceptable first RED evidence, but tests must eventually exercise behavior, not simply constants or mocks that implement the answer. Commands below use integration target names (`cargo test --test NAME`). Each test step names required assertions; helpers belong in `tests/support/`, not production backdoors. Run external commands with argument arrays. Use an injected clock/fault controller for deterministic tests instead of long sleeps.

Resolve routine crate/API/implementation choices without asking. If the plan has an interface typo or code evidence disproves an assumption, record a ruling, make the smallest invariant-preserving correction, update affected tests, and continue. Never weaken a safety invariant to make a test green. Genuine irreducible external blockers must be reported precisely; finish independent tasks first. The final reviewer may find more than one defect: fix all correctness/safety blockers with regression tests and recheck affected behavior; do not leave known data loss because a generic workflow suggests a fixed number of review rounds.

Completion requires real CLI-to-service-to-worker integration, production adapters, durable recovery, passing automated suites, truthful capability gating, runtime documentation, and a clean committed feature worktree. External qualification may remain unrun; report that separately from implementation completion. Do not describe a deployed working system unless it was actually qualified/deployed under separate authorization.

## File responsibility map

Keep existing `src/classify.rs`, `src/plan.rs`, `src/sidecar.rs` APIs for legacy behavior; the new engine must not call legacy timestamp-cutoff planning or first-dot identity grouping. Move the current `src/main.rs` intact to `src/bin/photo_sorter_legacy.rs` and give the new binary a new entry point. Avoid unrelated formatting/refactoring of legacy logic.

| Files | Responsibility |
| --- | --- |
| `src/main.rs`, `src/cli.rs` | clap CLI and exit-code/client formatting |
| `src/config.rs`, `src/domain.rs`, `src/error.rs` | validated profile/defaults, IDs/state values, structured failures |
| `src/fs/{mod,roots,paths,atomic,space}.rs` | root confinement, mount identity, exclusive publication, byte accounting inputs |
| `src/catalog/{mod,schema,models,jobs,identity,replay,maintenance,verify,conflicts}.rs` | local SQLite migrations, durable work, aliases and journal application |
| `src/archive/{mod,format,lock,journal,publish,projection,recovery}.rs` | passive NAS protocol and version/projection lifecycle |
| `src/import/{mod,inventory,metadata,pairing,sequences,planner,runner,bootstrap}.rs` | source identity through archive/local milestones |
| `src/storage/{mod,admission,residency,eviction,gc}.rs` | reservations, pin/readiness, recoverable originals eviction and snapshot GC |
| `src/transfer/{mod,rsync,verify,worker,protocol}.rs` | transfer subprocess, digest verification, isolated NAS worker |
| `src/edit/{mod,owner,snapshot,watch,reconcile,versions,rapidraw,refresh}.rs` | sidecar discovery/versioning and editor-closed application |
| `src/immich/{mod,client,mapping,ratings}.rs`, `src/ratings.rs` | real HTTP/schema adapter and pure rating reconciliation |
| `src/service/{mod,protocol,client,server,scheduler,recovery,status}.rs` | IPC, one owner, scheduling, reconnect, status |
| `src/qualification/{mod,report,nas,rapidraw,immich}.rs` | executable isolated deployment probes, capability records |
| `tests/support/{mod,rig,faults,commands,http}.rs`, `tests/fixtures/` | deterministic integration rigs, HTTP fake and provenance fixtures |
| `packaging/photo-sorter.service`, `config.example.toml`, `docs/{usage,recovery,migration,compatibility}.md` | user-operated deployment and recovery |

Create a module only when its owning task needs it; do not land an empty shell for the whole tree. All new integration tests are named in tasks below. Use `src/lib.rs` exports for testable public domain/services; keep filesystem/HTTP implementations behind narrow interfaces.

## Shared interface vocabulary

These definitions are established in T01/T03 and consumed unchanged unless a ledgered ruling updates all call sites. Types marked with a collection field include that field; avoid one unrelated `serde_json::Value` blob as the whole catalog.

- `type AppResult<T> = Result<T, AppError>`; `AppError { code: ErrorCode, message: String, retry: RetryClass }`; stable codes include InvalidConfig, Unsupported, Unavailable, Capacity, Conflict, Integrity, SourceChanged, Cancelled, Io.
- UUID newtypes: `ArchiveId`, `WriterId`, `ProfileId`, `GroupId`, `JobId`, `OperationId`, `VersionId`, `SidecarId`, `CommitId`. `Digest` validates exact encoded BLAKE3, not UUID.
- `ArchivePath` is validated relative UTF-8; `MembershipRevision(u64)`; `ByteCount(u64)` with checked operations; `Rating` represents 0..5; `RatingObservation` represents Unobserved, Stars, Rejected/Unsupported distinctly.
- `OriginalMember { digest: Digest, size: u64, path: ArchivePath, role: MediaRole }`; `Group { id: GroupId, membership_revision: MembershipRevision, members: Vec<OriginalMember>, primary_raw: Option<ArchivePath> }`. `MediaRole` distinguishes RAW/JPEG/other-still/video; do not confuse legacy JPEG-tree routing with JPEG eligibility.
- `Version { id: VersionId, sidecar: SidecarId, parents: Vec<VersionId>, body: VersionBody, origin: VersionOrigin }`; `VersionBody::Content { digest, size } | Reset`; origin distinguishes observed source snapshot from tool-derived projection.
- `Clock` supplies wall time for records and monotonic elapsed time for deadlines; `SystemClock` and test `ManualClock`. Wall-clock order never determines version ancestry.
- `FaultPoint` enum covers intent saved, stage partial, verified, original published, history published, commit published, projection published, local ack, unlink member, API sent, API received. `FaultInjector::hit(point) -> AppResult<()>`; production `NoFaults`.
- `Catalog` is owned by the coordinator. Worker inputs/results are serializable value objects, never a live SQLite connection. Ordinary filesystem helpers use `RootHandle` from T02.

## Task 01: Foundation, configuration and legacy isolation

**Files:** Modify `Cargo.toml`, `Cargo.lock`, `src/lib.rs`, `src/main.rs`; create `src/bin/photo_sorter_legacy.rs`, `src/config.rs`, `src/domain.rs`, `src/error.rs`, `tests/foundation.rs`, `tests/support/{mod,rig,faults}.rs`.

**Interfaces:** Produce domain vocabulary above; `Config::load(path: &Path) -> AppResult<Config>`; `Config::validate(&self) -> AppResult<()>`; `TestRig::new() -> TestRig` with separate temporary source, local, state, archive roots, manual clock and faults. The rig must never default to user paths. `Config` contains paths, profile/archive IDs (archive may be unbound before explicit init), byte/time defaults, import timezone, optional Immich config and capability evidence references.

- [ ] Write `defaults_and_root_overlap`, `byte_overflow`, `digest_validation`, `legacy_help_still_available`: assert exact numeric defaults, reject equal/nested roots and overflow/invalid digests; both binaries expose help without file mutation.
- [ ] Run `cargo test --test foundation`; observe failure before implementation.
- [ ] Isolate legacy binary and implement typed configuration/errors/IDs, manual clock/fault hooks and temporary rig. Set Cargo `default-run = "photo_sorter"`; keep the package/binary underscore spelling. Add only needed dependencies.
- [ ] Run `cargo test --test foundation` and `cargo test --lib`; expect exit 0 and existing classification/plan/sidecar tests passing.
- [ ] Commit: `feat: establish archive domain and isolate legacy importer`.

## Task 02: Confined filesystem primitives

**Files:** Create `src/fs/{mod,roots,paths,atomic,space}.rs`, `tests/filesystem.rs`; extend config validation.

**Interfaces:** `RootHandle::open(path: &Path, policy: RootPolicy) -> AppResult<RootHandle>`; `RootHandle::validate_identity() -> AppResult<()>`; `ArchivePath::parse(value: &str) -> AppResult<ArchivePath>`; `publish_noreplace(root: &RootHandle, staged: &ArchivePath, dest: &ArchivePath) -> AppResult<PublishOutcome>` (Published/Exists); `available_bytes(root: &RootHandle) -> AppResult<u64>`. Policy distinguishes local root from qualified mounted archive.

- [ ] Write `reject_escape_and_symlink_swap`, `no_replace_existing`, `mountpoint_replacement_does_not_fall_through`, `unicode_and_case_collision`, `non_utf8_is_reported`: assert unchanged sentinel outside root, unchanged destination bytes and no writes into replacement mountpoint; non-UTF-8 input yields actionable error.
- [ ] Run `cargo test --test filesystem`; observe failure.
- [ ] Implement descriptor-relative no-follow access, root/mount identity and no-replace publication. Use Linux primitives via a maintained crate or narrowly audited libc wrapper. Do not emulate exclusivity with check-then-rename. Build test double for mount identity without requiring privileged mounts.
- [ ] Run `cargo test --test filesystem`; all assertions pass; record local filesystem primitive capabilities only.
- [ ] Commit: `feat: confine archive filesystem operations`.

## Task 03: Local catalog, jobs, migrations and backups

**Files:** Create `src/catalog/{mod,schema,models,jobs}.rs`, `tests/catalog.rs`.

**Interfaces:** `Catalog::open(state: &RootHandle) -> AppResult<Catalog>`; `Catalog::enqueue(request: DurableRequest) -> AppResult<JobId>`; `Catalog::job(id: JobId) -> AppResult<JobRecord>`; `Catalog::transition(id: JobId, expected_generation: u64, change: JobTransition) -> AppResult<()>`; `Catalog::backup(dest: &Path) -> AppResult<()>`. `DurableRequest { operation_id, kind }`; kinds initially typed import/pin/bootstrap/refresh/clean/resolve/verify payloads, extended by their tasks.

Schema must represent archives/writers, blobs/metadata, groups/members/revisions/aliases, placements/source observations, sidecars/versions/heads/projections, commits/dependencies, jobs/attempts/reservations, residency/pin intents, rating observations/intents, mappings/conflicts/capabilities. Unique paths are archive-scoped; source observations reference digest, not date cutoff. All dirty and application-awaiting states are durable.

- [ ] Write `enqueue_idempotent`, `restart_pending_job`, `obsolete_attempt_cannot_ack`, `migration_backup_includes_wal`, `newer_schema_refused`: duplicate operation yields same job; reopening retains pending state; stale generation rejected; backup reads committed WAL content; unsupported future DB unchanged.
- [ ] Run `cargo test --test catalog`; observe failure.
- [ ] Implement transactional schema/migrations, DB constraints, durable job transitions and SQLite backup. File setup is local and exclusive; no remote I/O in transactions.
- [ ] Run `cargo test --test catalog`; expect pass, including reopening a real SQLite file rather than in-memory-only tests.
- [ ] Commit: `feat: persist catalog jobs and recovery state`.

## Task 04: Canonical identity and membership revisions

**Files:** Create `src/catalog/identity.rs`, `tests/identity.rs`.

**Interfaces:** `Catalog::canonical_group(id: GroupId) -> AppResult<GroupId>`; `Catalog::attach_member(id: GroupId, member: OriginalMember, evidence: PairEvidence) -> AppResult<MembershipRevision>`; `Catalog::merge_groups(ids: &[GroupId], evidence: PairEvidence) -> AppResult<GroupId>`; `PairEvidence` stores the compared context/camera/time data and decision, not a boolean shortcut. `Catalog::group(id) -> AppResult<Group>`.

- [ ] Write `same_placement_reuses_published_group`, `same_bytes_other_placement_stays_distinct`, `merge_order_independent`, `merge_preserves_pins_versions_conflicts`, `membership_change_invalidates_readiness`, `aliases_cannot_cycle`: assert smallest published UUID survivor, provisional never displaces published, all placements/heads retained, revision incremented and readiness cleared.
- [ ] Run `cargo test --test identity`; observe failure.
- [ ] Implement deterministic aliases and membership changes in transactions. Publication caller must refresh journal before using these routines (T10). Preserve local provisional state until canonicalization commits.
- [ ] Run `cargo test --test identity`; include shuffled merge/alias replay order.
- [ ] Commit: `feat: reconcile photograph identities without losing local state`.

## Task 05: Commit codec and deterministic journal replay

**Files:** Create `src/archive/{mod,format}.rs`, `src/catalog/replay.rs`, `tests/journal.rs`, `tests/fixtures/journal/`.

**Interfaces:** `CommitEnvelope::encode(&self) -> AppResult<Vec<u8>>`; `CommitEnvelope::decode(bytes: &[u8], expected: ArchiveId) -> AppResult<CommitEnvelope>`; `Catalog::apply_commit(commit: &CommitEnvelope) -> AppResult<ApplyOutcome>` (Applied/AlreadyApplied/Deferred); `Catalog::publication_readiness() -> AppResult<PublicationReadiness>` (Ready/Blocked reasons). Define tagged `ArchiveRecord` for original/member, aliases, sidecar version/head/conflict/resolution, rating observation/mapping; each carries required stable IDs/dependencies. Local pins/jobs never enter shared records.

- [ ] Write `payload_digest_uses_exact_decoded_bytes`, `out_of_order_dependencies`, `replay_idempotent`, `duplicate_id_different_contents_rejected`, `unknown_or_missing_payload_blocks_publication`, `same_bytes_distinct_ancestry`, `reset_is_tombstone`, `oversized_commit_rejected`: assert no partial DB application, no dependence on JSON key order or timestamps, and safe blocking.
- [ ] Run `cargo test --test journal`; observe failure.
- [ ] Implement envelope/operation validation, commit limits and dependency staging. Validate parent/sidecar ownership and cycles before transaction. Keep corrupt records visible and block unsafe publication/eviction; do not advance a sequence-only watermark past gaps.
- [ ] Run `cargo test --test journal`; replay shuffled complete fixture history into a fresh catalog and compare groups/heads/mappings, excluding private state.
- [ ] Commit: `feat: replay validated immutable archive records`.

## Task 06: Source inventory and metadata extraction cache

**Files:** Create `src/import/{mod,inventory,metadata}.rs`, `tests/import_metadata.rs`, `tests/support/commands.rs`, `tests/fixtures/exif/`.

**Interfaces:** `Inventory::open(source: RootHandle) -> AppResult<Inventory>`; `Inventory::next_batch(&mut self, limit: usize) -> AppResult<Vec<SourceEntry>>`; `recognize(entry: &SourceEntry, source: &SourceIdentity, catalog: &Catalog, strict: bool) -> AppResult<Recognition>`; `MetadataExtractor::extract(entries: &[SourceEntry]) -> AppResult<Vec<MetadataResult>>`; real `ExifToolExtractor`, counting test implementation. `SourceIdentity` includes attachment/incarnation reliability; `SourceEntry` has relative path/size/mtime precision and `SourceRole::Original(MediaRole) | Companion(CompanionKind)`; recognized companions are not discarded by a media-only filter. `MetadataResult` records digest/extractor version, date with source/offset/subseconds, camera identity, normalized shooting fields or explicit extraction failure.

- [ ] Write `repeat_known_source_zero_exif`, `unseen_same_label_requires_hash`, `strict_detects_hidden_change`, `known_digest_reuses_exif`, `source_changes_during_read`, `bad_exif_cached`, `datetime_preserves_wall_date`, `filename_arguments_are_literal`: assert zero extractor calls/copies for known unchanged sources, strong validation on untrusted attachments, batch size <=128, wall-date layout, mtime UTC fallback/UNDATED.
- [ ] Run `cargo test --test import_metadata`; observe failure.
- [ ] Implement safe enumeration, digest recognition and ExifTool batching with MakerNotes enabled (no `-fast2`). Request actual source filename in results and map responses by validated path, never array order. Preserve useful partial successes and errors. Do not collect the whole source/archive into RAM; process `next_batch` until empty. Hash recognized mutable source companions even when original shortcuts succeed.
- [ ] Run `cargo test --test import_metadata`; include a command-runner test asserting arguments and zero EXIF on repeat. Do not make all tests depend on personal archive access.
- [ ] Commit: `feat: replace timestamp imports with cached source identity`.

## Task 07: Reservations, pins and readiness

**Files:** Create `src/storage/{mod,admission,residency}.rs`, `tests/admission.rs`.

**Interfaces:** `admit(input: &AdmissionInput) -> AdmissionDecision` (Local { additional_bytes } / DirectNas / WaitingCapacity); input includes budget/occupied/reserved/available/reserve/group-existing/required/staging-overhead/NAS availability. `Catalog::request_pin(group: GroupId, operation: OperationId) -> AppResult<JobId>`; `Catalog::unpin(group: GroupId) -> AppResult<()>` clears explicit intent only; `Catalog::readiness(group: GroupId) -> AppResult<Readiness>`; `Catalog::reserve(job: JobId, filesystem: FilesystemId, remaining: u64) -> AppResult<ReservationId>`; readiness contains member revision, originals-ready, metadata-current and waiting reason.

- [ ] Write `exact_budget_boundary`, `partials_not_double_counted`, `resident_member_only_charges_missing_bytes`, `shared_filesystem_reserve`, `pin_protects_before_fetch`, `unpin_keeps_pending_and_conflict_protection`, `oversized_pin_stays_blocked`, `expanded_group_not_ready`, `enospc_after_admission`: assert checked arithmetic, per-filesystem reservations, durable pin intent and no false completed job.
- [ ] Run `cargo test --test admission`; observe failure.
- [ ] Implement pure admission plus catalog transactions for reservations/readiness. Reconcile actual staging bytes and remaining reservation after restart. Do not evict in the pure admission function.
- [ ] Run `cargo test --test admission`; assert protected edits can exceed target and block new admission without deletion.
- [ ] Commit: `feat: bound imports with durable space reservations and pin intents`.

## Task 08: Pairing and sequence classification using real evidence

**Files:** Create `src/import/{pairing,sequences,planner}.rs`, `tests/sequences.rs`, `tests/fixtures/exif/{burst,bracket,single}.json`, `tests/fixtures/exif/README.md`.

**Interfaces:** `pair_candidates(records: &[CaptureRecord]) -> Vec<PairDecision>`; `classify_sequences(records: &[CaptureRecord], existing: &[SequenceRecord], policy: &SequencePolicy) -> Vec<SequenceDecision>`; `plan_placements(groups: &[PlannedGroup], existing: &Catalog) -> AppResult<Vec<PlacementIntent>>`. `CaptureRecord` wraps metadata identity and source context, separate from bytes. Sequence decisions carry kind, member digests, confidence/incomplete state, persisted anchor and explanation.

- [ ] Add read-only extracted real metadata from spec section 16 (four P3217330..333 burst frames; three P5178585..587 bracket frames; `_A230938` single and corresponding first RAWs). Preserve only relevant metadata and provenance, no images/GPS/serial secrets. If personal paths unavailable, use already documented values as clearly transcribed fixtures and add a runnable extraction command for later qualification; do not claim a new read.
- [ ] Write `real_four_frame_burst`, `real_three_frame_bracket`, `single_not_sequence`, `same_second_and_missing_frame`, `counter_reset_separates`, `multiple_dcim_or_cameras_do_not_pair`, `first_dot_is_not_identity`, `split_import_stable_paths`, `different_shutter_still_bracket`: assert expected kinds/membership, no unrelated merge, 2s/5s thresholds, stable published paths and ORF+ORI primary selection.
- [ ] Run `cargo test --test sequences`; observe failure.
- [ ] Implement extraction normalization separately from grouping; pin any presentation-string parser to verified fixture/version/locale when raw numeric tags are unavailable. Remove old assumptions only in new engine. Preserve old folders and add logical membership without moving published files.
- [ ] Run `cargo test --test sequences` plus `cargo test --test import_metadata`; expect pass from RAW and JPEG evidence, not folder-name guesses.
- [ ] Commit: `fix: classify camera bursts and brackets from preserved MakerNotes`.

## Task 09: Verified rsync transfers and isolated NAS workers

**Files:** Create `src/transfer/{mod,rsync,verify,worker,protocol}.rs`, `tests/transfers.rs`, `tests/worker_isolation.rs`; wire a hidden worker entry point in `src/main.rs`.

**Interfaces:** `TransferSpec { job, generation, source, destination_root, staged_path, expected_digest, expected_size }`; `TransferReceipt { job, generation, digest, size, staged_path }`; `TransferEngine::stage(spec: &TransferSpec) -> AppResult<TransferReceipt>`; `WorkerSupervisor::submit(command: NasCommand) -> AppResult<WorkerTicket>` and `poll(ticket) -> WorkerState`. `NasCommand` is a typed enum extended by T10/T12/T18; results carry immutable values. `WorkerState` distinguishes active, completed, failed, timed-out-still-alive, and reaped. The child receives confined root handles/config and cannot open local SQLite. It inherits ownership-lock descriptors so a surviving old worker still excludes a new profile or working-root owner after coordinator death; do not rely on a PID-file existence check.

- [ ] Write `rsync_argv_has_no_destructive_flags`, `digest_mismatch_never_verified`, `partial_resume_source_revalidated`, `filenames_literal`, `stale_generation_result_ignored`, `cancel_uncertain_worker_keeps_reservation`, `hung_nas_does_not_block_local_work`, `timed_out_worker_not_replaced`: real temporary rsync transfers preserve expected digest, partial files never count archived, local timer/status test responds while worker is deliberately stuck, no second worker launches.
- [ ] Run `cargo test --test transfers --test worker_isolation`; observe failure.
- [ ] Implement explicit file/staging transfers, immutable source checks before/after, destination hashing and bounded stderr. Run NAS enumeration/stat/hash/flush/publication in the worker as well as rsync; keep destination access bound to validated root descriptor. Never expose argv secrets or use `--delete`, `--inplace`, `--remove-source-files`. Use child process groups/cancellation accounting; uncertain completion stays recoverable.
- [ ] Run `cargo test --test transfers --test worker_isolation`; include at least one actual rsync round trip. If executable absent, implement/report dependency error and document the exact unrun external test; do not call a mock a real transfer.
- [ ] Commit: `feat: transfer verified files without blocking offline operations`.

## Task 10: Archive identity, lock and original publication

**Files:** Create `src/archive/{lock,journal,publish,recovery}.rs`, `tests/archive_publish.rs`, `tests/archive_lock.rs`; extend worker protocol.

**Interfaces:** `ArchiveStore::initialize(root: &RootHandle, request: InitArchive) -> AppResult<ArchiveMarker>`; `acquire_publication(request: LockRequest) -> AppResult<PublicationGuard>`; `refresh_journal(guard: &PublicationGuard) -> AppResult<Vec<CommitEnvelope>>`; `publish_originals(plan: PublicationPlan) -> AppResult<PublicationReceipt>`; receipt includes committed IDs, canonical groups, projection-pending flag. `PublicationPlan` carries prepared staged members, proposed records, expected archive/parents and attempt ID; the worker revalidates under lock, and coordinator transactionally applies the resulting immutable records.

- [ ] Write `competing_init_one_identity`, `missing_marker_not_reinitialized`, `same_content_reuses_path`, `different_content_blocks_without_overwrite`, `fresh_lock_state_canonicalizes_duplicate`, `crash_after_rename_before_commit`, `commit_gap_not_skipped`, `expired_lock_not_stolen`, `recover_lock_requires_quiescence`: assert marker never recreated, prior bytes unchanged, no duplicate canonical group, orphan digest recovery, lock owner remains when uncertain.
- [ ] Run `cargo test --test archive_publish --test archive_lock`; observe failure.
- [ ] Implement immutable journal temp/flush/close/rename, exclusive lock and original-before-commit publication. Treat the short-lock fresh-state requirement as mandatory; hash/transfer outside lock when possible, revalidate dependent facts within it. Add structured recovery operation requiring explicit prior-owner stopped/all-writers-quiescent assertions; it cannot prove remote shutdown and must say so.
- [ ] Run focused tests; inject each original/commit/ack boundary and restart with independent catalog clients. A local filesystem concurrency test is required but explicitly not SMB qualification.
- [ ] Commit: `feat: publish recoverable archive commits under explicit coordination`.

## Task 11: End-to-end incremental and oversized imports

**Files:** Create `src/import/runner.rs`, `tests/import_flow.rs`; extend planner/jobs.

**Interfaces:** `ImportRequest { source_path, strict, dry_run, operation_id }`; `ImportCoordinator::prepare(request: ImportRequest) -> AppResult<ImportReport>`; `ImportCoordinator::advance(job: JobId) -> AppResult<JobProgress>`; `ImportReport` distinguishes recognized, local-ready, archive-committed, waiting and conflicting groups, plus per-stage timings. Consume T06 inventory, T08 placement, T07 reservations and T09/T10 transfer/publication.

- [ ] Write `overlapping_cards_both_orders_keep_unique_old_photo`, `repeat_zero_exif_zero_copy`, `evicted_known_original_not_reimported`, `larger_than_budget_direct_nas`, `offline_capacity_pause_then_resume`, `card_swap_cannot_resume_wrong_file`, `split_pair_expands_pin`, `conflict_group_does_not_block_unrelated`, `dry_run_changes_no_state`, `source_rrdata_rrexif_and_full_size_orig_preserved`, `mutable_source_companion_changed_on_repeat_import`, `orphan_companion_preserved_until_resolution`: assert bytes conserved, no whole-card local staging, no captured-date skip, separate milestones and no mutation by dry run.
- [ ] Run `cargo test --test import_flow`; observe failure.
- [ ] Implement bounded ingestion, durable child jobs and capacity transitions, local first then direct NAS. Preserve intended placements/operation identities across retries and destination changes. Unknown/failed EXIF never silently discards originals; fallback singleton placements follow spec. Do not download direct-NAS imports back automatically. Import recognized companions through the version/ownership pipeline; until T12/T13 land, preserve exact staged bytes with durable pending jobs. Full-size `.orig` and orphan companions obey capacity/direct-NAS policies. Never overwrite a live sidecar from a camera/source copy. Await archive work only where the requested completion milestone requires it.
- [ ] Run `cargo test --test import_flow`; use tiny test budgets for exhaustion and constants tests for actual 70 GiB defaults. Verify no source file was deleted/changed.
- [ ] Commit: `feat: import overlapping cards within a bounded local budget`.

## Task 12: Sidecar history, conflicts and NAS projections

**Files:** Create `src/edit/{mod,versions}.rs`, `src/archive/projection.rs`, `tests/sidecar_versions.rs`; extend archive records/catalog/worker protocol.

**Interfaces:** `VersionStore::record(snapshot: SnapshotIntent) -> AppResult<VersionId>`; `reconcile_heads(base: &[VersionId], local: &[VersionId], remote: &[VersionId]) -> VersionDecision`; `ArchiveStore::publish_versions(plan: VersionPublication) -> AppResult<PublicationReceipt>`; `Catalog::resolve_conflict(id: ConflictId, heads: &[VersionId], chosen: VersionId) -> AppResult<VersionId>`. Define `ConflictId` UUID and structured conflict kind/heads/owner. Snapshot intent contains sidecar ID, exact payload-or-reset, observed parents and operation ID; no content-hash-as-version shortcuts.

- [ ] Write `a_b_a_has_three_versions`, `same_bytes_different_parents_preserve_ancestry`, `concurrent_edits_keep_both_heads`, `reset_vs_edit_conflicts`, `commit_before_projection_recovery`, `unknown_live_nas_bytes_preserved`, `resolution_names_all_heads`, `old_upload_does_not_ack_new_save`: assert exact histories, idempotent retries, previous unambiguous live projection retained during conflict, current-byte unexpected changes never overwritten.
- [ ] Run `cargo test --test sidecar_versions`; observe failure.
- [ ] Implement immutable history upload/verify then locked journal head update then recoverable current-sidecar projection. Only acknowledge affected group eligible after projection reconciliation. Leave unknown NAS edits as protected conflicts; no automatic NAS GC. Resolution appends ancestry, never mutates old versions.
- [ ] Run `cargo test --test sidecar_versions` with crashes at history/commit/projection/local-ack boundaries.
- [ ] Commit: `feat: preserve edit history and recover sidecar projections`.

## Task 13: Saved-edit snapshots, notifications and reset detection

**Files:** Create `src/edit/{owner,snapshot,watch,reconcile}.rs`, `tests/edit_tracking.rs`, `tests/fixtures/sidecars/`.

**Interfaces:** `resolve_owner(path: &ArchivePath, members: &[OriginalMember]) -> OwnerDecision`; `Snapshotter::capture(owner: SidecarOwner) -> AppResult<SnapshotObservation>` (StableContent/StableAbsent/Retry/Unreadable/OpaqueInvalid); `EditTracker::observe(event: LocalEvent) -> AppResult<()>`; `EditTracker::reconcile(scope: ReconcileScope) -> AppResult<ReconcileReport>`. Snapshot references immutable local objects with digest and version parents. Observation format differs from archive Version and must not accidentally publish malformed JSON as valid editing state.

- [ ] Write `atomic_replace_save_detected`, `same_size_mtime_change_detected`, `startup_recovers_saved_edit`, `overflow_scan_finds_edit`, `unreadable_parent_not_reset`, `root_disappearance_not_mass_reset`, `temporary_absence_not_reset`, `stable_delete_becomes_versioned_reset`, `malformed_json_protected_opaque`, `virtual_copy_longest_owner_match`, `binary_companion_not_json`: assert exact retained bytes, no false resets, no EXIF, tool writes do not generate loops/activity.
- [ ] Run `cargo test --test edit_tracking`; observe failure.
- [ ] Implement local notify adapter plus deterministic event tests and periodic hashing of mutable files. Debounce 2s using injected time; stable reread/hash before capture; preserve opaque invalid bytes. Snapshot writes are durable/atomic and reserved/accounted. Reconcile known retained sidecars even when originals are evicted. Persist tool-write digests/removal intents before applying changes.
- [ ] Run `cargo test --test edit_tracking`; add a bounded real inotify smoke test with eventual polling deadline so event ordering does not make CI flaky.
- [ ] Commit: `feat: track saved RapidRAW edits across missed events and restarts`.

## Task 14: Safe incoming application and original-only eviction

**Files:** Create `src/edit/refresh.rs`, `src/storage/{eviction,gc}.rs`, `tests/refresh.rs`, `tests/eviction.rs`.

**Interfaces:** `RefreshRequest { groups, application_session: ApplicationSessionId, operation_id }`; `ApplicationSessionId` refers to a non-persisted foreground authorization created only by an explicit editor-closed assertion; `RefreshCoordinator::apply(request: RefreshRequest) -> AppResult<RefreshReport>`; `EvictionPlanner::candidates(catalog: &Catalog, now: Timestamp) -> AppResult<Vec<EvictionIntent>>`; `Evictor::execute(intent: EvictionIntent) -> AppResult<EvictionReport>`; `SnapshotGc::collect(catalog: &mut Catalog) -> AppResult<GcReport>`. Intents include membership revision, exact originals/versions, eligibility generation and retry identity.

- [ ] Write `refresh_without_closed_assertion_stages_only`, `refresh_snapshots_displaced_bytes`, `refresh_activity_invalidates_boundary`, `refresh_disconnect_or_restart_revokes_authorization`, `waiting_refresh_requires_new_assertion`, `refresh_no_wait_cannot_apply`, `watcher_expected_write_not_new_edit`, `late_save_survives_original_eviction`, `eviction_crash_between_members`, `pin_or_member_change_cancels_eviction`, `unknown_file_after_db_loss_protected`, `metadata_occupancy_can_block`, `only_verified_unreferenced_snapshots_collected`: assert no live-sidecar unlink, all history retained, no ready state after partial eviction, no discard of pending bytes.
- [ ] Run `cargo test --test refresh --test eviction`; observe failure.
- [ ] Implement serialized final local snapshot/version comparison and editor-closed replacement with displaced-byte preservation. Do not imply digest precheck locks RapidRAW. Persist staged work but never persist reusable application permission; wait/disconnect/restart revokes the foreground session, checked before each file. A fresh explicit request may resume; background retry cannot. Evict only immutable originals with durable intent/recheck. Distinguish source disappearance from reset, and storage cleanup from remote deletion. Safe local snapshot GC is reference-based; ordinary clean cannot delete live sidecars.
- [ ] Run focused tests with save injected after eligibility check/before unlink and during refresh prechecks; expect preserved saved bytes and reported boundary violation where applicable.
- [ ] Commit: `feat: apply incoming edits explicitly and evict only verified originals`.

## Task 15: Real Immich client, identity mapping and URL pins

**Files:** Create `src/immich/{mod,client,mapping}.rs`, `tests/immich_mapping.rs`, `tests/support/http.rs`, `tests/fixtures/immich/`.

**Interfaces:** `ImmichClient::asset(id: Uuid) -> AppResult<AssetInfo>`; `ImmichClient::server_info() -> AppResult<ServerInfo>`; `parse_photo_url(url: &str, server: &Url) -> AppResult<Uuid>`; `resolve_asset(asset: &AssetInfo, mappings: &[PrefixMapping], catalog: &Catalog) -> AppResult<GroupId>`; `PinCoordinator::request(urls: &[Url], operation: OperationId) -> AppResult<Vec<PinResult>>`. AssetInfo preserves server/library/path/media/checksum identity; PrefixMapping uses component-prefix matching, not textual replace.

- [ ] Write `configured_origin_only`, `userinfo_or_redirect_no_secret_leak`, `path_prefix_boundary_and_traversal`, `tiff_is_not_eligible_jpeg`, `ambiguous_stem_not_paired`, `known_url_works_offline`, `unknown_url_waits_for_immich`, `reindexed_asset_cannot_rebind_unrelated_photo`, `multi_url_partial_results`, `re_fetch_keeps_local_sidecars`: assert authenticated headers only to configured origin, genuine asset eligibility, correct RAW/JPEG paths, group-only pins and protected local updates.
- [ ] Run `cargo test --test immich_mapping`; observe failure.
- [ ] Implement real HTTP requests and DTOs from a pinned official Immich OpenAPI schema/source revision. Save only minimal sanitized contract fixtures plus source/version provenance, no keys/personal responses. Do not fabricate endpoint names from this plan; determine exact asset/server schema in the implementation research step and freeze tested compatibility. Optional API capability gates are distinct from network outage. Map supported JPEG by exact archive path and available verified identity; don't compare unlike checksum algorithms directly.
- [ ] Run `cargo test --test immich_mapping` against local HTTP fixture server including 401/403/404, timeout, bad schema and redirect. Fetch with T09/T07/T14 integration and mark latest-ready only after required metadata applied.
- [ ] Commit: `feat: pin archived photograph groups from Immich URLs`.

## Task 16: RapidRAW format adapter and pure rating reconciliation

**Files:** Create `src/edit/rapidraw.rs`, `src/ratings.rs`, `tests/ratings.rs`, `tests/rapidraw_adapter.rs`, `tests/fixtures/rapidraw/`.

**Interfaces:** `RapidRawAdapter::inspect(bytes: &[u8]) -> AppResult<EditDocument>`; `RapidRawAdapter::with_rating(document: &EditDocument, rating: Rating) -> AppResult<Vec<u8>>`; `RapidRawAdapter::initial_document(rating: Rating) -> AppResult<Vec<u8>>`; `reconcile_rating(input: &RatingInputs) -> RatingDecision`. Inputs include last common, last applied local, observed local/remote, pending canonical and observation IDs. Decisions distinguish unchanged, converge, local/remote update, conflict, unsupported. `EditDocument` retains unknown fields and can compare non-rating content semantically without generic merging.

- [ ] Inspect pinned RapidRAW v1.6.6 source (release commit `52138689a9c9aea400e37a30c902154b47db3799`) and inspected development commit `872804e8a21d4dff707f31fc9dcf5d3128ca663b`; record the exact supported serialization, default/reset and virtual-copy naming behavior with minimal sanitized native fixtures. Use a source-backed profile; installed runtime evidence is a later qualification. Unknown versions remain preservable but transformation-disabled.
- [ ] Write `ratings_round_trip_zero_three_five_zero`, `missing_vs_explicit_zero`, `rejected_not_coerced`, `unknown_fields_and_masks_survive`, `base_raw_only_not_virtual_copy`, `initial_sidecar_is_valid_native`, `pending_remote_star_unrelated_exposure_save`, `remote_rating_only_local_adjustment_merge`, `divergent_adjustments_conflict`, `reset_keeps_pending_rating_intent`: assert exact rating decisions, semantic equality of all non-rating data and retained source snapshots.
- [ ] Run `cargo test --test ratings --test rapidraw_adapter`; observe failure.
- [ ] Implement schema-profile adapter and pure three-baseline rating decisions. ORF over associated ORI; otherwise unique primary or explicit conflict. No broad recursive JSON merge. Distinguish derived canonical version from current local representation; never rewrite an open editor's live file here.
- [ ] Run focused tests, including fixture serialization consumed by the source-backed format validator. Lack of GUI test must remain reported as runtime-unverified, not adapter omission.
- [ ] Commit: `feat: reconcile photograph ratings without losing RAW adjustments`.

## Task 17: Durable ratings synchronization and API recovery

**Files:** Create `src/immich/ratings.rs`, `tests/ratings_sync.rs`; extend catalog/archive ratings records and real client.

**Interfaces:** `ImmichClient::rating(asset: Uuid) -> AppResult<RemoteRating>`; `ImmichClient::set_rating(request: RatingWrite) -> AppResult<WriteOutcome>`; `RatingSynchronizer::poll(scope: RatingScope) -> AppResult<RatingSyncReport>`; `RatingSynchronizer::advance(intent: RatingIntentId) -> AppResult<RatingSyncReport>`. Define UUID RatingIntentId; RatingWrite includes expected observation/precondition and desired value; write mode Disabled/Conditional/BestEffort is explicit. Bind intent to server+asset+JPEG identity+group, not an arbitrary cached path.

- [ ] Write `outbound_disabled_until_configured`, `one_sided_and_equal_changes_converge`, `both_changed_conflict`, `lost_response_then_new_remote_value_not_overwritten`, `unknown_initial_values_conflict`, `evicted_raw_not_downloaded_for_rating`, `readonly_key_keeps_mapping_usable`, `schema_upgrade_disables_writes`, `rating_save_does_not_clobber_pending_adjustments`: assert durable pre-send intent, safe read-back, no blind retry, preserved history and no new RAW transfer.
- [ ] Run `cargo test --test ratings_sync`; observe failure.
- [ ] Implement real tested API update DTO and confirmation semantics from the pinned schema. Determine conditional-update support from evidence; don't send an invented ETag header and claim CAS. If absent, implement explicit BestEffort plus residual-race status/documentation. Publish logical observations/derived sidecar versions through T12; local incoming updates remain T14 work. Only poll mapped assets initially; new/unmapped photographs discover ratings on URL resolution, not a full-library scan requirement.
- [ ] Run `cargo test --test ratings_sync` with local HTTP server delaying/reordering outcomes and mock outbox crashes; count original transfers to prove zero for metadata-only updates.
- [ ] Commit: `feat: synchronize ratings through durable versioned intents`.

## Task 18: Resumable bootstrap, adoption and local recovery

**Files:** Create `src/import/bootstrap.rs`, `src/catalog/{maintenance,verify,conflicts}.rs`, `tests/bootstrap.rs`; extend archive commands.

**Interfaces:** `BootstrapCoordinator::start(request: BootstrapRequest) -> AppResult<JobId>`; `BootstrapCoordinator::advance(job: JobId) -> AppResult<BootstrapReport>`; `CatalogRecovery::rebuild(request: RebuildRequest) -> AppResult<RecoveryReport>`; `IntegrityVerifier::audit(scope: VerifyScope) -> AppResult<IntegrityReport>` uses worker-mediated read-only archive access and local checks. BootstrapRequest names archive identity/scope, bounded batches and operation ID; RebuildRequest preserves existing DB/unknown local bytes before creating replacement state and assigns fresh writer identity where needed. Define `ConflictResolution` variants ChooseHead { version }, KeepAlternates, AdoptEqual { digest }, ChooseDistinctPath { path }, AssociateGroups { groups }, KeepIndependent { groups }, SelectPrimaryRaw { member }, SelectRatingAuthority { asset }, AssociateSidecar { sidecar, group }, RetainUnassociated { sidecar }; every request carries conflict ID and expected revision/heads. Validate identity/evidence and preserve bytes; user-selected associations record that provenance.

- [ ] Write `resume_does_not_repeat_completed_metadata`, `legacy_watermark_ignored`, `edited_jpeg_never_overwritten`, `video_ori_orig_unknown_preserved`, `sidecar_snapshotted_before_adoption`, `source_overlap_with_unbootstrapped_path`, `database_rebuild_preserves_unknown_local_bytes`, `cloned_profile_new_writer`, `competing_bootstrap_idempotent`, `offline_rebuild_when_service_cannot_open_db`, `offline_maintenance_refuses_active_service_or_worker`, `every_ambiguity_resolution_preserves_bytes`, `verify_reports_same_size_corruption_without_repair`: assert no original path changes, all formats retained, conflicts explicit, incomplete scans do not authorize deletion, and pins/private offline data not falsely declared restored.
- [ ] Run `cargo test --test bootstrap`; observe failure.
- [ ] Implement bounded inventory/hash/metadata checkpoints, conservative pairing and initial version adoption under publication coordination. Unknown files remain untouched with reported counts. Rebuild shared facts from immutable records; inventory retained local bytes as protected observations. Provide explicit backup/rebuild entry points, including offline maintenance under the same profile lock when the normal service cannot start. Preserve stopped DB/WAL/SHM and local-only bytes; refuse concurrent active or unreaped owners, do not downgrade a newer schema, and rebuild into a new location. Implement all typed ambiguity resolutions so safe independent/unassociated data can eventually leave protected state after archival; never discard a corrupt database automatically.
- [ ] Run `cargo test --test bootstrap`; interrupt/resume across several batches and inspect actual files/digests, not just row counts.
- [ ] Commit: `feat: bootstrap existing archives without reorganizing media`.

## Task 19: Versioned local protocol and complete CLI

**Files:** Create `src/cli.rs`, `src/service/{mod,protocol,client,server,status}.rs`, `tests/cli_protocol.rs`; replace provisional main entry point.

**Interfaces:** `Request { protocol_version: u32, request_id: OperationId, command: Command }`; `Response { protocol_version, request_id, outcome: ResponseOutcome }`; `Client::send(request: Request) -> AppResult<Response>`; `Service::run(config: Config) -> AppResult<()>`; `StatusSnapshot` has stable serialized fields specified below. Async signatures may add `async` consistently if Tokio is chosen; never block the runtime with NAS filesystem calls.

Commands: `init`, `archive init`, `service run/start/status`, `bootstrap`, `import`, `pin`, `unpin`, `refresh --editor-closed`, `clean`, `status --json`, `jobs show/wait/cancel`, `conflicts list/resolve`, `verify`, `archive recover-lock`, `catalog backup/rebuild`, `doctor`, `qualify` (T22). Configuration init may execute before a service exists under setup lock; ordinary mutations go to the single service. Explicit `catalog backup/rebuild --offline` is the section-25.12 exclusive maintenance exception for an unavailable service, not a second concurrent writer. Service start is explicit, not a hidden second writer.

Status fields: schema/protocol/profile/writer/archive IDs, observed archive identity/availability, capabilities with reasons, budget/reserve/filesystem free, resident/pinned/protected/evictable/staging/metadata bytes and remaining reservations, jobs/milestones/reasons, group membership revision/readiness/current metadata, unresolved conflicts, last successful operations, watcher recovery and worker state. Byte categories may overlap semantically; return both exclusive totals and labeled overlapping subsets so callers do not sum them as independent occupancy.

- [ ] Write `one_service_owns_profile`, `different_profiles_cannot_own_same_working_root`, `socket_permissions_and_peer_user`, `request_retries_same_job`, `oversized_or_wrong_protocol_rejected`, `exit_codes_distinguish_accepted_completed_blocked`, `disconnect_does_not_cancel`, `dry_run_no_markers_jobs_pins_or_reservations`, `json_has_required_fields`, `legacy_flags_fail_explicitly`: assert actual CLI responses/files and no secret in output.
- [ ] Run `cargo test --test cli_protocol`; observe failure.
- [ ] Implement client/server dispatch, bounded NDJSON, structured errors, job wait/cancel and formatting. Keep app operations reusable from future UI without HTTP. Do not access catalog from an independent CLI process. Wire every supported command to its real coordinator, including every `ConflictResolution` variant from T18, not only version/path collisions. Refresh application requires its live foreground session; reject applying refresh with `--no-wait` and revoke authorization after disconnect/wait/restart.
- [ ] Run `cargo test --test cli_protocol` by launching built binaries on temporary profiles; check exit codes 0/2/3/4/5 and `--no-wait` job IDs. Test user-facing commands, not only serialization.
- [ ] Commit: `feat: expose archive operations through a local service CLI`.

## Task 20: Continuous scheduling, reconnect and startup recovery

**Files:** Create `src/service/{scheduler,recovery}.rs`, `tests/service_flow.rs`, `tests/service_recovery.rs`; finish coordinator wiring.

**Interfaces:** `Scheduler::tick(now: Timestamp) -> AppResult<TickReport>`; `Scheduler::handle(event: ServiceEvent) -> AppResult<()>`; `ServiceRecovery::restore(catalog: &mut Catalog) -> AppResult<RecoveryReport>`. ServiceEvent includes command, local notification, worker completion, connectivity and timer. The coordinator alone changes catalog/reservations, with bounded async tasks for local work and one bounded NAS worker.

- [ ] Write `offline_import_edit_restart_reconnect_archive`, `held_recent_cache_uses_direct_import`, `service_down_saved_edits_caught`, `startup_reconciles_partials_reservations`, `hung_nas_status_and_local_work_respond`, `old_worker_cannot_resume_after_recovery`, `persistent_failure_backoff`, `queued_local_work_not_starved_by_poll`, `fresh_url_unavailable_does_not_block_import`, `all_commands_have_real_handlers`: assert end-to-end durable data and no busy loop, no replacement publisher while uncertainty remains.
- [ ] Run `cargo test --test service_flow --test service_recovery`; observe failure.
- [ ] Wire timers (2/60/300/600-second policies), polling, event-driven work, retries and startup observation. Resume dirty jobs before eviction; catch up shared records before publication; retain open-file/activity limitations. Watch only local tree; remote polling is worker-mediated. Shutdown requests cancel/reap safely but never wait indefinitely before reporting a stuck worker.
- [ ] Run both targets; simulate offline by adapter fault, not the developer's network. Include real temporary files, SQLite, subprocess CLI/worker and HTTP fixture server in at least one whole-flow test. State boundary mocks should inject failure, not replace all real implementations.
- [ ] Commit: `feat: run resilient background import and edit synchronization`.

## Task 21: Fault matrix, reconstruction and performance regressions

**Files:** Create `tests/{crash_matrix,archive_rebuild,performance_contract,concurrency}.rs`; expand support/faults; fix discovered production issues in owning modules.

**Interfaces:** Extend `TestRig::restart_service()`, `TestRig::kill_worker_at(point)`, `TestRig::assert_preserved(expected: &[ExpectedBytes])`, `TestRig::metrics() -> Metrics` (hash bytes, EXIF invocations, transfer bytes, per-stage elapsed). Actual kill/restart process tests complement injected returned errors.

- [ ] Write table-driven `every_durable_boundary_restarts_without_loss` for every FaultPoint with RAW+JPEG+sidecar/reset; `rebuild_matches_committed_state`; `two_clients_same_import_and_divergent_edit`; `corruption_blocks_publish_and_clean`; `repeat_import_zero_exif_zero_copy`; `catalog_scan_does_not_refetch_seen_records`; `bounded_batches_and_queue_memory`. Assert conserved originals/history, no ghost readiness, idempotent commit IDs, alias consistency and preserved local-only work.
- [ ] Run `cargo test --test crash_matrix --test archive_rebuild --test performance_contract --test concurrency`; observe failure for missing coverage/behavior, then fix defects with named regressions.
- [ ] Add digest/source-read/EXIF/transfer/journal timings and a reproducible benchmark harness on generated fixtures. No unmeasured wall-time target; tests assert call counts and boundedness. Exercise same-length corruption, disk-full, source removal, unknown schema, missing dependency, malformed sidecar, case collision and incomplete scans.
- [ ] Run the four targets and `cargo test --all-targets`; all required automated tests pass. Report any optional tool-dependent tests separately with exact reason; do not skip invariant tests because they are inconvenient.
- [ ] Commit: `test: exercise interrupted archive operations and repeat-import costs`.

## Task 22: Executable deployment qualification and capability gates

**Files:** Create `src/qualification/{mod,report,nas,rapidraw,immich}.rs`, `tests/qualification.rs`, `docs/compatibility.md`; wire CLI commands.

**Interfaces:** `CapabilityReport { schema_version, capability, backend_or_application, tested_version, mount_identity, config_fingerprint, scope, results, limitations }`; `qualify_nas(request: NasProbeRequest) -> AppResult<CapabilityReport>`; `qualify_rapidraw(request: EditorProbeRequest) -> AppResult<CapabilityReport>`; `qualify_immich(request: ImmichProbeRequest) -> AppResult<CapabilityReport>`; `CapabilityRegistry::allows(operation: OperationKind) -> CapabilityDecision`. Reports distinguish source/fixture evidence from actual runtime evidence, and single-client from two-client scope.

- [ ] Write `unqualified_production_write_blocked`, `local_test_backend_cannot_certify_network_mount`, `single_writer_archive_rejects_other_publisher`, `writer_mode_transition_requires_quiescence`, `single_client_not_multi_client_pass`, `version_or_mount_change_invalidates_evidence`, `readonly_doctor_never_writes`, `probe_requires_explicit_scratch_and_asset`, `probe_failure_retains_evidence_and_preserves_originals`: assert real guards at publication/rating paths, structured reasons, and no personal default roots/URLs.
- [ ] Run `cargo test --test qualification`; observe failure.
- [ ] Implement explicit archive publication modes and protected/journaled mode transitions from section 25.12. Implement NAS scratch probe for exclusive init/no-replace/rename/lock/flush/reopen, coordinated second-client handoff and disconnect/recovery observations. Immutable test object IDs allow each client to verify exact publications. Missing second client returns incomplete, never pass. Probe cleans only explicitly created scratch artifacts when no uncertain writer remains.
- [ ] Implement editor probe comparing user-provided isolated native sidecar saves/reopens against adapter expectations, and optional Immich probe on an explicitly identified isolated asset for 0->3->5->0/readback/schema behavior. Runtime GUI steps can require operator interaction and remain unrun during unattended code build; validators must execute comparisons and emit results, not be prose-only placeholders. Preserve/restore isolated test state when possible; uncertain outcomes reported. No production mutation is implicitly authorized by `doctor` or normal startup.
- [ ] Run `cargo test --test qualification` and isolated local-backend probe. Write compatibility table with fixture-verified profiles, runtime-tested profiles, unsupported/unknown versions and residual non-CAS race. Do not assert actual SMB/Immich/RapidRAW qualification absent results.
- [ ] Commit: `feat: gate production integrations on explicit qualification evidence`.

## Task 23: Packaging, migration documentation and final review

**Files:** Create `packaging/photo-sorter.service`, `config.example.toml`, `docs/{usage,recovery,migration}.md`; update `README.md`, `run.sh`, `.github/workflows/rust.yml`; create `tests/packaging.rs` only for meaningful runner/config behavior.

**Interfaces:** User-operated systemd unit starts `photo_sorter service run --config <explicit-profile>` via documented installation substitution; no hard-coded developer binary path. `run.sh` keeps quoted configurable card path and invokes `photo_sorter import` using an explicit profile; it never creates a profile/archive or falls back silently to legacy flags. Configuration example parses through the real Config loader in a temp-root test.

- [ ] Write `runner_quotes_space_in_camera_path`, `runner_missing_config_does_not_initialize`, `example_config_parses_with_temp_roots`, `unit_points_to_documented_entrypoint`. Test invocation using a fixture executable, not production source/NAS paths.
- [ ] Run `cargo test --test packaging`; observe failure.
- [ ] Document offline import/edit cycle; local vs archive completion; `pin` and `refresh --editor-closed`; retained sidecars; failed/waiting jobs; conflict/reset recovery; abandoned-lock shutdown preconditions; copied profile identities; database backup/rebuild; credentials/permissions; qualification; no-CAS limitations; GIMP/native Immich exclusions. Migration includes stop/exclude old Syncthing/importer before new cache, explicit archive init/bootstrap, dry run, validation, then user-installed service/runner cutover. Keep legacy runnable by explicit binary without implying concurrent safety. Update README's obsolete timestamp/performance claims.
- [ ] Update CI for `cargo fmt --check`, `cargo clippy --all-targets -- -D warnings`, `cargo test --all-targets`, installing rsync/ExifTool in the CI runner if needed. Preserve push restriction to main and PR main; never run CI on `meta/superpowers-plans`. If baseline lint debt exists, fix narrowly in touched legacy isolation or document exact justified exclusion; do not disable new-code checks globally.
- [ ] Run `cargo fmt --check`, `cargo clippy --all-targets -- -D warnings`, `cargo test --all-targets`, `cargo build --release --bins`, and `git diff --check`. Inspect every exit code. Run shipped CLI smoke flow on temp roots. Validate generated unit syntax with `systemd-analyze verify` if available, recording availability without installing it.
- [ ] Commit: `docs: package the offline archive workflow and recovery procedures`.
- [ ] Request the executing-plans workflow's fresh whole-branch review against spec sections 19/21/25 and this task coverage. Reviewer inspects actual diff/test evidence, not just author summary. Fix all correctness/safety blockers with focused RED/GREEN tests and affected full checks. Record limitations and any remaining nonblocking issues.
- [ ] Verify implementation worktree clean and all tasks implemented; preserve user's original main/untracked work. Leave implementation commits on `feat/offline-photo-archive`; do not merge, publish, install or enable production services. Final report: branch/path/head; verified test commands/results; features delivered; source-backed vs runtime-qualified adapters; exact remaining deployment probes and user cutover steps. Include runtime docs links, not a claim that deployment gates passed.

## Spec-to-task coverage and self-review

| Spec sections | Owning tasks |
| --- | --- |
| 1-5 architecture/workflow | T01, T09, T19, T20, T23 |
| 6/6.1 identity/data | T03-T05, T08 |
| 7 import identity/performance | T06, T11, T21 |
| 8-9 budget/pins/oversized | T07, T11, T14, T15 |
| 10 transfers | T02, T09, T10, T12 |
| 11 passive catalog/locks | T03-T05, T10, T12, T18, T22 |
| 12-13 editing/conflicts | T12-T14, T16-T17, T20 |
| 14 URL mapping | T15, T19 |
| 15 ratings | T16-T17, T22 |
| 16 real burst/bracket | T06, T08 |
| 17 migration | T18, T23 |
| 18 CLI/status | T19-T20 |
| 19-21 invariants/failures/acceptance | All owning regression tests; cross-system T21 |
| 22-24 gates/choices/evidence | T16-T17, T22-T23 |
| 25.1 defaults/completion | T01, T19-T20, T22-T23 |
| 25.2 roots/blocked I/O | T02, T09, T15, T20 |
| 25.3 jobs/pins/accounting/GC | T03-T04, T07, T14 |
| 25.4 codec/replay/schema/backups | T03-T05, T18 |
| 25.5 publication/projections | T02, T10, T12 |
| 25.6 ownership/ratings/outbox | T12-T17 |
| 25.7 source/layout/bootstrap/cutover | T06, T08, T11, T18, T23 |
| 25.8 CLI/probes | T19, T22-T23 |
| 25.9 formats/readiness/observability | T04, T06-T08, T13, T15 |
| 25.11 wait semantics/lock session | T10-T11, T19-T20 |
| 25.12 sessions/maintenance/resolutions/companions/modes | T06, T10-T14, T18-T20, T22 |
| 25.13 ownership/cancellation/verify/unpin | T02-T03, T07, T09, T18-T20 |

Self-review requirements before finishing: every task's produced interfaces have a producer and consumer; dependencies are implemented before use; no production credential/root is necessary for default tests; no tests replace the whole program with their own model; crash tests inspect preserved bytes as well as database flags; optional runtime probes are clearly separated from required invariant coverage. Unknown application capabilities are exercised through real implemented profile gates, never handled by leaving TODO handlers or unconditional Unsupported returns.

### Self-review clarifications incorporated before handoff

- T10/T20 must implement the section-25.11 lock-session handshake: worker-held lock plus fresh streamed replay/replanning before publication, without giving the worker the local SQLite connection. Add `lock_held_across_replay_and_publish` and `reacquire_requires_new_replay` to `tests/archive_lock.rs`.
- T11/T19 completion semantics follow section 25.11. Add `offline_default_ingested_returns_zero_with_pending_archive`, `archived_wait_returns_waiting_offline`, `pin_pending_refresh_returns_three_with_retained_intent`, and `aggregate_conflict_not_success` to `tests/cli_protocol.rs`. The default CLI must not wait forever for background archiving after a successful local import.
- Test support/domain structs not exhaustively listed here (for example `Timestamp`, `FilesystemId`, `ReservationId`, `SidecarOwner`, `PublicationGuard`) are defined by their owning task with typed fields required by that task. These are internal representation choices, not unresolved product behavior. Cross-task interfaces and serialized version-1 fields remain consistent.
- T01 source extensions and T15 actual JPEG eligibility follow section 25.9; T13 treats binary companions/large `.orig` as opaque. Do not reuse legacy `group_stem` or `MediaKind::Jpeg` to establish photo identity/Immich eligibility.

# Offline-first photo archive manager

Status: Reviewed execution specification; implementation defaults finalized in section 25. Deployment validation remains separate.
Date: 2026-10-25.
Repository: picture_sorter, origin git@github.com:tulku/picture_sorter.git.
Implementation baseline inspected: de1cffa6a785a594c7aebf5de2e2e8716c30a448.
Planning branch: meta/superpowers-plans (orphan; never merge into implementation branches).
Planning worktree: /home/tulku/Repos/picture_sorter/superpowers-plans.
Specification path: /home/tulku/Repos/picture_sorter/superpowers-plans/specs/2026-10-25-offline-photo-archive-design.md.

## 1. Purpose and user workflow

Evolve the existing Rust photo sorter into an offline-capable import, working-copy, and NAS archive manager. The laptop should ingest camera cards and edit recent photographs quickly without retaining the full archive. The NAS is the permanent filesystem archive and exposes only JPEGs to Immich as an external library. RapidRAW edits ordinary local files. The tool transports saved editing data; it does not render photographs.

A photograph normally consists of a camera RAW and camera JPEG, stored in separate RAW and JPEG directory trees. Their files, editor sidecars, and catalog relationship form one managed group for fetching and pinning. Eviction removes the group's immutable originals together but conservatively retains mutable sidecars and local snapshots; retained metadata alone does not make a group available offline. A photograph can legitimately have only one original or have additional associated originals; absence must be represented, not invented.

The user imports a card; edits the local RAW normally in RapidRAW; the service observes saved sidecars and archives them when connected. The user browses older JPEGs in Immich and pastes an Immich photo URL into the CLI to fetch and explicitly pin both originals and their sidecars. Once local files are available, editing does not require NAS or Immich access.

The initial version is CLI only. A background service owns the state and jobs. Its operations and structured status are reusable by a future web UI. There is no initial web server or NAS-side application/service.

## 2. Agreed requirements versus proposed design choices

The following have been agreed in the design conversation:

- Keep the same repository; reuse sound classification/layout code and redesign the import engine.
- Local editing uses RapidRAW only. GIMP work and independently saved exports are outside scope.
- Preserve filesystem editing data rather than use RapidRAW's native Immich editing store.
- NAS remains passive storage; no additional NAS-side service.
- Local operation must work offline while there is enough usable local space.
- Use a 70 GB managed local budget; oversized imports need not fit entirely on the laptop.
- Archive in the background; evict only safely archived copies; never mirror local eviction to NAS deletion.
- Protect unarchived originals and saved edits automatically.
- Explicitly pin archived photographs using Immich photo URLs; no date/folder selection in the first interface.
- Fetch and evict RAW/JPEG pairs together, preserving their separate folder layouts.
- Share star ratings between Immich's indexed JPEG and RapidRAW's RAW where possible.
- Retire the capture-date incremental cutoff in favor of per-file import records.
- Avoid rerunning ExifTool for recognized originals.
- Repair and simplify burst/HDR detection using real photographs and regression cases.
- Reuse rsync for bulk file transfers and implement archive-specific coordination in this tool.
- Use local filesystem notifications with catch-up scans, and poll NAS changes where needed.

The rest of this document makes design decisions concrete for review. The user subsequently requested review, required corrections, and an unattended implementation plan; section 25 now fixes the defaults needed for that build. These are explicit implementation defaults selected for the requested plan, rather than claims about production validation. Independent review also established two safety boundaries: retain mutable sidecars during ordinary cache eviction, and apply incoming changes to existing local sidecars only at an explicit editor-closed session boundary. Outgoing saved-edit archiving remains automatic.

## 3. Approaches considered

### A. Managed filesystem working copies (selected)

Keep RAWs, JPEGs, and RapidRAW sidecars in ordinary directories locally and on the NAS. Local databases track pending work and cache residency. Immutable shared catalog records on the NAS communicate archive information between computers. rsync handles selected file transfers.

Benefits: offline editing; archive remains usable outside Immich; consistent edit storage; explicit space management; no NAS daemon.

Costs: implement import identity, transfer lifecycle, metadata reconciliation, grouping, and recovery; mapping Immich JPEG URLs to RAWs needs a catalog bridge.

### B. RapidRAW native Immich editing (rejected for this version)

RapidRAW's development branch can download originals on demand, evict cache entries, and exchange edits across computers. However, it stores those edits in Immich custom asset metadata, does not fully solve offline library browsing/local import, and does not implement the desired normal Immich rating round trip. Mixing it with file-managed edits would create two editing stores. The latest release inspected, v1.6.6, does not include the integration.

### C. Syncthing mirror or shared live NAS database (rejected)

A mirrored photo tree couples local deletion to remote deletion, conflicting with cache eviction. Copying live database files between computers introduces database consistency and concurrent-writer problems. A directly shared SQLite database also makes offline availability and network-filesystem locking part of every operation. Synchronize committed archive records instead.

## 4. Non-goals

- Browser UI, remote multi-user administration, mobile client, or rendering service.
- Native Immich editing through RapidRAW and conversion of Immich custom rapidraw metadata.
- Editing the same photograph from two computers simultaneously.
- General-purpose bidirectional directory synchronization.
- Automatic deletion of originals from the NAS or camera cards.
- Automatic reorganization or renaming of the existing NAS archive.
- Synchronizing all RapidRAW settings, albums, presets, model downloads, or application caches.
- General JPEG/GIMP editing support or tracking arbitrary exports outside managed roots.
- Unlimited offline imports despite finite local capacity.
- Treating a NAS copy as protection against NAS failure; archive backups are a separate operational responsibility.
- Inferring HDR from appearance or exposure differences alone when camera metadata is unavailable.

Existing videos and supported media must remain accessible and safe through migration. A supported video can be managed as a singleton group; the tool must not silently drop VIDEO files merely because paired still photography is the main workflow.

## 5. Architecture and deployment

### 5.1 Laptop service

A single service instance per local profile owns the database, watches managed files, plans transfers, reconciles archive records, and enforces storage policy. Use a user-session service on Linux; propose systemd --user with automatic restart. No root privileges are required.

The CLI communicates over a local Unix socket restricted to the user's account. Requests and responses carry protocol versions and stable machine-readable fields. Human terminal formatting is a client concern. Do not add HTTP solely for a future web UI.

The service uses a local SQLite database on local storage. It may use WAL locally. Never place live database files or SQLite WAL/SHM files on the NAS or synchronize them through Syncthing.

If the service is unavailable, the CLI reports that clearly and may offer an explicit start command; it must not launch an independent concurrent database writer. Already fetched files remain editable in RapidRAW with the service stopped. Startup reconciliation detects saved edits made while it was stopped.

### 5.2 NAS

The archive is an existing mounted SMB/NFS directory. It holds media, current sidecars, hidden versioned metadata, and immutable shared catalog records. The tool needs no NAS-installed executable, SSH access, rsync daemon, or application database server.

The current configured archive path is /mnt/common/Camera/OM1-II/. The current camera mount in run.sh is /run/media/tulku/OM SYSTEM/. These are existing defaults to migrate/configure, not hard-coded universal paths. The local managed working root is a new configurable directory chosen at setup; do not reinterpret the NAS mount as the local working root.

The inspection environment reports a CIFS mount and read-only access. Deployment must verify actual read/write capabilities; do not change NAS exports, mount configuration, or permissions as part of implementation without explicit authorization.

### 5.3 Immich

Immich indexes JPEGs only, through its existing external library. The tool calls the configured Immich API to resolve photo URLs and reconcile ratings. Ordinary import, local editing, NAS file transfers, and local cache management do not depend on Immich being available.

Use normal folder mode in RapidRAW for all managed editing. A user choosing RapidRAW's native Immich editor would be outside the supported edit lifecycle and should be documented as such.

## 6. Terminology, identity, and local data model

Distinguish these entities; implementations may normalize tables differently:

| Entity | Required information |
| --- | --- |
| Archive | Stable random archive ID, format version, configured mount/root mapping |
| Computer | Stable random writer ID, local profile ID, monotonic publication sequence |
| Photograph | Stable group ID, original members, camera/capture identity hints, pairing confidence |
| Original blob | Cryptographic content digest, length, media role, validated metadata extraction result/version |
| Placement | Original identity, stable archive-relative path, local path when resident |
| Association | Sidecar owner, editor/type, virtual-copy ID if relevant, exact filename |
| Mutable version | Logical sidecar ID, content digest, immutable history object, parent version(s), publication origin |
| Source observation | Device/card identity when available, source-relative path, size, timestamp precision/values, digest association |
| Local residency | Present, missing, staging, partial, dirty, conflict, protected, explicit pin |
| Job | Stable ID, operation, dependencies, attempt state, progress, retry reason |
| Immich mapping | Configured server/library identity, JPEG asset UUID, associated JPEG original and photograph |
| Rating state | Last common rating, local observation, remote observation, pending intent and conflict state |
| Shared commit | Commit ID, writer/sequence, parent dependencies, schema version, operation records and payload digest |

Content digests identify immutable bytes; group IDs identify logical photographs. A RAW and JPEG are different blobs even when they represent one photograph. A catalog commit ID is not a content hash or an Immich UUID.

Version 1 uses BLAKE3 with the section-25 encoding. Streaming implementation remains a builder choice; changing the wire digest algorithm requires an explicit format version change. Do not use filenames, file lengths, or capture timestamps alone as proof of byte identity.

Treat metadata fingerprints as a performance cache, not a cryptographic identity guarantee. Source fingerprints include the archive/source context and timestamp precision; do not assume FAT timestamps have nanosecond accuracy or that volume labels uniquely identify cards.

A photograph's RAW/JPEG relationship can use same original stem, camera identity, capture metadata, and relative directory context. Filename collisions, reused counters, imported exports, multiple cameras, and equal capture seconds require explicit ambiguity handling. Do not join two distinct shoots solely because their stems match.

Persist actual archive paths. Resolve an Immich JPEG to its group through catalog mappings, not by blindly replacing JPEG with RAW in an arbitrary string.

### 6.1 Canonical photograph identities and aliases

Offline imports allocate provisional local group IDs. Before shared publication, a client acquires publication coordination and incorporates every complete shared commit visible under that lock, then replans identities and expected version parents against that state.

An identical digest at the same intended archive placement is evidence that a retry or overlapping-card import refers to an already recorded member. Reuse the existing canonical group ID, or publish an explicit alias from the provisional ID to it. Byte equality alone at a different placement does not merge logical photographs or independently edited instances. Content-addressed blobs may be shared while groups, placements, and sidecar ownership remain distinct.

If independently published RAW-only and JPEG-only groups are later proven to represent one photograph, publish a merge/alias record under the lock after checking pairing evidence and placement consistency. Ambiguous matches require review. Among already published canonical groups being merged, choose the lexicographically smallest group UUID as survivor; newly created provisional IDs never displace an existing published group. This ordering is independent of clocks, discovery order, and directory listing order. When a later proven merge introduces a smaller published UUID, publish an alias from the larger survivor to the smaller one; immutable old records continue resolving through acyclic aliases. Validate aliases against cycles and conflicting member ownership.

Merging preserves every original placement, source observation, local pin/protection, Immich mapping, and sidecar version lineage. Different edits or ratings remain explicit conflicts; a merge is never permission to overwrite them. Replay and local queries must resolve aliases identically. Local queued jobs are rebased to canonical identities without losing local-only bytes. A different-content destination or incompatible independently edited instance is not accepted as an identical duplicate.

The publisher must not allocate a second canonical group from a stale pre-lock snapshot. Even with one publisher lock, this fresh-state/canonicalization step is required.

## 7. Source recognition and incremental import

### 7.1 Normal import

1. Enumerate supported source files under DCIM when present, otherwise the selected directory; read basic attributes without invoking ExifTool.
2. Compare source observations to the local catalog and already incorporated shared records.
3. Skip recognized and accounted-for originals without EXIF extraction. A known pending original remains a pending transfer, not a duplicate import.
4. Hash uncertain candidates to identify bytes already known from another card or path.
5. Extract required metadata only for genuinely new originals, ambiguous pairing, or explicit metadata-version upgrades.
6. Record intent locally before creating externally visible destination files.
7. Choose local or direct-NAS transfer according to capacity and reachability.
8. Verify and record completed local/archive versions independently.

A missing local copy of a verified archived original is a cache miss, not an import gap. A capture time older than every archived file is still eligible if the original is unknown. Equal timestamps, clock resets, card switches, and overlapping mirror cards must not skip new originals.

### 7.2 Fast versus strict recognition

Normal mode may trust unchanged observations of immutable camera originals. This is a declared optimization: source bytes changed while preserving all fingerprint attributes can evade that shortcut. An explicit strict/verify mode rehashes source bytes.

Across a previously unseen card, common fingerprints are candidate matches, not proof; confirm ambiguous or untrusted observations by digest. The first encounter may require reading the whole file for hashing, but must not run EXIF once per known byte-identical duplicate.

New-file hashing can be streamed by tool-owned reads, but rsync subprocess transfers do not expose their byte stream to the tool. Do not promise that hashing is free or always piggybacks on rsync. Measure and document extra source/destination reads.

### 7.3 Source ownership and resumption

Never delete camera originals. If the card disappears, preserve job/source identity and partial-transfer state; resume only when the corresponding source is available and validated. The service must not mistake a different mounted card with the same volume label for the previous source.

The legacy .photo_sorter.json watermark is ignored by the new engine. Preserve it during migration; do not use it to declare unknown files already imported. The untracked fill_gaps.sh in the implementation checkout is user-owned and must not be modified, staged, or deleted.

## 8. Local budget, pins, and admission control

Proposed interpretation of the user's 70 GB: 70 GiB (70 * 1024^3 bytes), explicitly labelled in the CLI and configurable. Include managed original files, sidecars, managed staging files, and partial transfers in occupancy. Local catalog/log sizes must be reported separately and still respect the free-space reserve; do not conceal them.

Minimum free-space reserve: 5 GiB, configurable. This is the section-25 implementation default, not a user-specified number. Account for destinations sharing the same filesystem; use actual free space, reservations, and bounded transfer concurrency.

Three independent dimensions:

- Pending protection: an original or latest saved editing version is not verified on the NAS.
- Explicit pin: the user requests continued offline availability on this computer.
- Recent cache: no protection or pin; eligible for eviction oldest first.

Pins are local to a computer. Archive identities and versions are shared. Newly imported, unarchived originals are automatically protected, without requiring Immich indexing. Saved edits renew protection even for an already archived original. Pending protection ends automatically only after the exact saved version is verified and cataloged; explicit pins end only through unpin.

The budget is a target/admission limit, not permission to destroy protected data. Changes made by an external editor can grow a protected working set beyond the target. Report that condition, stop further admission as needed, and never discard those bytes.

Evict immutable original bytes only, treating all known originals in a photograph as one eviction unit, after the originals/current observed sidecars are verified archived and there is no known dirty state, pending transfer, unresolved conflict, explicit pin, active import/fetch, or recent activity hold. Retain current mutable sidecars, pending snapshots, and their parent directories; count retained metadata against the local budget even after original eviction. This prevents delayed watcher delivery from causing unlink of the only saved editing bytes. If retained metadata itself fills the budget, report/block admission; ordinary clean must not delete it. Proposed activity hold: 10 minutes after observed local access/edit activity; file watchers do not reliably prove whether an editor has an image open. Pinned images are the reliable way to retain a working set. Open originals may remain allocated by the OS after unlink; freed-space accounting must use observed filesystem capacity. The activity hold is a usability measure, not a proof against external-writer races. The supported workflow treats originals as immutable: configuration/options that edit RAW/JPEG original bytes in place are outside the automatic-eviction contract and must be detected/reported when observed.

Order eligible groups by last deliberate fetch/import or completed saved editing activity, oldest first; do not rely on camera timestamp or incidental background reads. Use deterministic tie-breakers. There is no per-date/per-folder pin interface initially.

On an explicit pin request that cannot fit after eligible eviction, fail or keep the request in an explicit blocked state with bytes required; do not partially mark the group pinned and complete. Multiple URL requests can progress independently, with per-group results.

If one group is itself larger than usable local space, direct-NAS import remains possible, but pinning/editing it locally is blocked until enough space is available. Do not split a RAW/JPEG pair's completed offline availability merely to declare success. Eviction may be interrupted between individual unlinks; persisted group eviction intent marks the entire group unavailable until recovery resolves it, and status reports actual remaining bytes. Retained sidecars are metadata, not evidence of original residency.

## 9. Imports exceeding local capacity

Prefer local ingestion while admission permits, so editing can start promptly. Process imports as bounded batches rather than staging the full card.

When local pressure grows:
1. Evict eligible archived groups.
2. Archive pending local batches if the NAS is reachable.
3. If local admission remains impossible and the NAS is writable, transfer remaining originals directly from the camera card to NAS staging.
4. If neither destination can accept data, pause with a clear actionable status; never silently exceed reservations or delete protected files.

An import can therefore exceed 70 GiB. Once local space fills, throughput depends on archival/direct-transfer speed. Direct-NAS files enter the shared archive and may be fetched later; do not automatically download the entire completed import again.

If the NAS disconnects during direct transfer, retain partial state and pause. If space recovers locally, the scheduler may switch a not-yet-started group to local ingestion; changes of destination must preserve job identity and avoid double accounting.

If RAW arrives in one import and JPEG in a later one, attach the member after validating the relationship and retain the existing group's stable paths. Group completeness is relative to known/declared members, not an assumption that every RAW must eventually get a JPEG.

## 10. Transfer, verification, and commit protocol

rsync is the bulk transfer engine between local paths, including a mounted NAS share. The tool selects explicit bounded file lists; it does not repeatedly mirror the archive tree. No --delete, --remove-source-files, or general destructive mirroring. Do not use --inplace against visible archive originals or sidecars.

Stage transfers in a tool-owned hidden area on the destination filesystem. Temporary names and transfer IDs are unique. Configure Immich exclusions for tool metadata/staging if its external-library roots would otherwise traverse those paths.

For immutable originals:
1. Persist job and expected identity.
2. Transfer to staging with recoverable partial handling.
3. Confirm size and strong destination digest against the expected immutable source.
4. Under archive coordination, ensure the final path is absent or has identical verified content.
5. Publish by same-filesystem rename/no-replace operation; a different existing file is a conflict, never an overwrite.
6. Publish the immutable archive catalog commit only after all group members promised by that commit are verified and in place.
7. Mark the corresponding local version archived. Recheck local edits before releasing protection.

rsync exit success alone and matching byte lengths are insufficient for the tool's durable verification claim. Record the verification scheme; acknowledge that hashing over a mounted share reads bytes back through the network/client caches and does not certify NAS power-loss durability or bit-rot immunity. NAS snapshots/backups remain separate.

For mutable sidecars, first create a coherent stable local snapshot, assign a version with expected parents, and store immutable history bytes on the NAS. Publish/update the live .rrdata representation only under the sidecar ownership/conflict protocol below. Journal each step for recovery. Never mark a newer local edit clean merely because an older snapshot finished uploading.

Partial group publication may leave verified orphan files after a crash; recovery reconciles them by digest before publishing a commit or declaring a conflict. Multi-file filesystem updates are not atomic. Readers and catalog recovery must distinguish verified individual files from a committed group.

Retries are idempotent by job IDs, blob digests, and commit IDs. Preserve errors and use bounded backoff. A cancelled job never silently removes the only original or saved editing version.

## 11. NAS file catalog and coordination

Proposed hidden namespace under the archive root: .photo-sorter/. Keep it outside Immich indexing. Exact filenames may be adjusted by the builder; semantics are required.

- archive.json: stable archive ID, format/schema version, digest conventions.
- commits/<writer-id>/<sequence>-<commit-id>.json: immutable committed archive changes.
- versions/<digest-prefix>/<digest>: immutable sidecar/rating-observation history payloads.
- staging/<writer-id>/<job-id>/: incomplete transfers/publications.
- locks/: short-lived exclusive publication coordination.
- snapshots/: optional future/rebuild acceleration; not a prerequisite for correctness.

Each commit includes schema version, archive ID, unique commit ID, writer ID/sequence, referenced parent versions/dependencies, records, and a payload digest. Timestamps are informational; causality and parent IDs determine version relationships. Clock order is not conflict resolution.

A client writes a complete temporary commit, flushes/closes it, and atomically renames it into place on the same share. No in-place rewriting of an existing commit. Synchronization imports complete supported records idempotently into a local database transaction. Out-of-order records remain pending until dependencies arrive. Malformed or unknown-required-version commits are quarantined/reported; never interpreted as deletion or silently discarded.

A minimal initial implementation may enumerate commit filenames on each reconnect and compare against locally stored IDs/high-water information. It must fetch only unseen immutable content. Do not trust a high-water mark alone when a writer has gaps. Periodic snapshot/compaction can be added later; never remove history until every supported reconstruction path remains valid. Report catalog scan costs separately from EXIF work.

Use an atomic server-mediated directory creation or equivalent tested exclusive primitive for a short archive-publication lock. The filesystem adapter must verify required create/rename semantics on the actual share. A lock contains archive ID, writer ID, operation ID, and diagnostic heartbeat. No automatic lock theft purely because of a timeout; clocks, suspend, and network partitions make that unsafe.

If a lock appears abandoned, status explains the owner and blocks conflicting publication. Recovery has a mandatory operational precondition: the previous owner is definitively stopped, all other writer services are stopped/quiesced, and no old transfer/publication can resume. If this cannot be established, recovery must refuse takeover. Inspect staged operations and the journal, retire the abandoned lock, restart one service, reconcile all committed/staged state, then permit other services to restart. A service whose previous operation/lock was administratively retired must discard that in-memory publication attempt and replan on restart.

Heartbeat expiry, checking a token before a write, or renaming a lock directory do not provide fencing against a suspended old process resuming. The passive-share design does not claim that guarantee. Recovery requires operator-enforced shutdown/quiescence, not permission to steal a live or uncertain lock. Non-conflicting local editing can continue subject to space; automatic remote publication remains blocked. Do not invent robust distributed leases without a service/coordinating filesystem primitive.

Lock acquisition alone does not establish fresh cross-client catalog visibility. The filesystem adapter must demonstrate that its post-acquisition reads observe the previous holder's completed publications despite SMB/NFS client caching, and that rename/create visibility meets the protocol. Configure supported cache/mount behavior or a tested fresh-read mechanism as needed; if the required semantics cannot be provided on the actual share, block shared publication and report the unsupported capability rather than proceeding from a possibly stale catalog. Validate this with two actual clients, not only a local filesystem mock.

The lock serializes tool clients' catalog/live-sidecar publication, not Immich or arbitrary external filesystem writers. An absent NAS or archive marker must never be treated as an empty archive. Validate archive ID and mount root before every batch and after reconnect. A mount disappearing and exposing an empty local directory must block archive writes.

The local database is rebuildable for shared archived facts. Retained local metadata remains accounted for and protected independently of original cache residency. Per-computer pins and unarchived data require a separate local database backup/recovery path; do not claim the NAS contains offline work that was never uploaded.

## 12. Transparent RapidRAW saved-edit tracking

Watch local managed folders for .rrdata, .rrexif, and supported associated metadata, including virtual-copy sidecars. Detect newly created directories, moves, replace-by-rename saves, and deletions. Track ownership patterns explicitly; a virtual-copy stem suffix must not be mistaken for a new camera photograph.

Use inotify on Linux, with a library adapter permitted. Debounce save bursts (proposed default 2 seconds), then require stable attributes and a successful parse before snapshotting. A partial JSON write is retried, not treated as a reset.

Notifications are hints. Reconcile managed state at startup, after watcher overflow, after watch reattachment, and periodically. Proposed periodic local reconciliation: every 5 minutes, with the database narrowing work. No periodic EXIF extraction for unchanged originals.

A snapshot checks source metadata/digest around reading; if the file changes, reschedule. Keep the saved bytes, digest, and local version until archive verification completes. RapidRAW need not be instrumented.

Unsaved in-memory editor changes cannot be observed or backed up. Prevent eviction based on explicit pins, detected activity, and dirty/pending state, not a promise to introspect editor memory.

RapidRAW may remove default-only sidecars. A sidecar deletion after an earlier saved edit is a versioned reset intent; it must not delete archived history or imply the photograph was evicted. Delay reset interpretation through the debounce/reconciliation cycle, preserve the prior version, and conflict-check against remote changes before applying it.

Service-controlled eviction/removal has explicit persisted intents so resulting delete events cannot be mistaken for edit resets. Ordinary eviction retains mutable sidecars and their directories rather than unlinking them. Unexpected disappearance of originals is reported as local missing state, never propagated as NAS deletion. Whether the latest saved sidecar can be reconstructed after accidental local deletion is determined from actual history; never clear pending data solely from an absent pathname.

### 12.1 Local mutable-file ownership boundary

Outgoing sidecar snapshots and NAS archiving are automatic. Incoming changes to an existing local .rrdata or other editor-writable sidecar are staged separately; the background service never blindly replaces or deletes that live file while an external editor may be writing it.

The initial supported application boundary is an explicit CLI refresh/apply operation after the user has closed RapidRAW and any other writer of the affected managed files. The command explains this precondition and requires an explicit editor-closed assertion. Process detection may help diagnose a running editor but is not a race-free lock. The user must not reopen the editor until the operation completes. The service serializes its own watcher/snapshot/application work during the operation, ingests a final local snapshot, compares version parents, preserves displaced bytes in local history, and only then replaces the live representation. If quiescence cannot be established or local state changes during the operation, retain the incoming version separately and report blocked/conflict state.

This is a deliberate first-version tradeoff, not a guarantee that prechecks plus rename coordinate an uncooperative editor. Initial materialization of a never-before-created local group likewise requires waiting for pin/fetch readiness before opening it. Re-fetching an evicted group with retained sidecars must compare/stage incoming versions rather than treating the local destination as metadata-free.

Ratings can be observed and uploaded automatically, but an incoming Immich rating that must change a local existing RAW sidecar follows the same application boundary. Status exposes metadata waiting for application. Existing offline-ready bytes remain usable at their recorded local version; do not claim they include unapplied remote changes. A future editor integration/session wrapper could automate this boundary, but is outside the initial scope.

## 13. Sidecar consistency across computers

The intended use is one editing computer per photograph at a time; no permanent reservation across offline sessions is assumed. Before fetching/editing an older photo on another computer, synchronize latest archived versions. Existing resident files with local edits must not be overwritten by an incoming version.

Compare local and remote heads to their last common parent:
- Only local changed: publish local snapshot.
- Only remote changed: fetch/stage the incoming version; apply to an existing local live sidecar only through the explicit quiescent boundary in section 12.1.
- Both equal by digest: converge without duplicate versions.
- Both changed differently: preserve both immutable versions and enter conflict state. Do not choose by modification time.
- Deletion/reset versus edit: a conflict, not automatic loss of the edit.

Proposed conflict interface: list the photograph, versions/origins, and allow selecting local, archive, or preserving a named alternate. Keep conflicts protected from eviction. The builder may map alternates to RapidRAW virtual copies after verifying the current naming format; never rely on unsupported format assumptions.

During incoming sidecar application at the supported quiescent boundary, use same-filesystem atomic local replacement and persist an expected-write digest so watchers recognize the tool's own writes. Preserve the displaced prior bytes before replacement. If external local activity invalidates the boundary or expected prior version, preserve the incoming version as an alternate and report a conflict. A digest precheck is not an atomic compare-and-swap against external writers.

Archive current .rrdata paths are conveniences for ordinary filesystem use. The immutable version records are the recovery history. Imported legacy sidecars receive a recorded initial version before any tool replacement.

## 14. Immich URL resolution, RAW/JPEG mapping, and offline pins

Accept URLs only for the configured Immich server. Parse the /photos/<UUID> asset ID; it is an asset identifier, not a photograph content hash. Do not use arbitrary user-provided URL hosts with stored API credentials.

When connected:
1. Authenticate with a configured API key and query the asset.
2. Require a supported JPEG in the configured external-library archive, or return a clear unsupported/mapping error.
3. Map Immich's container-visible originalPath through an explicit configured prefix mapping to archive-relative paths.
4. Resolve that JPEG via the catalog to the photograph and actual RAW/JPEG/sidecar paths.
5. Persist the asset/group mapping.
6. Check capacity; fetch verified versions into matching local relative paths.
7. Persist explicit pin intent before fetching; mark offline-ready only after the complete required group is locally verified. Report the materialized version and any incoming metadata awaiting explicit application; a refresh/pin request for the latest version cannot silently succeed with unapplied updates.

Do not match originals solely by basename. Confirm identity with available catalog/file/checksum information, accounting for Immich using a potentially different digest algorithm. A previously mapped URL may resolve locally while offline; if originals are missing, leave an explicit waiting-for-NAS state rather than claim offline readiness.

Fresh URLs cannot resolve without Immich unless a complete mapping is already cached. An API outage need not block fetching a known group from the NAS. Group/asset mappings must be refreshable after Immich reindexing or asset replacement; never bind unrelated photos after a path changes.

Pin includes associated originals and current saved editing data, not every frame of a burst merely because a folder contains them. User approval has covered photograph-level pins, not automatic whole-shoot pins. Preserve sequence folder paths when fetching one photograph.

Unpin removes only the explicit retention request; it does not immediately delete files or clear pending protection. clean evicts eligible groups. Both operations can use cached URL mappings while offline.

API credentials live in a local protected secret/config mechanism and never enter shared catalog records, process arguments, or logs. Section 25 fixes the initial credential source to a protected local file. Request only the API permissions needed by enabled operations; validate against the deployed Immich version.

## 15. Shared star ratings

The logical photograph has a single shared 0-5 star rating. For this version, synchronize stars only; tags, descriptions, album membership, and pick/reject semantics are not a generic metadata-sync feature. Distinguish no observation, explicit zero, and rejected (-1) values. Do not silently translate reject into a 0-5 rating or delete a photograph.

Read/write RapidRAW's RAW editing metadata through a version-tested adapter, preserving adjustments, masks, tags, and unknown fields. If multiple known original RAWs exist, define the primary editor member explicitly; ambiguous ownership is a conflict. Camera JPEG ratings in Immich represent the logical group's rating; distinct JPEG variants require explicit mappings.

Current evidence shows copying XMP sidecars alone is insufficient:
- Immich prefers NAME.JPG.xmp while RapidRAW resolves NAME.xmp/NAME.XMP.
- RapidRAW's XMP reader imports stars only when its existing rating is zero.
- Native Immich editing metadata is separate from Immich's normal asset rating.

Proposed reconciliation boundary: use Immich's normal asset-rating API for the indexed JPEG, and a RapidRAW adapter for the local RAW sidecar. Do not blindly copy one application's XMP onto another file. Preserve Immich's JPEG-side XMP as externally managed; the tool does not replace it with an old laptop copy. Use API read-back as well as appropriate metadata refresh behavior for the deployed version.

Keep a last-common rating value, the last rating actually applied to each local projection, and durable pending local/remote observations. A local file save is not by itself a local rating change: compare the rating field to its local-applied baseline, independently of adjustment changes. If incoming rating 5 is waiting while the local projection still says 3, an unrelated exposure edit that saves 3 must not reverse the pending 5. Archive the exact local saved snapshot for recovery, then construct any canonical merged sidecar version from the new adjustments and the reconciled rating without replacing the active local file. Record projection versus canonical version differences explicitly. A deliberate local rating change while remote changes are pending follows the conflict rules, including a deliberate reset to zero.

On reconnect:
- One side changed: reconcile to the changed value.
- Both changed to the same value: converge.
- Both changed differently: create a rating conflict, preserve observations, and request CLI resolution.
- A photo evicted locally still retains the catalog rating; do not redownload RAW bytes merely to observe an Immich star change.
- A rating change from Immich affecting an archived RAW should be archived as a new sidecar version, using existing sidecar history without downloading the RAW original merely to edit JSON.

Applying incoming ratings must not rewrite .rrdata while RapidRAW may be saving it. Stage the change and use the explicit quiescent application boundary in section 12.1; debounce/activity checks are not mutual exclusion. An external editor cannot be forced to reload its in-memory metadata; validate when reopen/refresh is required and document that boundary instead of promising live UI refresh.

IMPORTANT LIMITATION: Immich API support for compare-and-set updates must be investigated against the deployed version. A read-then-write API sequence is not atomic. Without server-side conditional updates, concurrent ratings made in Immich during the tool's short reconciliation window cannot be guaranteed conflict-free. Re-read before and after writes, preserve every observed/intended value, and document the residual race. Do not claim a lossless general multi-writer protocol, write directly to Immich's database, or invent a NAS service to conceal this limitation.

This rating boundary needs an isolated real-version round-trip experiment before implementation can claim compatibility. If reliable safe automatic writes cannot be supported, surface that as a design issue for user decision; do not silently drop the requirement.

## 16. Burst/HDR classification

### 16.1 Confirmed failure

The current get_exif_data function invokes exiftool -fast2 and requests SpecialMode and DriveMode. ExifTool documents that -fast2 skips EXIF MakerNotes. Those required Olympus tags are in MakerNotes. The exact fast read omitted the tags on actual archive photos; a normal read returned them.

The existing algorithm uses two separate passes over date-sorted groups with regexes for Sequence and Shot. Its input extraction failure is established; other grouping edge cases remain unverified. Do not attribute every classification failure to that one cause.

### 16.2 Real examples examined read-only

Burst JPEGs:
 /mnt/common/Camera/OM1-II/JPEG/2026/03/21/P3217330_BURST/P3217330.JPG
 through P3217333.JPG in that same directory.

They report Sequential, Shot 1 through Shot 4; SpecialMode sequence numbers 1-4; capture times 10:57:49, then 10:57:50 for the remaining frames; exposure compensation 0.

HDR/exposure-bracket JPEGs:
 /mnt/common/Camera/OM1-II/JPEG/2026/05/17/P5178585_HDR/P5178585.JPG
 through P5178587.JPG in that same directory.

They report AE Auto Bracketing, Shot 1 through Shot 3; all at 13:58:51; compensation 0, -2, +2; SpecialMode sequence 0.

The corresponding first RAWs under RAW/... with .ORF extensions report matching shooting metadata. Single-shot control _A230938.JPG under JPEG/2026/10/23 reports Single Shot; Mechanical shutter and sequence 0.

Existing folder names are hints, not ground-truth proof for every archive group. Preserve minimal relevant metadata fixtures, avoiding full personal image files in Git. Hash/sample provenance may be stored locally or in test documentation without publishing private images.

### 16.3 Required behavior

Extract camera mode and shot/sequence index with MakerNotes enabled. Prefer stable machine-readable/raw values where available and verified against actual files; do not build grouping primarily on localized presentation strings or exposure guessing.

Normalize metadata into a small domain record (camera, capture ordering, mode, shot index, relevant bracket parameters). Separate extraction, normalization, and sequence grouping. The exact simplification is left to implementation after tests establish expected behavior.

Distinguish exposure bracketing labelled HDR in the current archive from ordinary sequential bursts. Test normal single shots, shot counter reset, same-second frames, missing first/middle frames, multiple cameras, unrelated shoots, missing MakerNotes, and RAW/JPEG agreement.

Persist extracted metadata so overlapping or later imports can complete sequence membership without rerunning EXIF on known originals. Missing frames must not cause an unrelated sequence to be merged. Conservative unclassified/incomplete results are preferable to false grouping.

Stable paths override a later improved classification. Newly arriving members can join an existing recorded sequence folder. If a sequence was not known when its first member was published flat, record updated logical membership without silently relocating that existing member. A future explicit migration can reorganize paths; normal import must not invalidate Immich assets.

## 17. Migration and archive bootstrap

Keep the current implementation checkout unchanged until a separate implementation branch is created at the user-required worktree path. Do not commit specs/plans to it.

Bootstrap is explicit and resumable:
1. Initialize or validate archive identity without modifying existing media.
2. Inventory supported originals and sidecars, recording actual relative paths.
3. Hash originals and extract metadata in bounded batches; use stored results/checkpoints on restart.
4. Establish RAW/JPEG relationships conservatively and report ambiguities.
5. Record existing sidecars as initial immutable versions before managing future writes.
6. Publish verified archive records in batches.
7. Build/refresh Immich mappings as needed rather than requiring a complete Immich scan first.

Do not treat legacy file size alone as proof of a complete copy. Previously edited JPEGs may differ from camera bytes; preserve their archive contents and report uncertain duplicate relationships rather than overwrite them. .orig backups, .ori files, existing editor metadata, and unknown files must not be destroyed.

No automatic archive moves, folder flattening, image metadata rewriting, or deletion during bootstrap. Unknown files remain untouched. Bootstrap may be long and I/O-intensive; report progress and allow safe resumption.

Migrate run.sh only after the new catalog-based path is validated. Legacy flags may provide compatibility aliases or explicit deprecation errors, but --incremental must not keep its old timestamp semantics secretly. Preserve a documented way to run the legacy binary during rollout without enabling concurrent writers on the same managed tree.

## 18. CLI and status contract

Illustrative spelling, subject to repository conventions:

- photo-sorter service run/start/status
- photo-sorter init (configure local root, NAS root, archive identity, budget/reserve)
- photo-sorter bootstrap (read/record existing archive)
- photo-sorter import <card-path> [--dry-run] [--strict]
- photo-sorter pin <immich-url>...
- photo-sorter unpin <immich-url>...
- photo-sorter status [--json]
- photo-sorter refresh <immich-url>... --editor-closed (apply staged metadata at a quiescent session boundary)
- photo-sorter clean [--dry-run]
- photo-sorter conflicts list/resolve ...
- photo-sorter archive recover-lock ... (explicit administrative recovery)
- photo-sorter verify ... (scoped integrity audit, not routine import)

The existing binary is photo_sorter with an underscore. Final naming/aliases are left to implementation; avoid gratuitous breakage.

Human and JSON status must expose: service state; configured/observed archive ID; NAS availability/read-only/wrong-root; Immich availability; budget and free-space reserve; resident/pinned/pending/evictable/staging bytes without double counting; reservations; queued/running/paused/blocked jobs; per-group offline readiness; last successful sync; watcher recovery; errors and conflicts.

An unpin request succeeding is different from freeing bytes. An import reaching local storage is different from being archived. A job accepted is different from completed. Exit codes and structured status must make these distinctions.

Dry-run must not transfer media, publish archive commits, modify ratings, or evict files. Read-only validation may populate only explicitly allowed transient observations; it must not make a real operation appear completed.

## 19. Safety invariants

1. Local removal never implies remote deletion.
2. No automatic NAS or camera-original deletion.
3. No eviction of the only unarchived saved bytes.
4. Archived status refers to a verified exact version, not just a pathname or timestamp.
5. Unknown existing destination content is never silently overwritten.
6. A disconnected/wrong mount is never interpreted as an empty archive.
7. Original identities and capture dates are independent; dates never gate missing imports.
8. Only complete supported immutable catalog records affect shared state.
9. Immutable historical sidecar bytes survive reset/conflict/current-file replacement.
10. One service owns local writes; tool-client publications coordinate on the archive share.
11. External Immich writers are not magically covered by tool locks.
12. Filesystem notification loss cannot permanently hide saved local edits.
13. API secrets stay local and out of URLs/logs/catalog commits.
14. Shared paths are relative and validated against traversal/symlink escape; no remote record may write outside configured managed roots.
15. Unsupported archive schema/identity stops mutation rather than guessing.
16. User files and existing archive paths are not reorganized implicitly.

## 20. Failure and recovery matrix

| Event | Required outcome |
| --- | --- |
| NAS unavailable | Local import/edit continues within capacity; uploads wait |
| Immich unavailable | Normal local work/NAS archiving continues; fresh URL resolution and ratings wait |
| Local space exhausted, NAS available | Evict eligible groups or import directly to NAS |
| Local space exhausted, NAS unavailable | Pause; retain card files and pending work |
| Service crash while copying | Recover unique staging job; never count partials as archived |
| Crash after file rename before catalog commit | Reconcile orphan by expected digest; publish or report conflict |
| Catalog record arrives before dependency | Defer application; retry after dependencies arrive |
| Two computers edit same sidecar offline | Preserve both versions and expose conflict |
| Editor saves during upload | Archive snapshot; keep newer local version dirty/protected |
| Editor saves after eviction precheck | Original cache may leave, but mutable sidecar is retained and caught by reconciliation |
| Incoming metadata while editor may run | Stage separately; apply only at explicit editor-closed boundary |
| Concurrent same-photo offline imports | Reconcile fresh catalog under lock; canonicalize/alias identities and preserve edit conflicts |
| Editor removes default sidecar | Versioned reset after stability checks; preserve history |
| inotify overflow or service downtime | Catch-up scan finds saved changes without EXIF rerun |
| Mount disappears/replaced | Block writes until expected archive identity is validated |
| Stale publication lock | Block publication; recovery only after definitive owner shutdown and writer quiescence |
| Immich JPEG deleted/reindexed | Mark mapping stale; preserve NAS originals and edits |
| Database lost | Rebuild shared archived facts; recover local-only bytes by cautious inventory; report lost private state |
| Malformed .rrdata | Preserve bytes, retry/report; no reset inference or eviction |
| Ambiguous RAW/JPEG identity | No guessed pairing or destructive transfer; actionable review state |

## 21. Validation and acceptance criteria

Implementation must provide meaningful tests using real extracted metadata, a temporary filesystem archive, and fault injection. Do not test only mocked happy paths or mirror implementation details.

### Import identity and performance
- Import overlapping cards in either order; unique older originals are retained and duplicates recognized.
- A repeat import of unchanged known sources invokes zero ExifTool processes and copies zero accounted-for originals.
- An evicted archived original is not reimported.
- Same-name different-content files are preserved as conflicts/distinct identities.
- Group canonicalization produces the same survivor under shuffled record discovery, clocks, and alias replay.
- Strict mode detects a byte change hidden by unchanged fingerprints.
- Interrupted bootstrap resumes without redoing completed metadata batches.
- Report enumeration/hash/EXIF/transfer timings separately on a representative card; set no unmeasured wall-clock guarantee.

### Storage and offline operation
- Import more than the local budget with NAS connected; bounded staging/direct transfer completes without whole-import local storage.
- NAS outage pauses only dependent work; protected local editing continues.
- Explicit pins prevent eviction; unpin does not release pending protection.
- An editor save between cleanliness check and original unlink retains the sidecar bytes and eventually queues them; ordinary clean never deletes those sidecars.
- Retained sidecar/snapshot occupancy can block admission and remains visible after original eviction.
- Oversized pin fails clearly without claiming readiness.
- Budget accounting includes partials/reservations and does not double count shared files.
- Free-space reserve is respected by admission; externally caused overruns are reported, never repaired by deleting protected data.

### Transfer/catalog recovery
- Inject interruptions before/after each staging, verification, publication, and local-ack boundary.
- Replaying catalog commits/jobs is idempotent.
- Two offline imports of identical intended placements canonicalize to one photograph, even with different provisional IDs.
- Split RAW/JPEG published groups can merge through explicit alias records without losing pins, mappings, or divergent edits; same bytes at intentionally different placements do not force a merge.
- Recovery refuses a merely timed-out/suspended owner and requires definitive shutdown; retired attempts cannot resume after restart.
- No automatic operation deletes NAS originals.
- Confirm create/rename/locking semantics on the actual SMB/NFS deployment before relying on them, including cross-client visibility of completed commits immediately after lock handoff.
- Test archive marker mismatch and unmounted-directory fallback.
- Test a pair split over two imports and a collision discovered after partial publication.

### Editor and ratings
- RapidRAW ordinary-folder edit -> saved sidecar -> verified NAS version, without an explicit sync command.
- Saved offline edits survive service restart and later upload.
- RapidRAW atomic replace, deletion/reset, virtual copies, and malformed writes are handled.
- Dirty local .rrdata is never overwritten by incoming archive data.
- Incoming versions stay staged while an editor may be running; an editor save around application prechecks cannot be treated as safe automatic replacement.
- Explicit editor-closed application first snapshots local state, preserves displaced versions, and reports any invalidated quiescence assumption.
- Immich 0->3, 3->5, 5->0 and local reverse changes propagate through the tested adapter.
- Distinct simultaneous/offline ratings become conflicts; document unpreventable API race if no conditional update exists.
- An incoming rating waiting for application followed by an unrelated local adjustment save preserves the pending incoming value; stale unchanged local stars are not treated as new intent.
- Changing rating on an evicted photograph does not fetch its RAW original just to modify metadata.
- Verify actual RapidRAW refresh/reopen behavior and Immich metadata/API behavior on deployed versions.

### Sequences and compatibility
- The observed four-frame burst and three-frame bracket classify correctly from RAW and JPEG fixtures.
- Single shots do not become sequences.
- Separate/missing-frame sequences and split-card imports do not merge unrelated shoots.
- Existing HDR/BURST paths and Immich mappings stay stable across catalog/detector upgrades.
- VIDEO and uncommon supported originals survive bootstrap and remain accounted for.

## 22. Review questions and implementation gates

A fresh reviewer should focus on:
- Whether passive-share publication/locking and immutable records are sufficient under interruption and two computers.
- Whether sidecar deletion/reset, active-editor writes, and eviction are distinguishable in every recovery path.
- Whether the rating adapter can honor the requirement given current RapidRAW parsing and Immich API concurrency limits.
- Whether the catalog can reconstruct grouped archive state without requiring private local state or a NAS daemon.
- Whether budget/reservation calculations permit the oversized-import behavior without unbounded staging.
- Whether source recognition/pairing claims hide false-positive assumptions.
- Whether sequence normalization can remain simple without moving already indexed files.
- Whether migration can proceed without overwriting historic edited JPEGs or breaking Immich identity.

Before enabling the relevant production capability, validate (these are deployment gates, not blockers to building and testing the implementation):
1. Actual deployed RapidRAW version and .rrdata/virtual-copy/reset behavior, including external incoming rating changes.
2. Actual Immich version/API permissions, path mapping, rating read/write semantics, metadata refresh, and conditional-update capability.
3. Actual NAS writable-share rename, exclusive-create, lock, mount-identity, and failure semantics.
4. Configuration and documentation of the section-25 defaults, strict stale-lock recovery preconditions, and explicit rating write mode/concurrency limits.
5. Document the retained-sidecar and explicit editor-closed refresh boundaries before production use. Outgoing saved-edit archiving remains automatic.

Do not install software, alter permissions, call mutating Immich endpoints on personal photos, or reorganize the archive merely to resolve these gates. Isolated fixtures/test directories or an explicitly authorized test photo/library are the appropriate validation environment.

### 22.1 Independent review disposition

A fresh reviewer identified three blockers in the first draft: external-editor save races during sidecar eviction/replacement, stale-lock takeover without fencing, and duplicate logical photograph identities from concurrent offline imports. This revision addresses them through retained local mutable metadata plus explicit quiescent incoming application, operator-enforced writer shutdown before abandoned-lock recovery, and fresh-state canonicalization with explicit aliases/merges. The requested execution-readiness revision fixes their implementation defaults in section 25; production validation is still required. The review is not a claim of implemented safety.

The original review and response are stored beside this spec as 2026-10-25-offline-photo-archive-review.md. A subsequent review should check that the stated operational preconditions are acceptable and that the revised invariants actually follow from them.

## 23. Delegated implementation choices

The builder may select compatible Rust crates, internal data structures, and local notification/socket implementation. Section 25 and the implementation plan now fix public commands, module responsibilities, digest/wire contracts, batching limits, and default polling intervals. Preserve the specified observable behavior and safety invariants.

Suggested module boundaries: catalog/domain, source inventory/metadata, import planner, sequences, transfer adapter, archive journal/coordination, sidecar watcher/versioning, ratings/Immich adapter, storage policy, service, CLI. This is a responsibility map, not a required file-by-file implementation plan.

Do not add a NAS service, enable native Immich editing storage, replace explicit conflicts with last-modification-time wins, or relax archive verification as an implementation detail.

## 24. Source evidence and limitations

- Existing repository: src/main.rs, src/plan.rs, src/classify.rs, src/sidecar.rs, run.sh at the baseline above. Most pipeline behavior lives in main.rs. Existing tests do not cover end-to-end metadata extraction/classification.
- Existing untracked fill_gaps.sh uses --skip-existing; preserve as user work.
- ExifTool documentation: https://exiftool.org/exiftool_pod2.html ; -fast2 omits MakerNotes.
- Olympus metadata tag definitions: https://exiftool.org/TagNames/Olympus.html .
- rsync manual: https://download.samba.org/pub/rsync/rsync.1 ; local-path mode works over mounted shares without NAS rsync installation; transfer checks and ordinary quick checks differ.
- Immich XMP behavior: https://docs.immich.app/features/xmp-sidecars/ .
- Immich API documentation: https://api.immich.app/ .
- RapidRAW inspected development commit: 872804e8a21d4dff707f31fc9dcf5d3128ca663b.
- Native editing metadata: https://github.com/CyberTimon/RapidRAW/blob/872804e8a21d4dff707f31fc9dcf5d3128ca663b/src-tauri/src/immich/sync.rs .
- Native cache behavior: https://github.com/CyberTimon/RapidRAW/blob/872804e8a21d4dff707f31fc9dcf5d3128ca663b/src-tauri/src/immich/files.rs .
- RapidRAW local XMP behavior: https://github.com/CyberTimon/RapidRAW/blob/872804e8a21d4dff707f31fc9dcf5d3128ca663b/src-tauri/src/file_management.rs#L4476 .
- Latest release at research time: https://github.com/CyberTimon/RapidRAW/releases/tag/v1.6.6 ; source inspected separately confirms the XMP filename/rating limitations also exist there.

Research was read-only. No application changes, archive moves, rating writes, or implementation tests have been performed. Compatibility with the user's installed application versions and actual network-write semantics remains to be demonstrated before implementation claims are made.

## 25. Implementation contracts from the execution-readiness review

This section resolves underspecified details in earlier sections and takes precedence where it is more specific. These are implementation defaults selected for the requested unattended build, not claims about validated production capabilities. The implementation plan must cover every contract below.

### 25.1 Completion boundary and fixed defaults

The builder must implement all specified features and automated tests without requiring access to the personal archive, installed editor, second computer, or Immich credentials. Missing deployment access is not permission to omit an adapter, substitute a stub, claim end-to-end compatibility, or abandon unrelated tasks. Ship executable isolated probes and an explicit capability report. Production enablement remains a separate operation; no automatic service installation/start, personal archive initialization/bootstrap, NAS test writes, Immich mutations, or Syncthing reconfiguration during the build.

An unvalidated capability is disabled with a reason, not assumed supported. Core offline ingestion and preservation continue when external adapters are unavailable. Production NAS publication requires the configured share's capability evidence, and rating writes require an adapter profile and explicit configuration. Implement and test these gates as actual code. Multi-computer publication additionally requires the two-client visibility experiment. A local temporary directory backend is supported for automated tests, but does not certify SMB/NFS.

Defaults: budget 75,161,927,680 bytes (70 GiB); free-space reserve 5,368,709,120 bytes (5 GiB); debounce 2 seconds; local reconciliation 300 seconds; activity hold 600 seconds; archive/ratings polls 60 seconds while connected; one bulk transfer per profile; metadata batches of at most 128 files; strong digest BLAKE3 encoded `blake3:<64 lowercase hex>`. These are configurable except the version-1 digest encoding. Retry delay starts at 2 seconds, doubles to 300 seconds, and uses bounded jitter. Permanent configuration/authentication/schema failures wait for correction, not a hot retry loop.

Cache order is least recent successful local materialization or observed user edit, breaking ties by canonical group ID. Scanner/hash reads and tool-generated writes must not renew activity or cache age. Do not use filesystem atime. A full activity-held cache can cause direct-NAS import; it must not deadlock the import waiting for its own recent groups to age out.

### 25.2 Paths, roots, and process boundaries

Local working root, local state root, and archive root are distinct, non-overlapping trees; the selected source cannot contain or be contained by any of them. The working/state roots must be on local storage. Validate canonical roots, filesystem/mount identity, and a local root identity marker. Source scans never recurse through symlinks, tool state, or destination trees. Do not hard-link working originals to archive or camera files. Symbolic links, special files, unsupported filenames, and paths outside a managed root produce explicit skipped/blocked records rather than silent success.

Version 1 shared paths use valid UTF-8 relative components. Reject absolute paths, `.`/`..`, empty components, NUL, and platform separator ambiguities; reject non-UTF-8 names with actionable diagnostics rather than lossy conversion. Preserve case and Unicode spelling. Case-fold-equivalent destination collisions on the actual share are conflicts. Resolve sidecars by the longest known original filename plus verified suffix rules, not the legacy text-before-first-dot helper. Never copy unknown JSON fields into paths without validation.

Lexical validation or `canonicalize` followed by a later absolute-path write is insufficient. Confine filesystem operations beneath held directory descriptors, disallow symlink traversal, and validate opened objects. Keep NAS staging/publication tied to the validated mounted filesystem descriptor; losing a mount must never redirect a later `mkdir`/rsync/rename into the exposed local mountpoint. Revalidate mount/archive identity after reconnect and before completion acknowledgment. Root replacement, read failure, or incomplete directory enumeration is not evidence of mass file deletion or edit reset.

Invoke rsync/ExifTool directly with argument arrays; never a shell command string. Use option terminators/absolute input paths and NUL-delimited lists where applicable. Test spaces, quotes, leading dashes, newlines, and non-ASCII names. Constrain output capture and process concurrency. Do not allow redirected HTTP requests to leak Immich authorization; reject userinfo, unexpected origins, and encoded traversal. Secrets are read from a user-owned mode-0600 file, absent from argv/status/catalog/logs. Only explicitly configured HTTP(S) API origins are used; TLS verification stays enabled for HTTPS.

NAS `stat`, directory scans, hashes, and fsync can hang as well as rsync. Run all NAS filesystem work in a bounded worker process, separate from the local SQLite/socket/watcher coordinator; never hold a local database transaction across remote I/O. Timeout marks the worker unavailable and stops new NAS work, preserving reservations and uncertain publication state. Do not spawn replacement workers while an old worker could still publish; terminate/reap or require recovery before resuming. An unkillable kernel I/O wait is reported, not described as cancelled. Local status, local ingestion, and local snapshots must remain responsive.

### 25.3 Durable state machines and pins

Persist job intent, dependencies, reservations, operation ID, expected digest, expected parents, and attempt generation before work starts. Distinguish queued, running, waiting-source, waiting-NAS, waiting-capacity, waiting-application, conflict, failed, cancelled, and complete states. Persist local-ready and archive-committed milestones separately. A successful enqueue is not a successful import. Restart reconstructs nonterminal jobs; cancelled/obsolete attempt results cannot acknowledge newer versions.

Persist explicit pin intent before admission/fetch; protect existing resident members immediately. Track requested pin, achieved original residency, and metadata currency separately. Failed/oversized pins keep an explicit blocked request until unpinned/cancelled. Offline-ready means every currently known required original is verified local; latest-ready additionally requires current metadata application. Learning a new member invalidates readiness and schedules its fetch for an existing pin. Group aliases union pin intents and rebase queued jobs transactionally.

Admission uses additional required bytes, not the group's full size when members already exist. Count real partial/staging bytes once and reserve only their unwritten remainder; include atomic-replacement/snapshot overhead and each destination filesystem's free-space reserve. Check arithmetic for overflow. Reconcile reservations after crashes and handle ENOSPC after a successful precheck. NAS capacity is separate from the 70 GiB local budget. No precheck guarantees capacity against unrelated writers.

Retained current sidecars are never removed by ordinary clean. Duplicate immutable local snapshot/history objects may be garbage-collected only if their exact bytes are committed and verified on NAS and no pending job, conflict, displaced-version recovery, or current snapshot requires the local object. Persist the cleanup intent and references. If eligibility cannot be proven, retain and report it. Local snapshots and staging remain in budget accounting even when stored under the separate state root. Small catalog/log storage is separately reported; rotate bounded logs and obey free-space reserve.

### 25.4 Catalog wire format, replay, and corruption

Version-1 commits are UTF-8 JSON envelopes with `schema_version`, `archive_id`, `commit_id`, `writer_id`, `writer_sequence`, `dependencies`, `payload` (a JSON string containing operation records), and `payload_digest`. Digest the UTF-8 bytes of that exact payload string after JSON string decoding, not a reserialized JSON object and not the envelope containing its own digest. Envelope identity/path fields must agree. Validate dependency references and every record before applying any of a commit in one SQLite transaction. Deduplicate by commit ID plus verified envelope contents; an ID reused with different contents is corruption.

Operations have explicit tagged types and schema versions. Unknown required operations, corrupt commits, impossible aliases/parents, missing required payload objects, or unresolved dependencies block archive publication and eviction based on incomplete shared state. Retain prior valid local state for reading/editing. Reporting/quarantining a bad record does not permit pretending it never existed. A writer sequence is not the sole ordering mechanism; dependencies and version parents are explicit. Bounded records (maximum 8 MiB per commit) and bounded batches avoid one record per entire archive/import. Large binary sidecars live as external history objects, never inline JSON blobs.

Keep distinct version identity and content identity: the same bytes can occur after a reset or on two divergent lineages. Content equality allows storage reuse; it does not erase ancestry. A version records sidecar ID, unique version ID, parent version IDs, `content` or `reset`, digest/size for content, and observed-vs-derived origin. Duplicate retries reuse the same planned version ID. Resolutions name all superseded heads. DAG cycles or cross-sidecar parents are invalid. A reset is a typed tombstone, not zero-byte malformed JSON.

SQLite migrations are transactional and numbered. Back up an existing database using SQLite's backup facility before schema upgrade; never copy only a live WAL database's main file. An unsupported newer local schema prevents service startup/mutation. A copied/restored profile must acquire a new writer ID before publishing if it might coexist with its original; IDs and monotonically increasing sequences cannot silently fork. Rebuilding shared facts is distinct from recovering local-only state. An unknown local file recovered after database loss is protected until inventoried and verified, never evicted merely because its path looks archived.

### 25.5 Archive publication and mutable projections

Never implement no-replace with `exists()` followed by ordinary rename. Use a tested no-replace primitive or another tested atomic exclusive publication method; lack of support is a capability failure. Existing equal content is reused only after digest verification. NAS locks serialize tool publishers, not arbitrary writers; external writes to managed originals or RAW edit sidecars are unsupported and detected as conflicts. Immich owns JPEG XMP and is handled separately.

The authoritative mutable state is committed immutable history. Publication order: persist local intent; store/verify immutable history objects; acquire publication lock; refresh/validate complete journal; compare/canonicalize groups and parents; commit new heads/conflicts; then materialize current NAS sidecars from the committed result; finally acknowledge locally. Original media still precede the commit that references them. Current NAS sidecars are recoverable projections and may briefly lag committed heads. On crash after commit, replay repairs only a known projection whose previous digest/reset matches its recorded state. Unknown changed live bytes are preserved/ingested as a conflict, not overwritten as stale cache. A projection failure is visible and blocks affected-group eviction until reconciled.

On divergent sidecar edits, publish both immutable histories and explicit multi-head conflict state; leave the previous unambiguous live projection in place. Never let directory traversal order select the winner. CLI resolution creates a new version with all heads as parents. `keep-both` keeps both histories and the conflict until the user chooses a current head; do not invent a working RapidRAW virtual copy format. Existing validated virtual-copy files are transported faithfully.

Original publication, sidecar publication, journal replay, and local acknowledgment each need deterministic fault-injection boundaries. An already committed job whose response was lost must be discovered and acknowledged, not re-executed as a new intent. Orphan cleanup never removes unknown media or another writer's recoverable work. Automatic NAS history/commit garbage collection is out of scope.

### 25.6 Metadata ownership, reset detection, and ratings

Recognize ordinary `.rrdata` JSON, virtual-copy `.rrdata`, and `.rrexif` companions through version-tested filename rules. Do not assume `.rrexif` or every associated sidecar is JSON. Unknown fields in valid editor JSON survive unchanged semantically, and exact observed bytes are retained in history. Unrecognized or oversized metadata is preserved opaque and blocks transformations, not silently dropped. Set a configurable JSON transformation ceiling of 64 MiB; stream/hash larger opaque files rather than allocating arbitrarily.

NAS JPEG XMP remains externally managed: do not upload an old local copy onto it, treat its absence as an editor reset, or include it in tool-owned projection repair. RAW-side XMP and other supported companion files have explicit ownership and independent version state. Legacy `.orig`, PP3/DOP/ON1, and unknown files are preserved; importing them does not enable editor integrations beyond RapidRAW. Multiple-file metadata saves are not assumed atomic; verify each snapshot and report incomplete companions where the adapter requires them.

A missing root, failed scan, missing directory, transient atomic-save rename, or unreadable sidecar cannot create a reset. Require a stable per-file absence in an otherwise successfully observed managed directory after reconciliation/debounce, with a known previous version and no tool-removal intent. Preserve the prior bytes and report an inferred deletion intent. The tool cannot distinguish deliberate reset from accidental user deletion; both remain recoverable versioned intents. A reset observed concurrently with remote edit is a conflict. Equal file stats are not proof a new `.rrdata` is unchanged: on startup/overflow and periodic reconciliation hash recognized mutable metadata, even with preserved mtime/size. No EXIF is involved.

Only the base sidecar of the designated primary RAW contributes the photograph rating; virtual-copy ratings remain private to that copy. Prefer the single ORF over its paired ORI as primary; otherwise require a unique eligible RAW or explicit selection. JPEG-only groups retain an Immich/logical rating without inventing a RAW. Several independently indexed JPEG variants with distinct ratings require explicit authority selection/conflict handling, never last-item-wins.

No baseline is different from rating zero. On first reconciliation, a missing base sidecar/default with no prior observation contributes no local rating intent; one observed value establishes the baseline. Two observed unequal initial values create a conflict; equal values establish a baseline. Zero in a saved previously observed sidecar is a real value. Rejected/unsupported values remain observations and block star propagation. The adapter must validate how to create a minimal native sidecar when none exists, preserve all non-rating data, and handle a full editor reset without clearing a pending incoming rating merely because default metadata vanished.

Track three independent things: exact saved local version, canonical merged version, and last version/rating applied to each local live projection. Pending canonical stars must not make unrelated local adjustment saves into false conflicts or reversions. On two computers, a remote rating-only change plus local non-rating adjustment change merges only the known rating field onto the unchanged common non-rating parent; truly divergent non-rating JSON remains a conflict. Preserve source snapshots and record merge parents/provenance. The adapter separates rating-field changes from arbitrary JSON changes; never perform a generic recursive JSON merge of masks/adjustments.

Persist Immich write intent, expected observed value, mapping identity, and idempotent local operation ID before sending. After timeout/lost response, re-read and reconcile; never blindly retry an old desired rating over a newer observation. Advance the common baseline only after confirming the intended value and recording the outcome. Serialize local rating jobs per logical photograph. A CAS-capable adapter must use the server precondition; an adapter without CAS exposes the residual race and requires an explicit `best_effort` write mode. Default is observations/pending writes with outbound mutation disabled until configured. Read-only keys still allow supported URL mapping and observation; 401/403/404 and unsupported schema have distinct status.

Use exact deployed API schema/version contracts, not fabricated endpoint/field names. Adapter compatibility evidence is versioned and invalidated by an unsupported server/editor upgrade. No live personal-photo write is authorized by the implementation plan. Optional isolated app/server tests exercise both directions, zero resets, absent sidecars, virtual copies, delayed responses, and reopen behavior. If this evidence is unavailable, ship the real implemented adapter plus fixtures/probe and a disabled unverified production capability, with that limitation prominent in the final implementation report.

### 25.7 Pairing, layout, metadata errors, and bootstrap

Source-observation shortcuts require a reliably identified previously observed source incarnation. If stable card identity is unavailable, hash candidates on each new attachment; a shared volume label/path is insufficient. Verify source attributes/identity before and after hashing/transferring; a changed source invalidates the attempt. Persist normalized metadata by blob digest and extractor version. Invalid or missing EXIF creates a recorded result and fallback, not an endless every-scan subprocess retry.

Preserve capture wall-clock calendar date for layout, with offset/subseconds recorded separately. Do not reinterpret offset-less EXIF as UTC and then convert to host time. Fall back to source mtime interpreted with a configured import timezone (default UTC), then `UNDATED` if no usable time. Existing placements always win. Pair candidates require matching source-directory context/stem and compatible camera/capture evidence; contradictory/missing evidence stays unpaired/protected for review. Matching stems across numbered DCIM directories are insufficient. Unknown originals remain importable as singleton groups with a clear association warning.

For sequence inference, partition by camera identity, capture day, mode, source context and compatible bracket parameters. Require increasing shot indexes, increasing file counters when reliably parsed, and no index reset; consecutive observed frames must be within 2 seconds for burst or 5 seconds for AE bracket. Missing indexes may yield an incomplete sequence only with compatible anchors; do not join disconnected index fragments from different imports based on time/counter alone. Ambiguous cases stay flat/logically unresolved. These are conservative version-1 thresholds, configurable and fixture-tested; do not claim them as camera-guaranteed identifiers. Choose folder anchor from the first reliably known member and persist it. Avoid the legacy requirement that all bracketing use electronic shutter.

Before camera imports can publish into an existing archive, overlapping destination inventory must be bootstrapped or the candidate existing content explicitly hashed/adopted under the same bootstrap rules. Neither filename existence nor an incomplete archive scan proves a duplicate. Unexpected content at the desired path blocks that member/group with a collision record; do not silently add a suffix and create a second Immich asset. Provide explicit resolution to adopt equal content or choose a distinct recorded destination for a distinct image; never overwrite. Unrelated groups can continue.

Archive initialization is exclusive and explicit: an existing valid marker is reused only when requested archive identity agrees; competing init attempts cannot create two identities. Missing marker after a profile was bound is an error, never automatic reinitialization. Bootstrap records immutable initial sidecar versions with observed ownership; unknown files stay untouched. External writers (including old importer/Syncthing) must be stopped or excluded from managed paths during migration and publication. The build must not change those services itself. Provide user deployment instructions for this cutover; two-way Syncthing on the new cache would violate eviction semantics.

### 25.8 CLI completion and operational tests

Keep binary `photo_sorter`; retain legacy implementation as separately named `photo_sorter_legacy` with explicit documentation and no concurrent managed-tree use. New CLI subcommands never silently accept old incremental-cutoff semantics. `run.sh` delegates to configured new import only after rollout documentation explains initialization/bootstrap; it must not initialize a personal archive or choose a working root implicitly.

Mutation requests use caller-generated idempotency IDs scoped to a profile. `import`, `pin`, `refresh`, and `bootstrap` wait by default, with `--no-wait` returning a durable job ID; `jobs show/wait/cancel` inspect or control it. CLI disconnect or Ctrl-C stops waiting, not the service job; cancellation is explicit. Exit codes: 0 completed (or explicitly accepted with `--no-wait`), 2 invalid/config/protocol, 3 unavailable dependency/capacity or waiting-application, 4 conflict/integrity, 5 operation failure. Status JSON includes protocol version and per-job state/reason; errors are structured and secrets redacted.

`init` writes only local profile configuration under a local setup lock; archive creation is separate `archive init`. Use profile lock/Unix peer credentials and socket mode 0600. Requests are bounded newline-delimited JSON (maximum 1 MiB); reject invalid/oversized/incompatible requests. The one database-owning service serializes mutations and rechecks protection before eviction. Status/dry-run cannot create markers, reserve capacity, enqueue real jobs, alter pins, or update success state. An absent service has a clear error and explicit start/run path.

Provide isolated verification commands that produce machine-readable reports including tested versions, mount identity/options, probe scope, limitations, and results. Repeated test runs use unique scratch roots and never target `/mnt/common/Camera/OM1-II` or the real Immich URL by default. Two-client probes require explicit scratch-root coordination and report single-client execution as insufficient. Optional deployed probes are excluded from default CI and listed as unrun when unavailable. The implementation is complete only when real adapters, error paths, tests, docs, and service/CLI wiring exist; passing mock tests alone does not certify deployment.

### 25.9 Additional independent builder-check corrections

Preserve the legacy supported-original matrix: RAW CR2/NEF/ARW/DNG/RAW/ORF/ORI; still JPG/JPEG/TIF/TIFF/PNG; video MOV/MP4/AVI/MTS/M4V/MPG/MPEG (case-insensitive extension recognition). TIF/TIFF/PNG remain supported singleton/imported stills in the JPEG tree for compatibility, but only genuine JPG/JPEG assets qualify for the initial Immich URL-to-JPEG mapping. Validate the asset's media identity; legacy `MediaKind::Jpeg` alone is insufficient. Treat `.orig` as potentially full-size opaque preserved content, not necessarily small editor JSON. Unowned supported sidecars remain recorded/protected until ownership is resolved.

Every photograph has a membership revision; fetch/eviction intents reference it. New originals, merges, and alias changes require a transactional recheck before reporting readiness or unlinking. An older fetch completing cannot mark an expanded photograph fully ready. A pin intent propagates through the merge even when one member is not resident.

The saved-edit guarantee covers retained live sidecars and every coherently captured snapshot. Neither polling nor inotify guarantees capture of intermediate saves overwritten before observation. A valid parse plus stable attributes reduces torn-read risk but is not editor transaction locking; reread/hash for stable snapshots and retry changes. Preserve a stable malformed observed file as opaque diagnostic evidence, protect it, and block transforms instead of declaring it clean or reset. This does not promise snapshots of unsaved editor memory or every keystroke.

### 25.10 Additional primary references

SQLite backup API: https://www.sqlite.org/backup.html (consistent database snapshots; live main-file copying is inadequate).
Linux rename semantics: https://man7.org/linux/man-pages/man2/rename.2.html (ordinary rename can replace a destination; no-replace needs an explicit supported primitive).
notify documentation: https://docs.rs/notify/latest/notify/ (editor event patterns and network-filesystem limitations).
Immich XMP documentation reconfirmed: https://docs.immich.app/features/xmp-sidecars/ (rating jobs and filename preference).

### 25.11 Import wait semantics and lock-session ownership

`import --wait-for ingested` is the default: the requested import is ingested when each non-conflicting selected original has a verified durable local copy or a committed direct-NAS copy. It returns 0 for that milestone and explicitly reports how many locally ingested originals still await archive publication. `--wait-for archived` requires all selected originals to be archive-committed; offline/blocked work returns 3 with its durable job ID instead of hanging indefinitely. `--no-wait` only acknowledges acceptance. `jobs wait` uses the requested milestone and returns the structured waiting/conflict state when no immediate progress is possible. No wording may imply the card is safe to erase because background upload was merely queued. A whole-group conflict is not silently excluded from a successful aggregate result; independent groups may finish but the aggregate reports nonzero with per-group outcomes.

`pin` persists intent immediately and waits for latest-ready. If original files are ready but retained sidecars require explicit refresh, return waiting-application (3), keeping the pin and files; do not auto-assert editor-closed. `refresh` completes only at the requested applied versions. `bootstrap` completes only after its scoped records are committed. These waits do not prevent other local service work.

The NAS worker owns filesystem handles and a publication lock session, but not SQLite. A publication uses a bounded session protocol: begin/acquire lock; stream fresh journal deltas to coordinator; coordinator validates/applies all required deltas and computes canonical identities/parents in short local transactions; send prepared immutable commit plan back within that same session; worker validates the session and performs publication/projection; return receipts; coordinator acknowledges. Release lock on completed session or confirmed abort. Do not release/reacquire between refresh and commit without repeating refresh/replanning. No SQLite transaction waits for worker/network I/O. If the coordinator or worker disappears mid-session, retain uncertain publication state and follow abandoned-owner recovery; a local deadline is not fencing. If the worker is implemented with its own pure replay reducer instead, it must share the identical codec/reducer tests and return complete committed deltas; it must never maintain a second live shared SQLite store.

### 25.12 Session assertions, maintenance, and resolvable input states

The editor-closed assertion is an ephemeral application-session authorization, never a persisted boolean that permits later writes. Durable refresh staging/jobs may survive, but application authorization expires when the requesting client disconnects, the operation enters waiting/blocked state, or the service restarts. Require a fresh foreground `refresh --editor-closed` to resume application. Disallow `--no-wait` with an applying refresh; a stage-only operation may be asynchronous. Before every file application the service checks that the live session remains authorized. A write already in progress may complete; status/recovery report exactly which versions were applied. The user must keep the editor closed until the foreground operation completes; if its connection is lost, check status before reopening. Automatic retry never resurrects the authorization. Other ordinary jobs still survive CLI disconnection.

Provide `catalog backup --offline` and `catalog rebuild --offline` maintenance entry points for a service that cannot open its database. They acquire the same exclusive local profile lock and refuse when a normal service or unreaped worker owns the profile. Preserve stopped database/WAL/SHM files and local-only data before attempting recovery. A readable database uses the SQLite backup API; an unreadable one is preserved as forensic bytes and rebuilt into a new location only by explicit command. Unsupported-newer schema is not silently downgraded. Rebuild from journal and cautious local inventory, with a fresh writer identity and explicit lost-private-state report; do not infer old pins from NAS facts. This limited maintenance path is the exception to the ordinary single-service mutation rule, not an independent concurrent writer.

The conflict API has typed resolutions: choose sidecar head; leave preserved alternates unresolved; adopt identical original; choose distinct validated placement; associate compatible groups/members; keep ambiguous originals as independent groups; select primary RAW; select authoritative JPEG asset for rating; associate an orphan sidecar; or retain an opaque companion unassociated. Each resolution carries expected conflict revision/heads and is revalidated transactionally. Explicit associations are recorded with user-selected provenance rather than fabricated camera evidence. A deliberately independent original or unassociated preserved companion can become evictable only after its own exact archived bytes/record are verified. Never delete uncertain data to clear a conflict. CLI lists the required selectors/version IDs and implements each resolution, so ambiguity cannot become an unresolvable permanent space leak.

Source inventory includes recognized companions as well as originals. Import exact source `.rrdata`, virtual copies, `.rrexif`, XMP, `.orig`, and supported opaque companion formats with bounded accounting. Use validated ownership; uncertain files are preserved in a tool-owned local/archive history namespace and recorded as orphan companions pending resolution, never silently assigned or dropped. Importing onto an existing managed sidecar creates a candidate version/conflict and cannot overwrite newer editing data. A source `.orig` may be as large as an original and must participate in capacity/direct-NAS decisions. Repeat imports skip unchanged known originals but still inspect/hash mutable source companions; zero-EXIF/zero-original-copy does not mean ignoring new edits. Unknown non-supported files remain on the source with an explicit skipped count.

NAS publication modes are explicit: `single_writer` binds the archive to one writer ID and still requires qualified local-client filesystem semantics; `multi_writer` additionally requires qualified cross-client visibility. Another writer can read/fetch but cannot publish to a single-writer archive. Changing writer ownership or enabling multi-writer mode is an explicit archive administration operation with all publishers quiescent and no unresolved old I/O. The marker update is protected, versioned and journaled; an uncertain interrupted update blocks publication pending recovery. Cloning a profile never bypasses this restriction. Local temporary-filesystem test mode is explicitly scoped to non-network test roots and cannot silently certify/enable a production mount.

### 25.13 Final completeness checks

The working-root identity marker binds it to one local profile; take an exclusive working-root ownership lock as well as the profile lock. Two different configuration/state directories cannot concurrently manage the same local tree. A root identity mismatch blocks management rather than adopting another profile's files. Tests must cover this case without assuming all callers share a configuration filename.

Imported supported companions participate in the selected ingestion/archive completion milestone alongside originals. A preserved orphan can count as durably ingested, but unresolved association/conflict remains visible and prevents a misleading fully resolved result. Cancelling a job records cancellation intent immediately; only confirmed stopped attempts release their reservations or staging ownership. A timed-out worker that might still write cannot be treated as successfully cancelled.

`verify` is a read-only scoped audit of selected local/archive original digests, sidecar history payloads, journal consistency and current projection state; it produces explicit missing/corrupt/unknown findings and does not silently repair or delete. Known corruption protects affected data and blocks eviction/publication dependent on that evidence. `unpin` clears only the explicit pin request; pending-version, conflict, unreadable and active-job protection remains independent. Both need actual implemented handlers and regression tests.

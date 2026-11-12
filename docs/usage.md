# Operating the local archive

Use absolute paths on local filesystems for working and state roots. They must be disjoint from each other and the archive. State includes SQLite/WAL, immutable edit snapshots, reservations, job intents and private pins. Never synchronize the live database to the NAS.

Create a local profile explicitly; its parent must already exist:

```sh
photo_sorter --config /absolute/profile.toml init \
  --working-root /absolute/local/working \
  --state-root /absolute/local/state \
  --archive-root /absolute/mounted/archive
photo_sorter --config /absolute/profile.toml doctor --json
photo_sorter --config /absolute/profile.toml service run
```

`init` creates local configuration/directories only; archive initialization is a separate operation. Use the generated identities. The example config is a parsing example: do not clone its fixed identities into live profiles. A working-root marker and exclusive working/profile locks prevent two profiles from managing the same tree. A copied writer identity is not a second publisher. A new client needs separate local roots/profile/writer identity and the same validated archive ID; ownership or multi-writer changes require explicit administration and qualification.

Normal CLI commands connect to the one local service through its same-user mode-0600 socket. An unavailable service returns a structured error; `service run` is foreground and `service start` is the explicit detached alternative. The service and every worker retain ownership locks. Stop the complete process group and prove quiescence before recovery; a timeout does not fence an old writer.

After qualification and migration review:

```sh
photo_sorter --config /absolute/profile.toml archive init
photo_sorter --config /absolute/profile.toml import '/mounted/Camera card' --dry-run
photo_sorter --config /absolute/profile.toml import '/mounted/Camera card'
photo_sorter --config /absolute/profile.toml import '/mounted/Camera card' --wait-for archived
photo_sorter --config /absolute/profile.toml status --json
photo_sorter --config /absolute/profile.toml jobs show JOB_UUID --json
photo_sorter --config /absolute/profile.toml jobs wait JOB_UUID --wait-for archived
photo_sorter --config /absolute/profile.toml jobs cancel JOB_UUID
```

Default import completion is **ingested**: every selected non-conflicting original has a verified durable local copy or committed direct archive copy. Output distinguishes pending archive publication. `--wait-for archived` requires committed archive evidence; unavailable dependencies return waiting with job ID. `--no-wait` acknowledges a durable job only. Disconnect/Ctrl-C stops waiting, not the job. Use explicit cancellation; uncertain writing workers retain their reservations/session until positively completed or recovered. Conflicting groups make aggregate completion nonzero even when independent files succeed.

Offline edits are observed through local events plus periodic reconciliation. The service preserves each coherently captured saved state; it cannot capture unsaved editor memory or intermediate saves overwritten before observation. Stable malformed, rejected or oversized metadata remains preserved/protected rather than transformed. Deleting a sidecar is an explicit Reset version only after readable, debounced absence, never inferred from an unavailable root. Saved source companions are imported independently of unchanged originals.

```sh
photo_sorter --config /absolute/profile.toml pin GROUP_UUID
photo_sorter --config /absolute/profile.toml pin 'https://configured-immich/photos/ASSET_UUID'
photo_sorter --config /absolute/profile.toml refresh GROUP_UUID --stage-only
photo_sorter --config /absolute/profile.toml refresh GROUP_UUID --editor-closed
photo_sorter --config /absolute/profile.toml unpin GROUP_UUID
photo_sorter --config /absolute/profile.toml clean
```

Pin intent persists immediately, including while offline or over capacity. Latest-ready requires fresh shared metadata, every original member and the requested applied edit versions. Existing sidecars are retained; incoming edits wait for foreground refresh. Close the editor and keep it closed until applying refresh completes. Authorization expires on disconnect, waiting or restart; reconnect and assert it again. Applying refresh rejects `--no-wait`. Inspect status after a lost connection before reopening the editor. Unpin only removes private residency intent; it never deletes a file by itself. Cleanup rechecks archive identity, strong byte proofs, current membership, conflicts, dirty snapshots, pending work and activity hold; it does not delete mutable sidecars.

Stage-only refresh fetches and verifies canonical saved metadata without fetching originals or changing live sidecars. It supports `--no-wait` and completes when staging finishes. Default refresh walks every eligible resident group across bounded pages; explicit groups retain their durable progress. Applying refresh performs bulk copies in the maintenance worker and checks the foreground connection before each file. Valid foreground waits have no fixed duration cutoff; status remains independently available. Application receipts survive lost worker replies and are committed with each file's catalog acknowledgment. Refresh results show up to 128 applied versions plus the total; continue with `jobs show JOB_UUID --applied-after CURSOR` using `detail.applied_page.next`.

Exit codes: 0 completed milestone or explicitly accepted no-wait; 2 invalid/config/protocol; 3 waiting/unavailable/capacity/application; 4 conflict/integrity; 5 operation failure. JSON reports include protocol/schema versions and job reasons. Status/conflict pages default to 128; use `status --groups-after`, `--jobs-after`, `--conflicts-after` and `conflicts list --after` to continue. `verify` audits expected resident originals, immutable local snapshots and locally observed projections. Intentionally evicted originals and incoming edits awaiting application are distinct from missing or corrupt expected bytes. `verify --archive` audits immutable journal/history, originals and current Content/Reset projections. Both use bounded worker batches and perform no repair. Findings retain their root scope; a good NAS copy cannot clear a bad local copy. A new successful verification of corrected exact bytes clears the corresponding diagnostic.

For Immich configure an origin, owned regular mode-0600 key file, explicit library and exact container-to-archive prefix mappings. Credentials never go into job JSON or logs. Qualify using a separate explicitly selected test asset, then opt into `best_effort` only if its documented non-CAS race is acceptable. Configure `rapidraw_binary` and exact version and qualify native isolated saves before enabling transforms. Ordinary startup/doctor never launch applications, mutate ratings or run filesystem probes. See [compatibility](compatibility.md).

## User-installed service

No installer runs automatically. Review `packaging/photo-sorter.service`, replace `@BINDIR@` with the absolute installed executable directory and `@PROFILE@` with the absolute reviewed profile path. Quote substituted systemd arguments when paths contain spaces. Validate the generated file with `systemd-analyze --user verify /path/to/photo-sorter.service`, then install/enable it yourself as a user unit only after the migration checks. `KillMode=control-group` keeps all child workers in the service lifecycle. A blocked kernel I/O task can survive a stop deadline; verify actual processes before any takeover. Restart does not grant permission to steal an abandoned NAS lock.

Invoke the optional runner as `./run.sh /absolute/profile.toml '/mounted/Camera card'`; set `PHOTO_SORTER_BIN` if the executable is outside PATH. It preserves quoted arguments, refuses a missing profile/card, and forwards additional import options.

Saved snapshots participate in admission and reservations across the working and state filesystems. Large opaque saves can go directly through rsync into immutable archive history without a second local copy. If qualified archive access is unavailable, the live save remains protected and the capture job waits. Restart recovery uses operation-owned receipts; uncertain prefixes remain opaque evidence. A verified already-applied live file can satisfy pin metadata readiness without allocating a duplicate snapshot. External editor writes can exceed the cache budget; admission controls new tool allocations rather than deleting a live save to enforce a limit.

Status space measurements run in the local worker and include their observation time and any audit error. Null fields mean unmeasured; counts can lag active transfers. Snapshot, staging, retained companion/displaced and catalog-overhead fields explain the measured total; they overlap the broader metadata/resident views and must not be added indiscriminately. Admission uses fresh worker evidence tied to reservation changes. In-progress reservations retain their full admitted ceiling, and each allocation or cleanup invalidates the previous proof. This can temporarily charge both materialized partials and their reservation ceiling. Existing destinations qualify for zero-copy reuse only after strong stable verification; disappearance requires fresh admission before copying. Unknown symlinks and special files remain untouched and can block admission while status stays readable. Physical reconciliation rechecks known archived placements, invalidates stale readiness, and reports changed/unreadable bytes without silently replacing them.

`clean` queues bounded eviction work and verifies large originals, unused snapshots, hidden source-companion copies and completed transfer duplicates in the local maintenance worker. Status remains available while verification waits. The service repeats pin, membership, activity and reference checks before unlinking and refuses changed bytes. Live sidecars remain outside eviction. Unassociated companions require an explicit association or retain-unassociated choice plus exact immutable archival evidence before their hidden duplicate becomes reclaimable. Incomplete, uncertain or changed staging payloads remain preserved. Cleanup receipts retain the reason for refused deletion.

A distinct-path conflict choice starts a fresh transfer identity and retains the previous plan and local pathname. The service verifies retained local bytes when reusing them as source. It never moves a foreign NAS original or old metadata path to make the choice fit. Exact repeated resolution operations return their committed receipt; a new stale choice is rejected. Shared choices carry exact member/primary/conflict evidence, so private revision-counter differences between clients do not invalidate the same shared facts. An explicit NAS projection head choice authorizes replacement of the recorded observed predecessor while retaining displaced bytes.

Ambiguous capture associations preserve separate originals and protect every candidate group until an explicit decision. A unique ORF is primary over its ORI companions; unrelated RAW formats do not become one photograph merely through matching names and capture times. New rating conflict evidence remains immutable and separately visible. Matching saved native stars and freshly observed Immich stars can resolve the exact rating conflicts through the shared journal when the asset binding and archived save still match and no outgoing operation is uncertain. Authority, mapping and sidecar conflicts require their own applicable resolution; matching stars do not erase them.

Immich Favorite synchronizes with the mapped photograph's primary RAW RapidRAW Pick flag, independently of stars. Pick sets Favorite; clearing Pick clears Favorite; clearing Favorite clears Pick. **RapidRAW Reject wins:** a saved Reject clears Favorite and incoming Favorite never replaces Reject. Only the authoritative JPEG/base RAW pair participates; virtual copies retain their own flags. Existing authority selection handles multiple mapped JPEGs.

This bridge uses ordinary-folder `.rrdata` history. It does not integrate RapidRAW's native Immich cache. Immich holding only JPEGs does not prevent flag synchronization through the existing JPEG-to-RAW mapping, but does not supply a RAW for editing. Incoming flags are archived as immutable metadata and apply to a live sidecar through `refresh <group> --editor-closed`; reconciliation does not fetch a RAW. Live application retains the existing requirement that the photograph is resident; explicitly pin/fetch it first when needed. Stars and all non-flag edits remain intact. Saved Reject also cancels a staged incoming Pick while retaining its history.

Outgoing Favorite writes default to disabled and require explicit Immich `best_effort` mode, matching Favorite runtime evidence and native Pick qualification. See [compatibility](compatibility.md). Missing API fields, unreadable sidecars and unknown flags are not treated as false. On the first observation, differing known Pick/Favorite values are preserved as a visible disagreement; make the applications agree once to establish a common baseline. Reject retains its priority. Subsequent changes use the last agreed and actually applied states, so a staged incoming Pick does not bounce an old live neutral flag back to Immich. Unrelated identity, association and editing conflicts remain protected. Reject does not resolve independent star conflicts.

HTTP results that become uncertain remain durable across service restarts. The service checks the exact mapped JPEG and reads back before settling; it does not blindly resend an ambiguous update. Immich's API has no compare-and-set, so a user change between GET and PUT can still race. No deployment Favorite/flag qualification was performed by the build.

Changing the explicitly selected JPEG or primary RAW retires unsent Favorite work and preserves the old baseline and pending metadata as history. Sent requests keep their receipts. Different JPEG assets have independent ordered queues, so an unavailable old asset cannot starve the newly selected JPEG. For a field-only readback race with Reject, the old operation and observation remain durable; a new clear requires a fresh saved-flag observation and full JPEG preflight. Path, library or media-identity changes remain blocking identity failures.

## Nix package

The repository flake builds `photo_sorter` and `photo_sorter_legacy` on Linux and supplies ExifTool and rsync at runtime:

```sh
nix build .
./result/bin/photo_sorter --config /absolute/profile.toml doctor --json
nix develop
```

A consuming NixOS flake can pin GitHub main through its lock file:

```nix
inputs.picture-sorter.url = "git+ssh://git@github.com/tulku/picture_sorter.git?ref=main";
```

Select `inputs.picture-sorter.packages.${pkgs.stdenv.hostPlatform.system}.default` in the consuming configuration. Updating that input advances the source revision; ordinary rebuilds retain the locked revision. Service/profile installation and archive qualification remain separate from building the package.

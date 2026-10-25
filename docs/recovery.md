# Recovery without weakening preservation

First inspect `status --json`, the durable job and `conflicts list --json`. Keep source cards, working edits and state intact. An unavailable archive or stalled worker is not evidence of deletion or shutdown. Do not replace a live writer based on a deadline. Normal retry can reconcile exact same-operation receipts after uncertainty; another operation must not adopt them.

## Publication locks and mode updates

Stop every prior publisher and all its descendants, confirm no process or client can still write, and ensure the archive is mounted with its original identity. Only then use:

```sh
photo_sorter --config /absolute/profile.toml archive recover-lock \
  --prior-owner-stopped --all-writers-quiescent
```

This explicitly retires lock evidence; it never assumes a stale timestamp authorizes takeover. A NAS reconnect or service restart alone does not establish quiescence. If an old process is stuck in kernel I/O, leave publication blocked until its writes are positively resolved.

Mode changes require the complete expected marker, stable operation UUID, next owner/mode qualification and three explicit safety assertions. Their immutable control journal and displaced markers remain in `.photo-sorter/modes` and staging. A pending update blocks ordinary publication. Repeat the exact request with `recover:true` only after proving shutdown/quiescence/no unresolved old I/O. Preserve archive marker, the modes journal, commits, versions and retired lock evidence together.

## Metadata conflicts and Reset

`conflicts list` reports ID, revision, group, heads and selectors. Send an explicit JSON `ResolutionRequest` using `conflicts resolve --request /absolute/resolution.json`. It includes a stable `operation`, `conflict`, `expected_revision`, exact `expected_heads`, expected `(group, membership_revision)` pairs and tagged `resolution`. Stale selectors fail transactionally; inspect again rather than blindly retrying a new choice.

Actions cover `choose_head` with version; `keep_alternates` (deliberately unresolved); `adopt_equal` with digest; `choose_distinct_path` with validated archive-relative path; `associate_groups`; `keep_independent`; `select_primary_raw` with exact member; `select_rating_authority` with asset; `associate_sidecar`; and `retain_unassociated`. Explicit association carries user provenance. A Reset is a causal version that can be selected like saved content. Displaced and competing bytes remain retained. Do not remove files manually to make an ambiguity disappear.

## Catalog backup and rebuild

A readable catalog uses the SQLite backup API, not copying its main file while WAL is live:

```sh
photo_sorter --config /absolute/profile.toml catalog backup /absolute/local/backups/catalog.sqlite
photo_sorter --config /absolute/profile.toml catalog rebuild /absolute/local/new-state
```

Rebuild uses a separate new state directory, replays immutable shared records and inventories local saved data cautiously. It creates a fresh writer identity and reports lost private pins, jobs and other local-only intent. It does not install the replacement profile or silently adopt a single-writer archive. Review the report and explicit ownership/mode transition before cutover. Unknown local saved bytes remain untouched; do not infer pins from archive membership. Readiness is not restored from flags alone: actual local bytes must be checked.

If the service cannot open its database, stop it and positively reap its workers, then use the same commands with `--offline`. Maintenance acquires the same profile/working locks and refuses another owner. Preserve database/WAL/SHM and local-only snapshots before recovery. Offline backup retains forensic copies before attempting the backup API; an unreadable source may fail while preserving those copies. Rebuild never downgrades an unsupported newer schema into the old location. Retain the original state directory and forensic report until the replacement is independently validated.

An uncertain API rating write is reconciled by GET readback and identity, not a blind repeated PUT. A user change or reindexed JPEG identity blocks the old write. A failed isolated qualification probe may leave its test asset changed; inspect that isolated asset and retained report. Never use a production asset as the disposable probe.

Direct saved-history recovery retains its intent and prepared receipt under archive staging, keyed by the original operation. The bulk payload uses the corresponding generation-owned staging path. A retry can acknowledge the same exact immutable object after a later live overwrite. A prefix lacking coherent completion proof is preserved as opaque detached evidence; it cannot become an applied current save. Do not remove these objects while a writer might still be active.

A rebuilt profile remains protected until it has a successful fresh shared read and complete local capture/physical reconciliation. Only that recovery blocker is cleared; unknown shared records and dependencies remain blockers. Read-only reconciliation never changes the archive's writer ownership. Managed workers compare actual held root identities with the startup bindings, and rating writes recheck their leased local roots before PUT. A replaced root requires explicit correction/recovery rather than adoption of the replacement directory.

Cleanup records intent before verification/unlink. Restart rechecks ownership, exact archival evidence and current references; a missing already-unreferenced archived duplicate can be accounted for without guessing about live edits. Unknown or changed cleanup candidates remain retained. Completed local-transfer generations have positive quiescence evidence; a timeout does not provide it. Incomplete prefixes without an exact archived-copy proof remain charged to the budget and need inspection. Distinct-path choices retain prior local paths and upload plans for the same reason.

Managed working, state and archive roots must be physically disjoint, including bind aliases and nested bind mounts. Workers reject overlapping archive roots before initialization or publication; qualification scratch roots have the same boundary. Correct the profile or mount layout rather than bypassing the check.

An unreadable or malformed journal enumeration blocks publication and eviction until a complete successful namespace read. Audit diagnostics are scoped to their local, state or archive root and retain affected bytes. Restore or correct the exact expected evidence through a deliberate operator action, then rerun the appropriate verification. Successful verification clears its corrected diagnostics; it never uses a healthy copy on another root to dismiss a local finding. Expected local residency survives an unexpected physical loss and is cleared only by intentional eviction.

Audit and refresh cancellation retain their worker lease until positive completion. Foreground refresh authorization is never persisted: worker application checks the live coordinator session at every file boundary, and disconnect or cancellation leaves later files pending. Retained refresh temporary files remain charged to occupancy; inspect them before removing anything. After a stopped-service restart proves exclusive inherited profile ownership, refresh readmission measures retained bytes before replacing obsolete private reservations.

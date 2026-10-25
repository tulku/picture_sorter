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

Exit codes: 0 completed milestone or explicitly accepted no-wait; 2 invalid/config/protocol; 3 waiting/unavailable/capacity/application; 4 conflict/integrity; 5 operation failure. JSON reports include protocol/schema versions and job reasons. Status/conflict pages default to 128; use `status --groups-after`, `--jobs-after`, `--conflicts-after` and `conflicts list --after` to continue. `verify` audits local bytes; `verify --archive` reads actual archive objects without repair.

For Immich configure an origin, owned regular mode-0600 key file, explicit library and exact container-to-archive prefix mappings. Credentials never go into job JSON or logs. Qualify using a separate explicitly selected test asset, then opt into `best_effort` only if its documented non-CAS race is acceptable. Configure `rapidraw_binary` and exact version and qualify native isolated saves before enabling transforms. Ordinary startup/doctor never launch applications, mutate ratings or run filesystem probes. See [compatibility](compatibility.md).

## User-installed service

No installer runs automatically. Review `packaging/photo-sorter.service`, replace `@BINDIR@` with the absolute installed executable directory and `@PROFILE@` with the absolute reviewed profile path. Quote substituted systemd arguments when paths contain spaces. Validate the generated file with `systemd-analyze --user verify /path/to/photo-sorter.service`, then install/enable it yourself as a user unit only after the migration checks. `KillMode=control-group` keeps all child workers in the service lifecycle. A blocked kernel I/O task can survive a stop deadline; verify actual processes before any takeover. Restart does not grant permission to steal an abandoned NAS lock.

Invoke the optional runner as `./run.sh /absolute/profile.toml '/mounted/Camera card'`; set `PHOTO_SORTER_BIN` if the executable is outside PATH. It preserves quoted arguments, refuses a missing profile/card, and forwards additional import options.

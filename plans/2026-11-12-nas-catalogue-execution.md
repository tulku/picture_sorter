# NAS catalogue operational ledger

## User scope

Finish the no-argument SD shortcut and catalogue the pre-existing NAS archive so a normal card import recognizes older photos. User explicitly chose preserving NAS names and giving differing card JPEGs a _CAMERA_ORIGINAL suffix. Apply this lossless policy to the current card; never overwrite existing archive files or remove source/working copies.

## Installed state

- Syl service is active on package 327n2m8a8ml5gapnm7wy92jpkpj9wwk2 (source 6819fdf), v2 SingleWriter qualification passed. Metadata replacement remains disabled.
- Archive /mnt/common/Camera/OM1-II; state /home/tulku/.local/share/photo-sorter; working /home/tulku/Pictures/Working. Camera /run/media/tulku/OM SYSTEM mounted read-only.
- Archive inventory on NAS: 37,124 visible files, 470,071,509,250 bytes; includes 14,165 JPG, 14,057 ORF, 5,441 RRDATA, 2,278 XMP, 1,118 ORIG and other formats. Original names/layout remain in place.
- Eskarina Camera Syncthing peer is disconnected. Native NAS API key was read only in memory; no sync configuration changed.

## Recovery

- Consistent local catalogue backup path recorded in /tmp/photo-sorter-pre-bootstrap-backup-path: /home/tulku/.local/state/photo-sorter-backups/before-bootstrap-20261112T122206Z.sqlite.
- NAS ZFS snapshots retained: tank/files@photo-sorter-before-init-20261112T120008Z and tank/files@photo-sorter-before-catalogue-20261112T124223Z.

## Catalogue preparation and conflict policy

- At task start a user-launched whole-card import 5999cebd-dddf-4532-a979-bd29102e8d94 was already active/conflicted. Cancelled through native API without deleting copies. Final report: 1,792 selected, 95 archived, 95 then-known JPEG conflicts, 1,047 local-ready, 16,290,898,363 transfer bytes.
- RAW-only bootstrap 8e467942-3b19-43f0-bc8a-e5ad9c2124ca adopted 384 originals, then was cancelled to release its long-held publication session and let journalled preservation resolutions publish first. Restart catalogue with a fresh durable job after resolving pending imports.
- The native bootstrap currently treats any members digest mismatch as an altered committed original, even when that member only belongs to an unpublished/cancelled import. Therefore pending card candidates must be moved to their approved alternate names via native conflict resolution before catalogue adoption can reach their NAS paths.
- Prepared and submitted 95 native choose_distinct_path requests. They completed and left zero unresolved conflicts. Private per-file plan/requests/receipts: /home/tulku/.config/photo-sorter/conflict-resolutions/camera-originals-20261112/.
- Read-only catalogue query plus server-local b3sum preflight examined the remaining 893 pending JPEG members: 95 approved alternate targets were not yet on NAS; 798 unsuffixed NAS targets have different bytes. Report /tmp/photo-sorter-pending-jpeg-differences.json; candidate rows /tmp/photo-sorter-pending-jpeg-members.json. No live database writes were made outside the native API.
- A one-file native probe 68c6bbb3-f19d-4433-a39b-77c4497efcca confirmed expected old NAS path and returned Conflict with zero transfer bytes. It remains an acknowledged conflict until included in batch resolution.
- Copied only the remaining 798 camera JPEGs into an isolated source tree preserving their DCIM relative paths. Selection path in /tmp/photo-sorter-catalogue-jpeg-selection. Started native import 44d88d3a-06f2-4e61-ba5d-fd8a63b45183 with --no-wait to register all pending differences through supported APIs, not fabricate conflict rows. Start receipt /tmp/photo-sorter-catalogue-jpeg-preflight-start.json.
- Latest observation: source scan selected 798; mismatch processing still progresses. Transient WaitingCapacity is not a full-cache condition: local occupancy ~17.9 GB of 75.2 GB and only one ~13.5 MB active reservation; later native job states show Running. Check current job detail until all 798 are conflict/settled before resolving.

## Next actions

1. Wait for job 44d88d3a-06f2-4e61-ba5d-fd8a63b45183 to report selected=798, conflicting=798, waiting=0; ensure no unexpected archived targets. Fetch all conflicts with native pagination (128 maximum).
2. Build fresh group membership revision map via paginated photo_sorter status --groups-after. For every JPEG original_collision, create an owned 0600 ResolutionRequest with fresh operation UUID, current conflict revision/empty heads, group membership revision, and choose_distinct_path to original stem + _CAMERA_ORIGINAL + original extension. Verify each target is absent locally and on NAS before submitting. Existing first 95 per-file plan demonstrates exact JSON. Submit through native CLI --no-wait; wait until resolved. Do not modify database directly.
3. Cancel only the temporary preservation-probe import jobs after their collisions settle if needed for a quiescent catalogue; do not delete copied bytes. Confirm original unpublished membership paths are now free for actual NAS bytes. Read-only pending-member preflight can confirm no remaining differing unsuffixed NAS JPEG candidates.
4. Start whole-archive photo_sorter bootstrap --no-wait --json with a fresh operation. Record durable job ID; monitor progress and reasons. It reads/hashes hundreds of GB and may take hours. Existing files stay in place; sidecars/ORIG files are preserved as immutable history and may produce protected orphan-sidecar review items.
5. After completion verify archive/catalogue, preserve original NAS files via ZFS comparison, and report actual original/companion counts, unresolved protected metadata, and import readiness. Avoid claiming completion while the job is running.

## SD shortcut

- Direct /home/tulku/nixos edit per user preference; commit f30b717 adds three-line zero-argument dispatch to configuredCli and updates README.
- Built wrapper /nix/store/hj2wniskkah0wzld2z71jlzjvidjprvm-photo_sorter; tested shell syntax and exact argument capture for no args, status --json, custom import path containing spaces, --help. No real no-argument import was invoked during verification. Independent review found no blockers.
- Full NixOS build passed: /nix/store/pshc09rrnfjk4fs6av4hdgl6d6v6nl9d-nixos-system-syl-26.05.20261108.ac588ef. Shortcut requires user nh os switch --flake /home/tulku/nixos#syl activation; service executable/unit and profile are unchanged, but avoid unnecessary interruption while catalogue is active. No NixOS main push performed for this shortcut.

# Safe imports on a mount without atomic exchange

## Intent and selected approach

Enable safe imports on syl's existing SMB archive today. The owner chose import support now, with existing NAS metadata replacement deferred. Keep the established archive path /mnt/common/Camera/OM1-II, local roots, publisher identities and NixOS service. Do not change the share or require eskarina.

The real SingleWriter probe passed exclusive_init, no_replace, lock_exclusion and flush_reopen; rename_exchange failed with EINVAL. The Linux 7.3 CIFS implementation rejects flags other than RENAME_NOREPLACE. This is a client filesystem limitation, not a Samba setting.

## Alternatives

1. Separate immutable publication from exchange-based replacement (selected): enables imports with the operations already demonstrated, retaining disabled unsupported updates.
2. Add a recoverable replacement protocol over supported SMB operations: future work to enable metadata updates, including an intent record, retained predecessor, no-overwrite moves and crash/recovery handling. Readers must tolerate replacement gaps.
3. Run archive operations through a NAS-side worker: requires a new transport/deployment architecture and is outside today's import scope.

## Behavior and boundaries

For SingleWriter archive publication, require passing exclusive creation, no-overwrite publication, lock exclusion and flush/reopen checks with the existing exact mount/writer/archive/mode/client-boot fingerprint. An absent or failed required check still disables publication. Never enable production through the local test backend. Preserve the existing stricter MultiWriter requirements, including exchange and distinct-client evidence.

Add a separate exchange capability. Existing metadata replacement requires exchange evidence before staging or moving the existing file; unavailable exchange returns an explicit unsupported result while preserving live bytes. Creating a previously absent sidecar may use no-overwrite publication; equal existing content remains an idempotent no-op. Mode administration also requires exchange evidence for the target configuration before recording a pending mode change or taking an administration lock.

Expose immutable publication and metadata replacement decisions separately through the NAS worker status so users can distinguish safe imports from disabled replacement. Keep scope and reasons in qualification output and documentation. Continue running the exchange probe and retain its failed result; do not forge, remove or reinterpret evidence as a successful swap.

Originals stay immutable, verified and no-overwrite published. No unsafe rename fallback, plain copy over an existing file, fabricated peer assertion, automatic archive migration, or metadata synchronization enablement is part of this change.

## Validation and deployment

Use tests with a production-policy temporary filesystem and real fingerprint-bound reports: failed exchange permits SingleWriter immutable publication but never replacement; failed required base checks deny publication; MultiWriter remains gated; unsupported replacement and mode administration leave predecessor bytes and marker/pending state unchanged. Test the worker/CLI reports the split capabilities. Retain all existing tests and run relevant lint/build checks.

Reuse the clean personal source worktree /home/tulku/Repos/picture_sorter/offline-photo-archive. Keep this spec, the plan and execution ledger only on meta/superpowers-plans. After review and verified merge/push, update the picture-sorter input in /home/tulku/nixos directly. Build the new package/system; user activation may require interactive sudo. Then rerun real SingleWriter qualification in the existing separate scratch parent with a fresh session. Initialize the archive only after the required checks pass. Inspect legacy archive writers and retained backup coverage before bootstrap or real imports. A camera card is not currently mounted.

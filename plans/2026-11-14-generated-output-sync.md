# Generated output sync implementation

Spec: /home/tulku/Repos/picture_sorter/superpowers-plans/specs/2026-11-14-generated-output-sync.md
Code: /home/tulku/Repos/picture_sorter/sync-generated-files (base53317b3).

## Interfaces and ownership

Core owner: domain.rs/edit/owner.rs/edit/coherent.rs/edit/versions.rs/edit/reconcile.rs/edit/refresh.rs/catalog schema+identity/replay/storage readiness/archive journal+publish validators. Provide domain::GENERATED_OUTPUT_OWNER, domain::is_generated_output_path(&ArchivePath)->bool, domain::validate_saved_path(&ArchivePath,&str)->AppResult<()>, SidecarKind::Generated, Catalog::register_generated_output(&mut self,&ArchivePath)->AppResult<SidecarOwner>, Catalog::generated_owner(&self,&ArchivePath)->AppResult<Option<SidecarOwner>>. Add generated_observations schema described in spec. Existing sidecar-only validation remains strict.

Projection owner: archive/projection.rs plus new archive/generated.rs; all service/publication.rs generated-group selection/GroupCreated and generated projection recovery integration; qualification modules/new runtime probe; NAS worker dispatch/qualification request integration. Coordinate server qualification match edits with root. Export qualification::qualify_generated_outputs(&Config)->AppResult<CapabilityReport>, using only fresh private scratch. Base NAS fingerprint remains unchanged. Core supplies Generated owner helpers.

Root: service/observation.rs discovery/capture/debounce/absence/adjacent association, import/inventory.rs working-only project inclusion, service/status.rs, integration tests, documentation, runtime evidence/profile, source integration and Nix. Coordinate any shared edits instead of overwriting others.

## Work

- [ ] Core: failing tests for generated media capture/history and replay, camera-path/case/import collisions, ordinary sidecar rejection and restart ownership. Implement explicit role, identity/validators/readiness and observation schema. Tests must keep ordinary immutable originals protected and never publish generated Reset.
- [ ] Projection: failing tests for first publication, overwritten saves/history and foreign existing target. Implement immutable owner claim and generated-only recovery protocol. Crash tests before/after old move, publish and receipt; partial/foreign retained states never overwrite unknown bytes. Add SingleWriter qualification gate and deployable scratch probe. Update shared publisher for generated group definitions and adjacent metadata.
- [ ] Root: automatic unknown-media/project discovery, persistent stamp/debounce and unchanged-byte skipping, event/inventory/restart coverage, source deletion retention and exact orphan metadata association after base archival. Tests for image-before-metadata and reverse order, NAS offline capture, capacity, atomic rename/partial save, rewritten known camera file and unchanged repeat scan.
- [ ] Integrate and review role boundary/recovery, run full498+ suite/fmt/clippy, checked Nix package, real generated scratch qualification, source main merge/push and CI, Nix pin/system build/commit. Remove code worktree. Leave activation to user; after switching validate automatic archival of the two local TIFFs and disappearance of their orphan warnings.

Review focus: camera claim while NAS file absent; first-claim foreign arrival race; generated local absence preserving archive; stale captured stamp acknowledging newer bytes; existing orphan sidecar identity versus generated group. Each belongs to its owner tests above.

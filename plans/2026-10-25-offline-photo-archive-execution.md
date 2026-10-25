# SDD ledger — plan: /home/tulku/Sentrisense/repos/picture_sorter/superpowers-plans/plans/2026-10-25-offline-photo-archive-implementation.md

Implementation: /home/tulku/Sentrisense/repos/picture_sorter/offline-photo-archive
Branch: feat/offline-photo-archive; baseline de1cffa6a785a594c7aebf5de2e2e8716c30a448.
Approved: complete specification, section-25 defaults, all 23 tasks, continuous inline execution; final independent review only.
Safety: fixture roots only; no production archive/API/service/Syncthing operations, no merge/push of code.

Ruling: Keep durable ledger in this tracked planning-branch plans file and generated briefs/logs in this planning worktree's .superpowers workspace — explicit user planning-artifact isolation takes precedence over skill scratch placement/deletion; task bases must be taken from implementation HEAD rather than planning HEAD — cost if wrong: workflow scripts need explicit adaptation, no runtime effect.

Pre-flight interface scan:
- T01→all: IDs/errors/config/clock and faults are shared; establish typed validating serialization, not permissive raw strings.
- T02→T03/T09/T10/T13/T14/T18: descriptor-relative handles enforce confinement; NAS handles only in worker.
- T03/T04→T05/T07/T11/T12/T19: local transactional jobs and revision-aware identity; replay must preserve private pins/jobs.
- T05→T10/T12/T18/T20: identical commit codec; blocked records prevent publication/eviction.
- T06/T08→T11/T18: metadata cached by digest, source incarnation/context and still-vs-JPEG roles must remain distinct.
- T07→T11/T14/T15: pending pin and additional-byte accounting precede work, revision invalidates readiness.
- T09/T10→T11/T12/T18/T20: worker lock session encloses replay/replan/publication; no SQLite across remote I/O.
- T12/T13→T14/T16/T17: exact snapshots, unique causal versions, explicit reset and expected live projections; no live sidecar eviction.
- T14→T19/T20: ephemeral connection-scoped application authorization never survives waiting/restart.
- T15/T16→T17/T22: pinned source contracts plus disabled unverified runtime capability, explicit non-CAS write mode.
- T18→T19: typed resolutions and same-lock offline maintenance.
- T19/T20→T21/T23: real subprocess/HTTP/SQLite end-to-end tests and distinct completion milestones.
- T22→publication/ratings: guards at mutation boundary, scope/version/mount-bound qualification.
No unresolved cross-task specification conflicts found.

Tooling: system rustup shim cannot execute generic Linux toolchain on NixOS. Found existing Nix Rust 1.99.0 components and merged sysroot; /tmp/photo-archive-env selects them without altering installed tools. Baseline cargo test --lib --offline: 20 passed.
Task 01: in progress; BASE de1cffa; brief read. Dependency fetch pending.
Task 01 RED: cargo test --test foundation --offline failed on absent config/domain/legacy binary; full output /tmp/photo-t01-red.log. GREEN: cargo test --test foundation --lib --offline, 24 passed; cargo test --all-targets --offline passed (legacy binary also tested). Nix toolchain selection lives only in /tmp/photo-archive-env. New CLI remains provisional until T19.
Task 01: complete (commits de1cffa..6da9382; tests cargo test --all-targets --offline passed).
Task 02: complete (commits 6da9382..1f059d9; RED absent fs module /tmp/photo-t02-red.log; GREEN cargo test --test filesystem --offline 5 passed; cargo test --all-targets --offline passed). Local primitive evidence only, no NAS qualification.
Task 03: in progress; BASE 1f059d9; brief read; RED tests/catalog.rs absent catalog API.
Task 03: Ruling: SQLite's unix xFullPathname resolves /proc/self/fd directory symlinks and NOFOLLOW rejects that spelling — register a narrowly scoped VFS dispatch copy overriding only full-path normalization, retaining the held-directory path; unix open still uses O_NOFOLLOW on DB/WAL/SHM — cost if wrong: VFS lifetime/path assumptions could affect catalog durability; covered by real WAL/reopen/backup and root-replacement regressions, final review must inspect this boundary.
Task 03 GREEN: cargo test --test catalog --offline 6 passed, including real WAL backup and root replacement; full suite run before commit. No NAS I/O in SQLite transactions.
Task 03: complete (commits 1f059d9..85319fe; tests cargo test --test catalog --offline 6 passed; cargo test --all-targets --offline passed).
Task 04: in progress; BASE 85319fe; brief read; RED missing identity methods; GREEN focused identity tests run before full-suite commit. Methods preserve sidecar versions/heads by moving sidecar ownership rather than merging version IDs.
Task 04: complete (commits 85319fe..a9be9dc; focused tests 6 passed; full suite passed). Expanded merge/version/head/conflict integration assertions remain to be exercised together with T12's producer.
Task 05: in progress; brief read; RED missing archive/replay/version interfaces, focused GREEN 8 passed; added invalid-later-record atomic rollback regression before commit/full suite. Payload-object existence is worker-validated in T10/T12 before catalog replay; structural codec tests do not certify remote objects.
Task 05: complete (commits a9be9dc..a5b80dd; cargo test --all-targets --offline passed; journal target 9 passed). Follow-up integration coverage: worker payload availability and all-entity reconstruction T10/T12/T21.
Task 06: in progress; BASE a5b80dd; RED missing inventory/metadata methods; focused 8 passed, then metadata-cache size regression failed with digest-size mismatch. Fix separates extractor cache from verified blob length. Source metadata subprocess uses held file descriptors through /proc/<parent>/fd; no source pathname reopening. Exact external ExifTool invalid-file test included.

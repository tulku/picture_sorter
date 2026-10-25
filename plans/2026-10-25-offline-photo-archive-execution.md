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
Task 06 correction: initial fbc583e commit occurred despite failing actual_exiftool_literal_path_error_is_captured because a shell sequence continued after a failed test. No completion was valid at that commit. ExifTool correctly returned exit 0 but no useful capture fields for plain text; now missing capture metadata is an explicit cached error/fallback. Focused final run: 10/10 passed. Keep shell test exit status as a gate before commits from here onward.
Task 07 RED: missing storage/reservation/readiness API. Focused GREEN tests cover exact boundary, partials, existing bytes, free reserve, durable pin, unpin independence, oversized pin and revision readiness. enospc_after_admission currently covers retention through uncertain cancellation; actual transfer ENOSPC regression remains T09/T21 integration coverage, do not count this alone as ENOSPC verification.
Task 06: complete (commits a5b80dd..fbc583e plus ce176fa correction; full suite green before ce176fa/84643cb).
Task 07: complete (commit 84643cb; cargo test --test admission --test import_metadata --offline: 19 passed; full suite green). Transfer ENOSPC remains owning T09/T21 coverage.
Task 08: in progress; BASE 84643cb; brief read; transcribed fixtures explicitly documented, no personal paths read. RED absent APIs; first GREEN attempt 8/9 sequence tests; counter reset failed because sorting equal capture seconds by counters hid reset. Fix preserves stable filename order for equal seconds then validates counters as constraints.
Task 08: complete (commits 84643cb..062b9f0; full suite passed; sequences 11/11, metadata 10/10). Added RED→GREEN mixed RAW/JPEG frame normalization and ORF-over-ORI authority coverage. Fixture evidence remains transcription, not deployed camera/editor qualification.
Task 09: in progress; BASE 062b9f0; brief read; RED absent transfer/worker APIs. Real rsync tests first found /proc file descriptors were treated as symlinks and skipped; rsync must follow only the explicitly supplied held file descriptor via --copy-links (source tree enumeration itself still rejects symlinks). Process-isolation tests use SIGSTOP on real child, not a simulated in-process NAS worker.
Task 09: complete (commits 062b9f0..79ecc72; cargo test --all-targets --offline passed; transfers 6, worker isolation 3). Extra RED→GREEN protects visible files by requiring job/generation-owned hidden stage paths. No production service started.
Task 10: in progress; BASE 79ecc72; brief read. RED absent archive API; first focused GREEN 9 tests, worker protocol wired with actual persistent begin/list/read/ack/publish/end handlers. Added fresh shared-catalog canonicalization regression and fault after original rename before commit. Crash retry failed on missing stage; recover using exact verified already-published destination rather than requiring vanished staging bytes. Lock recovery preserves retired owner metadata and requires both shutdown/quiescence assertions.

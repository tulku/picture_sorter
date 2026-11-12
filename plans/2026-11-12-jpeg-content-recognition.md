# JPEG content recognition implementation plan

> **For agentic workers:** Use superpowers:executing-plans inline, task by task, with one fresh whole-branch review.

**Goal:** Recognise geotagged NAS JPEGs as existing images without copying the card versions.
**Architecture:** Keep byte digests unchanged. A streaming JPEG inspector computes a separate domain-separated content fingerprint and exact file digest. Source scan threads cache fingerprints; deferred import checks inspect an already archived candidate through the supervised NAS worker before any local copy. Store image-equivalence receipts separately from exact archived placements.
**Tech Stack:** Rust, blake3, SQLite, existing confined filesystem and worker protocol, Nix.
**Spec:** /home/tulku/Repos/picture_sorter/superpowers-plans/specs/2026-11-12-jpeg-content-recognition-design.md

## Global constraints

Keep whole-file integrity and RAW/sidecar verification byte-exact. Preserve NAS metadata, original files and prior suffix copies. Scope comparisons to exact expected or recorded prior destinations; ambiguity and malformed/unsupported JPEGs remain conservative. Cache identities on demand; do not interrupt or restart the active NAS catalogue. Honest accounting must distinguish image duplicates from byte-archived incoming files.

## Review focus

- JPEG metadata parsing is untrusted: bounded reads and checked TIFF offsets; malformed/truncated/unsupported input must not become an image match.
- Orientation, ICC/Adobe interpretation, quantisation/coding tables and actual image changes must remain distinguishable.
- Missing/changed NAS candidates and source mutations must fail safely before duplicate acknowledgement.
- Historical _CAMERA_ORIGINAL choices must not defeat canonical NAS recognition; multiple candidate destinations stay conflicts.
- Deferred checks must survive cancellation/restart without staging bytes or falsely marking source digests archived; sidecars must resolve to canonical owner receipts.

## Task 1: Streaming identity and cache

Files: create src/import/jpeg.rs and src/catalog/jpeg.rs; modify import/mod.rs, catalog/mod.rs/schema.rs, service/scan.rs and import/runner.rs; tests tests/jpeg_content.rs and unit cache tests.
Interfaces: JpegInspection {digest: Digest, size: u64, image: Option<Digest>}; inspect_jpeg(Read)->AppResult<JpegInspection>; inspect_jpeg_source(SourceEntry)->AppResult<JpegInspection>; Catalog::cache_jpeg_inspection and Catalog::jpeg_fingerprint. Fingerprints use jpeg-content-v1 domain separation; unsupported results are cached as null under the exact whole-file digest.
- [ ] Write failing tests for geotag/XMP/IPTC/comment invariance, unchanged exact-file digest contract, differing image bytes/tables/orientation/ICC, malformed/truncated metadata and image data; source mutation validation.
- [ ] Observe focused failure, implement bounded streaming inspector and additive cache table. Exclude descriptive APP1 and IPTC Photoshop resources; retain unknown/rendering blocks. Support baseline JPEG, conservatively reject unsupported forms.
- [ ] Wire source inspections into the existing scan thread and synchronous scan path, binding cache receipts to batch digest/size and SourceEntry validation. Verify focused tests and commit.

## Task 2: Deferred duplicate recognition

Files: create src/import/jpeg_duplicates.rs; modify runner.rs/local.rs/upload.rs, transfer/worker.rs, service/server.rs, catalog/schema.rs; tests tests/jpeg_import.rs.
Interfaces: deferred jpeg_checks keyed by import item hold unique canonical archived candidate and fallback member/group/facts. jpeg_duplicates stores canonical digest/path/group/size and content fingerprint. Add NasCommand::InspectJpegArchive and NasReply::JpegInspection; worker verifies expected full digest/size, file identity before/after, and archive namespace.
- [ ] Write failing native import tests for matching pixels/different GPS producing zero copied bytes, intact NAS metadata, separate duplicate count and no false source archived placement; repeated import uses cache.
- [ ] Select exact archived candidate at planned original destination or a recorded prior destination for the same source digest/group. Defer JPEG conflict/materialisation while comparison is pending. Source unsupported fingerprints use existing behaviour; ambiguous paths remain conflicts.
- [ ] Use UploadDriver inspection phase to perform NAS reads only in the supervised worker, sharing existing arbitration/cancellation. Complete image-equivalence receipts transactionally; mismatches rejoin existing collision/import logic. Leave byte-level original/member mappings untouched on image match.
- [ ] Exclude pending checks and image duplicates from local and NAS staging; report image_duplicates separately and finish jobs when exact archives plus image duplicates account for selection. Canonical duplicate receipts support source-sidecar ownership. Verify focused tests and commit.

## Task 3: Integration and release

Files: service tests, JPEG import tests, README/native CLI documentation as appropriate; Nix input updated directly in /home/tulku/nixos only after reviewed source integration.
- [ ] Verify actual image/orientation/colour change conflicts, unsupported fallback, ambiguous candidates, prior suffix choices, altered NAS and source evidence, cancellation/restart, mixed new/duplicate imports and canonical sidecar owner preservation.
- [ ] Run cargo fmt --check, cargo test --all-targets, cargo clippy --all-targets -- -D warnings; review whole branch with fresh reviewer. Address blockers with regression tests.
- [ ] Merge/push main under existing authorization, build Nix package and syl system, update Nix lock and commit. Keep current live catalogue untouched; deploy the new package only after its full native job is Complete. No automatic new-photo upload or suffix cleanup.
- [ ] Verify final jobs/receipts and status, push execution ledger only to planning branch, remove merged code worktree when no longer needed.

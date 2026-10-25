Synthetic native documents follow `ImageMetadata` and its Default implementation in the approved sources:

- v1.6.6: https://github.com/CyberTimon/RapidRAW/blob/52138689a9c9aea400e37a30c902154b47db3799/src-tauri/src/image_processing.rs
- Inspected development: https://github.com/CyberTimon/RapidRAW/blob/872804e8a21d4dff707f31fc9dcf5d3128ca663b/src-tauri/src/image_processing.rs
- Filename rules: https://github.com/CyberTimon/RapidRAW/blob/52138689a9c9aea400e37a30c902154b47db3799/src-tauri/src/file_management.rs

Both inspected metadata types require version, rating and adjustments. The default is version 1, rating 0, null adjustments, null tags; absent flag and exif are omitted. Flag values are pick/reject. Base sidecars append `.rrdata` to the full original filename; virtual copies append `.<six lowercase hex digits>.rrdata`. Ordinary default/reset saves may remove their sidecar. Mutable absence remains a recoverable inferred reset after reconciliation, never proof of deletion from the archive.

The edited fixture includes synthetic masks and future fields to test semantic preservation. Exact source bytes are retained separately from transformed JSON. These are source-backed profiles, not installed-application or GUI reopen qualification. Unknown versions and oversized/malformed documents remain preservable through the generic snapshot path while transformations are disabled.

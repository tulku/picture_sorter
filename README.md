# Offline photo archive

`photo_sorter` imports camera originals into a bounded local working tree, preserves observed saved edits, and publishes verified originals and immutable metadata history to an archive. A local SQLite catalog and one coordinator keep local work available while the archive is unavailable. Originals are immutable; editing data is retained separately with causal versions and explicit conflict handling.

The default local budget is 70 GiB with 5 GiB filesystem reserve. A completed local import can still await archive publication. Check the reported milestone and retained job before deciding what to do with a card. Repeat imports reuse unchanged files on an identified card, match existing archive paths by content, and recognise JPEG metadata-only variants by image content. Use `--strict` for full byte reads; copied files always receive integrity verification. No timestamp cutoff determines whether a photo was imported. Supported originals include CR2/NEF/ARW/DNG/RAW/ORF/ORI, JPG/JPEG/TIF/TIFF/PNG, and MOV/MP4/AVI/MTS/M4V/MPG/MPEG. Only genuine JPEGs qualify for initial Immich URL mapping.

Build on Linux with Rust 2024, rsync and ExifTool:

```sh
cargo build --release --bins
cargo test --all-targets
```

Start with [migration](docs/migration.md), then [usage and installation](docs/usage.md). Read [recovery](docs/recovery.md) before replacing a catalog or recovering a publication lock. [Compatibility and executable qualification](docs/compatibility.md) distinguishes fixture evidence from deployment qualification. Production archive writes, outgoing ratings and Favorite/Pick synchronization require explicit matching evidence; no personal archive, editor or Immich instance is qualified by the automated tests.

The optional integrations use source-backed RapidRAW profiles and the pinned Immich 3.3.5 API. Unknown editor formats remain opaque preserved content. GIMP projects and native Immich edits are outside the initial semantic synchronization scope. Immich rating writes require explicit best-effort mode because the API has no compare-and-set.

The old sorter remains available only as `photo_sorter_legacy`. Its incremental timestamp behavior is a separate legacy workflow; never run it or another importer/Syncthing writer concurrently over the new managed tree. `run.sh` requires an explicit profile and card path and delegates to the new CLI without initializing anything.

Development tests use generated temporary files, isolated service processes and loopback HTTP fixtures. They cover process-loss recovery, two local clients, preservation conflicts and measured repeat-import work. They do not certify NAS power-loss durability, deployed cross-client filesystem semantics or installed GUI/API behavior.

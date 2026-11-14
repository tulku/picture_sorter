# Event-driven photo service and simplification

User approved review items 1–4 and isolation of optional integration code. Preserve Immich and multi-computer support, archive formats, source index, overwrite safety and crash recovery. No live activation without the user switching Nix configuration.

Replace 5ms service polling with descriptor readiness and deadlines. Unix socket input/output, worker pipe replies/backpressure, and local inotify events wake the coordinator. Retain deadlines for debounce, remote archive discovery, retries, timeouts and rare reconciliation: remote SMB changes cannot depend on local inotify. No unconditional historical-job scans at active-worker frequency.

Local immutable verification records file identity stamps after successful hashing; unchanged files do not rehash on reconciliation or clean restart. Inotify changes and metadata differences invalidate evidence, overflow triggers reconciliation, explicit verify still hashes, and destructive paths retain their existing protections. Do not silently trust a changed/replaced file.

Settle abandoned local-transfer child jobs only once process quiescence is established; retain uncertain reservations. Remove legacy executable/exclusive modules/dependencies and production APIs with no runtime callers. Extract optional Immich convergence state/behavior behind a service integration boundary without changing sync semantics.

Approach: readiness-based coordinator using existing libc/Linux primitives rather than a new async runtime or slower fixed polling. Keep periodic metadata reconciliation for missed notifications, not periodic whole-library checksum reads. Target stable idle CPU below 1% of one core on representative catalogue; evaluate actual measurements, do not assert until verified.

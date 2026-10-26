# Favorite/Pick final independent review

Reviewed `8b1e75010c16f8ea831ad296a2869a6a41a261a1..197774c012b76978e37bd8fc3f0bb986cfd390de` on `feat/offline-photo-archive`, together with the approved spec, implementation plan, review package and execution rulings. The implementation checkout remained unchanged and clean. No subagents were used.

The implementation has substantial useful coverage: separate Favorite HTTP qualification, field-only requests, immutable incoming metadata, foreground application, async service wiring, exact conflict witnesses and snapshot references. The explicit initial-disagreement policy and source-backed 1.6.7 profile match the recorded rulings. Five state-transition and authority defects remain.

Critical: None. Important: five findings below. Minor: None.

1. **Important — Superseding an unsent source leaves its pending Favorite alive.**

   [src/immich/favorites.rs:570](/home/tulku/Repos/picture_sorter/offline-photo-archive/src/immich/favorites.rs:570), with the pending handling at [line 289](/home/tulku/Repos/picture_sorter/offline-photo-archive/src/immich/favorites.rs:289).

   Trigger: establish neutral/false agreement; save Pick and poll to prepare an outbound true; save neutral before sending; advance the old intent, then poll. The source guard correctly marks the intent superseded, but leaves `baseline.pending=true`. Reconciliation sees the current neutral flag equal to the old applied flag, so `KeepPending(true)` derives a new Pick. The reproduction produced a pending document with `flag:"pick"` while the saved flag and remote value were both neutral/false. Publication and refresh can therefore resurrect a canceled Pick.

   The same defect also strands an unchanged desired flag: if a newer metadata save still has Pick, the old intent is superseded but `outbound` remains true and prevents derivation or replacement intent preparation. The reproduction ended with no active intent, no derived pending version, and remote false despite local Pick.

   Repair: track whether pending work is inbound or outbound and which source/intent owns it. On supersession, reconcile the latest saved version against the last confirmed observations, retiring or replacing unsent work atomically. Never reinterpret a canceled outbound value as incoming metadata; never discard a sent/uncertain receipt. Regress both clear-before-send and a newer adjustment/star save retaining Pick, including both poll/advance orders.

2. **Important — A Reject clearing write can become a permanent Favorite conflict.**

   [src/immich/favorites.rs:694](/home/tulku/Repos/picture_sorter/offline-photo-archive/src/immich/favorites.rs:694), interacting with the conflict allowlist at [line 589](/home/tulku/Repos/picture_sorter/offline-photo-archive/src/immich/favorites.rs:589).

   Trigger: local Reject and remote Favorite true; send the required false; a concurrent remote edit restores true before readback, or an uncertain request is later read back as true. `finish` marks the intent conflicting and records a Favorite conflict containing only intent/expected/desired. The next poll prepares another Reject clear, but `advance` refuses it because this conflict lacks the `local` and `remote` fields recognized by the Reject exception. The isolated reproduction returned “unresolved photograph conflict blocks favorite write” and left Favorite true.

   This violates the binding correction that Reject wins and that Reject/Favorite pairing must not create a Favorite conflict. Repair: distinguish an identity failure from a field-only readback disagreement; after freshly proving the exact binding and current saved Reject, settle the old outcome and schedule the required clear without creating a blocking Favorite conflict. Preserve uncertain receipts and unrelated conflicts. Include post-PUT and restart/readback regressions.

3. **Important — An explicitly selected new JPEG authority cannot establish a Favorite baseline.**

   [src/immich/favorites.rs:219](/home/tulku/Repos/picture_sorter/offline-photo-archive/src/immich/favorites.rs:219); the existing authority resolution updates only `rating_authorities` at [src/catalog/conflicts.rs:625](/home/tulku/Repos/picture_sorter/offline-photo-archive/src/catalog/conflicts.rs:625).

   Trigger: establish a Favorite baseline with JPEG A; bind JPEG B to the same photograph; resolve the resulting authority ambiguity by selecting B through `SelectRatingAuthority`. Every subsequent Favorite poll skips A and rejects B because `baseline.asset` still names A. The rejection occurs before equal-value convergence, so making both applications agree cannot recover it. The reproduction applied the real semantic resolution records, then observed the same unresolved “favorite authority changed; baseline retained” conflict on repeated polls even though both sides were false. No supported Favorite resolution updates that baseline.

   Repair: give explicit authority selection a durable Favorite transition: retain old baseline/outbox evidence, safely retire unsent old-authority work, preserve uncertain sent outcomes, and establish an unknown/new-authority baseline only after verifying the selected binding. Preserve pending versions rather than dropping their history. Regress both existing-baseline and pending/uncertain authority changes.

4. **Important — A queued write can still use a RAW that is no longer primary.**

   [src/immich/favorites.rs:569](/home/tulku/Repos/picture_sorter/offline-photo-archive/src/immich/favorites.rs:569), before the send at [line 628](/home/tulku/Repos/picture_sorter/offline-photo-archive/src/immich/favorites.rs:628).

   Trigger: prepare a Pick intent from RAW A; then explicitly select RAW B as the photograph's primary RAW and resolve the ambiguity before sending. The source guard checks only that A's sidecar still has the captured local head. JPEG authority checks do not establish that this sidecar still belongs to the current primary RAW. Using real `SelectPrimaryRaw` resolution records, the reproduction selected `RAW/b.ORF`, then `advance` confirmed the obsolete A-based true write.

   The wrong RAW's flag can change the photograph's Favorite after the user has selected its authoritative RAW. Repair: before an unsent request, verify the canonical photograph, selected primary RAW, exact base-sidecar path/owner and captured local version together. Supersede/reconcile outdated authority work without losing sent uncertainty. Regress a primary change to neutral and to Reject, plus a group association that changes the effective primary.

5. **Important — The final HTTP preflight checks checksum but omits the rest of the JPEG binding.**

   [src/immich/client.rs:387](/home/tulku/Repos/picture_sorter/offline-photo-archive/src/immich/client.rs:387) and [line 431](/home/tulku/Repos/picture_sorter/offline-photo-archive/src/immich/client.rs:431); `FavoriteWrite` omits the expected path/library/type at [src/immich/favorite_api.rs:3](/home/tulku/Repos/picture_sorter/offline-photo-archive/src/immich/favorite_api.rs:3).

   Trigger: after polling/preparation, the upstream asset's external path or library changes while its checksum stays unchanged. `advance` compares the stored intent with the catalog mapping, but the live GET immediately before PUT checks only the checksum. The loopback reproduction prepared `/photos/JPEG/a.JPG`, returned `/external/a.JPG` from the actual HTTP preflight with the same checksum, and still received `Confirmed` after a true PUT. The discrepancy existed before preflight, so this is additional to the documented unavoidable GET/PUT non-CAS race.

   Repair: carry the full expected binding into the worker request and verify asset identity, external path, library, IMAGE/JPEG type and checksum on preflight and readback. Treat identity failures distinctly from Favorite disagreement so Reject cannot bypass them. Test path/library/type changes with unchanged checksum, including a change between preparation and worker execution.

Validation: the executor reported 377 passing isolated tests, fmt, strict Clippy, release build and release qualification smoke. I did not rerun the full suite. I compiled an independent temporary reproduction harness against the reviewed built library without editing repository code. It confirmed findings 1–4 through generated temporary catalogs and an in-memory `ImmichAccess`, including real semantic authority/primary selection records. Finding 5 used the existing generated loopback HTTP server and real `ImmichClient`; its confirmed write reproduced the identity gap. The final harness exited 0. These checks are generated fixtures, not deployment qualification. No personal photos, installed applications, production Immich/NAS services or credentials were accessed.

Declined to judge as outside the plan/spec: **None**. Deployment behavior remains unrun under the task's explicit access restriction; this review makes no deployment qualification claim.

Verdict: **With fixes; not ready until the five Important findings are addressed.** Add focused failing regressions for these transitions, repair them in the authorized single fix pass, then run the required checks. No second independent review is requested.


## Implementer closure

All five Important findings were accepted and fixed in `dace3ebd0d9a542bc77ddbc1e0e783393d025ab2`, after a failing regression for each finding. I1 now owns outgoing pending state explicitly, retires prepared sources atomically and consumes sent receipts before reconciling newer saves. I2 retains the original operation and readback; a field-only Reject difference requires fresh flag/binding reconciliation for a new clear and never becomes a Favorite conflict. Identity failures remain blocking. I3 retains old baselines and pending metadata in referenced history, safely retires unsent old-authority work and preserves uncertain old-asset receipts while independently ordering the selected asset. I4 checks the current canonical primary RAW, exact native base sidecar and saved source before sending, and retires primary-specific baselines without cross-sidecar derivation. I5 passes full external-path/library/IMAGE/JPEG/checksum identity into actual HTTP worker requests and distinguishes post-PUT identity failure from Favorite disagreement.

The final suite passed **393 tests, 0 failures, 0 ignored** across 37 test summaries; formatting, strict Clippy, release build and the real release CLI loopback qualification/restoration smoke passed. Additional regressions verify unobserved Favorite remains unknown and retired pending metadata remains referenced for GC. Exact RED→GREEN logs and cases are in the execution ledger. No Critical findings, deferred minors or declined-to-judge behaviors remain. This is implementer verification after the one independent review, not a second independent review.

Actual NAS, second-client, installed editor, deployed Immich and Syncthing qualification remain unrun and unverified capabilities remain disabled. No personal integration was accessed. Code branch remains committed, unpushed and unmerged in the requested personal repository worktree.

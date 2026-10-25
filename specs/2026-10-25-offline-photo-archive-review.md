# Independent design review

Date: 2026-10-25.
Reviewer: fresh agent /root/spec_review, with no conversation history.
Reviewed artifact: first draft of specs/2026-10-25-offline-photo-archive-design.md.
Scope: independent architecture/correctness review, not implementation validation.

## Original findings

1. Blocker: sidecar replacement and eviction lack a safe boundary against external saves (sections 8, 12-13, 15, 19). A save can arrive after a cleanliness/activity check and before unlink or replacement. Watcher delivery is delayed; atomic rename prevents partial visibility but is not conditional replacement. Required decision: retain mutable sidecars during original eviction and specify a separately safe incoming-application boundary.

2. Blocker: manual lock recovery does not exclude a suspended or partitioned previous owner resuming (section 11). Administrative takeover and heartbeat checks do not provide fencing. Required decision: definitively stop the prior owner and quiesce writers, or supply a real exclusion mechanism.

3. Blocker: concurrent photograph identity creation is unspecified (sections 6-7, 9, 11, 14). Different offline imports can allocate different logical IDs for identical originals; accepting equal destination bytes does not reconcile sidecar ownership, pairing, and Immich mappings. Required decision: refresh shared state under coordination and define canonicalization/aliases, including split RAW/JPEG imports; byte equality must not merge independent placements automatically.

4. Recommendation: acceptance tests should explicitly cover saves between checks and unlink/replacement, resumed old lock owners, and same-content imports with differing group IDs/split member arrivals.

The reviewer found no additional blocker in the bounded rsync/direct-NAS import approach or ordinary reservation model. Existing application-version and NAS capability gates remain necessary. No files or photographs were modified by the reviewer.

## Author response in revised draft

- Section 8: evict immutable original bytes as a photograph unit; retain mutable sidecars/snapshots/parent directories and count their space. Activity holds are explicitly not synchronization guarantees.
- Section 12.1: stage incoming mutable metadata; apply to existing live files through an explicit editor-closed session boundary. Preserve displaced versions. Outgoing saved-edit archiving remains automatic.
- Section 11: stale-lock recovery requires definitive previous-owner shutdown and all writer services quiescent. No timeout-based theft or implied fencing.
- Section 6.1: reconcile complete shared records under the publication lock, reuse canonical IDs for identical intended placements, and publish validated alias/merge records while retaining sidecar lineages and conflicts.
- Sections 20-21: add the targeted recovery/concurrency acceptance scenarios.
- Section 22: identify the review-driven behavior changes as proposed policies requiring user approval.

## Remaining limitations to assess

- Operator-enforced editor/writer quiescence is an explicit supported precondition, not an automatically enforceable distributed guarantee.
- Automatic incoming/live metadata replacement is deferred; the CLI-only first version requires an explicit refresh/apply step for existing local sidecars.
- Immich rating writes may lack compare-and-set support; the spec requires version validation and makes the residual concurrent-write window explicit.
- No runtime tests of the proposed system have been performed.

## Follow-up verdict

The independent reviewer re-read the revised contracts and found all three original blockers resolved at the design level under the stated operating preconditions, with no remaining contradiction blocking approval of those proposed policies. This is not implementation validation.

Three follow-up details were incorporated into the spec: deterministic canonical UUID ordering independent of clocks/discovery; explicit field-level tracking so an unrelated local adjustment save cannot reverse an unapplied incoming rating; and real two-client testing of catalog visibility after lock handoff on the deployed share.

User approval, actual application-version compatibility experiments, and NAS capability validation remain outstanding.

## Execution-readiness review, 2026-10-25

The user requested another review, corrections, and an implementation plan executable unattended by a fresh sol agent. The main review inspected the specification, existing Rust entry points/classifier, runner, CI restrictions, and primary SQLite/rename/notify/Immich documentation. A second fresh builder-check agent independently read the spec and code without conversation history. This is a document review, not a claim of production validation.

| Finding / implementer trap | Resolution in spec |
| --- | --- |
| Deployment prerequisites could halt the entire unattended build or be silently waived | 25.1/25.8 separate complete code/tests from disabled unverified production capabilities |
| NAS I/O can hang even without rsync, freezing offline operation | 25.2 bounded process isolation, no replacement while old worker can publish |
| Root/mount races, symlink escape, source/destination recursion, shell arguments | 25.2 held-root confinement and validated argument/path contracts |
| Pins protected too late; later membership silently reports ready | 25.3/25.9 durable intent and membership revision |
| Reservations double count partials or omit snapshot overhead | 25.3 additional-byte accounting and ENOSPC recovery |
| Content digest incorrectly used as causal version ID | 25.4 separate IDs, reset type, ancestry, immutable payload reuse |
| Unknown/corrupt records silently skipped before publication | 25.4 block publication/eviction on incomplete shared state |
| Ordinary rename silently overwrites, commit/projection crash ambiguity | 25.5 explicit no-replace, authoritative commit before mutable projection |
| External NAS edits erased by projection recovery | 25.5 preserve unexpected live bytes as conflict |
| Scanner absence interpreted as reset; unchanged stats hide edits | 25.6 successful directory observation and periodic mutable hashing |
| Virtual-copy rating incorrectly becomes group rating; stale rating undoes remote | 25.6 base primary RAW authority, three baselines, field-specific merge |
| Lost API response causes stale rating retry | 25.6 durable intent/read-back and explicit non-CAS mode |
| Reuse legacy first-dot pairing or TIFF/PNG-as-JPEG classification | 25.2/25.7/25.9 exact owner matching, conservative pairing, format matrix |
| Date timezone shifts, sequence fragments merged, bootstrap collisions | 25.7 stable wall-clock layout, explicit thresholds, conservative adoption |
| Claims every edit saved, including unsampled intermediate states | 25.9 precise saved-snapshot guarantee |
| Local history grows without accounting; live SQLite copied as backup | 25.3/25.4 proven-redundant local snapshot cleanup and SQLite backup API |
| Old Syncthing/importer still writes managed tree | 25.7 documented manual cutover; no automatic production reconfiguration |

The fresh reviewer independently identified the version-ID, unexpected NAS-sidecar, deletion-vs-inspection, legacy-format, membership-readiness, and intermediate-save issues; all were incorporated. Remaining production evidence includes real NAS failure/visibility semantics and exact installed RapidRAW/Immich behavior. No personal photos, ratings, service configuration, or implementation code were changed.

## Final builder check of the implementation plan

A follow-up builder check found four material gaps, now resolved in spec section 25.12 and the owning plan tasks:

- An editor-closed boolean must not survive a delayed/restarted refresh; application requires a live foreground session, with no automatic reauthorization.
- Catalog repair needs an explicit offline maintenance path when the service cannot open its DB, protected by the same profile lock and preservation of old state.
- Pairing/primary-RAW/JPEG-authority/orphan-sidecar ambiguities need executable typed resolutions, not only sidecar-version conflict choices.
- Source companions must enter the import pipeline, including full-size opaque `.orig` files, without overwriting newer editor data or being ignored by original-only fast scans.

The author's self-review additionally fixed import wait milestones, worker/coordinator lock-session ownership, and explicit single-writer versus multi-writer qualification. The plan includes regression cases and task ownership for these findings. No review can guarantee absence of further defects; fault injection, actual whole-branch code review, and deployment qualification remain required.

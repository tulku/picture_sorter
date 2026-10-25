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

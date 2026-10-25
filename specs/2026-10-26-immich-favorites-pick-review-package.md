# Favorite/Pick review package

Implementation checkout: /home/tulku/Repos/picture_sorter/offline-photo-archive
Range: 8b1e75010c16f8ea831ad296a2869a6a41a261a1..197774c012b76978e37bd8fc3f0bb986cfd390de
The base archive implementation already had an independent whole-branch review; review this complete Favorite extension and its integration with surrounding archive safety boundaries.

Spec: /home/tulku/Repos/picture_sorter/superpowers-plans/specs/2026-10-25-immich-favorites-pick-design.md
Plan: /home/tulku/Repos/picture_sorter/superpowers-plans/plans/2026-10-26-immich-favorites-pick-implementation.md
Ledger: /home/tulku/Repos/picture_sorter/superpowers-plans/plans/2026-10-26-immich-favorites-pick-execution.md

Review focus: Reject while incoming Pick is pending; stale source versions before outbound send; field isolation from stars; absent favorite fields; uncertain HTTP recovery; mapping identity and authority; fair worker scheduling; native qualification scope; immutable history protection and alias migration; foreground refresh guards; preserve divergent adjustments and masks.

Implementation: independent native Pick adapter; exact pinned 1.6.7 source profile; separate scoped Favorite API/probe and editor Pick runtime checks; SQLite baselines/outbox; causal derived metadata; Favorite convergence facts; actual async worker lanes and foreground CLI refresh; existing observer recursion fix exposed by refresh-blocked queues. No production integration was accessed. Deployment evidence remains required and unrun.

Verification before review: targeted 5 HTTP +18 sync +16 qualification +12 adapter tests passed; fmt and strict clippy passed. Full isolated suite and release build results are recorded in the execution ledger when completed. The final report must distinguish deployment tests from generated fixture tests.

Review is read-only on the implementation checkout: no edits, commits, checkout changes, or extra subagents. Return concrete findings with severity, file/line, trigger and effect; explain safe repair direction. Before verdict list every behavior declined as outside scope, or explicitly None. Do not downgrade behavior solely because the spec is silent. The executor will grade findings and address Critical/Important items once with RED→GREEN and full suite verification.

# Readable conflicts and delayed NAS edit catch-up

The user finds conflicts list unusable and has used only one computer. Read-only inspection found three nas_projection records for two RapidRAW sidecars. Each observed NAS digest is a committed ancestor in a single saved-edit chain; the newest projection expects a later committed predecessor. This is delayed projection, not divergent edits.

Automatically replay an already-committed successor projection when the observed NAS bytes match its expected archived ancestor and the successor lies on the sole current head's ancestry. Preserve worker content checks and archive proofs; never relax projection preconditions or overwrite unknown bytes. Exclude generated outputs. Make at most one historical catch-up attempt per publication session. A fresh observation proving known ancestry can retire old nas_projection ambiguity records through the shared journal; the separate projection remains pending until actually applied. This distinction matters on the user's NAS: rename_exchange fails with EINVAL, so existing live edit replacement remains blocked even though immutable edit history is backed up.

Original-photo uploads were observed progressing from 516 to 510 queued while the summary said uploads paused. Display edit publication errors on a separate Saved edits line, and only use archive availability/publication capability for the original backup line. Do not claim that a shared edit error pauses all uploads.

Default conflicts list must show paths, understandable reasons and affected-file context. Keep the original JSON records available under --json, adding presentation context separately. Unknown/competing bytes remain conflicts and no live conflict is manually cleared.

Approaches: replay committed steps preserves existing compare-before-write checks. Relaxing expected-content comparisons would discard safety guarantees. Keeping manual resolutions for proven linear backlog is unnecessary user work.

Existing authorization to proceed directly without design/plan approval rounds applies. Code worktree: /home/tulku/Repos/picture_sorter/readable-conflicts. Planning remains on meta/superpowers-plans.

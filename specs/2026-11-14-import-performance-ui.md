# Fast import accounting and useful terminal output

Imports reuse the persistent card and archive catalogues, copy only new originals locally, and upload in the background. Preserve all integrity, quota, reserve, metadata, cancellation and machine JSON contracts.

Replace per-file recursive capacity measurements with a reconciled baseline and conservative tracked allocations. Read current free space cheaply at admission, reconcile unknown filesystem changes periodically, and retain crash-safe accounting. Batch catalogue recognition commits and avoid writing unchanged source entries.

Interactive imports show stable simultaneous card, local-copy and upload progress plus current activity. Piped output stays concise and contains no terminal controls. Completion distinguishes safely copied files from queued uploads. Human status presents bounded current work, upload queue, cache and disk information; --json remains detailed.

Validate accounting transitions, live mutating cached scans and output contracts, then run the complete native and Nix checks. Merge and push source main, pin/build the Nix configuration, and leave activation to the user.

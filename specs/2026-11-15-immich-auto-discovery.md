# Automatic Immich matching

Discover the configured external library automatically in bounded background API pages. Resolve exact translated archive JPEG paths and library identity against known originals, without pinning/downloading them. Skip generated TIFFs, trashed assets, absent catalog entries and unrelated libraries. Do not upload, recreate or delete missing Immich assets. Retry transient API errors and revisit library pages on later poll cycles. Preserve existing identity checks and ambiguous mapping conflicts.

User explicitly selected first-sync union: existing Pick or Favorite wins, except explicit RapidRAW Reject clears Favorite. Subsequent changes retain existing three-way conflict behavior. Native editor and API qualification remain mandatory. Automatic discovery must not monopolize HTTP processing or block local work. Existing capability status must reflect real qualification rather than hardcoded false. Report matching progress compactly.

Chosen approach: paginate Immich metadata search through the existing HTTP worker, resolve and bind bounded results on service thread. Alternatives of one-time database seeding or requiring pins fail ongoing library sync and user intent. No new dependencies or secrets. Current live key successfully reads metadata search.

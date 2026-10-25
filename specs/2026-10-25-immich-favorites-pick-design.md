# Immich favorites and RapidRAW Pick synchronization

Status: approved, including the user-directed Reject precedence rule.

## Goal and approach

Synchronize Immich's native `isFavorite` boolean with the primary RAW sidecar's RapidRAW Pick flag through picture_sorter's existing JPEG-to-photograph mapping. Reuse the existing saved-metadata history, durable HTTP work, conflict handling and foreground application boundary. Keep star ratings independent. This operates in ordinary-folder RapidRAW mode; it does not depend on RapidRAW's native Immich cache or custom metadata store. A one-way mirror would not meet the requested synchronization, and mapping favorites onto a star threshold would conflate independent user choices.

## Proposed behavior

- Favorite on in Immich corresponds to `flag: "pick"` in the mapped RAW's `.rrdata`.
- Favorite off clears an existing Pick to the native neutral representation; clearing a Pick sends `isFavorite: false` to Immich.
- Reject remains a distinct local flag and wins over Immich Favorite. A saved Reject clears the remote favorite; incoming Favorite never replaces Reject and this pairing does not create a favorite conflict. Cancel stale pending Picks without losing other editing data.
- Preserve ratings, adjustments, masks, tags, virtual copies and unknown metadata fields. Only the authoritative mapped JPEG/base RAW flag participates, with existing authority selection for ambiguous groups.
- Missing API fields, unavailable sidecars and unreadable history are unobserved, never implicit false values. Establish baselines without treating absent information as a user edit.
- Compare changes against the last agreed state and the actual applied local version. Persist pending incoming values without bouncing the old live flag back to Immich. Concurrent contradictory changes preserve both observations for explicit resolution.
- Archive local saved flag changes normally. Stage incoming changes as immutable derived metadata; apply to an existing live sidecar only during foreground `refresh --editor-closed`. Do not fetch a RAW merely to reconcile saved metadata.
- Outgoing requests change only `isFavorite`, validate the exact JPEG binding, and use read-before-write/readback. Preserve uncertain outbox state across crashes; do not blindly resend after an ambiguous result. Existing API non-CAS limitations remain explicit.
- Add actual CLI/service/adapter wiring, isolated tests and executable favorite/pick qualification. Existing star-only evidence must not silently qualify favorite writes or native flag transforms. Do not mutate personal assets, archives, editor state or installed services while implementing.

## Validation required

Both directions and clearing; initial unknown states; unchanged stars/edits/masks; Reject priority and stale pending Pick cancellation; changes made while incoming application is pending; identity changes; duplicate observations; offline and crash recovery; fair scheduling with star updates; real loopback `isFavorite` PUT/readback; real staged/foreground metadata application; exact-version isolated qualification with guarded restoration; regression suite and documentation.

## Sources inspected

- Immich v3.3.5 asset update schema: https://github.com/immich-app/immich/blob/v3.3.5/server/src/dtos/asset.dto.ts (`isFavorite` is independent from `rating`).
- RapidRAW native source previously inspected by this repository: `set_flag_for_paths` sets `metadata.flag` independently; supported existing representations are Pick, Reject and neutral.
- Existing implementation: `src/immich/ratings.rs`, `src/immich/client.rs`, `src/edit/rapidraw.rs`, service HTTP scheduling and qualification modules.

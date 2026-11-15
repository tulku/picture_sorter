# Immich Favorite setup and 3.4 compatibility

User requested pinning and Favorite sync with https://photos.candlekeep.uk, SOPS key storage, and explicitly authorized toggling/restoring asset 90afa0cb-52d4-4768-81f6-5b6002032e2e without deletion. Direct authenticated test on v3.4.0 passed: false to true to false, unchanged identity/rating, image present. Evidence is private local `immich-favorite-api-check.json`.

Nix configuration provides the SOPS key through a private regular file in /run/photo-sorter-tulku, and maps library bd4f8595-fec6-4b94-bd5f-421dac3af1e6 from /shares/common/Camera/OM1-II to archive root. Full system build passed. Outgoing writes remain disabled pending exact runtime qualification and native RapidRAW Pick evidence.

Implementation in /home/tulku/Repos/picture_sorter/immich-favorites-34: allow Favorite API versions 3.3.5 and 3.4.0, including their real v-prefixed server-about representations. Record exact observed version in Favorite qualification evidence; never reuse evidence across versions. Leave star-rating support unchanged. Test qualification/readback/restoration and invalidation across upgrades and unsupported versions. Run focused tests, formatting, Clippy, independent review, and publish a draft PR. Perform built-in Favorite qualification with the new binary on the authorized image, restoring it; do not fabricate evidence or mark native RapidRAW qualified without native saves. No image deletion.

Remaining native editor checks need actual isolated saves/reopen/reset/Pick/clear/Reject. Do not bypass them to claim complete sync.

Execution: regression failed on v-prefixed versions before the fix, then all 103 relevant integration tests passed. Independent review found no material issues. Built-in qualification using an uncommitted temporary example passed supported_schema, favorite_true, favorite_false and restore_original on v3.4.0. Report saved owned mode 0600 to `/home/tulku/.local/share/photo-sorter/immich-favorites-runtime.json`. Temporary example and temporary decrypted key were removed. Outgoing synchronization remains disabled pending native editor evidence and integration of the code change.

Formatting and Clippy passed. Code commit `60281ee`; draft PR https://github.com/tulku/picture_sorter/pull/3. Final GET confirmed isFavorite=false, isTrashed=false, rating=null. No delete request was issued. No Nix package pin or live service was changed in this step.

# Recognise JPEG imports despite geotagging

## Intent

Recognise an SD JPEG as already imported when its image matches the existing NAS JPEG but geotags or other descriptive metadata differ. Retain the NAS version and its metadata without uploading another copy.

## Approach

Use a separate, versioned image-content fingerprint for JPEG duplicate recognition. Keep the current full-file digest for transfer verification, archive byte identity, and integrity checks. Replacing full-file hashes everywhere would lose exact-copy verification; comparing filenames or RAW companions alone would not verify JPEG content.

## Behaviour

- Compare the encoded JPEG image structure/data, excluding descriptive EXIF/XMP/IPTC metadata. Preserve rendering differences such as orientation and colour profiles in the identity so they are not silently treated as equivalent.
- Scope recognition to the expected archived photo/capture identity. Ambiguous matches remain conflicts; do not globally collapse intentionally duplicated photos.
- An image match retains the NAS JPEG and metadata and records the incoming JPEG as recognised without copying it to Working or NAS. Record equivalence separately; never claim the incoming whole-file digest has been archived byte-for-byte.
- Different image content continues through existing collision handling. Unsupported or malformed JPEGs fall back conservatively to the existing exact-file/conflict behaviour.
- RAW and sidecar identity and transfer verification remain byte-exact.
- Compute/cache fingerprints on demand, keyed by verified file digests; do not restart the current NAS catalogue or require another full NAS scan. A later implementation may populate the cache during normal scanning.
- Recognition must also work when an earlier preserve-both choice reserved a _CAMERA_ORIGINAL destination. Existing copies and journal decisions are retained; this change performs no cleanup/deletions.

## Validation

Prove metadata-only geotag differences result in zero copied photo bytes and preserve NAS metadata. Test actual image differences, orientation/colour changes, malformed files, ambiguous matches, repeated imports, reserved alternate paths, and source/NAS mutation during comparison. Preserve existing transfer/integrity tests and honest import accounting.

## Scope

This changes JPEG import recognition. It does not redesign NAS metadata replacement or weaken full-file integrity checking. No automatic deletion of the 20 suffix copies created earlier.

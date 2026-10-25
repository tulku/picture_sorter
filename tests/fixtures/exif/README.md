These fixtures transcribe the relevant observations documented in design section 16.2. They are not fresh reads of personal photographs. Directory context is synthetic. The single-shot capture time is synthetic; only its date, mode, shutter and sequence were documented. Camera identity is supplied explicitly by tests. Matching first RAW observations use the documented same shooting metadata; no original image bytes are included.

To qualify another camera/editor installation, extract only selected isolated files with:

```
exiftool -j -DateTimeOriginal -SubSecTimeOriginal -OffsetTimeOriginal -Make -Model -SpecialMode -DriveMode -ShutterMode -ExposureCompensation -- /explicit/test/file.ORF
```

The presentation-string parser recognizes English Olympus Sequential/Shot and AE Auto Bracketing/Shot forms. Unknown forms remain unclassified. Camera identity and directory context must be established separately. Existing placements remain authoritative.

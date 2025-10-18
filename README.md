# Picture Sorter

A fast, intelligent photo organization tool written in Rust that automatically sorts photos by date into organized directory structures while detecting and preserving photo sequences (HDR and burst modes).

## Features

- **Automatic Date-based Organization**: Sorts photos into `RAW/YYYY/MM/DD/` and `JPEG/YYYY/MM/DD/` structure based on EXIF data
- **Sequence Detection**: Automatically detects and groups HDR sequences and burst sequences into dedicated folders
- **Incremental Processing**: Only process files newer than the most recent file in destination (great for regular imports)
- **EXIF Date Extraction**: Uses photo metadata for accurate date sorting with file modification time fallback
- **Parallel Processing**: Multi-threaded EXIF data processing for improved performance
- **Comprehensive Validation**: Checks for file conflicts and provides detailed error reporting
- **Dry Run Mode**: Preview operations without actually copying files
- **Progress Tracking**: Real-time progress bars for all operations

## Prerequisites

**exiftool** must be installed on your system:

- **Ubuntu/Debian**: `sudo apt install libimage-exiftool-perl`
- **macOS**: `brew install exiftool`
- **Other systems**: See [exiftool.org/install.html](https://exiftool.org/install.html)

## Installation

### Option 1: Build from Source

```bash
git clone https://github.com/tulku/picture_sorter.git
cd picture_sorter
cargo build --release
```

The executable will be located at `target/release/photo_sorter`.

### Option 2: Direct Cargo Install

```bash
cargo install --git https://github.com/tulku/picture_sorter.git
```

## Usage

### Basic Usage

```bash
# Sort all photos from source to destination
photo_sorter /path/to/source/photos /path/to/organized/photos

# Preview what would be copied (dry run)
photo_sorter --dry-run /path/to/source/photos /path/to/organized/photos
```

### Incremental Mode

Perfect for regular imports - only processes files newer than the most recent file in your destination:

```bash
# Only process new photos since last run
photo_sorter --incremental /path/to/source/photos /path/to/organized/photos

# Dry run with incremental mode
photo_sorter --incremental --dry-run /path/to/source/photos /path/to/organized/photos
```

## Directory Structure

The tool organizes photos into this structure:

```
destination/
├── RAW/
│   ├── 2024/
│   │   ├── 10/
│   │   │   ├── 15/
│   │   │   │   ├── IMG_1234.CR2
│   │   │   │   ├── IMG_1235.CR2
│   │   │   │   └── IMG_1236_HDR/          # HDR sequence folder
│   │   │   │       ├── IMG_1236.CR2
│   │   │   │       ├── IMG_1237.CR2
│   │   │   │       └── IMG_1238.CR2
│   │   │   └── 16/
│   │   │       └── IMG_1240_BURST/        # Burst sequence folder
│   │   │           ├── IMG_1240.CR2
│   │   │           ├── IMG_1241.CR2
│   │   │           └── IMG_1242.CR2
│   │   └── 11/
│   └── 2025/
└── JPEG/
    ├── 2024/
    │   ├── 10/
    │   │   ├── 15/
    │   │   │   ├── IMG_1234.JPG
    │   │   │   ├── IMG_1235.JPG
    │   │   │   └── IMG_1236_HDR/
    │   │   │       ├── IMG_1236.JPG
    │   │   │       ├── IMG_1237.JPG
    │   │   │       └── IMG_1238.JPG
    │   │   └── 16/
    │   └── 11/
    └── 2025/
```

## Supported File Types

### RAW Formats
- Canon: `.cr2`
- Nikon: `.nef`
- Sony: `.arw`
- Adobe: `.dng`
- Olympus: `.orf`
- Generic: `.raw`

### JPEG Formats
- `.jpg`, `.jpeg`

### Associated Files
The tool also copies associated files (like `.xmp` sidecar files) and places them in the appropriate RAW or JPEG directories based on their naming convention.

## Sequence Detection

### HDR Sequences
Automatically detects HDR (High Dynamic Range) photo sequences based on EXIF metadata:
- Looks for "AE Auto Bracketing" and "Electronic shutter" in DriveMode
- Groups consecutive shots (Shot 1, Shot 2, Shot 3, etc.)
- Creates folders named `{first_photo}_HDR`

### Burst Sequences
Detects burst/continuous shooting sequences:
- Reads sequence numbers from EXIF SpecialMode field
- Groups consecutive numbered shots
- Creates folders named `{first_photo}_BURST`

## Command Line Options

```
Usage: photo_sorter [OPTIONS] <INPUT_DIR> <OUTPUT_DIR>

Arguments:
  <INPUT_DIR>   Input directory path
  <OUTPUT_DIR>  Output directory path

Options:
      --dry-run      Print actions without copying files
      --incremental  Only process files newer than the most recent file in the destination directory
  -h, --help         Print help
  -V, --version      Print version
```

## Example Workflows

### Initial Photo Import
```bash
# First time organizing a large photo collection
photo_sorter --dry-run ~/Downloads/Photos ~/Pictures/Organized
# Review the plan, then run for real:
photo_sorter ~/Downloads/Photos ~/Pictures/Organized
```

### Regular Import Routine
```bash
# After each photo session, only import new photos. First try with dry_run
cargo run --release -- --dry-run --incremental /run/media/tulku/OM\ SYSTEM/ ~/Shares/Camera/OM1-II/
# And if you are happy, run without
cargo run --release -- --incremental /run/media/tulku/OM\ SYSTEM/ ~/Shares/Camera/OM1-II/
```

### Camera Card Import
```bash
# Import directly from camera card
photo_sorter --incremental /media/camera-card ~/Pictures/Organized
```

## Error Handling

The tool provides comprehensive error reporting:
- **File conflicts**: Reports if destination files already exist
- **Permission issues**: Identifies files that can't be read or written
- **Invalid dates**: Falls back to file modification time for photos without valid EXIF dates
- **Missing exiftool**: Clear instructions for installing the required dependency

## Performance

- **Multi-threaded**: EXIF processing uses all available CPU cores
- **Efficient scanning**: Incremental mode scans destination in reverse chronological order
- **Memory efficient**: Processes files in batches to handle large photo collections
- **Progress tracking**: Real-time feedback on processing status

## Contributing

1. Fork the repository
2. Create a feature branch: `git checkout -b feature-name`
3. Make your changes and test them
4. Commit your changes: `git commit -am 'Add feature'`
5. Push to the branch: `git push origin feature-name`
6. Submit a pull request

## License

This project is licensed under the MIT License - see the LICENSE file for details.

## Troubleshooting

### "exiftool is not installed"
Install exiftool using your system's package manager (see Prerequisites section).

### "Destination already exists" errors
The tool won't overwrite existing files. Either:
- Move/rename conflicting files in the destination
- Use a different destination directory
- Delete the conflicting files if you're sure they're duplicates

### Photos appear in wrong date folders
This can happen if:
- EXIF DateTimeOriginal is missing or invalid (falls back to file modification time)
- Camera clock was set incorrectly when photos were taken
- Files were modified after being taken

### Sequences not detected properly
Sequence detection relies on specific EXIF metadata patterns. If your camera uses different metadata formats, the sequences may not be detected. This is primarily tested with Canon cameras.
use chrono::{DateTime, Datelike, NaiveDateTime, Utc};
use clap::Parser;
use indicatif::{ProgressBar, ProgressStyle};
use rayon::prelude::*;
use regex::Regex;
use serde_json::Value;
use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

#[derive(Parser)]
#[command(author, version, about, long_about = None)]
struct Args {
    /// Input directory path
    input_dir: String,
    /// Output directory path
    output_dir: String,
    /// Print actions without copying files
    #[arg(long)]
    dry_run: bool,
    /// Only process files newer than the most recent file in the destination directory
    #[arg(long)]
    incremental: bool,
}

#[derive(Debug)]
struct ValidationError {
    file: String,
    reason: String,
}

#[derive(Debug, Clone)]
enum SequenceType {
    Burst(String), // folder name
    Hdr(String),   // folder name
}

fn check_exiftool_installed() -> Result<(), Box<dyn std::error::Error>> {
    match Command::new("exiftool").arg("-ver").output() {
        Ok(output) => {
            if output.status.success() {
                let version = String::from_utf8_lossy(&output.stdout).trim().to_string();
                println!("Found exiftool version: {}", version);
                Ok(())
            } else {
                Err("exiftool command failed to execute properly".into())
            }
        }
        Err(_) => {
            Err("exiftool is not installed or not found in PATH. 

Please install exiftool to use this program:
- On Ubuntu/Debian: sudo apt install libimage-exiftool-perl
- On macOS: brew install exiftool
- On other systems: https://exiftool.org/install.html".into())
        }
    }
}

fn get_exif_data(file_path: &Path) -> Result<Value, Box<dyn std::error::Error>> {
    let output = Command::new("exiftool").arg("-j").arg(file_path).output()?;
    let json: Vec<Value> = serde_json::from_slice(&output.stdout)?;
    Ok(json.into_iter().next().unwrap_or(Value::Null))
}

fn get_exif_date(exif: &Value) -> Option<DateTime<Utc>> {
    if let Some(date_str) = exif.get("DateTimeOriginal").and_then(|v| v.as_str()) {
        if let Ok(dt) = NaiveDateTime::parse_from_str(date_str, "%Y:%m:%d %H:%M:%S") {
            let date = DateTime::<Utc>::from_naive_utc_and_offset(dt, Utc);
            // Validate date is reasonable (between 1990 and 2050)
            if date.year() >= 1990 && date.year() <= 2050 {
                return Some(date);
            }
        }
    }
    None
}

fn get_sequence_info(exif: &Value, re: &Regex) -> u32 {
    if let Some(special_mode) = exif.get("SpecialMode").and_then(|v| v.as_str()) {
        if let Some(captures) = re.captures(special_mode) {
            if let Some(seq_str) = captures.get(1) {
                if let Ok(seq) = seq_str.as_str().parse::<u32>() {
                    return seq;
                }
            }
        }
    }
    0
}

fn get_hdr_info(exif: &Value, hdr_re: &Regex) -> Option<u32> {
    if let Some(drive_mode) = exif.get("DriveMode").and_then(|v| v.as_str()) {
        if drive_mode.contains("AE Auto Bracketing") && drive_mode.contains("Electronic shutter") {
            if let Some(captures) = hdr_re.captures(drive_mode) {
                if let Some(shot_str) = captures.get(1) {
                    if let Ok(shot_num) = shot_str.as_str().parse::<u32>() {
                        return Some(shot_num);
                    }
                }
            }
        }
    }
    None
}

fn is_raw_file(filename: &str) -> bool {
    let ext = filename.to_lowercase();
    ext.ends_with(".cr2")
        || ext.ends_with(".nef")
        || ext.ends_with(".arw")
        || ext.ends_with(".dng")
        || ext.ends_with(".raw")
        || ext.ends_with(".orf")
}

fn is_jpeg_file(filename: &str) -> bool {
    let ext = filename.to_lowercase();
    ext.ends_with(".jpg") || ext.ends_with(".jpeg")
}

fn find_most_recent_file_in_destination(
    output_dir: &Path,
) -> Result<Option<DateTime<Utc>>, Box<dyn std::error::Error>> {
    let raw_dir = output_dir.join("RAW");
    let jpeg_dir = output_dir.join("JPEG");

    let mut most_recent_date: Option<DateTime<Utc>> = None;
    let mut files_checked = 0;

    // Check both RAW and JPEG directories
    for base_dir in [&raw_dir, &jpeg_dir] {
        if !base_dir.exists() {
            continue;
        }

        // Walk through year/month/day directories in reverse order for efficiency
        let mut year_dirs: Vec<_> = fs::read_dir(base_dir)?
            .filter_map(|entry| {
                let entry = entry.ok()?;
                let path = entry.path();
                if path.is_dir() {
                    if let Some(year_name) = path.file_name()?.to_str() {
                        if let Ok(year) = year_name.parse::<u32>() {
                            return Some((year, path));
                        }
                    }
                }
                None
            })
            .collect();

        // Sort years in descending order (most recent first)
        year_dirs.sort_by(|a, b| b.0.cmp(&a.0));

        'outer: for (_year, year_dir) in year_dirs {
            let mut month_dirs: Vec<_> = fs::read_dir(&year_dir)?
                .filter_map(|entry| {
                    let entry = entry.ok()?;
                    let path = entry.path();
                    if path.is_dir() {
                        if let Some(month_name) = path.file_name()?.to_str() {
                            if let Ok(month) = month_name.parse::<u32>() {
                                return Some((month, path));
                            }
                        }
                    }
                    None
                })
                .collect();

            // Sort months in descending order (most recent first)
            month_dirs.sort_by(|a, b| b.0.cmp(&a.0));

            for (_month, month_dir) in month_dirs {
                let mut day_dirs: Vec<_> = fs::read_dir(&month_dir)?
                    .filter_map(|entry| {
                        let entry = entry.ok()?;
                        let path = entry.path();
                        if path.is_dir() {
                            if let Some(day_name) = path.file_name()?.to_str() {
                                if let Ok(day) = day_name.parse::<u32>() {
                                    return Some((day, path));
                                }
                            }
                        }
                        None
                    })
                    .collect();

                // Sort days in descending order (most recent first)
                day_dirs.sort_by(|a, b| b.0.cmp(&a.0));

                for (_day, day_dir) in day_dirs {
                    // Check all files and subdirectories in this day directory
                    fn check_directory_for_photos(
                        dir: &Path,
                        most_recent: &mut Option<DateTime<Utc>>,
                        files_checked: &mut usize,
                    ) -> Result<(), Box<dyn std::error::Error>> {
                        for entry in fs::read_dir(dir)? {
                            let entry = entry?;
                            let path = entry.path();

                            if path.is_dir() {
                                // Recursively check subdirectories (for sequence folders)
                                check_directory_for_photos(&path, most_recent, files_checked)?;
                            } else if path.is_file() {
                                if let Some(filename) = path.file_name().and_then(|n| n.to_str()) {
                                    if is_raw_file(filename) || is_jpeg_file(filename) {
                                        *files_checked += 1;

                                        // First try to get EXIF date
                                        if let Ok(exif) = get_exif_data(&path) {
                                            if let Some(exif_date) = get_exif_date(&exif) {
                                                if most_recent
                                                    .map_or(true, |current| exif_date > current)
                                                {
                                                    *most_recent = Some(exif_date);
                                                }
                                                continue;
                                            }
                                        }

                                        // Fall back to modification time
                                        if let Ok(metadata) = fs::metadata(&path) {
                                            if let Ok(mtime) = metadata.modified() {
                                                let mtime_dt = DateTime::<Utc>::from(mtime);
                                                if most_recent
                                                    .map_or(true, |current| mtime_dt > current)
                                                {
                                                    *most_recent = Some(mtime_dt);
                                                }
                                            }
                                        }
                                    }
                                }
                            }
                        }
                        Ok(())
                    }

                    check_directory_for_photos(
                        &day_dir,
                        &mut most_recent_date,
                        &mut files_checked,
                    )?;

                    // If we found files in this day and we're going in reverse chronological order,
                    // we can be confident this is the most recent date
                    if most_recent_date.is_some() {
                        break 'outer;
                    }
                }
            }
        }
    }

    if files_checked > 0 {
        println!("Scanned {} files in destination directory.", files_checked);
        if let Some(date) = most_recent_date {
            println!(
                "Most recent file found: {}",
                date.format("%Y-%m-%d %H:%M:%S UTC")
            );
        }
    } else {
        println!("No photo files found in destination directory.");
    }

    Ok(most_recent_date)
}

fn collect_all_files_recursive(directory: &Path) -> Vec<PathBuf> {
    let mut all_files = Vec::new();

    fn collect_recursive(dir: &Path, files: &mut Vec<PathBuf>) {
        if let Ok(entries) = fs::read_dir(dir) {
            for entry in entries.flatten() {
                let path = entry.path();
                if path.is_file() {
                    files.push(path);
                } else if path.is_dir() {
                    collect_recursive(&path, files);
                }
            }
        }
    }

    collect_recursive(directory, &mut all_files);
    all_files
}

fn group_files_by_base(directory: &Path) -> HashMap<String, Vec<PathBuf>> {
    let all_files = collect_all_files_recursive(directory);
    let mut groups: HashMap<String, Vec<PathBuf>> = HashMap::new();

    let pb = ProgressBar::new(all_files.len() as u64);
    pb.set_style(
        ProgressStyle::default_bar()
            .template("{spinner:.green} [{elapsed_precise}] [{wide_bar:.cyan/blue}] {pos}/{len} ({eta}) Grouping files...")
            .expect("Failed to set progress bar style"),
    );

    for file_path in all_files {
        if let Some(filename) = file_path.file_name().and_then(|n| n.to_str()) {
            let base = filename.split('.').next().unwrap_or("").to_string();
            groups.entry(base).or_default().push(file_path);
        }
        pb.inc(1);
    }

    pb.finish_with_message("File grouping complete");
    groups
}

fn cache_exif_data(groups: &HashMap<String, Vec<PathBuf>>) -> HashMap<String, (PathBuf, Value)> {
    let mut representative_files = Vec::new();
    for (base, file_list) in groups {
        let photo_files: Vec<PathBuf> = file_list
            .iter()
            .filter(|f| {
                if let Some(filename) = f.file_name().and_then(|n| n.to_str()) {
                    is_raw_file(filename) || is_jpeg_file(filename)
                } else {
                    false
                }
            })
            .cloned()
            .collect();

        if !photo_files.is_empty() {
            // Pick JPEG if available, else first photo file
            let rep_file = photo_files
                .iter()
                .find(|f| {
                    if let Some(filename) = f.file_name().and_then(|n| n.to_str()) {
                        is_jpeg_file(filename)
                    } else {
                        false
                    }
                })
                .unwrap_or(&photo_files[0])
                .clone();
            representative_files.push((base.clone(), rep_file));
        }
    }

    let pb = ProgressBar::new(representative_files.len() as u64);
    pb.set_style(
        ProgressStyle::default_bar()
            .template("{spinner:.green} [{elapsed_precise}] [{wide_bar:.cyan/blue}] {pos}/{len} ({eta}) Caching EXIF data...")
            .expect("Failed to set progress bar style"),
    );

    let exif_cache: HashMap<String, (PathBuf, Value)> = representative_files
        .par_iter()
        .filter_map(|(base, rep_file)| match get_exif_data(rep_file) {
            Ok(data) => {
                pb.inc(1);
                Some((base.clone(), (rep_file.clone(), data)))
            }
            Err(_) => {
                pb.inc(1);
                None
            }
        })
        .collect();

    pb.finish_with_message("EXIF caching complete");
    exif_cache
}

fn detect_sequences(
    files: &HashMap<String, Vec<PathBuf>>,
    exif_cache: &HashMap<String, (PathBuf, Value)>,
) -> HashMap<String, SequenceType> {
    let burst_re = Regex::new(r"Sequence:\s*(\d+)").expect("Invalid regex for burst sequence");
    let hdr_re = Regex::new(r"Shot\s+(\d+)").expect("Invalid regex for HDR sequence");

    // Collect all photo bases with their sequence numbers and dates
    let mut photo_info: Vec<(String, u32, Option<u32>, DateTime<Utc>)> = Vec::new();

    for (base, file_list) in files {
        let photo_files: Vec<PathBuf> = file_list
            .iter()
            .filter(|f| {
                if let Some(filename) = f.file_name().and_then(|n| n.to_str()) {
                    is_raw_file(filename) || is_jpeg_file(filename)
                } else {
                    false
                }
            })
            .cloned()
            .collect();

        if !photo_files.is_empty() {
            let Some((_, exif)) = exif_cache.get(base) else {
                continue;
            };
            let burst_seq_num = get_sequence_info(exif, &burst_re);
            let hdr_shot_num = get_hdr_info(exif, &hdr_re);
            let date = get_exif_date(exif).unwrap_or_else(|| {
                // Fallback to modification time if no EXIF date
                Utc::now()
            });
            photo_info.push((base.clone(), burst_seq_num, hdr_shot_num, date));
        }
    }

    // Sort by date first, then by name to establish order
    photo_info.sort_by(|a, b| a.3.cmp(&b.3).then_with(|| a.0.cmp(&b.0)));

    let pb = ProgressBar::new(photo_info.len() as u64);
    pb.set_style(
        ProgressStyle::default_bar()
            .template("{spinner:.green} [{elapsed_precise}] [{wide_bar:.cyan/blue}] {pos}/{len} ({eta}) Detecting sequences...")
            .expect("Failed to set progress bar style"),
    );

    let mut sequences: HashMap<String, SequenceType> = HashMap::new();

    // First pass: detect HDR sequences
    let mut current_hdr_sequence: Vec<String> = Vec::new();
    let mut expected_hdr_shot = 1u32;
    let mut hdr_sequence_name = String::new();

    for (base, _burst_seq_num, hdr_shot_num, _date) in &photo_info {
        pb.inc(1);

        if let Some(shot_num) = hdr_shot_num {
            if *shot_num == 1 {
                // Start of a new HDR sequence
                if current_hdr_sequence.len() > 1 {
                    // Finish previous HDR sequence
                    for hdr_base in &current_hdr_sequence {
                        sequences.insert(
                            hdr_base.clone(),
                            SequenceType::Hdr(hdr_sequence_name.clone()),
                        );
                    }
                }
                hdr_sequence_name = format!("{}_HDR", base);
                current_hdr_sequence = vec![base.clone()];
                expected_hdr_shot = 2;
            } else if *shot_num == expected_hdr_shot && !current_hdr_sequence.is_empty() {
                // Continue current HDR sequence
                current_hdr_sequence.push(base.clone());
                expected_hdr_shot += 1;
            } else {
                // HDR sequence broken - finish current sequence if it has multiple photos
                if current_hdr_sequence.len() > 1 {
                    for hdr_base in &current_hdr_sequence {
                        sequences.insert(
                            hdr_base.clone(),
                            SequenceType::Hdr(hdr_sequence_name.clone()),
                        );
                    }
                }

                // Check if this starts a new HDR sequence
                if *shot_num == 1 {
                    hdr_sequence_name = format!("{}_HDR", base);
                    current_hdr_sequence = vec![base.clone()];
                    expected_hdr_shot = 2;
                } else {
                    current_hdr_sequence.clear();
                    expected_hdr_shot = 1;
                }
            }
        } else {
            // Not part of HDR - finish current HDR sequence if any
            if current_hdr_sequence.len() > 1 {
                for hdr_base in &current_hdr_sequence {
                    sequences.insert(
                        hdr_base.clone(),
                        SequenceType::Hdr(hdr_sequence_name.clone()),
                    );
                }
            }
            current_hdr_sequence.clear();
            expected_hdr_shot = 1;
        }
    }

    // Don't forget the last HDR sequence
    if current_hdr_sequence.len() > 1 {
        for hdr_base in &current_hdr_sequence {
            sequences.insert(
                hdr_base.clone(),
                SequenceType::Hdr(hdr_sequence_name.clone()),
            );
        }
    }

    // Second pass: detect burst sequences (only for photos not already in HDR sequences)
    let mut current_burst_sequence: Vec<String> = Vec::new();
    let mut expected_burst_num = 0u32;
    let mut burst_sequence_name = String::new();

    for (base, burst_seq_num, _hdr_shot_num, _date) in &photo_info {
        // Skip if already part of an HDR sequence
        if sequences.contains_key(base) {
            continue;
        }

        if *burst_seq_num == 0 {
            // Not part of a burst sequence - finish current sequence if any
            if current_burst_sequence.len() > 1 {
                for burst_base in &current_burst_sequence {
                    sequences.insert(
                        burst_base.clone(),
                        SequenceType::Burst(burst_sequence_name.clone()),
                    );
                }
            }
            current_burst_sequence.clear();
            expected_burst_num = 0;
        } else if *burst_seq_num == 1 {
            // Start of a new burst sequence
            if current_burst_sequence.len() > 1 {
                for burst_base in &current_burst_sequence {
                    sequences.insert(
                        burst_base.clone(),
                        SequenceType::Burst(burst_sequence_name.clone()),
                    );
                }
            }
            burst_sequence_name = format!("{}_BURST", base);
            current_burst_sequence = vec![base.clone()];
            expected_burst_num = 2;
        } else if *burst_seq_num == expected_burst_num && !current_burst_sequence.is_empty() {
            // Continue current burst sequence
            current_burst_sequence.push(base.clone());
            expected_burst_num += 1;
        } else {
            // Burst sequence broken - finish current sequence if it has multiple photos
            if current_burst_sequence.len() > 1 {
                for burst_base in &current_burst_sequence {
                    sequences.insert(
                        burst_base.clone(),
                        SequenceType::Burst(burst_sequence_name.clone()),
                    );
                }
            }

            // Check if this starts a new burst sequence
            if *burst_seq_num == 1 {
                burst_sequence_name = format!("{}_BURST", base);
                current_burst_sequence = vec![base.clone()];
                expected_burst_num = 2;
            } else {
                current_burst_sequence.clear();
                expected_burst_num = 0;
            }
        }
    }

    // Don't forget the last burst sequence
    if current_burst_sequence.len() > 1 {
        for burst_base in &current_burst_sequence {
            sequences.insert(
                burst_base.clone(),
                SequenceType::Burst(burst_sequence_name.clone()),
            );
        }
    }

    pb.finish_with_message(format!(
        "Sequence detection complete. Found {} photos in sequences.",
        sequences.len()
    ));

    // Print detected sequences for debugging
    let mut hdr_sequences: Vec<String> = Vec::new();
    let mut burst_sequences: Vec<String> = Vec::new();

    for (_base, seq_type) in &sequences {
        match seq_type {
            SequenceType::Hdr(folder_name) => {
                if !hdr_sequences.contains(folder_name) {
                    hdr_sequences.push(folder_name.clone());
                }
            }
            SequenceType::Burst(folder_name) => {
                if !burst_sequences.contains(folder_name) {
                    burst_sequences.push(folder_name.clone());
                }
            }
        }
    }

    if !hdr_sequences.is_empty() {
        println!("Detected {} HDR sequences.", hdr_sequences.len());
    } else {
        println!("No HDR sequences detected.");
    }

    if !burst_sequences.is_empty() {
        println!("Detected {} BURST sequences.", burst_sequences.len());
    } else {
        println!("No BURST sequences detected.");
    }

    sequences
}

fn determine_target_base(
    filename: &str,
    raw_dir: &Path,
    jpeg_dir: &Path,
    default_base: &Path,
) -> PathBuf {
    if is_raw_file(filename) {
        raw_dir.to_path_buf()
    } else if is_jpeg_file(filename) {
        jpeg_dir.to_path_buf()
    } else {
        // For associated files, parse the name
        let parts: Vec<&str> = filename.split('.').collect();
        if parts.len() >= 3 {
            let format = parts[parts.len() - 2].to_uppercase();
            if format == "ORF"
                || format == "CR2"
                || format == "NEF"
                || format == "ARW"
                || format == "DNG"
                || format == "RAW"
            {
                raw_dir.to_path_buf()
            } else if format == "JPG" || format == "JPEG" {
                jpeg_dir.to_path_buf()
            } else {
                default_base.to_path_buf()
            }
        } else {
            default_base.to_path_buf()
        }
    }
}

fn validate_and_plan_copy(
    output_dir: &Path,
    groups: &HashMap<String, Vec<PathBuf>>,
    sequences: &HashMap<String, SequenceType>,
    exif_cache: &HashMap<String, (PathBuf, Value)>,
    cutoff_date: Option<DateTime<Utc>>,
) -> Result<Vec<(PathBuf, PathBuf)>, Vec<ValidationError>> {
    let raw_dir = output_dir.join("RAW");
    let jpeg_dir = output_dir.join("JPEG");
    let mut errors = Vec::new();
    let mut copy_plan = Vec::new();

    let total_files: u64 = groups.values().map(|fl| fl.len() as u64).sum();
    let pb = ProgressBar::new(total_files);
    pb.set_style(
        ProgressStyle::default_bar()
            .template("{spinner:.green} [{elapsed_precise}] [{wide_bar:.cyan/blue}] {pos}/{len} ({eta}) Validating files...")
            .expect("Failed to set progress bar style"),
    );

    for (base, file_list) in groups {
        // Prefer JPEG for representative, else any photo
        let photo_file_opt = file_list
            .iter()
            .find(|f| {
                if let Some(filename) = f.file_name().and_then(|n| n.to_str()) {
                    is_jpeg_file(filename)
                } else {
                    false
                }
            })
            .or_else(|| {
                file_list.iter().find(|f| {
                    if let Some(filename) = f.file_name().and_then(|n| n.to_str()) {
                        is_raw_file(filename) || is_jpeg_file(filename)
                    } else {
                        false
                    }
                })
            });

        let (photo_file, date) = if let Some(photo_file) = photo_file_opt {
            let date = if let Some((_, exif)) = exif_cache.get(base) {
                get_exif_date(exif)
            } else {
                None
            };

            let final_date = if let Some(valid_date) = date {
                valid_date
            } else {
                // Try to get file modification time as fallback
                match fs::metadata(photo_file) {
                    Ok(metadata) => match metadata.modified() {
                        Ok(mtime) => DateTime::<Utc>::from(mtime),
                        Err(_) => {
                            errors.push(ValidationError {
                                file: photo_file.display().to_string(),
                                reason: "Cannot get file modification time".to_string(),
                            });
                            pb.inc(file_list.len() as u64);
                            continue;
                        }
                    },
                    Err(_) => {
                        errors.push(ValidationError {
                            file: photo_file.display().to_string(),
                            reason: "Cannot read file metadata".to_string(),
                        });
                        pb.inc(file_list.len() as u64);
                        continue;
                    }
                }
            };
            (photo_file.clone(), final_date)
        } else {
            // No photo file, use mtime of first file
            let first_file = &file_list[0];
            match fs::metadata(first_file) {
                Ok(metadata) => match metadata.modified() {
                    Ok(mtime) => {
                        let date = DateTime::<Utc>::from(mtime);
                        (first_file.clone(), date)
                    }
                    Err(_) => {
                        errors.push(ValidationError {
                            file: first_file.display().to_string(),
                            reason: "Cannot get file modification time".to_string(),
                        });
                        pb.inc(file_list.len() as u64);
                        continue;
                    }
                },
                Err(_) => {
                    errors.push(ValidationError {
                        file: first_file.display().to_string(),
                        reason: "Cannot read file metadata".to_string(),
                    });
                    pb.inc(file_list.len() as u64);
                    continue;
                }
            }
        };

        // Skip this group if incremental mode is enabled and the date is not newer than cutoff
        if let Some(cutoff) = cutoff_date {
            if date <= cutoff {
                pb.inc(file_list.len() as u64);
                continue;
            }
        }

        let year = date.format("%Y").to_string();
        let month = date.format("%m").to_string();
        let day = date.format("%d").to_string();

        // Check if this base is part of a sequence
        let seq_folder = sequences.get(base).map(|seq_type| match seq_type {
            SequenceType::Burst(folder_name) => folder_name.clone(),
            SequenceType::Hdr(folder_name) => folder_name.clone(),
        });

        // Default target_base for the group
        let default_target_base =
            if let Some(filename) = photo_file.file_name().and_then(|n| n.to_str()) {
                if is_raw_file(filename) {
                    &raw_dir
                } else {
                    &jpeg_dir
                }
            } else {
                &jpeg_dir
            };

        for file_path in file_list {
            // Validate source file exists and is a regular file
            match fs::metadata(file_path) {
                Ok(metadata) => {
                    if !metadata.is_file() {
                        errors.push(ValidationError {
                            file: file_path.display().to_string(),
                            reason: "Source is not a regular file".to_string(),
                        });
                        pb.inc(1);
                        continue;
                    }
                }
                Err(_) => {
                    errors.push(ValidationError {
                        file: file_path.display().to_string(),
                        reason: "Source file does not exist or cannot be accessed".to_string(),
                    });
                    pb.inc(1);
                    continue;
                }
            }

            let filename = file_path.file_name().and_then(|n| n.to_str()).unwrap_or("");
            let target_base =
                determine_target_base(filename, &raw_dir, &jpeg_dir, default_target_base);
            let mut target_path = target_base.join(&year).join(&month).join(&day);
            if let Some(ref seq_folder_name) = seq_folder {
                target_path = target_path.join(seq_folder_name);
            }
            let dest = target_path.join(filename);

            // Check if destination already exists
            if dest.exists() {
                errors.push(ValidationError {
                    file: file_path.display().to_string(),
                    reason: format!("Destination already exists: {}", dest.display()),
                });
                pb.inc(1);
                continue;
            }

            copy_plan.push((file_path.clone(), dest));
            pb.inc(1);
        }
    }

    // Sort copy plan by source path for meaningful dry-run output order
    copy_plan.sort_by(|a, b| a.0.cmp(&b.0));

    pb.finish_with_message("File validation complete");

    if errors.is_empty() {
        Ok(copy_plan)
    } else {
        Err(errors)
    }
}

fn copy_files(
    copy_plan: Vec<(PathBuf, PathBuf)>,
    dry_run: bool,
) -> Result<(), Box<dyn std::error::Error>> {
    let pb = ProgressBar::new(copy_plan.len() as u64);
    pb.set_style(
        ProgressStyle::default_bar()
            .template("{spinner:.green} [{elapsed_precise}] [{wide_bar:.cyan/blue}] {pos}/{len} ({eta}) Copying files...")
            .expect("Failed to set progress bar style"),
    );

    for (source, dest) in copy_plan {
        if dry_run {
            println!("Would copy {} to {}", source.display(), dest.display());
        } else {
            if let Some(parent) = dest.parent() {
                fs::create_dir_all(parent)?;
            }
            fs::copy(&source, &dest)?;
        }
        pb.inc(1);
    }

    pb.finish_with_message("File copying complete");
    Ok(())
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let args = Args::parse();
    
    // Check if exiftool is available before proceeding
    check_exiftool_installed()?;
    
    let input_dir = PathBuf::from(&args.input_dir);
    let output_dir = PathBuf::from(&args.output_dir);

    // Handle incremental mode
    let cutoff_date = if args.incremental {
        println!(
            "Incremental mode enabled. Scanning destination directory for most recent file..."
        );
        match find_most_recent_file_in_destination(&output_dir)? {
            Some(date) => {
                println!(
                    "Only processing files newer than: {}",
                    date.format("%Y-%m-%d %H:%M:%S UTC")
                );
                Some(date)
            }
            None => {
                println!("No existing files found in destination. Processing all files.");
                None
            }
        }
    } else {
        None
    };

    let groups = group_files_by_base(&input_dir);
    let exif_cache = cache_exif_data(&groups);
    let sequences = detect_sequences(&groups, &exif_cache);

    match validate_and_plan_copy(&output_dir, &groups, &sequences, &exif_cache, cutoff_date) {
        Ok(copy_plan) => {
            if cutoff_date.is_some() {
                println!(
                    "Incremental validation successful! {} new files ready to copy.",
                    copy_plan.len()
                );
            } else {
                println!(
                    "Validation successful! {} files ready to copy.",
                    copy_plan.len()
                );
            }

            if copy_plan.is_empty() {
                println!("No files to process.");
            } else {
                copy_files(copy_plan, args.dry_run)?;
            }
        }
        Err(errors) => {
            println!(
                "Validation failed! Found {} problematic files:",
                errors.len()
            );
            for error in &errors {
                println!("  {} - {}", error.file, error.reason);
            }
            println!("\nPlease fix these issues before proceeding.");
            std::process::exit(1);
        }
    }

    Ok(())
}

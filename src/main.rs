use chrono::{DateTime, Datelike, NaiveDateTime, Utc};
use clap::Parser;
use indicatif::{ProgressBar, ProgressStyle};
use photo_sorter::classify::{
    classify_name, dest_tree, group_stem, is_jpeg_file, is_raw_file, FileRole, MediaKind,
};
use photo_sorter::plan::{decide, orig_sidecar_path, PlanDecision, PlanInput};
use photo_sorter::sidecar::{self, IncrementalState};
use rayon::prelude::*;
use regex::Regex;
use serde_json::Value;
use std::collections::HashMap;
use std::fs::{self, OpenOptions};
use std::io;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicUsize, Ordering};

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
    /// Only process files newer than the dest sidecar / newest dest photo
    #[arg(long)]
    incremental: bool,
    /// Overwrite files that already exist in the destination directory
    #[arg(long = "override")]
    force_override: bool,
    /// Skip files that already exist in the destination directory (instead of erroring)
    #[arg(long = "skip-existing")]
    skip_existing: bool,
}

#[derive(Debug)]
struct ValidationError {
    file: String,
    reason: String,
}

#[derive(Debug, Clone)]
enum SequenceType {
    Burst(String),
    Hdr(String),
}

struct PlanResult {
    copy_plan: Vec<(PathBuf, PathBuf)>,
    move_plan: Vec<(PathBuf, PathBuf)>,
    skipped_cutoff: usize,
    skipped_existing: usize,
    recopied: usize,
    newest_copied: Option<(DateTime<Utc>, String)>,
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
        Err(_) => Err("exiftool is not installed or not found in PATH.

Please install exiftool to use this program:
- On Ubuntu/Debian: sudo apt install libimage-exiftool-perl
- On macOS: brew install exiftool
- On other systems: https://exiftool.org/install.html"
            .into()),
    }
}

fn get_exif_data(file_path: &Path) -> Result<Value, Box<dyn std::error::Error>> {
    let output = Command::new("exiftool")
        .args([
            "-j",
            "-fast2",
            "-DateTimeOriginal",
            "-CreateDate",
            "-SpecialMode",
            "-DriveMode",
        ])
        .arg(file_path)
        .output()?;
    if output.stdout.is_empty() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(format!(
            "exiftool produced no JSON for {}: {}",
            file_path.display(),
            stderr.trim()
        )
        .into());
    }
    let json: Vec<Value> = serde_json::from_slice(&output.stdout)?;
    Ok(json.into_iter().next().unwrap_or(Value::Null))
}

fn parse_exif_datetime(date_str: &str) -> Option<DateTime<Utc>> {
    let truncated = if date_str.len() >= 19 {
        &date_str[..19]
    } else {
        date_str
    };
    let dt = NaiveDateTime::parse_from_str(truncated, "%Y:%m:%d %H:%M:%S").ok()?;
    let date = DateTime::<Utc>::from_naive_utc_and_offset(dt, Utc);
    if date.year() >= 1990 && date.year() <= 2050 {
        Some(date)
    } else {
        None
    }
}

fn get_exif_date(exif: &Value) -> Option<DateTime<Utc>> {
    for key in ["DateTimeOriginal", "CreateDate"] {
        if let Some(date_str) = exif.get(key).and_then(|v| v.as_str()) {
            if let Some(date) = parse_exif_datetime(date_str) {
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
        let is_bracket = drive_mode.contains("HDR")
            || drive_mode.contains("AE Auto Bracketing")
            || drive_mode.contains("AE Bracketing")
            || drive_mode.contains("Exposure Bracketing");
        if is_bracket {
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

fn numbered_dirs(dir: &Path) -> io::Result<Vec<(u32, PathBuf)>> {
    let mut dirs = Vec::new();
    for entry in fs::read_dir(dir)? {
        let entry = entry?;
        let path = entry.path();
        if !path.is_dir() {
            continue;
        }
        if let Some(name) = path.file_name().and_then(|n| n.to_str()) {
            if let Ok(n) = name.parse::<u32>() {
                dirs.push((n, path));
            }
        }
    }
    dirs.sort_by(|a, b| b.0.cmp(&a.0));
    Ok(dirs)
}

fn day_has_photos(dir: &Path) -> bool {
    fn walk(dir: &Path) -> bool {
        let Ok(entries) = fs::read_dir(dir) else {
            return false;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() {
                if walk(&path) {
                    return true;
                }
            } else if let Some(name) = path.file_name().and_then(|n| n.to_str()) {
                if matches!(classify_name(name), FileRole::Media(_)) {
                    return true;
                }
            }
        }
        false
    }
    walk(dir)
}

fn newest_day_dir(base_dir: &Path) -> io::Result<Option<PathBuf>> {
    if !base_dir.exists() {
        return Ok(None);
    }
    for (_year, year_dir) in numbered_dirs(base_dir)? {
        for (_month, month_dir) in numbered_dirs(&year_dir)? {
            for (_day, day_dir) in numbered_dirs(&month_dir)? {
                if day_has_photos(&day_dir) {
                    return Ok(Some(day_dir));
                }
            }
        }
    }
    Ok(None)
}

fn max_date_in_dir(
    dir: &Path,
) -> Result<Option<(DateTime<Utc>, PathBuf)>, Box<dyn std::error::Error>> {
    let output = Command::new("exiftool")
        .args([
            "-j",
            "-r",
            "-fast2",
            "-DateTimeOriginal",
            "-CreateDate",
            "-ext",
            "orf",
            "-ext",
            "ori",
            "-ext",
            "jpg",
            "-ext",
            "jpeg",
            "-ext",
            "dng",
            "-ext",
            "cr2",
            "-ext",
            "nef",
            "-ext",
            "arw",
            "-ext",
            "raw",
            "-ext",
            "mov",
            "-ext",
            "mp4",
            "-ext",
            "tif",
            "-ext",
            "tiff",
            "-ext",
            "png",
        ])
        .arg(dir)
        .output()?;
    if output.stdout.is_empty() {
        return Ok(None);
    }
    let json: Vec<Value> = serde_json::from_slice(&output.stdout)?;
    let mut best: Option<(DateTime<Utc>, PathBuf)> = None;
    for item in json {
        let Some(path) = item
            .get("SourceFile")
            .and_then(|v| v.as_str())
            .map(PathBuf::from)
        else {
            continue;
        };
        let Some(date) = get_exif_date(&item) else {
            continue;
        };
        if best.as_ref().map_or(true, |(d, _)| date > *d) {
            best = Some((date, path));
        }
    }
    Ok(best)
}

fn find_most_recent_file_in_destination(
    output_dir: &Path,
) -> Result<Option<(DateTime<Utc>, PathBuf)>, Box<dyn std::error::Error>> {
    let mut best: Option<(DateTime<Utc>, PathBuf)> = None;
    for tree in ["RAW", "JPEG", "VIDEO"] {
        let Some(day_dir) = newest_day_dir(&output_dir.join(tree))? else {
            continue;
        };
        println!("Scanning newest {} day: {}", tree, day_dir.display());
        if let Some((date, path)) = max_date_in_dir(&day_dir)? {
            if best.as_ref().map_or(true, |(d, _)| date > *d) {
                best = Some((date, path));
            }
        }
    }
    if let Some((date, path)) = &best {
        println!(
            "Most recent dest file: {} ({})",
            date.format("%Y-%m-%d %H:%M:%S"),
            path.display()
        );
    } else {
        println!("No photo files found in destination directory.");
    }
    Ok(best)
}

fn relative_to_output(output_dir: &Path, path: &Path) -> String {
    path.strip_prefix(output_dir)
        .map(|p| p.display().to_string())
        .unwrap_or_else(|_| path.display().to_string())
}

fn input_root(input_dir: &Path) -> PathBuf {
    let dcim = input_dir.join("DCIM");
    if dcim.is_dir() {
        dcim
    } else {
        input_dir.to_path_buf()
    }
}

fn collect_all_files_recursive(directory: &Path) -> Vec<PathBuf> {
    let mut all_files = Vec::new();

    fn collect_recursive(dir: &Path, files: &mut Vec<PathBuf>) {
        let Ok(entries) = fs::read_dir(dir) else {
            return;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_file() {
                if let Some(name) = path.file_name().and_then(|n| n.to_str()) {
                    if !matches!(classify_name(name), FileRole::Ignore) {
                        files.push(path);
                    }
                }
            } else if path.is_dir() {
                if let Some(name) = path.file_name().and_then(|n| n.to_str()) {
                    if name.starts_with('.') {
                        continue;
                    }
                }
                collect_recursive(&path, files);
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
            if let Some(base) = group_stem(filename) {
                groups.entry(base.to_string()).or_default().push(file_path);
            }
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
                f.file_name()
                    .and_then(|n| n.to_str())
                    .is_some_and(|name| matches!(classify_name(name), FileRole::Media(_)))
            })
            .cloned()
            .collect();

        if !photo_files.is_empty() {
            let rep_file = photo_files
                .iter()
                .find(|f| {
                    f.file_name()
                        .and_then(|n| n.to_str())
                        .is_some_and(is_jpeg_file)
                })
                .or_else(|| {
                    photo_files
                        .iter()
                        .find(|f| {
                            f.file_name()
                                .and_then(|n| n.to_str())
                                .is_some_and(is_raw_file)
                        })
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

    let failed = AtomicUsize::new(0);
    let exif_cache: HashMap<String, (PathBuf, Value)> = representative_files
        .par_iter()
        .filter_map(|(base, rep_file)| match get_exif_data(rep_file) {
            Ok(data) => {
                pb.inc(1);
                Some((base.clone(), (rep_file.clone(), data)))
            }
            Err(_) => {
                failed.fetch_add(1, Ordering::Relaxed);
                pb.inc(1);
                None
            }
        })
        .collect();

    pb.finish_with_message("EXIF caching complete");
    let failed = failed.load(Ordering::Relaxed);
    if failed > 0 {
        eprintln!("Warning: exiftool failed for {failed} representative file(s).");
    }
    exif_cache
}

fn file_mtime_utc(path: &Path) -> Option<DateTime<Utc>> {
    fs::metadata(path)
        .ok()
        .and_then(|m| m.modified().ok())
        .map(DateTime::<Utc>::from)
}

fn detect_sequences(
    files: &HashMap<String, Vec<PathBuf>>,
    exif_cache: &HashMap<String, (PathBuf, Value)>,
) -> HashMap<String, SequenceType> {
    let burst_re = Regex::new(r"Sequence:\s*(\d+)").expect("Invalid regex for burst sequence");
    let hdr_re = Regex::new(r"Shot\s+(\d+)").expect("Invalid regex for HDR sequence");

    let mut photo_info: Vec<(String, u32, Option<u32>, DateTime<Utc>)> = Vec::new();

    for (base, file_list) in files {
        let has_photo = file_list.iter().any(|f| {
            f.file_name()
                .and_then(|n| n.to_str())
                .is_some_and(|name| matches!(classify_name(name), FileRole::Media(_)))
        });
        if !has_photo {
            continue;
        }
        let Some((rep_file, exif)) = exif_cache.get(base) else {
            continue;
        };
        let burst_seq_num = get_sequence_info(exif, &burst_re);
        let hdr_shot_num = get_hdr_info(exif, &hdr_re);
        let date = get_exif_date(exif).or_else(|| file_mtime_utc(rep_file));
        let Some(date) = date else {
            continue;
        };
        photo_info.push((base.clone(), burst_seq_num, hdr_shot_num, date));
    }

    photo_info.sort_by(|a, b| a.3.cmp(&b.3).then_with(|| a.0.cmp(&b.0)));

    let pb = ProgressBar::new(photo_info.len() as u64);
    pb.set_style(
        ProgressStyle::default_bar()
            .template("{spinner:.green} [{elapsed_precise}] [{wide_bar:.cyan/blue}] {pos}/{len} ({eta}) Detecting sequences...")
            .expect("Failed to set progress bar style"),
    );

    let mut sequences: HashMap<String, SequenceType> = HashMap::new();

    let mut current_hdr_sequence: Vec<String> = Vec::new();
    let mut expected_hdr_shot = 1u32;
    let mut hdr_sequence_name = String::new();

    for (base, _burst_seq_num, hdr_shot_num, _date) in &photo_info {
        pb.inc(1);

        if let Some(shot_num) = hdr_shot_num {
            if *shot_num == 1 {
                if current_hdr_sequence.len() > 1 {
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
                current_hdr_sequence.push(base.clone());
                expected_hdr_shot += 1;
            } else {
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
        } else {
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

    if current_hdr_sequence.len() > 1 {
        for hdr_base in &current_hdr_sequence {
            sequences.insert(
                hdr_base.clone(),
                SequenceType::Hdr(hdr_sequence_name.clone()),
            );
        }
    }

    let mut current_burst_sequence: Vec<String> = Vec::new();
    let mut expected_burst_num = 0u32;
    let mut burst_sequence_name = String::new();

    for (base, burst_seq_num, _hdr_shot_num, _date) in &photo_info {
        if sequences.contains_key(base) {
            continue;
        }

        if *burst_seq_num == 0 {
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
            current_burst_sequence.push(base.clone());
            expected_burst_num += 1;
        } else {
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
        }
    }

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

    let hdr_count = sequences
        .values()
        .filter(|s| matches!(s, SequenceType::Hdr(_)))
        .map(|s| match s {
            SequenceType::Hdr(n) => n.clone(),
            _ => String::new(),
        })
        .collect::<std::collections::HashSet<_>>()
        .len();
    let burst_count = sequences
        .values()
        .filter(|s| matches!(s, SequenceType::Burst(_)))
        .map(|s| match s {
            SequenceType::Burst(n) => n.clone(),
            _ => String::new(),
        })
        .collect::<std::collections::HashSet<_>>()
        .len();

    if hdr_count > 0 {
        println!("Detected {hdr_count} HDR sequences.");
    } else {
        println!("No HDR sequences detected.");
    }
    if burst_count > 0 {
        println!("Detected {burst_count} BURST sequences.");
    } else {
        println!("No BURST sequences detected.");
    }

    sequences
}

fn determine_target_base(
    filename: &str,
    raw_dir: &Path,
    jpeg_dir: &Path,
    video_dir: &Path,
    default_base: &Path,
) -> PathBuf {
    match classify_name(filename) {
        FileRole::Media(kind) | FileRole::Sidecar(kind) => match kind {
            MediaKind::Raw => raw_dir.to_path_buf(),
            MediaKind::Jpeg => jpeg_dir.to_path_buf(),
            MediaKind::Video => video_dir.to_path_buf(),
        },
        FileRole::SidecarUnknown | FileRole::Ignore => default_base.to_path_buf(),
    }
}

fn default_base_for_file<'a>(
    filename: &str,
    raw_dir: &'a Path,
    jpeg_dir: &'a Path,
    video_dir: &'a Path,
) -> &'a Path {
    match classify_name(filename) {
        FileRole::Media(MediaKind::Raw) | FileRole::Sidecar(MediaKind::Raw) => raw_dir,
        FileRole::Media(MediaKind::Video) | FileRole::Sidecar(MediaKind::Video) => video_dir,
        _ => jpeg_dir,
    }
}

fn note_newest(
    newest: &mut Option<(DateTime<Utc>, String)>,
    date: DateTime<Utc>,
    dest: &Path,
    output_dir: &Path,
) {
    if newest.as_ref().map_or(true, |(d, _)| date > *d) {
        *newest = Some((date, relative_to_output(output_dir, dest)));
    }
}

fn validate_and_plan_copy(
    output_dir: &Path,
    groups: &HashMap<String, Vec<PathBuf>>,
    sequences: &HashMap<String, SequenceType>,
    exif_cache: &HashMap<String, (PathBuf, Value)>,
    cutoff_date: Option<DateTime<Utc>>,
    force_override: bool,
    skip_existing: bool,
    incremental: bool,
) -> Result<PlanResult, Vec<ValidationError>> {
    let raw_dir = output_dir.join(dest_tree(MediaKind::Raw));
    let jpeg_dir = output_dir.join(dest_tree(MediaKind::Jpeg));
    let video_dir = output_dir.join(dest_tree(MediaKind::Video));
    let mut errors = Vec::new();
    let mut copy_plan = Vec::new();
    let mut move_plan = Vec::new();
    let mut skipped_cutoff: usize = 0;
    let mut skipped_existing: usize = 0;
    let mut recopied: usize = 0;
    let mut newest_copied: Option<(DateTime<Utc>, String)> = None;

    let total_files: u64 = groups.values().map(|fl| fl.len() as u64).sum();
    let pb = ProgressBar::new(total_files);
    pb.set_style(
        ProgressStyle::default_bar()
            .template("{spinner:.green} [{elapsed_precise}] [{wide_bar:.cyan/blue}] {pos}/{len} ({eta}) Validating files...")
            .expect("Failed to set progress bar style"),
    );

    for (base, file_list) in groups {
        let photo_file_opt = file_list
            .iter()
            .find(|f| {
                f.file_name()
                    .and_then(|n| n.to_str())
                    .is_some_and(is_jpeg_file)
            })
            .or_else(|| {
                file_list.iter().find(|f| {
                    f.file_name()
                        .and_then(|n| n.to_str())
                        .is_some_and(|name| matches!(classify_name(name), FileRole::Media(_)))
                })
            });

        let (photo_file, date) = if let Some(photo_file) = photo_file_opt {
            let date = exif_cache
                .get(base)
                .and_then(|(_, exif)| get_exif_date(exif))
                .or_else(|| file_mtime_utc(photo_file));
            let Some(date) = date else {
                errors.push(ValidationError {
                    file: photo_file.display().to_string(),
                    reason: "Cannot get EXIF date or modification time".to_string(),
                });
                pb.inc(file_list.len() as u64);
                continue;
            };
            (photo_file.clone(), date)
        } else {
            let first_file = &file_list[0];
            let Some(date) = file_mtime_utc(first_file) else {
                errors.push(ValidationError {
                    file: first_file.display().to_string(),
                    reason: "Cannot get file modification time".to_string(),
                });
                pb.inc(file_list.len() as u64);
                continue;
            };
            (first_file.clone(), date)
        };

        let year = date.format("%Y").to_string();
        let month = date.format("%m").to_string();
        let day = date.format("%d").to_string();

        let seq_folder = sequences.get(base).map(|seq_type| match seq_type {
            SequenceType::Burst(folder_name) => folder_name.clone(),
            SequenceType::Hdr(folder_name) => folder_name.clone(),
        });

        let default_target_base = photo_file
            .file_name()
            .and_then(|n| n.to_str())
            .map(|name| default_base_for_file(name, &raw_dir, &jpeg_dir, &video_dir))
            .unwrap_or(jpeg_dir.as_path());

        for file_path in file_list {
            let metadata = match fs::metadata(file_path) {
                Ok(metadata) if metadata.is_file() => metadata,
                Ok(_) => {
                    errors.push(ValidationError {
                        file: file_path.display().to_string(),
                        reason: "Source is not a regular file".to_string(),
                    });
                    pb.inc(1);
                    continue;
                }
                Err(_) => {
                    errors.push(ValidationError {
                        file: file_path.display().to_string(),
                        reason: "Source file does not exist or cannot be accessed".to_string(),
                    });
                    pb.inc(1);
                    continue;
                }
            };
            let source_len = metadata.len();

            let filename = file_path.file_name().and_then(|n| n.to_str()).unwrap_or("");
            let target_base =
                determine_target_base(filename, &raw_dir, &jpeg_dir, &video_dir, default_target_base);
            let day_dir = target_base.join(&year).join(&month).join(&day);
            let flat_dest = day_dir.join(filename);
            let dest = if let Some(ref seq_folder_name) = seq_folder {
                day_dir.join(seq_folder_name).join(filename)
            } else {
                flat_dest.clone()
            };

            let dest_meta = fs::metadata(&dest).ok();
            let dest_exists = dest_meta.as_ref().is_some_and(|m| m.is_file());
            let dest_len = dest_meta.map(|m| m.len());
            let has_orig = fs::metadata(orig_sidecar_path(&dest))
                .ok()
                .is_some_and(|m| m.is_file());
            let flat_exists = seq_folder.is_some()
                && fs::metadata(&flat_dest).ok().is_some_and(|m| m.is_file());
            let seq_dest_exists = seq_folder.is_some() && dest_exists;

            let decision = decide(PlanInput {
                dest_exists,
                dest_len,
                source_len,
                date,
                cutoff: cutoff_date,
                in_sequence: seq_folder.is_some(),
                flat_exists,
                seq_dest_exists,
                force_override,
                skip_existing,
                incremental,
                has_orig,
            });

            match decision {
                PlanDecision::Copy => {
                    copy_plan.push((file_path.clone(), dest.clone()));
                    note_newest(&mut newest_copied, date, &dest, output_dir);
                }
                PlanDecision::RecopyIncomplete => {
                    copy_plan.push((file_path.clone(), dest.clone()));
                    recopied += 1;
                    note_newest(&mut newest_copied, date, &dest, output_dir);
                }
                PlanDecision::MoveFromFlat => {
                    move_plan.push((flat_dest, dest));
                }
                PlanDecision::SkipOlderThanCutoff => {
                    skipped_cutoff += 1;
                }
                PlanDecision::SkipExisting => {
                    skipped_existing += 1;
                }
                PlanDecision::ConflictExists => {
                    errors.push(ValidationError {
                        file: file_path.display().to_string(),
                        reason: format!("Destination already exists: {}", dest.display()),
                    });
                }
                PlanDecision::ConflictFlatAndSequence => {
                    errors.push(ValidationError {
                        file: file_path.display().to_string(),
                        reason: format!(
                            "File exists at both flat and sequence paths: {} and {}",
                            flat_dest.display(),
                            dest.display()
                        ),
                    });
                }
            }
            pb.inc(1);
        }
    }

    copy_plan.sort_by(|a, b| a.0.cmp(&b.0));
    move_plan.sort_by(|a, b| a.0.cmp(&b.0));

    pb.finish_with_message("File validation complete");

    if errors.is_empty() {
        Ok(PlanResult {
            copy_plan,
            move_plan,
            skipped_cutoff,
            skipped_existing,
            recopied,
            newest_copied,
        })
    } else {
        Err(errors)
    }
}

fn set_dest_mtime(dest: &Path, source: &Path) -> io::Result<()> {
    let metadata = fs::metadata(source)?;
    let mtime = metadata.modified()?;
    OpenOptions::new().write(true).open(dest)?.set_modified(mtime)
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
            let tmp_name = format!(
                "{}.part",
                dest.file_name().and_then(|n| n.to_str()).unwrap_or("file")
            );
            let tmp = dest.with_file_name(tmp_name);
            if let Err(e) = (|| -> Result<(), Box<dyn std::error::Error>> {
                fs::copy(&source, &tmp)?;
                let src_len = fs::metadata(&source)?.len();
                let tmp_len = fs::metadata(&tmp)?.len();
                if src_len != tmp_len {
                    return Err(format!(
                        "size mismatch after copy of {} (src {src_len}, dest {tmp_len})",
                        source.display()
                    )
                    .into());
                }
                if let Err(e) = set_dest_mtime(&tmp, &source) {
                    eprintln!(
                        "Warning: could not preserve mtime on {}: {e}",
                        tmp.display()
                    );
                }
                fs::rename(&tmp, &dest)?;
                Ok(())
            })() {
                let _ = fs::remove_file(&tmp);
                return Err(e);
            }
        }
        pb.inc(1);
    }

    pb.finish_with_message("File copying complete");
    Ok(())
}

fn move_files(
    move_plan: Vec<(PathBuf, PathBuf)>,
    dry_run: bool,
) -> Result<(), Box<dyn std::error::Error>> {
    let pb = ProgressBar::new(move_plan.len() as u64);
    pb.set_style(
        ProgressStyle::default_bar()
            .template("{spinner:.green} [{elapsed_precise}] [{wide_bar:.cyan/blue}] {pos}/{len} ({eta}) Moving misplaced files...")
            .expect("Failed to set progress bar style"),
    );

    for (from, to) in move_plan {
        if dry_run {
            println!("Would move {} -> {}", from.display(), to.display());
        } else {
            if let Some(parent) = to.parent() {
                fs::create_dir_all(parent)?;
            }
            if let Err(e) = fs::rename(&from, &to) {
                fs::copy(&from, &to)?;
                fs::remove_file(&from)?;
                let _ = e;
            }
        }
        pb.inc(1);
    }

    pb.finish_with_message("File moves complete");
    Ok(())
}

fn resolve_cutoff(
    output_dir: &Path,
    incremental: bool,
    dry_run: bool,
) -> Result<Option<DateTime<Utc>>, Box<dyn std::error::Error>> {
    if !incremental {
        return Ok(None);
    }
    println!("Incremental mode enabled.");
    if let Some(state) = sidecar::load(output_dir)? {
        if let Some(date) = state.cutoff_dt() {
            println!(
                "Loaded sidecar cutoff {} ({})",
                date.format("%Y-%m-%d %H:%M:%S"),
                state.last_file
            );
            return Ok(Some(date));
        }
        eprintln!(
            "Warning: ignoring unreadable sidecar cutoff {:?}",
            state.cutoff
        );
    }
    println!("No sidecar found. Scanning destination for most recent file...");
    match find_most_recent_file_in_destination(output_dir)? {
        Some((date, path)) => {
            let last_file = relative_to_output(output_dir, &path);
            if !dry_run {
                sidecar::save(output_dir, &IncrementalState::new(date, &last_file))?;
                println!("Wrote {}", sidecar::sidecar_path(output_dir).display());
            }
            println!(
                "Only processing files on or after: {}",
                date.format("%Y-%m-%d %H:%M:%S")
            );
            Ok(Some(date))
        }
        None => {
            println!("No existing files found in destination. Processing all files.");
            Ok(None)
        }
    }
}

fn maybe_update_sidecar(
    output_dir: &Path,
    dry_run: bool,
    previous: Option<DateTime<Utc>>,
    newest_copied: Option<(DateTime<Utc>, String)>,
) -> Result<(), Box<dyn std::error::Error>> {
    if dry_run {
        return Ok(());
    }
    let Some((date, last_file)) = newest_copied else {
        return Ok(());
    };
    if previous.is_some_and(|p| date <= p) {
        return Ok(());
    }
    sidecar::save(output_dir, &IncrementalState::new(date, last_file))?;
    println!("Updated {}", sidecar::sidecar_path(output_dir).display());
    Ok(())
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let args = Args::parse();

    check_exiftool_installed()?;

    let input_dir = PathBuf::from(&args.input_dir);
    let output_dir = PathBuf::from(&args.output_dir);

    if !input_dir.is_dir() {
        return Err(format!("Input directory does not exist: {}", input_dir.display()).into());
    }
    if !output_dir.is_dir() {
        return Err(format!("Output directory does not exist: {}", output_dir.display()).into());
    }

    let _ = rayon::ThreadPoolBuilder::new().num_threads(4).build_global();

    let cutoff_date = resolve_cutoff(&output_dir, args.incremental, args.dry_run)?;

    let source_root = input_root(&input_dir);
    if source_root != input_dir {
        println!("Scanning DCIM: {}", source_root.display());
    }

    let groups = group_files_by_base(&source_root);
    let exif_cache = cache_exif_data(&groups);
    let sequences = detect_sequences(&groups, &exif_cache);

    match validate_and_plan_copy(
        &output_dir,
        &groups,
        &sequences,
        &exif_cache,
        cutoff_date,
        args.force_override,
        args.skip_existing,
        args.incremental,
    ) {
        Ok(plan) => {
            if plan.skipped_cutoff > 0 {
                println!(
                    "Skipped {} file(s) older than cutoff (already in destination).",
                    plan.skipped_cutoff
                );
            }
            if plan.skipped_existing > 0 {
                println!(
                    "Skipped {} file(s) that already exist in destination.",
                    plan.skipped_existing
                );
            }
            if plan.recopied > 0 {
                println!(
                    "Re-copying {} incomplete dest file(s).",
                    plan.recopied
                );
            }

            if !plan.move_plan.is_empty() {
                println!(
                    "\n{} file(s) found at flat path - will move to sequence folder:",
                    plan.move_plan.len()
                );
                move_files(plan.move_plan, args.dry_run)?;
            }

            if args.incremental {
                println!(
                    "\nIncremental check: {} new file(s) to copy.",
                    plan.copy_plan.len()
                );
            } else {
                println!("\n{} new file(s) to copy.", plan.copy_plan.len());
            }

            if plan.copy_plan.is_empty() {
                println!("No files to copy.");
            } else {
                copy_files(plan.copy_plan, args.dry_run)?;
            }

            maybe_update_sidecar(&output_dir, args.dry_run, cutoff_date, plan.newest_copied)?;
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

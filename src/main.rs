use clap::Parser;
use indicatif::{ProgressBar, ProgressStyle};
use rayon::prelude::*;
use regex::Regex;
use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use chrono::{DateTime, NaiveDateTime, Utc, Datelike};
use serde_json::Value;

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
}

#[derive(Debug)]
struct ValidationError {
    file: String,
    reason: String,
}

fn get_exif_data(file_path: &Path) -> Result<Value, Box<dyn std::error::Error>> {
    let output = Command::new("exiftool")
        .arg("-j")
        .arg(file_path)
        .output()?;
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
    if let Some(special_mode) = exif.get("SpecialMode").and_then(|v| v.as_str())
        && let Some(captures) = re.captures(special_mode)
            && let Some(seq_str) = captures.get(1)
                && let Ok(seq) = seq_str.as_str().parse::<u32>() {
                    return seq;
                }
    0
}

fn is_raw_file(filename: &str) -> bool {
    let ext = filename.to_lowercase();
    ext.ends_with(".cr2") || ext.ends_with(".nef") || ext.ends_with(".arw") || ext.ends_with(".dng") || ext.ends_with(".raw") || ext.ends_with(".orf")
}

fn is_jpeg_file(filename: &str) -> bool {
    let ext = filename.to_lowercase();
    ext.ends_with(".jpg") || ext.ends_with(".jpeg")
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
        let photo_files: Vec<PathBuf> = file_list.iter()
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
            let rep_file = photo_files.iter()
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
        .filter_map(|(base, rep_file)| {
            match get_exif_data(rep_file) {
                Ok(data) => {
                    pb.inc(1);
                    Some((base.clone(), (rep_file.clone(), data)))
                }
                Err(_) => {
                    pb.inc(1);
                    None
                }
            }
        })
        .collect();
    
    pb.finish_with_message("EXIF caching complete");
    exif_cache
}

fn detect_sequences(
    files: &HashMap<String, Vec<PathBuf>>,
    exif_cache: &HashMap<String, (PathBuf, Value)>,
) -> Vec<(String, Vec<String>)> {
    let re = Regex::new(r"Sequence:\s*(\d+)").expect("Invalid regex for sequence");
    
    // Collect all photo bases with their sequence numbers and dates
    let mut photo_info: Vec<(String, u32, DateTime<Utc>)> = Vec::new();
    
    for (base, file_list) in files {
        let photo_files: Vec<PathBuf> = file_list.iter()
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
            if let Some((_, exif)) = exif_cache.get(base) {
                let seq_num = get_sequence_info(exif, &re);
                let date = get_exif_date(exif).unwrap_or_else(|| {
                    // Fallback to modification time if no EXIF date
                    Utc::now()
                });
                photo_info.push((base.clone(), seq_num, date));
            }
        }
    }
    
    // Sort by date first, then by name to establish order
    photo_info.sort_by(|a, b| a.2.cmp(&b.2).then_with(|| a.0.cmp(&b.0)));
    
    let pb = ProgressBar::new(photo_info.len() as u64);
    pb.set_style(
        ProgressStyle::default_bar()
            .template("{spinner:.green} [{elapsed_precise}] [{wide_bar:.cyan/blue}] {pos}/{len} ({eta}) Detecting sequences...")
            .expect("Failed to set progress bar style"),
    );

    let mut sequences: Vec<(String, Vec<String>)> = Vec::new();
    let mut current_sequence: Vec<String> = Vec::new();
    let mut expected_seq_num = 0u32;
    let mut sequence_name = String::new();
    
    for (base, seq_num, _date) in photo_info {
        pb.inc(1);
        
        if seq_num == 0 {
            // Not part of a sequence - finish current sequence if any
            if current_sequence.len() > 1 {
                sequences.push((sequence_name.clone(), current_sequence.clone()));
            }
            current_sequence.clear();
            expected_seq_num = 0;
        } else if seq_num == 1 {
            // Start of a new sequence
            if current_sequence.len() > 1 {
                sequences.push((sequence_name.clone(), current_sequence.clone()));
            }
            sequence_name = format!("seq_{}", base);
            current_sequence = vec![base];
            expected_seq_num = 2;
        } else if seq_num == expected_seq_num && !current_sequence.is_empty() {
            // Continue current sequence
            current_sequence.push(base);
            expected_seq_num += 1;
        } else {
            // Sequence broken - finish current sequence if it has multiple photos
            if current_sequence.len() > 1 {
                sequences.push((sequence_name.clone(), current_sequence.clone()));
            }
            
            // Check if this starts a new sequence
            if seq_num == 1 {
                sequence_name = format!("seq_{}", base);
                current_sequence = vec![base];
                expected_seq_num = 2;
            } else {
                current_sequence.clear();
                expected_seq_num = 0;
            }
        }
    }
    
    // Don't forget the last sequence
    if current_sequence.len() > 1 {
        sequences.push((sequence_name, current_sequence));
    }
    
    pb.finish_with_message(format!("Sequence detection complete. Found {} sequences.", sequences.len()));
    sequences
}

fn determine_target_base(filename: &str, raw_dir: &Path, jpeg_dir: &Path, default_base: &Path) -> PathBuf {
    if is_raw_file(filename) {
        raw_dir.to_path_buf()
    } else if is_jpeg_file(filename) {
        jpeg_dir.to_path_buf()
    } else {
        // For associated files, parse the name
        let parts: Vec<&str> = filename.split('.').collect();
        if parts.len() >= 3 {
            let format = parts[parts.len() - 2].to_uppercase();
            if format == "ORF" || format == "CR2" || format == "NEF" || format == "ARW" || format == "DNG" || format == "RAW" {
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
    sequences: &[(String, Vec<String>)],
    exif_cache: &HashMap<String, (PathBuf, Value)>,
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
        let photo_file_opt = file_list.iter()
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
                    Ok(metadata) => {
                        match metadata.modified() {
                            Ok(mtime) => DateTime::<Utc>::from(mtime),
                            Err(_) => {
                                errors.push(ValidationError {
                                    file: photo_file.display().to_string(),
                                    reason: "Cannot get file modification time".to_string(),
                                });
                                pb.inc(file_list.len() as u64);
                                continue;
                            }
                        }
                    }
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
                Ok(metadata) => {
                    match metadata.modified() {
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
                    }
                }
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

        let year = date.format("%Y").to_string();
        let month = date.format("%m").to_string();
        let day = date.format("%d").to_string();

        let seq_dir = sequences.iter().find(|(seq_base, _)| seq_base == base).map(|(_, _)| base.clone());

        // Default target_base for the group
        let default_target_base = if let Some(filename) = photo_file.file_name().and_then(|n| n.to_str()) {
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
            let target_base = determine_target_base(filename, &raw_dir, &jpeg_dir, default_target_base);
            let mut target_path = target_base.join(&year).join(&month).join(&day);
            if let Some(ref seq) = seq_dir {
                target_path = target_path.join(seq);
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
    let input_dir = PathBuf::from(&args.input_dir);
    let output_dir = PathBuf::from(&args.output_dir);

    let groups = group_files_by_base(&input_dir);
    let exif_cache = cache_exif_data(&groups);
    let sequences = detect_sequences(&groups, &exif_cache);
    
    match validate_and_plan_copy(&output_dir, &groups, &sequences, &exif_cache) {
        Ok(copy_plan) => {
            println!("Validation successful! {} files ready to copy.", copy_plan.len());
            copy_files(copy_plan, args.dry_run)?;
        }
        Err(errors) => {
            println!("Validation failed! Found {} problematic files:", errors.len());
            for error in &errors {
                println!("  {} - {}", error.file, error.reason);
            }
            println!("\nPlease fix these issues before proceeding.");
            std::process::exit(1);
        }
    }
    
    Ok(())
}

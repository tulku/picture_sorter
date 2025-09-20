use clap::Parser;
use indicatif::{ProgressBar, ProgressStyle};
use rayon::prelude::*;
use regex::Regex;
use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use chrono::{DateTime, NaiveDateTime, Utc};
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

fn get_exif_data(file_path: &Path) -> Result<Value, Box<dyn std::error::Error>> {
    let output = Command::new("exiftool")
        .arg("-j")
        .arg(file_path)
        .output()?;
    let json: Vec<Value> = serde_json::from_slice(&output.stdout)?;
    Ok(json.into_iter().next().unwrap_or(Value::Null))
}

fn get_exif_date(exif: &Value) -> Option<DateTime<Utc>> {
    if let Some(date_str) = exif.get("DateTimeOriginal").and_then(|v| v.as_str())
        && let Ok(dt) = NaiveDateTime::parse_from_str(date_str, "%Y:%m:%d %H:%M:%S") {
            return Some(DateTime::<Utc>::from_naive_utc_and_offset(dt, Utc));
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

fn group_files_by_base(directory: &Path) -> HashMap<String, Vec<String>> {
    let mut groups: HashMap<String, Vec<String>> = HashMap::new();
    if let Ok(entries) = fs::read_dir(directory) {
        let entries_vec: Vec<_> = entries.collect();
        let pb = ProgressBar::new(entries_vec.len() as u64);
        pb.set_style(
            ProgressStyle::default_bar()
                .template("{spinner:.green} [{elapsed_precise}] [{wide_bar:.cyan/blue}] {pos}/{len} ({eta}) Grouping files...")
                .expect("Failed to set progress bar style"),
        );
        for entry in entries_vec {
            if let Ok(entry) = entry
                && let Some(filename) = entry.file_name().to_str() {
                    let base = filename.split('.').next().unwrap_or("").to_string();
                    groups.entry(base).or_default().push(filename.to_string());
                }
            pb.inc(1);
        }
        pb.finish_with_message("File grouping complete");
    }
    groups
}

fn cache_exif_data(groups: &HashMap<String, Vec<String>>, directory: &Path) -> HashMap<String, (String, Value)> {
    let mut representative_files = Vec::new();
    for (base, file_list) in groups {
        let photo_files: Vec<String> = file_list.iter().filter(|f| is_raw_file(f) || is_jpeg_file(f)).cloned().collect();
        if !photo_files.is_empty() {
            // Pick JPEG if available, else first photo file
            let rep_file = photo_files.iter().find(|f| is_jpeg_file(f)).unwrap_or(&photo_files[0]).clone();
            representative_files.push((base.clone(), rep_file));
        }
    }
    let pb = ProgressBar::new(representative_files.len() as u64);
    pb.set_style(
        ProgressStyle::default_bar()
            .template("{spinner:.green} [{elapsed_precise}] [{wide_bar:.cyan/blue}] {pos}/{len} ({eta}) Caching EXIF data...")
            .expect("Failed to set progress bar style"),
    );
    let exif_cache: HashMap<String, (String, Value)> = representative_files
        .par_iter()
        .filter_map(|(base, rep_file)| {
            let file_path = directory.join(rep_file);
            match get_exif_data(&file_path) {
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
    files: &HashMap<String, Vec<String>>,
    exif_cache: &HashMap<String, (String, Value)>,
) -> Vec<(String, Vec<String>)> {
    let re = Regex::new(r"Sequence:\s*(\d+)").expect("Invalid regex for sequence");
    
    // Collect all photo bases with their sequence numbers and dates
    let mut photo_info: Vec<(String, u32, DateTime<Utc>)> = Vec::new();
    
    for (base, file_list) in files {
        let photo_files: Vec<String> = file_list.iter().filter(|f| is_raw_file(f) || is_jpeg_file(f)).cloned().collect();
        if !photo_files.is_empty() {
            if let Some((_, exif)) = exif_cache.get(base) {
                let seq_num = get_sequence_info(exif, &re);
                let date = get_exif_date(exif).unwrap_or_else(|| {
                    // Fallback to modification time if no EXIF date
                    Utc::now() // This should ideally use file mtime, but we don't have the directory here
                });
                photo_info.push((base.clone(), seq_num, date));
            }
        }
    }
    
    // Sort by date first, then by name to establish order
    photo_info.sort_by(|a, b| a.2.cmp(&b.2).then_with(|| a.0.cmp(&b.0)));
    
    // Debug: Print all photos with their sequence numbers
    println!("\nDebug: All photos with sequence numbers (sorted by date/name):");
    for (base, seq_num, date) in &photo_info {
        println!("  {} -> seq: {}, date: {}", base, seq_num, date.format("%Y-%m-%d %H:%M:%S"));
    }
    println!();
    
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
    
    // Debug: Print detected sequences
    println!("\nDebug: Detected sequences:");
    if sequences.is_empty() {
        println!("  No sequences found.");
    } else {
        for (seq_name, bases) in &sequences {
            println!("  Sequence '{}' contains {} photos:", seq_name, bases.len());
            for base in bases {
                println!("    - {}", base);
            }
        }
    }
    println!();
    
    // Convert base names back to photo files
    sequences.into_iter().map(|(seq_name, bases)| {
        let mut all_photo_files = Vec::new();
        for base in bases {
            if let Some(file_list) = files.get(&base) {
                let photo_files: Vec<String> = file_list.iter().filter(|f| is_raw_file(f) || is_jpeg_file(f)).cloned().collect();
                all_photo_files.extend(photo_files);
            }
        }
        (seq_name, all_photo_files)
    }).collect()
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

fn copy_files(
    input_dir: &Path,
    output_dir: &Path,
    dry_run: bool,
    groups: &HashMap<String, Vec<String>>,
    sequences: &[(String, Vec<String>)],
    exif_cache: &HashMap<String, (String, Value)>,
) -> Result<(), Box<dyn std::error::Error>> {
    let raw_dir = output_dir.join("RAW");
    let jpeg_dir = output_dir.join("JPEG");
    if !dry_run {
        fs::create_dir_all(&raw_dir)?;
        fs::create_dir_all(&jpeg_dir)?;
    }

    let total_files: u64 = groups.values().map(|fl| fl.len() as u64).sum();
    let pb = ProgressBar::new(total_files);
    pb.set_style(
        ProgressStyle::default_bar()
            .template("{spinner:.green} [{elapsed_precise}] [{wide_bar:.cyan/blue}] {pos}/{len} ({eta}) Copying files...")
            .expect("Failed to set progress bar style"),
    );

    for (base, file_list) in groups {
        // Prefer JPEG for representative, else any photo
        let photo_file_opt = file_list.iter().find(|f| is_jpeg_file(f)).or_else(|| file_list.iter().find(|f| is_raw_file(f) || is_jpeg_file(f))).cloned();
        let (photo_file, date) = if let Some(ref photo_file) = photo_file_opt {
            let date = if let Some((_, exif)) = exif_cache.get(base) {
                get_exif_date(exif)
            } else {
                None
            }.unwrap_or_else(|| {
                let file_path = input_dir.join(photo_file);
                let metadata = fs::metadata(&file_path).expect("Failed to get file metadata");
                let mtime = metadata.modified().expect("Failed to get modification time");
                DateTime::<Utc>::from(mtime)
            });
            (photo_file.clone(), date)
        } else {
            // No photo file, use mtime of first file
            let first_file = &file_list[0];
            let file_path = input_dir.join(first_file);
            let metadata = fs::metadata(&file_path).expect("Failed to get file metadata");
            let mtime = metadata.modified().expect("Failed to get modification time");
            let date = DateTime::<Utc>::from(mtime);
            (first_file.clone(), date)
        };

        let year = date.format("%Y").to_string();
        let month = date.format("%m").to_string();
        let day = date.format("%d").to_string();

        let seq_dir = sequences.iter().find(|(seq_base, _)| seq_base == base).map(|(_, _)| base.clone());

        // Default target_base for the group
        let default_target_base = if is_raw_file(&photo_file) {
            &raw_dir
        } else {
            &jpeg_dir
        };

        for f in file_list {
            let target_base = determine_target_base(f, &raw_dir, &jpeg_dir, default_target_base);
            let mut target_path = target_base.join(&year).join(&month).join(&day);
            if let Some(ref seq) = seq_dir {
                target_path = target_path.join(seq);
            }

            let source = input_dir.join(f);
            let dest = target_path.join(f);

            if dry_run {
                println!("Would copy {} to {}", source.display(), dest.display());
            } else {
                fs::create_dir_all(&target_path)?;
                fs::copy(&source, &dest)?;
            }
            pb.inc(1);
        }
    }
    pb.finish_with_message("File copying complete");
    Ok(())
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let args = Args::parse();
    let input_dir = PathBuf::from(&args.input_dir);
    let output_dir = PathBuf::from(&args.output_dir);

    let groups = group_files_by_base(&input_dir);
    let exif_cache = cache_exif_data(&groups, &input_dir);
    let sequences = detect_sequences(&groups, &exif_cache);
    copy_files(&input_dir, &output_dir, args.dry_run, &groups, &sequences, &exif_cache)?;
    Ok(())
}

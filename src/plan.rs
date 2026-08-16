//! Per-file copy/move decision for incremental imports.

use chrono::{DateTime, Utc};
use std::path::{Path, PathBuf};

pub fn orig_sidecar_path(dest: &Path) -> PathBuf {
    match dest.file_name().and_then(|n| n.to_str()) {
        Some(name) => dest.with_file_name(format!("{name}.orig")),
        None => dest.with_extension("orig"),
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PlanDecision {
    Copy,
    RecopyIncomplete,
    MoveFromFlat,
    SkipOlderThanCutoff,
    SkipExisting,
    ConflictExists,
    ConflictFlatAndSequence,
}

#[derive(Debug, Clone, Copy)]
pub struct PlanInput {
    pub dest_exists: bool,
    pub dest_len: Option<u64>,
    pub source_len: u64,
    pub date: DateTime<Utc>,
    pub cutoff: Option<DateTime<Utc>>,
    pub in_sequence: bool,
    pub flat_exists: bool,
    pub seq_dest_exists: bool,
    pub force_override: bool,
    pub skip_existing: bool,
    pub incremental: bool,
    /// True when dest has a sibling `NAME.ext.orig` (in-place edit backup).
    pub has_orig: bool,
}

fn dest_looks_truncated(input: &PlanInput) -> bool {
    let Some(dest_len) = input.dest_len else {
        return false;
    };
    // Edits can shrink or grow the file by a little. Only a dest under half
    // the source size (and with no .orig backup) looks like a dropped copy.
    dest_len.saturating_mul(2) < input.source_len && !input.has_orig
}

pub fn decide(input: PlanInput) -> PlanDecision {
    if input.in_sequence && input.flat_exists && !input.force_override {
        if input.seq_dest_exists {
            if input.skip_existing || input.incremental {
                return PlanDecision::SkipExisting;
            }
            return PlanDecision::ConflictFlatAndSequence;
        }
        return PlanDecision::MoveFromFlat;
    }

    if input.dest_exists && !input.force_override {
        if dest_looks_truncated(&input) {
            return PlanDecision::RecopyIncomplete;
        }
        if input.skip_existing || input.incremental {
            return PlanDecision::SkipExisting;
        }
        return PlanDecision::ConflictExists;
    }

    if input.incremental {
        if let Some(cutoff) = input.cutoff {
            if input.date < cutoff {
                return PlanDecision::SkipOlderThanCutoff;
            }
        }
    }

    PlanDecision::Copy
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;
    use std::path::{Path, PathBuf};

    fn t(h: u32, m: u32, s: u32) -> DateTime<Utc> {
        Utc.with_ymd_and_hms(2026, 5, 15, h, m, s).unwrap()
    }

    fn base() -> PlanInput {
        PlanInput {
            dest_exists: false,
            dest_len: None,
            source_len: 100,
            date: t(13, 13, 49),
            cutoff: None,
            in_sequence: false,
            flat_exists: false,
            seq_dest_exists: false,
            force_override: false,
            skip_existing: false,
            incremental: false,
            has_orig: false,
        }
    }

    #[test]
    fn same_second_remaining_frames_are_copied() {
        let mut input = base();
        input.incremental = true;
        input.cutoff = Some(t(13, 13, 49));
        input.date = t(13, 13, 49);
        input.dest_exists = false;
        assert_eq!(decide(input), PlanDecision::Copy);
    }

    #[test]
    fn older_than_cutoff_is_skipped() {
        let mut input = base();
        input.incremental = true;
        input.cutoff = Some(t(13, 13, 49));
        input.date = t(13, 13, 48);
        assert_eq!(decide(input), PlanDecision::SkipOlderThanCutoff);
    }

    #[test]
    fn incremental_skips_complete_dest_instead_of_erroring() {
        let mut input = base();
        input.incremental = true;
        input.cutoff = Some(t(13, 13, 49));
        input.date = t(13, 13, 49);
        input.dest_exists = true;
        input.dest_len = Some(100);
        assert_eq!(decide(input), PlanDecision::SkipExisting);
    }

    #[test]
    fn without_incremental_existing_dest_is_a_conflict() {
        let mut input = base();
        input.dest_exists = true;
        input.dest_len = Some(100);
        assert_eq!(decide(input), PlanDecision::ConflictExists);
    }

    #[test]
    fn dry_run_uses_the_same_conflict_rule() {
        // skip_existing is the only thing that turns a conflict into a skip
        let mut input = base();
        input.dest_exists = true;
        input.dest_len = Some(100);
        input.skip_existing = true;
        assert_eq!(decide(input), PlanDecision::SkipExisting);
    }

    #[test]
    fn incomplete_dest_is_recopied() {
        let mut input = base();
        input.incremental = true;
        input.dest_exists = true;
        input.dest_len = Some(10);
        input.source_len = 100;
        assert_eq!(decide(input), PlanDecision::RecopyIncomplete);
    }

    #[test]
    fn slightly_smaller_dest_without_orig_is_kept() {
        let mut input = base();
        input.incremental = true;
        input.dest_exists = true;
        input.dest_len = Some(14_456_530);
        input.source_len = 14_466_603;
        input.has_orig = false;
        assert_eq!(decide(input), PlanDecision::SkipExisting);
    }

    #[test]
    fn edited_dest_with_orig_sidecar_is_not_recopied() {
        let mut input = base();
        input.incremental = true;
        input.dest_exists = true;
        input.dest_len = Some(14_456_530);
        input.source_len = 14_466_603;
        input.has_orig = true;
        assert_eq!(decide(input), PlanDecision::SkipExisting);
    }

    #[test]
    fn orig_sidecar_keeps_full_filename() {
        assert_eq!(
            orig_sidecar_path(Path::new("/dest/JPEG/2026/05/15/P5157579.JPG")),
            PathBuf::from("/dest/JPEG/2026/05/15/P5157579.JPG.orig")
        );
    }

    #[test]
    fn dest_larger_than_source_is_treated_as_existing() {
        let mut input = base();
        input.incremental = true;
        input.dest_exists = true;
        input.dest_len = Some(200);
        input.source_len = 100;
        assert_eq!(decide(input), PlanDecision::SkipExisting);
    }

    #[test]
    fn sequence_move_happens_even_when_older_than_cutoff() {
        let mut input = base();
        input.incremental = true;
        input.cutoff = Some(t(13, 13, 49));
        input.date = t(10, 0, 0);
        input.in_sequence = true;
        input.flat_exists = true;
        input.seq_dest_exists = false;
        assert_eq!(decide(input), PlanDecision::MoveFromFlat);
    }

    #[test]
    fn both_flat_and_sequence_is_a_conflict() {
        let mut input = base();
        input.in_sequence = true;
        input.flat_exists = true;
        input.seq_dest_exists = true;
        assert_eq!(decide(input), PlanDecision::ConflictFlatAndSequence);
    }
}

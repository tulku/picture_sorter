use super::CaptureRecord;
use crate::domain::*;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub enum SequenceKind {
    Burst,
    Bracket,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SequenceRecord {
    pub kind: SequenceKind,
    pub members: Vec<Digest>,
    pub anchor: String,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SequenceDecision {
    pub kind: SequenceKind,
    pub members: Vec<Digest>,
    pub anchor: String,
    pub incomplete: bool,
    pub explanation: String,
}
pub struct SequencePolicy {
    pub burst_gap_seconds: i64,
    pub bracket_gap_seconds: i64,
}
impl Default for SequencePolicy {
    fn default() -> Self {
        Self {
            burst_gap_seconds: 2,
            bracket_gap_seconds: 5,
        }
    }
}
fn mode(record: &CaptureRecord) -> Option<(SequenceKind, u64)> {
    let mode = record.metadata.raw.get("DriveMode")?.as_str()?;
    let kind = if mode.contains("AE Auto Bracketing") {
        SequenceKind::Bracket
    } else if mode.contains("Sequential") {
        SequenceKind::Burst
    } else {
        return None;
    };
    let shot = mode
        .split("Shot ")
        .nth(1)?
        .split(|c: char| !c.is_ascii_digit())
        .next()?
        .parse()
        .ok()?;
    Some((kind, shot))
}
fn time(record: &CaptureRecord) -> Option<chrono::NaiveDateTime> {
    chrono::NaiveDateTime::parse_from_str(record.metadata.capture.get(..19)?, "%Y:%m:%d %H:%M:%S")
        .ok()
}
pub fn classify_sequences(
    records: &[CaptureRecord],
    existing: &[SequenceRecord],
    policy: &SequencePolicy,
) -> Vec<SequenceDecision> {
    let mut representatives = Vec::new();
    let mut equivalents = BTreeMap::new();
    for pair in super::pair_candidates(records) {
        let members = records
            .iter()
            .filter(|r| pair.members.contains(&r.member.path))
            .collect::<Vec<_>>();
        if let Some(first) = members.first() {
            representatives.push((*first).clone());
            equivalents.insert(
                first.member.digest.clone(),
                members
                    .iter()
                    .map(|r| r.member.digest.clone())
                    .collect::<Vec<_>>(),
            );
        }
    }
    let existing = existing
        .iter()
        .map(|sequence| SequenceRecord {
            kind: sequence.kind,
            anchor: sequence.anchor.clone(),
            members: equivalents
                .iter()
                .filter(|(_, ds)| ds.iter().any(|d| sequence.members.contains(d)))
                .map(|(d, _)| d.clone())
                .collect(),
        })
        .collect::<Vec<_>>();
    let mut decisions = classify_representatives(&representatives, &existing, policy);
    for decision in &mut decisions {
        decision.members = decision
            .members
            .iter()
            .flat_map(|d| {
                equivalents
                    .get(d)
                    .cloned()
                    .unwrap_or_else(|| vec![d.clone()])
            })
            .collect();
    }
    decisions
}
fn classify_representatives(
    records: &[CaptureRecord],
    existing: &[SequenceRecord],
    policy: &SequencePolicy,
) -> Vec<SequenceDecision> {
    let mut buckets: BTreeMap<
        (String, String, String, SequenceKind),
        Vec<(&CaptureRecord, u64, chrono::NaiveDateTime)>,
    > = BTreeMap::new();
    for record in records {
        let Some((kind, shot)) = mode(record) else {
            continue;
        };
        let Some(time) = time(record) else { continue };
        if record.metadata.camera.is_empty() {
            continue;
        }
        buckets
            .entry((
                record.context.clone(),
                record.metadata.camera.clone(),
                time.date().to_string(),
                kind,
            ))
            .or_default()
            .push((record, shot, time));
    }
    let mut result = Vec::new();
    for ((_, _, _, kind), mut frames) in buckets {
        frames.sort_by(|(a, _, ta), (b, _, tb)| {
            ta.cmp(tb).then_with(|| a.member.path.cmp(&b.member.path))
        });
        let mut run = Vec::new();
        for frame in frames {
            let contiguous =
                run.last()
                    .is_none_or(|previous: &(&CaptureRecord, u64, chrono::NaiveDateTime)| {
                        frame.1 > previous.1
                            && frame.2 >= previous.2
                            && (frame.2 - previous.2).num_seconds()
                                <= if kind == SequenceKind::Burst {
                                    policy.burst_gap_seconds
                                } else {
                                    policy.bracket_gap_seconds
                                }
                            && match (previous.0.counter, frame.0.counter) {
                                (Some(a), Some(b)) => b > a,
                                _ => true,
                            }
                    });
            if !contiguous {
                finish(&mut result, &run, kind, existing);
                run.clear();
            }
            run.push(frame);
        }
        finish(&mut result, &run, kind, existing);
    }
    result
}
fn finish(
    result: &mut Vec<SequenceDecision>,
    frames: &[(&CaptureRecord, u64, chrono::NaiveDateTime)],
    kind: SequenceKind,
    existing: &[SequenceRecord],
) {
    if frames.len() < 2 {
        return;
    }
    let members: Vec<_> = frames
        .iter()
        .map(|(r, _, _)| r.member.digest.clone())
        .collect();
    let matches: Vec<_> = existing
        .iter()
        .filter(|s| s.kind == kind && s.members.iter().any(|d| members.contains(d)))
        .collect();
    if matches.len() > 1 {
        return;
    }
    let anchor = matches
        .first()
        .map(|s| s.anchor.clone())
        .unwrap_or_else(|| {
            format!(
                "{}_{}",
                frames[0].0.stem,
                if kind == SequenceKind::Burst {
                    "BURST"
                } else {
                    "HDR"
                }
            )
        });
    result.push(SequenceDecision {
        kind,
        members,
        anchor,
        incomplete: frames[0].1 != 1 || frames.windows(2).any(|w| w[1].1 != w[0].1 + 1),
        explanation:
            "compatible camera/context/day/mode, increasing shot/counter and bounded capture gap"
                .into(),
    });
}

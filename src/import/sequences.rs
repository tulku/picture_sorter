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
type SequencePartition = (String, String, String, SequenceKind);
type SequenceFrame<'a> = (&'a CaptureRecord, u64, chrono::NaiveDateTime);
fn classify_representatives(
    records: &[CaptureRecord],
    existing: &[SequenceRecord],
    policy: &SequencePolicy,
) -> Vec<SequenceDecision> {
    let mut buckets: BTreeMap<SequencePartition, Vec<SequenceFrame<'_>>> = BTreeMap::new();
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
impl crate::catalog::Catalog {
    pub fn classify_import_sequences(
        &mut self,
        records: &[CaptureRecord],
    ) -> crate::error::AppResult<Vec<SequenceDecision>> {
        use crate::error::AppError;
        use rusqlite::params;
        if records.len() > 128 {
            return Err(AppError::invalid("sequence input batch exceeds 128"));
        }
        let mut window = std::collections::BTreeMap::new();
        for record in records {
            record_capture_evidence(&self.db, record)?;
            window.insert(
                (record.member.digest.clone(), record.member.path.clone()),
                record.clone(),
            );
        }
        for record in records {
            let Some(time) = time(record) else {
                continue;
            };
            let second = time.and_utc().timestamp();
            let mut query=self.db.prepare("SELECT payload FROM capture_facts WHERE context=? AND camera=? AND second BETWEEN ? AND ? ORDER BY abs(second-?),digest,path LIMIT 256")?;
            for row in query.query_map(
                params![
                    record.context,
                    record.metadata.camera,
                    second - 5,
                    second + 5,
                    second
                ],
                |r| r.get::<_, String>(0),
            )? {
                let capture: CaptureRecord = serde_json::from_str(&row?)?;
                window
                    .entry((capture.member.digest.clone(), capture.member.path.clone()))
                    .or_insert(capture);
                if window.len() >= 512 {
                    break;
                }
            }
            if window.len() >= 512 {
                break;
            }
        }
        let mut existing = Vec::new();
        for capture in window.values() {
            let mut query = self
                .db
                .prepare("SELECT DISTINCT kind,anchor FROM sequence_members WHERE digest=?")?;
            for row in query.query_map([capture.member.digest.as_str()], |r| {
                Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?))
            })? {
                let (kind, anchor) = row?;
                existing.push(SequenceRecord {
                    kind: serde_json::from_str(&kind)?,
                    anchor,
                    members: vec![capture.member.digest.clone()],
                });
            }
        }
        let mut merged = std::collections::BTreeMap::new();
        for entry in existing {
            merged
                .entry((entry.kind, entry.anchor))
                .or_insert_with(Vec::new)
                .extend(entry.members);
        }
        let existing = merged
            .into_iter()
            .map(|((kind, anchor), members)| SequenceRecord {
                kind,
                anchor,
                members,
            })
            .collect::<Vec<_>>();
        let window = window.into_values().collect::<Vec<_>>();
        let decisions = classify_sequences(&window, &existing, &SequencePolicy::default());
        for decision in &decisions {
            record_sequence_evidence(
                &self.db,
                &SequenceRecord {
                    kind: decision.kind,
                    anchor: decision.anchor.clone(),
                    members: decision.members.clone(),
                },
            )?;
        }
        Ok(decisions)
    }
    pub fn sequence_records_for(
        &self,
        digest: &Digest,
    ) -> crate::error::AppResult<Vec<crate::archive::ArchiveRecord>> {
        let mut result = Vec::new();
        let mut query = self
            .db
            .prepare("SELECT payload FROM capture_facts WHERE digest=? ORDER BY path LIMIT 128")?;
        for row in query.query_map([digest.as_str()], |r| r.get::<_, String>(0))? {
            result.push(crate::archive::ArchiveRecord::CaptureRecorded {
                capture: serde_json::from_str(&row?)?,
            });
        }
        let mut query=self.db.prepare("SELECT kind,anchor FROM sequence_members WHERE digest=? ORDER BY kind,anchor LIMIT 128")?;
        for row in query.query_map([digest.as_str()], |r| {
            Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?))
        })? {
            let (kind, anchor) = row?;
            let mut members=self.db.prepare("SELECT digest FROM sequence_members WHERE kind=? AND anchor=? AND digest<>? ORDER BY digest LIMIT 511")?;
            let mut members = members
                .query_map(rusqlite::params![kind, anchor, digest.as_str()], |r| {
                    r.get::<_, String>(0)
                })?
                .map(|r| Digest::parse(&r?))
                .collect::<crate::error::AppResult<Vec<_>>>()?;
            members.push(digest.clone());
            result.push(crate::archive::ArchiveRecord::SequenceRecorded {
                sequence: SequenceRecord {
                    kind: serde_json::from_str(&kind)?,
                    anchor,
                    members,
                },
            });
        }
        Ok(result)
    }
}
pub(crate) fn record_capture_evidence(
    db: &rusqlite::Connection,
    capture: &CaptureRecord,
) -> crate::error::AppResult<()> {
    let mut capture = capture.clone();
    capture.source_mtime = None;
    capture.metadata.raw = serde_json::json!({"DriveMode":capture.metadata.raw.get("DriveMode")});
    let payload = serde_json::to_string(&capture)?;
    if payload.len() > 16 * 1024 {
        return Err(crate::error::AppError::invalid(
            "normalized capture facts exceed bound",
        ));
    }
    let second = time(&capture).map(|t| t.and_utc().timestamp());
    db.execute("INSERT INTO capture_facts(digest,path,context,camera,second,payload) VALUES(?,?,?,?,?,?) ON CONFLICT DO NOTHING",rusqlite::params![capture.member.digest.as_str(),capture.member.path.as_str(),capture.context,capture.metadata.camera,second,payload])?;
    Ok(())
}
pub(crate) fn record_sequence_evidence(
    db: &rusqlite::Connection,
    sequence: &SequenceRecord,
) -> crate::error::AppResult<()> {
    if sequence.members.len() > 512
        || sequence.anchor.len() > 255
        || sequence.anchor.contains('/')
        || sequence.anchor.is_empty()
    {
        return Err(crate::error::AppError::invalid(
            "invalid sequence evidence bound/anchor",
        ));
    }
    for digest in &sequence.members {
        db.execute(
            "INSERT INTO sequence_members(digest,kind,anchor) VALUES(?,?,?) ON CONFLICT DO NOTHING",
            rusqlite::params![
                digest.as_str(),
                serde_json::to_string(&sequence.kind)?,
                sequence.anchor
            ],
        )?;
    }
    Ok(())
}

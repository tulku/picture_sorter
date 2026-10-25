use super::MetadataResult;
use crate::{catalog::PairFacts, domain::*};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CaptureRecord {
    pub member: OriginalMember,
    pub metadata: MetadataResult,
    pub context: String,
    pub stem: String,
    pub counter: Option<u64>,
    pub source_mtime: Option<std::time::SystemTime>,
}
impl CaptureRecord {
    pub fn new(member: OriginalMember, metadata: MetadataResult) -> Self {
        let (context, name) = member
            .path
            .as_str()
            .rsplit_once('/')
            .unwrap_or(("", member.path.as_str()));
        let stem = name
            .rsplit_once('.')
            .map(|(stem, _)| stem)
            .unwrap_or(name)
            .to_owned();
        let digits = stem
            .chars()
            .rev()
            .take_while(char::is_ascii_digit)
            .collect::<String>()
            .chars()
            .rev()
            .collect::<String>();
        Self {
            context: context.to_owned(),
            stem,
            counter: digits.parse().ok(),
            source_mtime: None,
            member,
            metadata,
        }
    }
    pub fn facts(&self) -> PairFacts {
        PairFacts {
            context: self.context.clone(),
            stem: self.stem.clone(),
            camera: self.metadata.camera.clone(),
            capture: self.metadata.capture.clone(),
        }
    }
}
#[derive(Debug, Clone)]
pub struct PairDecision {
    pub members: Vec<ArchivePath>,
    pub primary_raw: Option<ArchivePath>,
    pub evidence: Option<crate::catalog::PairEvidence>,
    pub warning: Option<String>,
}
pub fn pair_candidates(records: &[CaptureRecord]) -> Vec<PairDecision> {
    let mut buckets: BTreeMap<(String, String, String, String), Vec<&CaptureRecord>> =
        BTreeMap::new();
    let mut result = Vec::new();
    for record in records {
        if record.metadata.camera.is_empty()
            || record.metadata.capture.is_empty()
            || matches!(record.member.role, MediaRole::Video | MediaRole::OtherStill)
        {
            result.push(single(record));
        } else {
            buckets
                .entry((
                    record.context.clone(),
                    record.stem.clone(),
                    record.metadata.camera.clone(),
                    record.metadata.capture.clone(),
                ))
                .or_default()
                .push(record);
        }
    }
    for entries in buckets.values() {
        let raw: Vec<_> = entries
            .iter()
            .filter(|e| e.member.role == MediaRole::Raw)
            .copied()
            .collect();
        let jpeg = entries
            .iter()
            .filter(|e| e.member.role == MediaRole::Jpeg)
            .count();
        let orf: Vec<_> = raw
            .iter()
            .filter(|e| {
                e.member
                    .path
                    .as_str()
                    .to_ascii_lowercase()
                    .ends_with(".orf")
            })
            .collect();
        let primary = if orf.len() == 1
            && raw.iter().all(|e| {
                e.member
                    .path
                    .as_str()
                    .to_ascii_lowercase()
                    .ends_with(".orf")
                    || e.member
                        .path
                        .as_str()
                        .to_ascii_lowercase()
                        .ends_with(".ori")
            }) {
            Some(orf[0].member.path.clone())
        } else if raw.len() == 1 {
            Some(raw[0].member.path.clone())
        } else {
            None
        };
        if jpeg > 1 || (raw.len() > 1 && primary.is_none()) {
            result.extend(entries.iter().map(|r| single(r)));
            continue;
        }
        let evidence = entries.first().zip(entries.last()).map(|(left, right)| {
            crate::catalog::PairEvidence::Compared {
                left: left.facts(),
                right: right.facts(),
            }
        });
        result.push(PairDecision {
            members: entries.iter().map(|r| r.member.path.clone()).collect(),
            primary_raw: primary,
            evidence,
            warning: if entries.len() == 1 {
                Some("unpaired original".into())
            } else {
                None
            },
        });
    }
    result
}
fn single(record: &CaptureRecord) -> PairDecision {
    PairDecision {
        members: vec![record.member.path.clone()],
        primary_raw: if record.member.role == MediaRole::Raw {
            Some(record.member.path.clone())
        } else {
            None
        },
        evidence: None,
        warning: Some("association requires review".into()),
    }
}

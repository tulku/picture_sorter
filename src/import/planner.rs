use super::CaptureRecord;
use crate::{
    catalog::Catalog,
    domain::*,
    error::{AppError, AppResult},
};
#[derive(Debug, Clone)]
pub struct PlannedGroup {
    pub id: GroupId,
    pub records: Vec<CaptureRecord>,
    pub sequence: Option<String>,
}
#[derive(Debug, Clone)]
pub struct PlacementIntent {
    pub group: GroupId,
    pub member: OriginalMember,
    pub source: ArchivePath,
}
pub fn plan_placements(
    groups: &[PlannedGroup],
    existing: &Catalog,
    timezone: &str,
) -> AppResult<Vec<PlacementIntent>> {
    let mut result = Vec::new();
    for group in groups {
        let previous = existing.group(group.id).ok();
        for record in &group.records {
            let mut member = record.member.clone();
            if let Some(old) = previous.as_ref().and_then(|g| {
                g.members
                    .iter()
                    .find(|m| m.digest == member.digest && m.role == member.role)
            }) {
                member.path = old.path.clone();
            } else {
                let tree = match member.role {
                    MediaRole::Raw => "RAW",
                    MediaRole::Jpeg | MediaRole::OtherStill => "JPEG",
                    MediaRole::Video => "VIDEO",
                };
                let filename = member
                    .path
                    .as_str()
                    .rsplit('/')
                    .next()
                    .ok_or_else(|| AppError::invalid("missing source filename"))?;
                let day = if let Some(day) = &record.metadata.capture_day {
                    day.clone()
                } else if let Some(mtime) = record.source_mtime {
                    record.metadata.fallback_day(mtime, timezone)?
                } else {
                    "UNDATED".into()
                };
                member.path = ArchivePath::parse(&match &group.sequence {
                    Some(sequence) => format!("{tree}/{day}/{sequence}/{filename}"),
                    None => format!("{tree}/{day}/{filename}"),
                })?;
            }
            result.push(PlacementIntent {
                group: group.id,
                member,
                source: record.member.path.clone(),
            });
        }
    }
    Ok(result)
}

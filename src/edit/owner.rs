use crate::domain::*;
use serde::{Deserialize, Serialize};
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum SidecarKind {
    Generated,
    RapidRaw,
    Rrexif,
    RawXmp,
    JpegXmpExternal,
    Opaque,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SidecarOwner {
    pub id: SidecarId,
    pub group: Option<GroupId>,
    pub path: ArchivePath,
    pub kind: SidecarKind,
    pub virtual_copy: Option<String>,
}
#[derive(Debug, Clone)]
pub enum OwnerDecision {
    Owned {
        member: ArchivePath,
        kind: SidecarKind,
        virtual_copy: Option<String>,
    },
    Unassociated,
}
pub fn resolve_owner(path: &ArchivePath, members: &[OriginalMember]) -> OwnerDecision {
    let mut members = members.iter().collect::<Vec<_>>();
    members.sort_by_key(|m| std::cmp::Reverse(m.path.as_str().len()));
    for member in members {
        let Some(suffix) = path.as_str().strip_prefix(member.path.as_str()) else {
            continue;
        };
        let result = match suffix {
            ".rrdata" => Some((SidecarKind::RapidRaw, None)),
            ".rrexif" => Some((SidecarKind::Rrexif, None)),
            ".xmp" => Some((
                if member.role == MediaRole::Jpeg {
                    SidecarKind::JpegXmpExternal
                } else {
                    SidecarKind::RawXmp
                },
                None,
            )),
            ".orig" | ".pp3" | ".dop" | ".on1" => Some((SidecarKind::Opaque, None)),
            _ => suffix
                .strip_prefix('.')
                .and_then(|s| s.strip_suffix(".rrdata"))
                .filter(|s| {
                    s.len() == 6
                        && s.bytes()
                            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
                })
                .map(|id| (SidecarKind::RapidRaw, Some(id.to_owned()))),
        };
        if let Some((kind, virtual_copy)) = result {
            return OwnerDecision::Owned {
                member: member.path.clone(),
                kind,
                virtual_copy,
            };
        }
    }
    OwnerDecision::Unassociated
}

impl SidecarKind {
    pub fn saved_owner(self) -> &'static str {
        if self == Self::Generated {
            GENERATED_OUTPUT_OWNER
        } else {
            "opaque"
        }
    }
}

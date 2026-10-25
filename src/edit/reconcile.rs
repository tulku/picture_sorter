use super::*;
use crate::{catalog::Catalog, domain::*, error::AppResult, fs::RootHandle};
#[derive(Debug, Clone)]
pub enum LocalEvent {
    Changed(ArchivePath),
    RescanRequired,
}
pub enum ReconcileScope {
    All,
}
#[derive(Default, Debug)]
pub struct ReconcileReport {
    pub captured: u64,
    pub resets: u64,
    pub unreadable: u64,
    pub invalid: u64,
    pub retry: u64,
}
pub struct EditTracker<'a> {
    local: &'a RootHandle,
    state: &'a RootHandle,
    catalog: &'a mut Catalog,
    clock: &'a dyn Clock,
    pending: bool,
}
impl<'a> EditTracker<'a> {
    pub fn new(
        local: &'a RootHandle,
        state: &'a RootHandle,
        catalog: &'a mut Catalog,
        clock: &'a dyn Clock,
    ) -> Self {
        Self {
            local,
            state,
            catalog,
            clock,
            pending: true,
        }
    }
    pub fn observe(&mut self, _event: LocalEvent) -> AppResult<()> {
        self.pending = true;
        Ok(())
    }
    pub fn reconcile(&mut self, _scope: ReconcileScope) -> AppResult<ReconcileReport> {
        self.discover()?;
        let mut report = ReconcileReport::default();
        let mut after = String::new();
        loop {
            let mut q = self.catalog.db.prepare(
                "SELECT id,group_id,path,owner FROM sidecars WHERE id>? ORDER BY id LIMIT 128",
            )?;
            let rows = q
                .query_map([&after], |r| {
                    Ok((
                        r.get::<_, String>(0)?,
                        r.get::<_, Option<String>>(1)?,
                        r.get::<_, String>(2)?,
                        r.get::<_, String>(3)?,
                    ))
                })?
                .collect::<Result<Vec<_>, _>>()?;
            drop(q);
            if rows.is_empty() {
                break;
            }
            for (id, group, path, owner) in rows {
                after = id.clone();
                let sidecar = SidecarOwner {
                    id: serde_json::from_str(&format!("\"{id}\""))?,
                    group: group
                        .map(|g| serde_json::from_str(&format!("\"{g}\"")))
                        .transpose()?,
                    path: ArchivePath::parse(&path)?,
                    kind: if owner == "rapidraw-base" || owner == "rapidraw-virtual" {
                        SidecarKind::RapidRaw
                    } else if owner == "jpeg-xmp-external" {
                        SidecarKind::JpegXmpExternal
                    } else {
                        SidecarKind::Opaque
                    },
                    virtual_copy: None,
                };
                match Snapshotter::new(
                    self.local,
                    self.state,
                    self.catalog,
                    self.clock,
                    2,
                    64 * 1024 * 1024,
                )
                .capture(&sidecar)?
                {
                    SnapshotObservation::StableContent { changed: true, .. } => {
                        report.captured += 1
                    }
                    SnapshotObservation::StableAbsent { changed: true, .. } => report.resets += 1,
                    SnapshotObservation::Unreadable { .. } => report.unreadable += 1,
                    SnapshotObservation::OpaqueInvalid { .. } => report.invalid += 1,
                    SnapshotObservation::Retry => report.retry += 1,
                    _ => {}
                }
            }
        }
        self.pending = false;
        Ok(report)
    }
}

impl EditTracker<'_> {
    fn discover(&mut self) -> AppResult<()> {
        let mut inventory = crate::import::Inventory::open(RootHandle::open(
            self.local.path(),
            crate::fs::RootPolicy::Local,
        )?)?;
        loop {
            let batch = inventory.next_batch(128)?;
            if batch.is_empty() {
                break;
            }
            for entry in batch {
                if !matches!(entry.role, crate::import::SourceRole::Companion(_)) {
                    continue;
                }
                let exists: bool = self.catalog.db.query_row(
                    "SELECT EXISTS(SELECT 1 FROM sidecars WHERE path=?)",
                    [entry.path.as_str()],
                    |r| r.get(0),
                )?;
                if exists {
                    continue;
                }
                let mut q=self.catalog.db.prepare("SELECT m.path,m.digest,b.size,m.role,m.group_id FROM members m JOIN blobs b ON b.digest=m.digest WHERE substr(?,1,length(m.path))=m.path ORDER BY length(m.path) DESC LIMIT 128")?;
                let rows = q
                    .query_map([entry.path.as_str()], |r| {
                        Ok((
                            r.get::<_, String>(0)?,
                            r.get::<_, String>(1)?,
                            r.get::<_, u64>(2)?,
                            r.get::<_, String>(3)?,
                            r.get::<_, String>(4)?,
                        ))
                    })?
                    .collect::<Result<Vec<_>, _>>()?;
                drop(q);
                let members = rows
                    .iter()
                    .map(|(p, d, size, role, _)| {
                        Ok(OriginalMember {
                            path: ArchivePath::parse(p)?,
                            digest: Digest::parse(d)?,
                            size: *size,
                            role: serde_json::from_str(role)?,
                        })
                    })
                    .collect::<AppResult<Vec<_>>>()?;
                let (group, owner) = match resolve_owner(&entry.path, &members) {
                    OwnerDecision::Owned {
                        member,
                        kind,
                        virtual_copy,
                    } => {
                        let group = rows
                            .iter()
                            .find(|r| r.0 == member.as_str())
                            .map(|r| serde_json::from_str(&format!("\"{}\"", r.4)))
                            .transpose()?;
                        let owner = match kind {
                            SidecarKind::RapidRaw => {
                                if virtual_copy.is_some() {
                                    "rapidraw-virtual"
                                } else {
                                    "rapidraw-base"
                                }
                            }
                            SidecarKind::Rrexif => "rrexif",
                            SidecarKind::RawXmp => "raw-xmp",
                            SidecarKind::JpegXmpExternal => "jpeg-xmp-external",
                            SidecarKind::Opaque => "opaque",
                        };
                        (group, owner)
                    }
                    OwnerDecision::Unassociated => (None, "opaque-unassociated"),
                };
                self.catalog
                    .register_sidecar(SidecarId::new(), group, entry.path, owner)?;
            }
        }
        Ok(())
    }
}

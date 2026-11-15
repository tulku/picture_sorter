use super::*;
use crate::{
    config::Config,
    error::{AppError, AppResult},
    fs::*,
};
fn read_report_file(path: &std::path::Path) -> AppResult<Vec<u8>> {
    if path.extension().and_then(|s| s.to_str()) != Some("json") {
        return Err(AppError::invalid(
            "capability evidence must be a JSON report",
        ));
    }
    let parent = RootHandle::open(
        path.parent()
            .ok_or_else(|| AppError::invalid("evidence requires local parent"))?,
        RootPolicy::Local,
    )?;
    let name = ArchivePath::parse(
        path.file_name()
            .and_then(|n| n.to_str())
            .ok_or_else(|| AppError::invalid("evidence name must be UTF-8"))?,
    )?;
    let file = parent.open_relative(&name, libc::O_RDONLY | libc::O_NONBLOCK, 0)?;
    use std::os::unix::fs::MetadataExt;
    let metadata = file.metadata()?;
    if !metadata.is_file()
        || metadata.uid() != unsafe { libc::geteuid() }
        || metadata.mode() & 0o777 != 0o600
    {
        return Err(AppError::invalid(
            "capability evidence requires owned mode-0600 regular file",
        ));
    }
    drop(file);
    parent.read(&name, 1024 * 1024)
}
pub fn load_registry(config: &Config) -> AppResult<CapabilityRegistry> {
    let mut reports = load_manual_reports(config)?;
    match read_report_file(&config.state_root.join("automatic-nas-qualification.json")) {
        Ok(bytes) => {
            let report: CapabilityReport = serde_json::from_slice(&bytes)?;
            reports.retain(|old| {
                old.capability != report.capability
                    || old.config_fingerprint != report.config_fingerprint
            });
            reports.push(report);
        }
        Err(e) if e.errno == Some(libc::ENOENT) => {}
        Err(e) => return Err(e),
    }
    Ok(CapabilityRegistry::new(reports))
}

fn load_manual_reports(config: &Config) -> AppResult<Vec<CapabilityReport>> {
    let mut reports = Vec::new();
    for path in &config.capability_evidence {
        reports.push(serde_json::from_slice::<CapabilityReport>(
            &read_report_file(path)?,
        )?);
    }
    let current = config.state_root.join("capability-registry.json");
    match read_report_file(&current) {
        Ok(bytes) => {
            for report in serde_json::from_slice::<Vec<CapabilityReport>>(&bytes)? {
                reports.retain(|old| {
                    old.capability != report.capability
                        || old.config_fingerprint != report.config_fingerprint
                });
                reports.push(report);
            }
        }
        Err(e) if e.errno == Some(libc::ENOENT) => {}
        Err(e) => return Err(e),
    }
    if reports.len() > 128 {
        return Err(AppError::invalid("too many capability reports"));
    }
    Ok(reports)
}
pub(crate) fn persist_automatic_report(
    config: &Config,
    report: &CapabilityReport,
) -> AppResult<()> {
    let root = RootHandle::open(&config.state_root, RootPolicy::Local)?;
    root.mkdir_all(&ArchivePath::parse(".photo-sorter/capabilities")?)?;
    let staged = ArchivePath::parse(&format!(
        ".photo-sorter/capabilities/automatic-{}.json",
        OperationId::new()
    ))?;
    root.write_new(&staged, &serde_json::to_vec(report)?)?;
    let target = ArchivePath::parse("automatic-nas-qualification.json")?;
    if publish_noreplace(&root, &staged, &target)? == PublishOutcome::Exists {
        root.exchange(&staged, &target)?;
    }
    Ok(())
}
pub fn persist_report(config: &Config, report: &CapabilityReport) -> AppResult<std::path::PathBuf> {
    let root = RootHandle::open(&config.state_root, RootPolicy::Local)?;
    root.mkdir_all(&ArchivePath::parse(".photo-sorter/capabilities")?)?;
    let bytes = serde_json::to_vec(report)?;
    if bytes.len() > 1024 * 1024 {
        return Err(AppError::invalid("capability report exceeds bound"));
    }
    let digest = crate::domain::Digest::of(&bytes);
    let record = ArchivePath::parse(&format!(
        ".photo-sorter/capabilities/{}.json",
        &digest.as_str()[7..]
    ))?;
    match root.write_new(&record, &bytes) {
        Ok(()) => {}
        Err(e) if e.errno == Some(libc::EEXIST) && root.read(&record, 1024 * 1024)? == bytes => {}
        Err(e) => return Err(e),
    }
    let mut reports = load_manual_reports(config)?;
    reports.retain(|old| {
        old.capability != report.capability || old.config_fingerprint != report.config_fingerprint
    });
    reports.push(report.clone());
    let registry = serde_json::to_vec(&reports)?;
    if registry.len() > 1024 * 1024 || reports.len() > 128 {
        return Err(AppError::invalid(
            "capability registry exceeds bound; retain evidence history and retire obsolete active reports",
        ));
    }
    let staged = ArchivePath::parse(&format!(
        ".photo-sorter/capabilities/registry-{}.json",
        crate::domain::OperationId::new()
    ))?;
    if report.capability == "nas-publication" {
        persist_automatic_report(config, report)?;
    }
    root.write_new(&staged, &registry)?;
    let target = ArchivePath::parse("capability-registry.json")?;
    if publish_noreplace(&root, &staged, &target)? == PublishOutcome::Exists {
        root.exchange(&staged, &target)?;
    }
    Ok(root.path().join(record.as_str()))
}

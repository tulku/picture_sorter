use photo_sorter::{
    catalog::Catalog,
    config::Config,
    domain::OperationId,
    fs::{RootHandle, RootPolicy},
    import::{ImportCoordinator, ImportRequest},
    metrics,
};
fn main() -> Result<(), Box<dyn std::error::Error>> {
    let files = std::env::args()
        .nth(1)
        .map(|n| n.parse::<usize>())
        .transpose()?
        .unwrap_or(32);
    if !(1..=10000).contains(&files) {
        return Err("generated fixture count must be 1..10000".into());
    }
    let fixture = tempfile::tempdir()?;
    for root in ["source", "local", "state", "archive"] {
        std::fs::create_dir(fixture.path().join(root))?;
    }
    for n in 0..files {
        let mut bytes = vec![0u8; 65536];
        bytes[..8].copy_from_slice(&(n as u64).to_le_bytes());
        std::fs::write(
            fixture.path().join("source").join(format!("{n:05}.JPG")),
            bytes,
        )?;
    }
    let mut config = Config::new(
        fixture.path().join("local"),
        fixture.path().join("state"),
        fixture.path().join("archive"),
    );
    config.reserve = 0;
    let root = RootHandle::open(&config.state_root, RootPolicy::Local)?;
    let mut engine = ImportCoordinator::new(config, Catalog::open(&root)?);
    let mut results = vec![];
    for pass in ["first", "unchanged"] {
        let before = metrics::snapshot();
        let start = std::time::Instant::now();
        let job = engine
            .prepare(ImportRequest {
                source_path: fixture.path().join("source"),
                strict: false,
                dry_run: false,
                operation_id: OperationId::new(),
            })?
            .job
            .ok_or("import did not produce a job")?;
        for _ in 0..files {
            let report = engine.advance(job)?;
            if report.waiting == 0 {
                break;
            }
        }
        let report = engine.report(job)?;
        results.push(serde_json::json!({"pass":pass,"elapsed_ms":start.elapsed().as_millis(),"import":report,"measured":metrics::snapshot().difference(&before)}));
    }
    println!(
        "{}",
        serde_json::to_string_pretty(
            &serde_json::json!({"fixture":"generated non-photographic bytes; EXIF failure caching is included","files":files,"bytes_per_file":65536,"passes":results})
        )?
    );
    Ok(())
}

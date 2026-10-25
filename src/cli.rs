use crate::{
    catalog::*,
    config::Config,
    domain::*,
    error::{AppError, AppResult},
    fs::*,
    service::{self, protocol::*},
};
use clap::{Parser, Subcommand};
use std::path::PathBuf;
#[derive(Parser)]
#[command(version, about = "Offline photo archive manager")]
pub struct Cli {
    #[arg(long, global = true)]
    pub config: Option<PathBuf>,
    #[arg(long, global = true)]
    pub json: bool,
    #[arg(long, global = true)]
    pub request_id: Option<OperationId>,
    #[arg(long, global = true)]
    pub dry_run: bool,
    #[arg(long, global = true)]
    pub no_wait: bool,
    #[arg(long, global = true, value_enum, default_value = "ingested")]
    pub wait_for: Milestone,
    #[command(subcommand)]
    pub command: CliCommand,
}
#[derive(Subcommand)]
pub enum CliCommand {
    Init {
        #[arg(long)]
        working_root: PathBuf,
        #[arg(long)]
        state_root: PathBuf,
        #[arg(long)]
        archive_root: PathBuf,
    },
    Service {
        #[command(subcommand)]
        command: ServiceCommand,
    },
    Archive {
        #[command(subcommand)]
        command: ArchiveCommand,
    },
    Import {
        source: PathBuf,
        #[arg(long)]
        strict: bool,
    },
    Bootstrap {
        #[arg(long)]
        scope: Option<ArchivePath>,
    },
    Pin {
        target: String,
    },
    Unpin {
        group: GroupId,
    },
    Refresh {
        groups: Vec<GroupId>,
        #[arg(long)]
        editor_closed: bool,
        #[arg(long)]
        stage_only: bool,
    },
    Clean,
    Status,
    Jobs {
        #[command(subcommand)]
        command: JobsCommand,
    },
    Conflicts {
        #[command(subcommand)]
        command: ConflictsCommand,
    },
    Verify {
        #[arg(long)]
        archive: bool,
    },
    Catalog {
        #[command(subcommand)]
        command: CatalogCommand,
    },
    Doctor,
    Qualify {
        #[arg(long)]
        request: PathBuf,
    },
}
#[derive(Subcommand)]
pub enum ServiceCommand {
    Run,
    Start,
    Status,
}
#[derive(Subcommand)]
pub enum ArchiveCommand {
    Init,
    RecoverLock {
        #[arg(long)]
        prior_owner_stopped: bool,
        #[arg(long)]
        all_writers_quiescent: bool,
    },
}
#[derive(Subcommand)]
pub enum JobsCommand {
    Show { job: JobId },
    Wait { job: JobId },
    Cancel { job: JobId },
}
#[derive(Subcommand)]
pub enum ConflictsCommand {
    List,
    Resolve {
        #[arg(long)]
        request: PathBuf,
    },
}
#[derive(Subcommand)]
pub enum CatalogCommand {
    Backup {
        destination: PathBuf,
        #[arg(long)]
        offline: bool,
    },
    Rebuild {
        replacement: PathBuf,
        #[arg(long)]
        offline: bool,
    },
}
pub fn run(cli: Cli) -> AppResult<i32> {
    let path = cli
        .config
        .as_ref()
        .ok_or_else(|| AppError::invalid("an explicit --config profile is required"))?;
    if let CliCommand::Init {
        working_root,
        state_root,
        archive_root,
    } = cli.command
    {
        if cli.dry_run {
            return Ok(0);
        }
        let mut config = Config::new(working_root, state_root, archive_root);
        config.archive_id = Some(ArchiveId::new());
        config.validate()?;
        let parent = path
            .parent()
            .ok_or_else(|| AppError::invalid("profile requires parent directory"))?;
        let root = RootHandle::open(parent, RootPolicy::Local)?;
        let _lock = OwnershipLock::acquire(&root, ".photo-sorter-setup.lock")?;
        ensure_local_directory(&config.state_root)?;
        ensure_local_directory(&config.working_root)?;
        let name = path
            .file_name()
            .and_then(|n| n.to_str())
            .ok_or_else(|| AppError::invalid("invalid profile filename"))?;
        root.write_new(
            &ArchivePath::parse(name)?,
            toml::to_string_pretty(&config)
                .map_err(|_| AppError::invalid("profile cannot be serialized"))?
                .as_bytes(),
        )?;
        return Ok(0);
    }
    let config = Config::load(path)?;
    match &cli.command {
        CliCommand::Service {
            command: ServiceCommand::Run,
        } => {
            service::Service::run(config)?;
            return Ok(0);
        }
        CliCommand::Service {
            command: ServiceCommand::Start,
        } => {
            let child = std::process::Command::new(std::env::current_exe()?)
                .args(["--config"])
                .arg(path)
                .args(["service", "run"])
                .stdin(std::process::Stdio::null())
                .stdout(std::process::Stdio::null())
                .stderr(std::process::Stdio::null())
                .spawn()?;
            let begin = std::time::Instant::now();
            loop {
                if service::Client::connect(&config).is_ok() {
                    return Ok(0);
                }
                if begin.elapsed() > std::time::Duration::from_secs(5) {
                    return Err(AppError::new(
                        crate::error::ErrorCode::Unavailable,
                        format!("service start did not become ready; process {}", child.id()),
                    ));
                }
                std::thread::sleep(std::time::Duration::from_millis(25));
            }
        }
        CliCommand::Catalog {
            command:
                CatalogCommand::Rebuild {
                    replacement,
                    offline: true,
                },
        } => {
            if cli.dry_run {
                return Ok(0);
            }
            let report = CatalogRecovery::rebuild(RebuildRequest {
                state_root: config.state_root.clone(),
                working_root: config.working_root.clone(),
                replacement: replacement.clone(),
                profile: config.profile_id,
                archive: config
                    .archive_id
                    .ok_or_else(|| AppError::invalid("archive identity not bound"))?,
                commits: vec![],
            })?;
            println!("{}", serde_json::to_string(&report)?);
            return Ok(3);
        }
        CliCommand::Catalog {
            command:
                CatalogCommand::Backup {
                    destination,
                    offline: true,
                },
        } => {
            if cli.dry_run {
                return Ok(0);
            }
            service::offline_backup(&config, destination)?;
            return Ok(0);
        }
        _ => {}
    }
    let command = match cli.command {
        CliCommand::Status
        | CliCommand::Service {
            command: ServiceCommand::Status,
        } => Command::Status,
        CliCommand::Import { source, strict } => Command::Import { source, strict },
        CliCommand::Bootstrap { scope } => Command::Bootstrap { scope },
        CliCommand::Pin { target } => Command::Pin { target },
        CliCommand::Unpin { group } => Command::Unpin { group },
        CliCommand::Refresh {
            groups,
            editor_closed,
            stage_only,
        } => {
            if editor_closed && cli.no_wait && !stage_only {
                return Err(AppError::invalid(
                    "applying refresh requires a live foreground connection; remove --no-wait",
                ));
            }
            Command::Refresh {
                groups,
                editor_closed,
                stage_only,
            }
        }
        CliCommand::Clean => Command::Clean,
        CliCommand::Jobs {
            command: JobsCommand::Show { job },
        } => Command::JobShow { job },
        CliCommand::Jobs {
            command: JobsCommand::Wait { job },
        } => Command::JobWait { job },
        CliCommand::Jobs {
            command: JobsCommand::Cancel { job },
        } => Command::JobCancel { job },
        CliCommand::Conflicts {
            command: ConflictsCommand::List,
        } => Command::ConflictsList,
        CliCommand::Conflicts {
            command: ConflictsCommand::Resolve { request },
        } => Command::ConflictResolve {
            request: read_json(&request)?,
        },
        CliCommand::Verify { archive } => Command::Verify { archive },
        CliCommand::Archive {
            command: ArchiveCommand::Init,
        } => Command::ArchiveInit,
        CliCommand::Archive {
            command:
                ArchiveCommand::RecoverLock {
                    prior_owner_stopped,
                    all_writers_quiescent,
                },
        } => Command::RecoverLock {
            prior_owner_stopped,
            all_writers_quiescent,
        },
        CliCommand::Catalog {
            command: CatalogCommand::Backup { destination, .. },
        } => Command::CatalogBackup { destination },
        CliCommand::Catalog {
            command: CatalogCommand::Rebuild { replacement, .. },
        } => Command::CatalogRebuild { replacement },
        CliCommand::Doctor => Command::Doctor,
        CliCommand::Qualify { request } => Command::Qualify {
            request: read_json(&request)?,
        },
        _ => return Err(AppError::invalid("invalid command combination")),
    };
    let response = service::Client::connect(&config)?.send(Request {
        protocol_version: PROTOCOL_VERSION,
        request_id: cli.request_id.unwrap_or_default(),
        command,
        dry_run: cli.dry_run,
        no_wait: cli.no_wait,
        wait_for: cli.wait_for,
    })?;
    let code = response.exit_code();
    if cli.json {
        println!("{}", serde_json::to_string(&response)?);
    } else {
        print_response(&response)?;
    }
    Ok(code)
}
fn print_response(response: &Response) -> AppResult<()> {
    match &response.outcome {
        ResponseOutcome::Accepted { job } => println!("Accepted job {job}"),
        ResponseOutcome::Error { error } => eprintln!("{error}"),
        ResponseOutcome::Job { job, detail } => println!(
            "Job {}: {:?}\n{}",
            job.id,
            job.state,
            serde_json::to_string_pretty(detail)?
        ),
        ResponseOutcome::Data { data } => println!("{}", serde_json::to_string_pretty(data)?),
    }
    Ok(())
}
fn read_json<T: serde::de::DeserializeOwned>(path: &std::path::Path) -> AppResult<T> {
    use std::io::Read;
    let mut bytes = vec![];
    std::fs::File::open(path)?
        .take((MAX_MESSAGE + 1) as u64)
        .read_to_end(&mut bytes)?;
    if bytes.len() > MAX_MESSAGE {
        return Err(AppError::invalid("JSON request exceeds one MiB"));
    }
    Ok(serde_json::from_slice(&bytes)?)
}
fn ensure_local_directory(path: &std::path::Path) -> AppResult<()> {
    crate::config::normalized(path)?;
    let mut ancestor = path;
    loop {
        match RootHandle::open(ancestor, RootPolicy::Local) {
            Ok(root) => {
                if ancestor != path {
                    let suffix = path
                        .strip_prefix(ancestor)
                        .map_err(|_| AppError::invalid("invalid local root"))?
                        .to_str()
                        .ok_or_else(|| AppError::invalid("root must be UTF-8"))?;
                    root.mkdir_all(&ArchivePath::parse(suffix)?)?;
                }
                return Ok(());
            }
            Err(e) if e.errno == Some(libc::ENOENT) => {
                ancestor = ancestor.parent().ok_or(e)?;
            }
            Err(e) => return Err(e),
        }
    }
}

use crate::{
    config::Config,
    error::{AppError, AppResult, ErrorCode},
    immich::*,
    qualification::*,
};
use std::{
    cell::RefCell,
    collections::HashMap,
    sync::mpsc::{self, Receiver, SyncSender, TryRecvError},
    time::{Duration, Instant},
};
#[derive(Debug, Clone, PartialEq, Eq)]
enum Key {
    Asset(uuid::Uuid),
    Server,
    Write(uuid::Uuid),
}
enum Task {
    Asset(uuid::Uuid),
    Server,
    Write(RatingWrite),
}
enum Reply {
    Asset(uuid::Uuid, AppResult<AssetInfo>),
    Server(AppResult<ServerInfo>),
    Write(uuid::Uuid, AppResult<WriteOutcome>),
}
struct Inner {
    send: SyncSender<Task>,
    receive: Receiver<Reply>,
    active: Option<Key>,
    assets: HashMap<uuid::Uuid, (Instant, AppResult<AssetInfo>)>,
    server: Option<(Instant, AppResult<ServerInfo>)>,
    writes: HashMap<uuid::Uuid, AppResult<WriteOutcome>>,
}
pub(crate) struct AsyncImmich {
    origin: url::Url,
    inner: RefCell<Inner>,
    mode: RatingWriteMode,
    config: Config,
}
impl AsyncImmich {
    pub fn new(config: &Config) -> AppResult<Option<Self>> {
        let Some(immich) = config.immich.clone() else {
            return Ok(None);
        };
        let origin = url::Url::parse(&immich.origin)
            .map_err(|_| AppError::invalid("invalid configured Immich origin"))?;
        let mode = match immich.write_mode.as_str() {
            "disabled" => RatingWriteMode::Disabled,
            "best_effort" => RatingWriteMode::BestEffort,
            "conditional" => RatingWriteMode::Conditional,
            _ => return Err(AppError::invalid("unknown Immich write mode")),
        };
        let registry = load_registry(config).unwrap_or_default();
        let (send, commands) = mpsc::sync_channel(1);
        let (results, receive) = mpsc::sync_channel(1);
        let worker_origin = origin.clone();
        let worker_registry = registry.clone();
        let worker_config = config.clone();
        std::thread::spawn(move || {
            let mut client: Option<ImmichClient> = None;
            while let Ok(task) = commands.recv() {
                let configured = if client.is_some() {
                    Ok(())
                } else {
                    match ImmichClient::new(worker_origin.clone(), &immich.key_file) {
                        Ok(new) => {
                            client = Some(new.with_rating_policy(mode, worker_registry.clone()));
                            Ok(())
                        }
                        Err(error) => Err(error),
                    }
                };
                let reply = match task {
                    Task::Asset(asset) => Reply::Asset(
                        asset,
                        configured.and_then(|_| client.as_ref().unwrap().asset(asset)),
                    ),
                    Task::Server => Reply::Server(
                        configured.and_then(|_| client.as_ref().unwrap().server_info()),
                    ),
                    Task::Write(write) => {
                        if let Some(client) = client.as_mut() {
                            client.update_rating_registry(
                                load_registry(&worker_config).unwrap_or_default(),
                            );
                        }
                        Reply::Write(
                            write.asset,
                            configured.and_then(|_| client.as_ref().unwrap().set_rating(write)),
                        )
                    }
                };
                if results.send(reply).is_err() {
                    break;
                }
            }
        });
        Ok(Some(Self {
            origin,
            mode,
            config: config.clone(),
            inner: RefCell::new(Inner {
                send,
                receive,
                active: None,
                assets: HashMap::new(),
                server: None,
                writes: HashMap::new(),
            }),
        }))
    }
    pub fn pump(&self) -> AppResult<()> {
        let mut inner = self.inner.borrow_mut();
        loop {
            match inner.receive.try_recv() {
                Ok(reply) => {
                    match reply {
                        Reply::Asset(asset, result) => {
                            inner.assets.insert(asset, (Instant::now(), result));
                            if inner.assets.len() > 256
                                && let Some(key) =
                                    inner.assets.keys().copied().find(|key| *key != asset)
                            {
                                inner.assets.remove(&key);
                            }
                        }
                        Reply::Server(result) => inner.server = Some((Instant::now(), result)),
                        Reply::Write(asset, result) => {
                            inner.assets.remove(&asset);
                            inner.writes.insert(asset, result);
                        }
                    }
                    inner.active = None;
                }
                Err(TryRecvError::Empty) => break,
                Err(TryRecvError::Disconnected) => {
                    return Err(AppError::new(
                        ErrorCode::Unavailable,
                        "Immich request worker stopped",
                    ));
                }
            }
        }
        Ok(())
    }
    pub fn has_ready_asset(&self, asset: uuid::Uuid) -> bool {
        self.inner
            .borrow()
            .assets
            .get(&asset)
            .is_some_and(|(at, result)| at.elapsed() < Duration::from_secs(30) && result.is_ok())
    }
    pub fn asset_failed(&self, asset: uuid::Uuid) -> bool {
        self.inner
            .borrow()
            .assets
            .get(&asset)
            .is_some_and(|(at, result)| at.elapsed() < Duration::from_secs(30) && result.is_err())
    }
    pub fn in_flight(&self) -> bool {
        self.inner.borrow().active.is_some()
    }
    fn queue(&self, key: Key, task: Task) -> AppResult<()> {
        let mut inner = self.inner.borrow_mut();
        if inner.active.is_some() {
            return Err(pending());
        }
        inner.send.try_send(task).map_err(|_| pending())?;
        inner.active = Some(key);
        Ok(())
    }
    fn server_info(&self) -> AppResult<ServerInfo> {
        self.pump()?;
        if let Some((at, result)) = &self.inner.borrow().server
            && at.elapsed() < Duration::from_secs(60)
        {
            return result.clone();
        }
        self.queue(Key::Server, Task::Server)?;
        Err(pending())
    }
}
fn pending() -> AppError {
    AppError::new(
        ErrorCode::Unavailable,
        "Immich operation is queued or in flight; local work continues",
    )
}
impl ImmichAccess for AsyncImmich {
    fn origin(&self) -> &url::Url {
        &self.origin
    }
    fn asset(&self, asset: uuid::Uuid) -> AppResult<AssetInfo> {
        self.pump()?;
        if self.inner.borrow().active == Some(Key::Write(asset)) {
            return Err(pending());
        }
        if let Some((at, result)) = self.inner.borrow().assets.get(&asset)
            && at.elapsed() < Duration::from_secs(30)
        {
            return result.clone();
        }
        self.queue(Key::Asset(asset), Task::Asset(asset))?;
        Err(pending())
    }
    fn outbound_enabled(&self) -> bool {
        self.mode != RatingWriteMode::Disabled
    }
    fn set_rating(&self, write: RatingWrite) -> AppResult<WriteOutcome> {
        let info = self.server_info()?;
        let decision =
            load_registry(&self.config)
                .unwrap_or_default()
                .allows(OperationKind::RatingWrite {
                    origin: &self.origin,
                    asset: write.asset,
                    version: &info.version,
                });
        if !decision.allowed || self.mode != RatingWriteMode::BestEffort {
            return Err(AppError::new(ErrorCode::Unsupported, decision.reason));
        }
        self.inner.borrow_mut().assets.remove(&write.asset);
        self.queue(Key::Write(write.asset), Task::Write(write))?;
        Ok(WriteOutcome::Uncertain)
    }
    fn write_progress(&self, asset: uuid::Uuid) -> WriteProgress {
        if let Err(error) = self.pump() {
            return WriteProgress::Finished(Err(error));
        }
        let mut inner = self.inner.borrow_mut();
        if let Some(result) = inner.writes.remove(&asset) {
            return WriteProgress::Finished(result);
        }
        if inner.active == Some(Key::Write(asset)) {
            WriteProgress::Pending
        } else {
            WriteProgress::None
        }
    }
}

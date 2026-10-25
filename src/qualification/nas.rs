use crate::{archive::PublicationMode, domain::*, error::AppResult, fs::RootHandle};
pub const NAS_IMPLEMENTATION: &str = concat!("photo-archive-linux-v1/", env!("CARGO_PKG_VERSION"));
pub fn nas_fingerprint(
    root: &RootHandle,
    archive: ArchiveId,
    writer: WriterId,
    mode: &PublicationMode,
    revision: u64,
) -> AppResult<Digest> {
    fingerprint_with_environment(
        root,
        archive,
        writer,
        mode,
        revision,
        &client_environment()?,
    )
}
#[derive(Clone, Serialize)]
struct ClientEnvironment {
    machine: Digest,
    boot: Digest,
    kernel: String,
}
fn client_environment() -> AppResult<ClientEnvironment> {
    let boot = std::fs::read("/proc/sys/kernel/random/boot_id")?;
    let kernel = std::fs::read_to_string("/proc/sys/kernel/osrelease")?
        .trim()
        .to_owned();
    if boot.is_empty() || kernel.is_empty() {
        return Err(AppError::invalid("client boot/kernel identity unavailable"));
    }
    Ok(ClientEnvironment {
        machine: machine()?,
        boot: Digest::of(&boot),
        kernel,
    })
}
fn fingerprint_with_environment(
    root: &RootHandle,
    archive: ArchiveId,
    writer: WriterId,
    mode: &PublicationMode,
    revision: u64,
    environment: &ClientEnvironment,
) -> AppResult<Digest> {
    Ok(Digest::of(&serde_json::to_vec(&(
        NAS_IMPLEMENTATION,
        root.path(),
        root.mount_identity()?,
        archive,
        writer,
        mode,
        revision,
        environment,
    ))?))
}

use super::{CapabilityReport, ProbeOutcome, ProbeResult, ProbeScope};
use crate::{
    error::{AppError, ErrorCode},
    fs::*,
};
use serde::{Deserialize, Serialize};
use std::{
    io::{Read, Write},
    path::PathBuf,
};
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum NasProbePhase {
    Prepare,
    Peer,
    Verify,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct NasProbeRequest {
    pub archive_root: PathBuf,
    pub archive: ArchiveId,
    pub writer: WriterId,
    pub mode: PublicationMode,
    pub mode_revision: u64,
    pub scratch_root: PathBuf,
    pub session: OperationId,
    pub phase: NasProbePhase,
    pub local_test_backend: bool,
    pub prior_peer_stopped: bool,
    pub all_writers_quiescent: bool,
    pub disconnect_observed: bool,
    pub interrupt_bytes: u64,
}
#[derive(Serialize, Deserialize)]
struct ProbeSession {
    schema_version: u32,
    archive: ArchiveId,
    writer: WriterId,
    machine: Digest,
    boot: Digest,
    fingerprint: Digest,
    nonce: String,
    results: Vec<ProbeResult>,
}
#[derive(Serialize, Deserialize)]
struct PeerEvidence {
    schema_version: u32,
    archive: ArchiveId,
    writer: WriterId,
    machine: Digest,
    boot: Digest,
    nonce: String,
    bytes: u64,
    digest: Digest,
}
fn p(name: &str) -> AppResult<ArchivePath> {
    ArchivePath::parse(name)
}
fn open_root(path: &std::path::Path, test: bool) -> AppResult<RootHandle> {
    use std::os::unix::fs::MetadataExt;
    if test {
        RootHandle::open(path, RootPolicy::TestArchive)
    } else {
        RootHandle::open(
            path,
            RootPolicy::Archive {
                expected_device: std::fs::metadata(path)?.dev(),
            },
        )
    }
}
fn machine() -> AppResult<Digest> {
    let bytes = std::fs::read("/etc/machine-id")
        .or_else(|_| std::fs::read("/proc/sys/kernel/random/boot_id"))?;
    if bytes.is_empty() {
        return Err(AppError::invalid("machine identity is unavailable"));
    }
    Ok(Digest::of(&bytes))
}
fn retain(root: &RootHandle, name: &str, bytes: &[u8]) -> AppResult<()> {
    let path = p(name)?;
    match root.write_new(&path, bytes) {
        Ok(()) => Ok(()),
        Err(e)
            if e.errno == Some(libc::EEXIST) && root.read(&path, bytes.len() as u64)? == bytes =>
        {
            Ok(())
        }
        Err(e) => Err(e),
    }
}
fn result(name: &str, test: impl FnOnce() -> AppResult<()>) -> ProbeResult {
    match test() {
        Ok(()) => ProbeResult {
            name: name.into(),
            outcome: ProbeOutcome::Pass,
            detail: "scratch operation completed and bytes reopened exactly".into(),
        },
        Err(error) => ProbeResult {
            name: name.into(),
            outcome: ProbeOutcome::Fail,
            detail: error.to_string(),
        },
    }
}
fn verify(root: &RootHandle, name: &str, bytes: &[u8]) -> AppResult<()> {
    if root.read(&p(name)?, bytes.len() as u64)? != bytes {
        return Err(AppError::integrity("scratch bytes differ"));
    }
    Ok(())
}
fn basic(root: &RootHandle) -> Vec<ProbeResult> {
    vec![
        result("exclusive_init", || {
            retain(root, "exclusive.json", b"owned probe")?;
            if root
                .write_new(&p("exclusive.json")?, b"replacement")
                .is_ok()
            {
                return Err(AppError::integrity("exclusive creation replaced bytes"));
            }
            verify(root, "exclusive.json", b"owned probe")
        }),
        result("no_replace", || {
            retain(root, "published", b"original")?;
            retain(root, "competing", b"different")?;
            if publish_noreplace(root, &p("competing")?, &p("published")?)?
                != PublishOutcome::Exists
            {
                return Err(AppError::integrity(
                    "no-replace publication overwrote target",
                ));
            }
            verify(root, "published", b"original")?;
            verify(root, "competing", b"different")
        }),
        result("rename_exchange", || {
            retain(root, "exchange-a", b"a")?;
            retain(root, "exchange-b", b"b")?;
            root.exchange(&p("exchange-a")?, &p("exchange-b")?)?;
            verify(root, "exchange-a", b"b")?;
            verify(root, "exchange-b", b"a")
        }),
        result("lock_exclusion", || {
            root.mkdir_exclusive(&p("exclusive-lock")?)?;
            let second = root.mkdir_exclusive(&p("exclusive-lock")?);
            if !matches!(second,Err(error) if error.errno==Some(libc::EEXIST)) {
                return Err(AppError::integrity(
                    "exclusive lock did not exclude second owner",
                ));
            }
            root.unlink(&p("exclusive-lock")?, true)
        }),
        result("flush_reopen", || {
            retain(root, "flushed", b"durable probe bytes")?;
            root.sync()?;
            let reopened = open_root(root.path(), root.is_test_archive())?;
            verify(&reopened, "flushed", b"durable probe bytes")
        }),
    ]
}
fn block(nonce: &str) -> Vec<u8> {
    let seed = blake3::hash(nonce.as_bytes());
    (0..128 * 1024).map(|n| seed.as_bytes()[n % 32]).collect()
}
pub fn qualify_nas(request: NasProbeRequest) -> AppResult<CapabilityReport> {
    let archive_path = crate::config::normalized(&request.archive_root)?;
    let scratch_path = crate::config::normalized(&request.scratch_root)?;
    if archive_path.starts_with(&scratch_path) || scratch_path.starts_with(&archive_path) {
        return Err(AppError::invalid(
            "qualification requires an explicit scratch directory outside the archive tree",
        ));
    }
    if request.interrupt_bytes == 0 || request.interrupt_bytes > 1024 * 1024 * 1024 {
        return Err(AppError::invalid(
            "interrupted-write probe must be 1 byte..1 GiB",
        ));
    }
    let archive = open_root(&archive_path, request.local_test_backend)?;
    let scratch = open_root(&scratch_path, request.local_test_backend)?;
    crate::fs::ensure_disjoint_roots(&archive, &scratch)?;
    if archive.mount_number() != scratch.mount_number() || archive.device() != scratch.device() {
        return Err(AppError::invalid(
            "scratch must use the same mounted filesystem as the target archive",
        ));
    }
    let fingerprint = nas_fingerprint(
        &archive,
        request.archive,
        request.writer,
        &request.mode,
        request.mode_revision,
    )?;
    let environment = client_environment()?;
    let machine = environment.machine.clone();
    let boot = environment.boot.clone();
    let is_peer = matches!(&request.phase, NasProbePhase::Peer);
    let dir = format!(".photo-sorter-qualification/{}", request.session);
    let session_path = p(&dir)?;
    let mut fresh = false;
    if matches!(request.phase, NasProbePhase::Prepare) {
        scratch.mkdir_all(&p(".photo-sorter-qualification")?)?;
        match scratch.mkdir_exclusive(&session_path) {
            Ok(()) => fresh = true,
            Err(e) if e.errno == Some(libc::EEXIST) => {}
            Err(e) => return Err(e),
        }
    }
    let root = open_root(&scratch.path().join(&dir), request.local_test_backend)?;
    let mut session: ProbeSession = match root.read(&p("session.json")?, 1024 * 1024) {
        Ok(bytes) => serde_json::from_slice(&bytes)?,
        Err(e)
            if e.errno == Some(libc::ENOENT) && matches!(request.phase, NasProbePhase::Prepare) =>
        {
            if !fresh {
                return Err(AppError::new(
                    ErrorCode::Conflict,
                    "unmarked or interrupted scratch session is retained; use a new session identity",
                ));
            }
            root.write_new(&p("intent.json")?,&serde_json::to_vec(&serde_json::json!({"schema_version":1,"archive":request.archive,"writer":request.writer,"machine":machine,"fingerprint":fingerprint}))?)?;
            let session = ProbeSession {
                schema_version: 1,
                archive: request.archive,
                writer: request.writer,
                machine: machine.clone(),
                boot: boot.clone(),
                fingerprint: fingerprint.clone(),
                nonce: format!("{}:{}", request.session, OperationId::new()),
                results: basic(&root),
            };
            root.write_new(&p("session.json")?, &serde_json::to_vec(&session)?)?;
            session
        }
        Err(e) => return Err(e),
    };
    if session.schema_version != 1 || session.archive != request.archive {
        return Err(AppError::integrity(
            "scratch session identity/schema differs",
        ));
    }
    let mut scope = ProbeScope::SingleClient;
    let mut peer_outcome = ProbeOutcome::Unrun;
    let mut disconnect_outcome = ProbeOutcome::Unrun;
    let mut peer_detail =
        "run the peer phase on a distinct NAS client with a distinct writer identity".to_owned();
    let mut disconnect_detail = "disconnect/stop/recovery observation has not run".to_owned();
    'probe: {
        match request.phase {
            NasProbePhase::Prepare => {
                if session.writer != request.writer
                    || session.machine != machine
                    || session.boot != boot
                    || session.fingerprint != fingerprint
                {
                    return Err(AppError::new(
                        ErrorCode::Conflict,
                        "scratch session belongs to another qualification configuration",
                    ));
                }
                retain(&root, "handoff-owner", session.nonce.as_bytes())?;
            }
            NasProbePhase::Peer => {
                verify(&root, "handoff-owner", session.nonce.as_bytes())?;
                if session.machine == machine
                    || session.boot == boot
                    || session.writer == request.writer
                {
                    peer_outcome = ProbeOutcome::Inconclusive;
                    peer_detail =
                    "same machine, kernel boot or copied writer identity cannot qualify multi-client behavior"
                        .into();
                } else {
                    let bytes = block(&session.nonce);
                    let mut expected = blake3::Hasher::new();
                    let mut remaining = request.interrupt_bytes;
                    while remaining > 0 {
                        let n = remaining.min(bytes.len() as u64) as usize;
                        expected.update(&bytes[..n]);
                        remaining -= n as u64;
                    }
                    let peer = PeerEvidence {
                        schema_version: 1,
                        archive: request.archive,
                        writer: request.writer,
                        machine: machine.clone(),
                        boot: boot.clone(),
                        nonce: session.nonce.clone(),
                        bytes: request.interrupt_bytes,
                        digest: Digest::parse(&format!("blake3:{}", expected.finalize().to_hex()))?,
                    };
                    if available_bytes(&root)? < request.interrupt_bytes.saturating_add(1024 * 1024)
                    {
                        return Err(AppError::new(
                            ErrorCode::Capacity,
                            "scratch filesystem does not have probe headroom",
                        ));
                    }
                    root.write_new(&p("peer.json")?, &serde_json::to_vec(&peer)?)?;
                    retain(&root, "handoff-peer", session.nonce.as_bytes())?;
                    root.mkdir_exclusive(&p("disconnect-lock")?)?;
                    root.write_new(
                        &p("disconnect-lock/owner.json")?,
                        &serde_json::to_vec(&peer)?,
                    )?;
                    let mut payload = root.create_new(&p("interrupted-payload")?)?;
                    let mut remaining = request.interrupt_bytes;
                    while remaining > 0 {
                        let n = remaining.min(bytes.len() as u64) as usize;
                        payload.write_all(&bytes[..n])?;
                        payload.sync_all()?;
                        remaining -= n as u64;
                    }
                    root.write_new(&p("peer-write-complete.json")?, &serde_json::to_vec(&peer)?)?;
                    root.sync()?;
                    peer_outcome = ProbeOutcome::Pass;
                    peer_detail="distinct client observed the fresh owner nonce and published its own immutable receipt".into();
                }
            }
            NasProbePhase::Verify => {
                if session.machine != machine
                    || session.writer != request.writer
                    || session.fingerprint != fingerprint
                {
                    return Err(AppError::new(
                        ErrorCode::Conflict,
                        "verification must run on the preparing client/configuration",
                    ));
                }
                let peer_bytes = match root.read(&p("peer.json")?, 65536) {
                    Ok(bytes) => bytes,
                    Err(e) if e.errno == Some(libc::ENOENT) => break 'probe,
                    Err(e) => return Err(e),
                };
                let peer: PeerEvidence = serde_json::from_slice(&peer_bytes)?;
                if peer.schema_version != 1
                    || peer.archive != request.archive
                    || peer.machine == machine
                    || peer.boot == boot
                    || peer.writer == request.writer
                    || peer.nonce != session.nonce
                {
                    return Err(AppError::integrity(
                        "peer receipt does not prove distinct-client handoff",
                    ));
                }
                verify(&root, "handoff-peer", session.nonce.as_bytes())?;
                peer_outcome = ProbeOutcome::Pass;
                peer_detail =
                    "preparing client reopened the distinct peer's fresh nonce receipt".into();
                if request.prior_peer_stopped
                    && request.all_writers_quiescent
                    && request.disconnect_observed
                {
                    let owner: PeerEvidence = serde_json::from_slice(
                        &root.read(&p("disconnect-lock/owner.json")?, 65536)?,
                    )?;
                    if owner.nonce != peer.nonce
                        || owner.machine != peer.machine
                        || owner.boot != peer.boot
                    {
                        return Err(AppError::integrity("disconnect lock owner differs"));
                    }
                    let expected = block(&session.nonce);
                    let mut payload = root.open_relative(
                        &p("interrupted-payload")?,
                        libc::O_RDONLY | libc::O_NONBLOCK,
                        0,
                    )?;
                    let metadata = payload.metadata()?;
                    if !metadata.is_file() {
                        return Err(AppError::integrity("interrupted payload is not regular"));
                    }
                    let size = metadata.len();
                    if size > peer.bytes {
                        return Err(AppError::integrity(
                            "interrupted scratch payload exceeds owned intent",
                        ));
                    }
                    let mut position = 0usize;
                    let mut buffer = [0; 128 * 1024];
                    loop {
                        let n = payload.read(&mut buffer)?;
                        if n == 0 {
                            break;
                        }
                        for byte in &buffer[..n] {
                            if *byte != expected[position % expected.len()] {
                                return Err(AppError::integrity(
                                    "interrupted scratch prefix differs",
                                ));
                            }
                            position += 1;
                        }
                    }
                    let retired = p(&format!("retired-disconnect-{}", OperationId::new()))?;
                    publish_noreplace(&root, &p("disconnect-lock")?, &retired)?;
                    root.mkdir_exclusive(&p("recovered-lock")?)?;
                    root.unlink(&p("recovered-lock")?, true)?;
                    root.sync()?;
                    disconnect_outcome = ProbeOutcome::Pass;
                    disconnect_detail="operator-attested disconnect and positive peer shutdown; owned prefix preserved and scratch lock recovery verified".into();
                    scope = ProbeScope::TwoClient;
                }
            }
        }
    }
    if is_peer {
        for result in &mut session.results {
            result.outcome = ProbeOutcome::Unrun;
            result.detail="Owner checks do not certify this peer; run a separate prepare/verify session on this client".into();
        }
    }
    session.results.push(ProbeResult {
        name: "peer_handoff".into(),
        outcome: peer_outcome,
        detail: peer_detail,
    });
    session.results.push(ProbeResult {
        name: "disconnect_recovery".into(),
        outcome: disconnect_outcome,
        detail: disconnect_detail,
    });
    let report=CapabilityReport {schema_version:1,capability:"nas-publication".into(),backend_or_application:if request.local_test_backend {"local-test"}else {"linux-filesystem"}.into(),tested_version:NAS_IMPLEMENTATION.into(),mount_identity:Some(archive.mount_identity()?),config_fingerprint:fingerprint,scope,results:session.results,limitations:vec![format!("Client kernel: {}. Evidence is bound to this client boot; each writer requires its own prepare/verify session.",environment.kernel),"Flush/reopen does not simulate NAS power loss or certify storage hardware durability.".into(),"Disconnect evidence includes explicit operator attestation and requires positive prior-writer shutdown; elapsed time never authorizes takeover.".into(),"Artifacts are retained only inside this explicitly requested scratch session; no original/archive object is repaired or removed.".into()]};
    root.write_new(
        &p(&format!("report-{}.json", OperationId::new()))?,
        &serde_json::to_vec(&report)?,
    )?;
    Ok(report)
}
#[cfg(test)]
mod environment_tests {
    use super::*;
    #[test]
    fn copied_mount_and_writer_evidence_is_invalid_on_another_boot_or_kernel() {
        let temp = tempfile::tempdir().unwrap();
        let root = RootHandle::open(temp.path(), RootPolicy::TestArchive).unwrap();
        let archive = ArchiveId::new();
        let writer = WriterId::new();
        let env = ClientEnvironment {
            machine: Digest::of(b"client"),
            boot: Digest::of(b"boot-one"),
            kernel: "kernel-one".into(),
        };
        let first = fingerprint_with_environment(
            &root,
            archive,
            writer,
            &PublicationMode::SingleWriter,
            1,
            &env,
        )
        .unwrap();
        let reboot = ClientEnvironment {
            boot: Digest::of(b"boot-two"),
            ..env.clone()
        };
        let upgrade = ClientEnvironment {
            kernel: "kernel-two".into(),
            ..env
        };
        assert_ne!(
            first,
            fingerprint_with_environment(
                &root,
                archive,
                writer,
                &PublicationMode::SingleWriter,
                1,
                &reboot
            )
            .unwrap()
        );
        assert_ne!(
            first,
            fingerprint_with_environment(
                &root,
                archive,
                writer,
                &PublicationMode::SingleWriter,
                1,
                &upgrade
            )
            .unwrap()
        );
    }
}

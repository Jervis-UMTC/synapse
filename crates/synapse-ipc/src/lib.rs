//! Authenticated local IPC for Synapse.

use std::{
    error::Error,
    fmt, fs,
    fs::{File, OpenOptions},
    io::{self, Read, Write},
    path::{Path, PathBuf},
    process,
    sync::atomic::{AtomicU64, Ordering},
    thread,
    time::{Duration, Instant},
};

use interprocess::local_socket::{
    prelude::*, ConnectOptions, GenericNamespaced, ListenerOptions, Stream,
};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use synapse_core::knowledge::{KnowledgeRecord, KnowledgeRelation};
use synapse_store::{FileStore, KnowledgeHit, KnowledgeQuery, KnowledgeStatus};
use sysinfo::{Pid, ProcessRefreshKind, ProcessesToUpdate, System, UpdateKind};

const IPC_PROTOCOL_VERSION: u8 = 1;
const TRUST_DIRECTORY: &str = "ipc-trust-v1";
const MAX_TRUST_ENTRY_BYTES: usize = 1024;
const MAX_TRUST_ENTRIES: usize = 64;
const MAX_EXECUTABLE_BYTES: u64 = 512 * 1024 * 1024;
const MAX_FRAME_BYTES: usize = 12 * 1024 * 1024;
const IO_TIMEOUT: Duration = Duration::from_secs(3);
const CONNECT_ATTEMPTS: usize = 50;
const CONNECT_RETRY_DELAY: Duration = Duration::from_millis(20);
static NEXT_TEMP_FILE: AtomicU64 = AtomicU64::new(0);

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TrustedExecutable {
    pub version: u8,
    pub client_id: String,
    pub executable_sha256: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "op", rename_all = "snake_case")]
pub enum IpcRequest {
    Ping,
    InsertRecord { record: KnowledgeRecord },
    InsertRelation { relation: KnowledgeRelation },
    Get { id: String },
    Query { query: KnowledgeQuery },
    Status { id: String },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "result", rename_all = "snake_case")]
pub enum IpcValue {
    Pong,
    Record { record: Option<KnowledgeRecord> },
    Hits { hits: Vec<KnowledgeHit> },
    Status { status: Option<KnowledgeStatus> },
    InsertedRecord { record: KnowledgeRecord },
    InsertedRelation { relation: KnowledgeRelation },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "status", rename_all = "snake_case")]
pub enum IpcResponse {
    Ok { value: IpcValue },
    Error { message: String },
}

#[derive(Debug)]
pub enum IpcError {
    Io(io::Error),
    Serialization(String),
    FrameTooLarge {
        max_bytes: usize,
    },
    InvalidTrustEntry {
        reason: String,
    },
    TrustEntryAlreadyExists {
        client_id: String,
    },
    TrustCapacityExceeded {
        max_entries: usize,
    },
    ClientNotAuthorized {
        client_id: String,
    },
    PeerProcessIdUnavailable,
    PeerProcessUnavailable {
        pid: u32,
    },
    PeerExecutableUnavailable {
        pid: u32,
    },
    PeerIdentityChanged,
    ExecutableTooLarge {
        max_bytes: u64,
    },
    UntrustedPeer,
    AmbiguousPeerIdentity,
    SourceIdentityMismatch {
        claimed: String,
        authenticated: String,
    },
    Remote(String),
}

impl fmt::Display for IpcError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Io(error) => write!(formatter, "IPC I/O error: {error}"),
            Self::Serialization(reason) => write!(formatter, "IPC serialization error: {reason}"),
            Self::FrameTooLarge { max_bytes } => {
                write!(formatter, "IPC frame exceeds the limit of {max_bytes} bytes")
            }
            Self::InvalidTrustEntry { reason } => {
                write!(formatter, "IPC trust entry is invalid: {reason}")
            }
            Self::TrustEntryAlreadyExists { client_id } => {
                write!(formatter, "IPC trust for client '{client_id}' already exists")
            }
            Self::TrustCapacityExceeded { max_entries } => write!(
                formatter,
                "IPC trust store exceeds its bounded capacity of {max_entries} entries"
            ),
            Self::ClientNotAuthorized { client_id } => write!(
                formatter,
                "authorization policy contains no write-capable client '{client_id}'"
            ),
            Self::PeerProcessIdUnavailable => {
                write!(formatter, "operating system did not provide an IPC peer process id")
            }
            Self::PeerProcessUnavailable { pid } => {
                write!(formatter, "IPC peer process {pid} is no longer available")
            }
            Self::PeerExecutableUnavailable { pid } => write!(
                formatter,
                "cannot resolve the executable for IPC peer process {pid}"
            ),
            Self::PeerIdentityChanged => {
                write!(formatter, "IPC peer process identity changed during authentication")
            }
            Self::ExecutableTooLarge { max_bytes } => write!(
                formatter,
                "executable exceeds the IPC identity hashing limit of {max_bytes} bytes"
            ),
            Self::UntrustedPeer => write!(formatter, "IPC peer executable is not trusted"),
            Self::AmbiguousPeerIdentity => {
                write!(formatter, "IPC peer executable maps to more than one client identity")
            }
            Self::SourceIdentityMismatch {
                claimed,
                authenticated,
            } => write!(
                formatter,
                "claimed source '{claimed}' does not match authenticated IPC client '{authenticated}'"
            ),
            Self::Remote(message) => write!(formatter, "IPC request failed: {message}"),
        }
    }
}

impl Error for IpcError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::Io(error) => Some(error),
            _ => None,
        }
    }
}

impl From<io::Error> for IpcError {
    fn from(error: io::Error) -> Self {
        Self::Io(error)
    }
}

pub fn endpoint_name(store_root: &Path) -> Result<String, IpcError> {
    let absolute = if store_root.is_absolute() {
        store_root.to_path_buf()
    } else {
        std::env::current_dir()?.join(store_root)
    };
    let mut hasher = Sha256::new();
    hasher.update(absolute.to_string_lossy().as_bytes());
    let digest = hasher.finalize();
    Ok(format!("synapse-v1-{}", hex_lower(&digest[..16])))
}

pub fn trust_executable(
    store: &FileStore,
    client_id: &str,
    executable: &Path,
) -> Result<TrustedExecutable, IpcError> {
    let policy = store
        .authorization_policy()
        .map_err(|error| IpcError::Remote(error.to_string()))?;
    if !policy
        .clients
        .iter()
        .any(|client| client.id == client_id && (client.write_records || client.write_relations))
    {
        return Err(IpcError::ClientNotAuthorized {
            client_id: client_id.to_owned(),
        });
    }

    let fingerprint = executable_fingerprint(executable)?;
    let entry = TrustedExecutable {
        version: IPC_PROTOCOL_VERSION,
        client_id: client_id.to_owned(),
        executable_sha256: fingerprint,
    };
    validate_trust_entry(&entry)?;

    let directory = store.root().join(TRUST_DIRECTORY);
    create_private_dir_all(&directory)?;
    let count = trust_entry_paths(&directory)?.len();
    if count >= MAX_TRUST_ENTRIES {
        return Err(IpcError::TrustCapacityExceeded {
            max_entries: MAX_TRUST_ENTRIES,
        });
    }

    let serialized =
        serde_json::to_vec(&entry).map_err(|error| IpcError::Serialization(error.to_string()))?;
    if serialized.len() > MAX_TRUST_ENTRY_BYTES {
        return Err(IpcError::InvalidTrustEntry {
            reason: "serialized trust entry exceeds its storage bound".to_owned(),
        });
    }

    let final_path = directory.join(trust_file_name(client_id));
    let (temporary_path, mut temporary_file) = create_temporary_file(&directory)?;
    let result = (|| -> Result<(), IpcError> {
        temporary_file.write_all(&serialized)?;
        temporary_file.sync_all()?;
        drop(temporary_file);
        match fs::hard_link(&temporary_path, &final_path) {
            Ok(()) => Ok(()),
            Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {
                Err(IpcError::TrustEntryAlreadyExists {
                    client_id: client_id.to_owned(),
                })
            }
            Err(error) => Err(IpcError::Io(error)),
        }
    })();
    let _ = fs::remove_file(&temporary_path);
    result?;
    Ok(entry)
}

pub fn executable_fingerprint(path: &Path) -> Result<String, IpcError> {
    let file = File::open(path)?;
    let metadata = file.metadata()?;
    if metadata.len() > MAX_EXECUTABLE_BYTES {
        return Err(IpcError::ExecutableTooLarge {
            max_bytes: MAX_EXECUTABLE_BYTES,
        });
    }

    let mut file = file.take(MAX_EXECUTABLE_BYTES + 1);
    let mut hasher = Sha256::new();
    let mut buffer = [0u8; 64 * 1024];
    let mut total = 0u64;
    loop {
        let read = file.read(&mut buffer)?;
        if read == 0 {
            break;
        }
        total += read as u64;
        if total > MAX_EXECUTABLE_BYTES {
            return Err(IpcError::ExecutableTooLarge {
                max_bytes: MAX_EXECUTABLE_BYTES,
            });
        }
        hasher.update(&buffer[..read]);
    }
    Ok(hex_lower(&hasher.finalize()))
}

pub fn serve(store: FileStore) -> Result<(), IpcError> {
    serve_connections(store, None)
}

pub fn serve_once(store: FileStore) -> Result<(), IpcError> {
    serve_connections(store, Some(1))
}

pub fn request(store_root: &Path, request: &IpcRequest) -> Result<IpcValue, IpcError> {
    let endpoint = endpoint_name(store_root)?;
    let name = endpoint
        .as_str()
        .to_ns_name::<GenericNamespaced>()
        .map_err(IpcError::Io)?;

    let mut last_error = None;
    for attempt in 0..CONNECT_ATTEMPTS {
        match ConnectOptions::new().name(name.clone()).connect_sync() {
            Ok(mut stream) => {
                stream.set_nonblocking(true)?;
                write_frame(&mut stream, request)?;
                let response: IpcResponse = read_frame(&mut stream)?;
                return match response {
                    IpcResponse::Ok { value } => Ok(value),
                    IpcResponse::Error { message } => Err(IpcError::Remote(message)),
                };
            }
            Err(error) => {
                last_error = Some(error);
                if attempt + 1 < CONNECT_ATTEMPTS {
                    thread::sleep(CONNECT_RETRY_DELAY);
                }
            }
        }
    }

    Err(IpcError::Io(last_error.unwrap_or_else(|| {
        io::Error::new(
            io::ErrorKind::NotFound,
            "Synapse IPC endpoint is unavailable",
        )
    })))
}

fn serve_connections(store: FileStore, limit: Option<usize>) -> Result<(), IpcError> {
    let endpoint = endpoint_name(store.root())?;
    let name = endpoint
        .as_str()
        .to_ns_name::<GenericNamespaced>()
        .map_err(IpcError::Io)?;
    let listener = ListenerOptions::new().name(name).create_sync()?;

    let mut served = 0usize;
    loop {
        let mut stream = listener.accept()?;
        stream.set_nonblocking(true)?;
        let response = match read_frame::<IpcRequest>(&mut stream) {
            Ok(request) => match handle_request(&store, &stream, request) {
                Ok(value) => IpcResponse::Ok { value },
                Err(error) => IpcResponse::Error {
                    message: error.to_string(),
                },
            },
            Err(error) => IpcResponse::Error {
                message: error.to_string(),
            },
        };
        write_frame(&mut stream, &response)?;

        served += 1;
        if limit.is_some_and(|limit| served >= limit) {
            return Ok(());
        }
    }
}

fn handle_request(
    store: &FileStore,
    stream: &Stream,
    request: IpcRequest,
) -> Result<IpcValue, IpcError> {
    match request {
        IpcRequest::Ping => Ok(IpcValue::Pong),
        IpcRequest::InsertRecord { record } => {
            let authenticated = authenticate_peer(store, stream)?;
            require_source(&record.source, &authenticated)?;
            store
                .insert(&record)
                .map_err(|error| IpcError::Remote(error.to_string()))?;
            Ok(IpcValue::InsertedRecord { record })
        }
        IpcRequest::InsertRelation { relation } => {
            let authenticated = authenticate_peer(store, stream)?;
            require_source(&relation.source, &authenticated)?;
            store
                .insert_relation(&relation)
                .map_err(|error| IpcError::Remote(error.to_string()))?;
            Ok(IpcValue::InsertedRelation { relation })
        }
        IpcRequest::Get { id } => {
            let record = store
                .get(&id)
                .map_err(|error| IpcError::Remote(error.to_string()))?;
            Ok(IpcValue::Record { record })
        }
        IpcRequest::Query { query } => {
            let hits = store
                .query(&query)
                .map_err(|error| IpcError::Remote(error.to_string()))?;
            Ok(IpcValue::Hits { hits })
        }
        IpcRequest::Status { id } => {
            let status = store
                .status(&id)
                .map_err(|error| IpcError::Remote(error.to_string()))?;
            Ok(IpcValue::Status { status })
        }
    }
}

fn require_source(claimed: &str, authenticated: &str) -> Result<(), IpcError> {
    if claimed == authenticated {
        return Ok(());
    }
    Err(IpcError::SourceIdentityMismatch {
        claimed: claimed.to_owned(),
        authenticated: authenticated.to_owned(),
    })
}

fn authenticate_peer(store: &FileStore, stream: &Stream) -> Result<String, IpcError> {
    let creds = stream.peer_creds()?;
    let pid = creds.pid().ok_or(IpcError::PeerProcessIdUnavailable)?;
    let fingerprint = fingerprint_process(pid)?;
    trusted_client_for_fingerprint(store, &fingerprint)
}

fn fingerprint_process(pid: u32) -> Result<String, IpcError> {
    let sys_pid = Pid::from_u32(pid);
    let mut system = System::new();
    let refresh_kind = ProcessRefreshKind::nothing().with_exe(UpdateKind::Always);
    system.refresh_processes_specifics(ProcessesToUpdate::Some(&[sys_pid]), true, refresh_kind);
    let process = system
        .process(sys_pid)
        .ok_or(IpcError::PeerProcessUnavailable { pid })?;
    let start_time = process.start_time();
    let executable = process
        .exe()
        .filter(|path| !path.as_os_str().is_empty())
        .ok_or(IpcError::PeerExecutableUnavailable { pid })?
        .to_path_buf();
    let fingerprint = executable_fingerprint(&executable)?;

    system.refresh_processes_specifics(ProcessesToUpdate::Some(&[sys_pid]), true, refresh_kind);
    let process = system
        .process(sys_pid)
        .ok_or(IpcError::PeerProcessUnavailable { pid })?;
    let executable_after = process
        .exe()
        .filter(|path| !path.as_os_str().is_empty())
        .ok_or(IpcError::PeerExecutableUnavailable { pid })?;
    if process.start_time() != start_time || executable_after != executable {
        return Err(IpcError::PeerIdentityChanged);
    }

    Ok(fingerprint)
}

fn trusted_client_for_fingerprint(
    store: &FileStore,
    fingerprint: &str,
) -> Result<String, IpcError> {
    let directory = store.root().join(TRUST_DIRECTORY);
    let paths = match trust_entry_paths(&directory) {
        Ok(paths) => paths,
        Err(IpcError::Io(error)) if error.kind() == io::ErrorKind::NotFound => {
            return Err(IpcError::UntrustedPeer)
        }
        Err(error) => return Err(error),
    };
    if paths.len() > MAX_TRUST_ENTRIES {
        return Err(IpcError::TrustCapacityExceeded {
            max_entries: MAX_TRUST_ENTRIES,
        });
    }

    let mut matched = None;
    for path in paths {
        let entry = read_trust_entry(&path)?;
        if entry.executable_sha256 == fingerprint {
            if matched.is_some() {
                return Err(IpcError::AmbiguousPeerIdentity);
            }
            matched = Some(entry.client_id);
        }
    }
    matched.ok_or(IpcError::UntrustedPeer)
}

fn trust_entry_paths(directory: &Path) -> Result<Vec<PathBuf>, IpcError> {
    let entries = fs::read_dir(directory)?;
    let mut paths = Vec::new();
    for entry in entries {
        let entry = entry?;
        if entry.file_type()?.is_file()
            && entry.path().extension().and_then(|value| value.to_str()) == Some("json")
        {
            paths.push(entry.path());
            if paths.len() > MAX_TRUST_ENTRIES {
                return Err(IpcError::TrustCapacityExceeded {
                    max_entries: MAX_TRUST_ENTRIES,
                });
            }
        }
    }
    paths.sort();
    Ok(paths)
}

fn read_trust_entry(path: &Path) -> Result<TrustedExecutable, IpcError> {
    let file = File::open(path)?;
    if file.metadata()?.len() > MAX_TRUST_ENTRY_BYTES as u64 {
        return Err(IpcError::InvalidTrustEntry {
            reason: "stored trust entry exceeds its size bound".to_owned(),
        });
    }
    let mut bytes = Vec::new();
    file.take((MAX_TRUST_ENTRY_BYTES + 1) as u64)
        .read_to_end(&mut bytes)?;
    if bytes.len() > MAX_TRUST_ENTRY_BYTES {
        return Err(IpcError::InvalidTrustEntry {
            reason: "stored trust entry exceeds its size bound".to_owned(),
        });
    }
    let entry: TrustedExecutable =
        serde_json::from_slice(&bytes).map_err(|error| IpcError::InvalidTrustEntry {
            reason: error.to_string(),
        })?;
    validate_trust_entry(&entry)?;
    Ok(entry)
}

fn validate_trust_entry(entry: &TrustedExecutable) -> Result<(), IpcError> {
    if entry.version != IPC_PROTOCOL_VERSION {
        return Err(IpcError::InvalidTrustEntry {
            reason: format!(
                "unsupported trust entry version {}; expected {IPC_PROTOCOL_VERSION}",
                entry.version
            ),
        });
    }
    if entry.client_id.trim().is_empty() || entry.client_id.len() > 100 {
        return Err(IpcError::InvalidTrustEntry {
            reason: "client id must contain 1..=100 UTF-8 bytes".to_owned(),
        });
    }
    if entry.executable_sha256.len() != 64
        || !entry
            .executable_sha256
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
    {
        return Err(IpcError::InvalidTrustEntry {
            reason: "executable SHA-256 must be 64 lowercase hexadecimal characters".to_owned(),
        });
    }
    Ok(())
}

fn trust_file_name(client_id: &str) -> String {
    let mut hasher = Sha256::new();
    hasher.update(client_id.as_bytes());
    format!("{}.json", hex_lower(&hasher.finalize()))
}

fn write_frame<T: Serialize>(stream: &mut Stream, value: &T) -> Result<(), IpcError> {
    let bytes =
        serde_json::to_vec(value).map_err(|error| IpcError::Serialization(error.to_string()))?;
    if bytes.len() > MAX_FRAME_BYTES {
        return Err(IpcError::FrameTooLarge {
            max_bytes: MAX_FRAME_BYTES,
        });
    }
    let length = u32::try_from(bytes.len()).map_err(|_| IpcError::FrameTooLarge {
        max_bytes: MAX_FRAME_BYTES,
    })?;
    write_all_bounded(stream, &length.to_be_bytes())?;
    write_all_bounded(stream, &bytes)?;
    Ok(())
}

fn read_frame<T: for<'de> Deserialize<'de>>(stream: &mut Stream) -> Result<T, IpcError> {
    let mut length = [0u8; 4];
    read_exact_bounded(stream, &mut length)?;
    let length = u32::from_be_bytes(length) as usize;
    if length > MAX_FRAME_BYTES {
        return Err(IpcError::FrameTooLarge {
            max_bytes: MAX_FRAME_BYTES,
        });
    }
    let mut bytes = vec![0u8; length];
    read_exact_bounded(stream, &mut bytes)?;
    serde_json::from_slice(&bytes).map_err(|error| IpcError::Serialization(error.to_string()))
}

fn write_all_bounded(stream: &mut Stream, mut bytes: &[u8]) -> Result<(), IpcError> {
    let deadline = Instant::now() + IO_TIMEOUT;
    while !bytes.is_empty() {
        match stream.write(bytes) {
            Ok(0) => wait_for_io(deadline)?,
            Ok(written) => bytes = &bytes[written..],
            Err(error) if error.kind() == io::ErrorKind::Interrupted => continue,
            Err(error) if error.kind() == io::ErrorKind::WouldBlock => {
                wait_for_io(deadline)?;
            }
            Err(error) => return Err(IpcError::Io(error)),
        }
    }
    Ok(())
}

fn read_exact_bounded(stream: &mut Stream, mut bytes: &mut [u8]) -> Result<(), IpcError> {
    let deadline = Instant::now() + IO_TIMEOUT;
    while !bytes.is_empty() {
        match stream.read(bytes) {
            Ok(0) => wait_for_io(deadline)?,
            Ok(read) => {
                let remaining = bytes;
                bytes = &mut remaining[read..];
            }
            Err(error) if error.kind() == io::ErrorKind::Interrupted => continue,
            Err(error) if error.kind() == io::ErrorKind::WouldBlock => {
                wait_for_io(deadline)?;
            }
            Err(error) => return Err(IpcError::Io(error)),
        }
    }
    Ok(())
}

fn wait_for_io(deadline: Instant) -> Result<(), IpcError> {
    if Instant::now() >= deadline {
        return Err(IpcError::Io(io::Error::new(
            io::ErrorKind::TimedOut,
            "IPC frame I/O exceeded its time bound",
        )));
    }
    thread::sleep(Duration::from_millis(1));
    Ok(())
}

fn hex_lower(bytes: &[u8]) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut output = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        output.push(char::from(HEX[usize::from(byte >> 4)]));
        output.push(char::from(HEX[usize::from(byte & 0x0f)]));
    }
    output
}

fn create_temporary_file(directory: &Path) -> Result<(PathBuf, File), IpcError> {
    for _ in 0..32 {
        let sequence = NEXT_TEMP_FILE.fetch_add(1, Ordering::Relaxed);
        let path = directory.join(format!(".synapse-ipc-tmp-{}-{sequence}", process::id()));
        match open_private_new_file(&path) {
            Ok(file) => return Ok((path, file)),
            Err(error) if error.kind() == io::ErrorKind::AlreadyExists => continue,
            Err(error) => return Err(IpcError::Io(error)),
        }
    }
    Err(IpcError::Io(io::Error::new(
        io::ErrorKind::AlreadyExists,
        "could not allocate an IPC temporary file",
    )))
}

#[cfg(unix)]
fn open_private_new_file(path: &Path) -> io::Result<File> {
    use std::os::unix::fs::OpenOptionsExt;
    let mut options = OpenOptions::new();
    options.write(true).create_new(true).mode(0o600);
    options.open(path)
}

#[cfg(not(unix))]
fn open_private_new_file(path: &Path) -> io::Result<File> {
    OpenOptions::new().write(true).create_new(true).open(path)
}

#[cfg(unix)]
fn create_private_dir_all(path: &Path) -> io::Result<()> {
    use std::os::unix::fs::DirBuilderExt;
    let mut builder = fs::DirBuilder::new();
    builder.recursive(true).mode(0o700);
    builder.create(path)
}

#[cfg(not(unix))]
fn create_private_dir_all(path: &Path) -> io::Result<()> {
    fs::create_dir_all(path)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn endpoint_name_is_stable_and_short() {
        let root = Path::new("some/local/store");
        let first = endpoint_name(root).expect("endpoint should resolve");
        let second = endpoint_name(root).expect("endpoint should resolve consistently");
        assert_eq!(first, second);
        assert!(first.starts_with("synapse-v1-"));
        assert!(first.len() < 107);
    }

    #[test]
    fn protocol_round_trip_preserves_record_request() {
        let record = KnowledgeRecord::new(
            "record-1",
            "IPC knowledge",
            "fact",
            "writer",
            1,
            synapse_core::knowledge::Confidence::High,
            synapse_core::knowledge::KnowledgeState::Active,
            synapse_core::knowledge::Provenance {
                basis: synapse_core::knowledge::ProvenanceBasis::Observed,
                detail: None,
            },
        )
        .expect("fixture should be valid");
        let request = IpcRequest::InsertRecord { record };
        let bytes = serde_json::to_vec(&request).expect("request should serialize");
        let decoded: IpcRequest =
            serde_json::from_slice(&bytes).expect("request should deserialize");
        assert_eq!(decoded, request);
    }
}

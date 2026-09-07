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
use synapse_store::{FileStore, KnowledgeHit, KnowledgeQuery, KnowledgeStatus, StoreError};
use sysinfo::{Pid, ProcessRefreshKind, ProcessesToUpdate, System, UpdateKind};

const LEGACY_IPC_PROTOCOL_VERSION: u8 = 1;
const IPC_PROTOCOL_VERSION: u8 = 2;
const TRUST_ENTRY_VERSION: u8 = 1;
const ENDPOINT_NAMESPACE_VERSION: u8 = 1;
const TRUST_DIRECTORY: &str = "ipc-trust-v1";
const MAX_TRUST_ENTRY_BYTES: usize = 1024;
const MAX_TRUST_ENTRIES: usize = 64;
const MAX_EXECUTABLE_BYTES: u64 = 512 * 1024 * 1024;
const MAX_FRAME_BYTES: usize = 12 * 1024 * 1024;
const IO_PROGRESS_TIMEOUT: Duration = Duration::from_secs(3);
const RESPONSE_START_TIMEOUT: Duration = Duration::from_secs(10);
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
    InsertRecord {
        record: KnowledgeRecord,
    },
    InsertAddressedRecord {
        record: KnowledgeRecord,
    },
    InsertRelation {
        relation: KnowledgeRelation,
    },
    InsertSuccessor {
        record: KnowledgeRecord,
        relation: KnowledgeRelation,
    },
    InsertAddressedSuccessor {
        record: KnowledgeRecord,
        relation: KnowledgeRelation,
    },
    Get {
        id: String,
    },
    Query {
        query: KnowledgeQuery,
    },
    QueryAddressed {
        query: KnowledgeQuery,
    },
    Status {
        id: String,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "result", rename_all = "snake_case")]
pub enum IpcValue {
    Pong,
    Record {
        record: Option<KnowledgeRecord>,
    },
    Hits {
        hits: Vec<KnowledgeHit>,
    },
    Status {
        status: Option<KnowledgeStatus>,
    },
    InsertedRecord {
        record: KnowledgeRecord,
    },
    InsertedRelation {
        relation: KnowledgeRelation,
    },
    InsertedSuccessor {
        record: KnowledgeRecord,
        relation: KnowledgeRelation,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "status", rename_all = "snake_case")]
pub enum IpcResponse {
    Ok { value: Box<IpcValue> },
    Error { message: String },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
struct ProtocolEnvelope<T> {
    #[serde(default = "default_protocol_version")]
    protocol_version: u8,
    #[serde(flatten)]
    payload: T,
}

impl<T> ProtocolEnvelope<T> {
    fn new(payload: T) -> Self {
        Self::with_version(payload, IPC_PROTOCOL_VERSION)
    }

    fn with_version(payload: T, protocol_version: u8) -> Self {
        Self {
            protocol_version,
            payload,
        }
    }

    fn validate(&self) -> Result<(), IpcError> {
        if matches!(
            self.protocol_version,
            LEGACY_IPC_PROTOCOL_VERSION | IPC_PROTOCOL_VERSION
        ) {
            return Ok(());
        }
        Err(IpcError::UnsupportedProtocolVersion {
            received: self.protocol_version,
            supported: IPC_PROTOCOL_VERSION,
        })
    }
}

fn default_protocol_version() -> u8 {
    LEGACY_IPC_PROTOCOL_VERSION
}

#[derive(Debug)]
pub enum IpcError {
    Io(io::Error),
    Serialization(String),
    FrameTooLarge {
        max_bytes: usize,
    },
    UnsupportedProtocolVersion {
        received: u8,
        supported: u8,
    },
    ProtocolUpgradeRequired {
        required: u8,
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
    Store(StoreError),
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
            Self::UnsupportedProtocolVersion {
                received,
                supported,
            } => write!(
                formatter,
                "unsupported IPC protocol version {received}; expected {LEGACY_IPC_PROTOCOL_VERSION} or {supported}"
            ),
            Self::ProtocolUpgradeRequired { required } => write!(
                formatter,
                "IPC protocol version {required} is required for scope/key addressed knowledge"
            ),
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
            Self::Store(error) => write!(formatter, "IPC store metadata error: {error}"),
            Self::Remote(message) => write!(formatter, "IPC request failed: {message}"),
        }
    }
}

impl Error for IpcError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::Io(error) => Some(error),
            Self::Store(error) => Some(error),
            _ => None,
        }
    }
}

impl From<io::Error> for IpcError {
    fn from(error: io::Error) -> Self {
        Self::Io(error)
    }
}

impl From<StoreError> for IpcError {
    fn from(error: StoreError) -> Self {
        Self::Store(error)
    }
}

pub fn endpoint_name(store_root: &Path) -> Result<String, IpcError> {
    let store_id = FileStore::new(store_root).store_id()?;
    Ok(format!(
        "synapse-v{ENDPOINT_NAMESPACE_VERSION}-{}",
        &store_id[..32]
    ))
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
        version: TRUST_ENTRY_VERSION,
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
                write_frame(&mut stream, &ProtocolEnvelope::new(wire_request(request)))?;
                let response: ProtocolEnvelope<IpcResponse> = read_response_frame(&mut stream)?;
                response.validate()?;
                return match response.payload {
                    IpcResponse::Ok { value } => Ok(*value),
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

fn wire_request(request: &IpcRequest) -> IpcRequest {
    match request {
        IpcRequest::InsertRecord { record } if record_requires_v2(record) => {
            IpcRequest::InsertAddressedRecord {
                record: record.clone(),
            }
        }
        IpcRequest::InsertSuccessor { record, relation } if record_requires_v2(record) => {
            IpcRequest::InsertAddressedSuccessor {
                record: record.clone(),
                relation: relation.clone(),
            }
        }
        IpcRequest::Query { query } if query.scope.is_some() || query.key.is_some() => {
            IpcRequest::QueryAddressed {
                query: query.clone(),
            }
        }
        _ => request.clone(),
    }
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
        let (response, response_version) =
            match read_frame::<ProtocolEnvelope<IpcRequest>>(&mut stream) {
                Ok(request) => {
                    let request_version = request.protocol_version;
                    match request.validate() {
                        Ok(()) => {
                            let response =
                                match ensure_request_supported(request_version, &request.payload) {
                                    Ok(()) => {
                                        match handle_request(&store, &stream, request.payload) {
                                            Ok(value) => match ensure_value_supported(
                                                request_version,
                                                &value,
                                            ) {
                                                Ok(()) => IpcResponse::Ok {
                                                    value: Box::new(value),
                                                },
                                                Err(error) => IpcResponse::Error {
                                                    message: error.to_string(),
                                                },
                                            },
                                            Err(error) => IpcResponse::Error {
                                                message: error.to_string(),
                                            },
                                        }
                                    }
                                    Err(error) => IpcResponse::Error {
                                        message: error.to_string(),
                                    },
                                };
                            (response, request_version)
                        }
                        Err(error) => (
                            IpcResponse::Error {
                                message: error.to_string(),
                            },
                            IPC_PROTOCOL_VERSION,
                        ),
                    }
                }
                Err(error) => (
                    IpcResponse::Error {
                        message: error.to_string(),
                    },
                    IPC_PROTOCOL_VERSION,
                ),
            };
        write_frame(
            &mut stream,
            &ProtocolEnvelope::with_version(response, response_version),
        )?;

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
        IpcRequest::InsertRecord { record } | IpcRequest::InsertAddressedRecord { record } => {
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
        IpcRequest::InsertSuccessor { record, relation }
        | IpcRequest::InsertAddressedSuccessor { record, relation } => {
            let authenticated = authenticate_peer(store, stream)?;
            require_source(&record.source, &authenticated)?;
            require_source(&relation.source, &authenticated)?;
            store
                .insert_successor(&record, &relation)
                .map_err(|error| IpcError::Remote(error.to_string()))?;
            Ok(IpcValue::InsertedSuccessor { record, relation })
        }
        IpcRequest::Get { id } => {
            let record = store
                .get(&id)
                .map_err(|error| IpcError::Remote(error.to_string()))?;
            Ok(IpcValue::Record { record })
        }
        IpcRequest::Query { query } | IpcRequest::QueryAddressed { query } => {
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

fn ensure_request_supported(protocol_version: u8, request: &IpcRequest) -> Result<(), IpcError> {
    if protocol_version != LEGACY_IPC_PROTOCOL_VERSION || !request_requires_v2(request) {
        return Ok(());
    }
    Err(IpcError::ProtocolUpgradeRequired {
        required: IPC_PROTOCOL_VERSION,
    })
}

fn ensure_value_supported(protocol_version: u8, value: &IpcValue) -> Result<(), IpcError> {
    if protocol_version != LEGACY_IPC_PROTOCOL_VERSION || !value_requires_v2(value) {
        return Ok(());
    }
    Err(IpcError::ProtocolUpgradeRequired {
        required: IPC_PROTOCOL_VERSION,
    })
}

fn request_requires_v2(request: &IpcRequest) -> bool {
    match request {
        IpcRequest::InsertRecord { record } | IpcRequest::InsertSuccessor { record, .. } => {
            record_requires_v2(record)
        }
        IpcRequest::InsertAddressedRecord { .. }
        | IpcRequest::InsertAddressedSuccessor { .. }
        | IpcRequest::QueryAddressed { .. } => true,
        IpcRequest::Query { query } => query.scope.is_some() || query.key.is_some(),
        IpcRequest::Ping
        | IpcRequest::InsertRelation { .. }
        | IpcRequest::Get { .. }
        | IpcRequest::Status { .. } => false,
    }
}

fn value_requires_v2(value: &IpcValue) -> bool {
    match value {
        IpcValue::Record { record } => record.as_ref().is_some_and(record_requires_v2),
        IpcValue::Hits { hits } => hits.iter().any(|hit| record_requires_v2(&hit.record)),
        IpcValue::Status { status } => status
            .as_ref()
            .is_some_and(|status| record_requires_v2(&status.record)),
        IpcValue::InsertedRecord { record } | IpcValue::InsertedSuccessor { record, .. } => {
            record_requires_v2(record)
        }
        IpcValue::Pong | IpcValue::InsertedRelation { .. } => false,
    }
}

fn record_requires_v2(record: &KnowledgeRecord) -> bool {
    record.scope.is_some() || record.key.is_some()
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
    let pid = peer_process_id(stream)?;
    let fingerprint = fingerprint_process(pid)?;
    trusted_client_for_fingerprint(store, &fingerprint)
}

fn peer_process_id(stream: &Stream) -> Result<u32, IpcError> {
    let creds = stream.peer_creds()?;
    if let Some(pid) = creds.pid() {
        return normalize_peer_pid(pid);
    }

    fallback_peer_process_id(stream)
}

#[cfg(target_os = "macos")]
fn fallback_peer_process_id(stream: &Stream) -> Result<u32, IpcError> {
    let pid = match stream {
        Stream::UdSocket(stream) => {
            nix::sys::socket::getsockopt(stream.inner(), nix::sys::socket::sockopt::LocalPeerPid)
                .map_err(|error| IpcError::Io(io::Error::from_raw_os_error(error as i32)))?
        }
    };
    normalize_peer_pid(pid)
}

#[cfg(not(target_os = "macos"))]
fn fallback_peer_process_id(_stream: &Stream) -> Result<u32, IpcError> {
    Err(IpcError::PeerProcessIdUnavailable)
}

fn normalize_peer_pid<T>(pid: T) -> Result<u32, IpcError>
where
    u32: TryFrom<T>,
{
    u32::try_from(pid).map_err(|_| IpcError::PeerProcessIdUnavailable)
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
    if entry.version != TRUST_ENTRY_VERSION {
        return Err(IpcError::InvalidTrustEntry {
            reason: format!(
                "unsupported trust entry version {}; expected {TRUST_ENTRY_VERSION}",
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
    let mut frame = Vec::with_capacity(4 + bytes.len());
    frame.extend_from_slice(&length.to_be_bytes());
    frame.extend_from_slice(&bytes);
    write_all_bounded(stream, &frame)
}

fn read_frame<T: for<'de> Deserialize<'de>>(stream: &mut Stream) -> Result<T, IpcError> {
    read_frame_with_prefix_timeout(stream, IO_PROGRESS_TIMEOUT)
}

fn read_response_frame<T: for<'de> Deserialize<'de>>(stream: &mut Stream) -> Result<T, IpcError> {
    read_frame_with_prefix_timeout(stream, RESPONSE_START_TIMEOUT)
}

fn read_frame_with_prefix_timeout<T: for<'de> Deserialize<'de>>(
    stream: &mut Stream,
    prefix_timeout: Duration,
) -> Result<T, IpcError> {
    let mut length = [0u8; 4];
    read_exact_with_timeout(stream, &mut length, prefix_timeout)?;
    let length = u32::from_be_bytes(length) as usize;
    if length > MAX_FRAME_BYTES {
        return Err(IpcError::FrameTooLarge {
            max_bytes: MAX_FRAME_BYTES,
        });
    }
    let mut bytes = vec![0u8; length];
    read_exact_with_timeout(stream, &mut bytes, IO_PROGRESS_TIMEOUT)?;
    serde_json::from_slice(&bytes).map_err(|error| IpcError::Serialization(error.to_string()))
}

fn write_all_bounded(stream: &mut Stream, mut bytes: &[u8]) -> Result<(), IpcError> {
    const INITIAL_WRITE_CHUNK: usize = 64 * 1024;

    let mut deadline = Instant::now() + IO_PROGRESS_TIMEOUT;
    let mut chunk_limit = bytes.len().clamp(1, INITIAL_WRITE_CHUNK);
    while !bytes.is_empty() {
        let chunk_len = bytes.len().min(chunk_limit);
        match stream.write(&bytes[..chunk_len]) {
            Ok(0) => {
                if chunk_len > 1 {
                    chunk_limit = (chunk_len / 2).max(1);
                } else {
                    wait_for_io(deadline, "write")?;
                }
            }
            Ok(written) => {
                bytes = &bytes[written..];
                deadline = Instant::now() + IO_PROGRESS_TIMEOUT;
                chunk_limit = bytes.len().clamp(1, INITIAL_WRITE_CHUNK);
            }
            Err(error) if error.kind() == io::ErrorKind::Interrupted => continue,
            Err(error) if error.kind() == io::ErrorKind::WouldBlock => {
                if chunk_len > 1 {
                    chunk_limit = (chunk_len / 2).max(1);
                } else {
                    wait_for_io(deadline, "write")?;
                }
            }
            Err(error) => return Err(IpcError::Io(error)),
        }
    }
    Ok(())
}

fn read_exact_with_timeout(
    stream: &mut Stream,
    mut bytes: &mut [u8],
    timeout: Duration,
) -> Result<(), IpcError> {
    let mut deadline = Instant::now() + timeout;
    while !bytes.is_empty() {
        match stream.read(bytes) {
            Ok(0) => wait_for_io(deadline, "read")?,
            Ok(read) => {
                let remaining = bytes;
                bytes = &mut remaining[read..];
                deadline = Instant::now() + timeout;
            }
            Err(error) if error.kind() == io::ErrorKind::Interrupted => continue,
            Err(error) if error.kind() == io::ErrorKind::WouldBlock => {
                wait_for_io(deadline, "read")?;
            }
            Err(error) => return Err(IpcError::Io(error)),
        }
    }
    Ok(())
}

fn wait_for_io(deadline: Instant, direction: &'static str) -> Result<(), IpcError> {
    if Instant::now() >= deadline {
        return Err(IpcError::Io(io::Error::new(
            io::ErrorKind::TimedOut,
            format!("IPC frame {direction} exceeded its progress time bound"),
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
    fn peer_pid_conversion_is_checked_across_platform_integer_types() {
        assert_eq!(
            normalize_peer_pid(42_u32).expect("u32 pid should remain valid"),
            42
        );
        assert_eq!(
            normalize_peer_pid(42_i32).expect("positive Unix pid should convert"),
            42
        );
        assert!(matches!(
            normalize_peer_pid(-1_i32),
            Err(IpcError::PeerProcessIdUnavailable)
        ));
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn macos_local_peer_pid_fallback_reads_the_connected_process() {
        use interprocess::local_socket::ListenerOptions;

        let name = format!("synapse-macos-peer-pid-test-{}", process::id())
            .to_ns_name::<GenericNamespaced>()
            .expect("test endpoint name should be valid");
        let listener = ListenerOptions::new()
            .name(name.clone())
            .create_sync()
            .expect("listener should bind");
        let client = Stream::connect(name).expect("client should connect");
        let server = listener.accept().expect("server should accept");

        assert_eq!(
            peer_process_id(&server).expect("macOS should expose LOCAL_PEERPID"),
            process::id()
        );
        drop(client);
    }

    fn endpoint_test_root(label: &str) -> PathBuf {
        let sequence = NEXT_TEMP_FILE.fetch_add(1, Ordering::Relaxed);
        let root = std::env::temp_dir().join(format!(
            "synapse-ipc-endpoint-test-{}-{sequence}-{label}",
            process::id()
        ));
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(&root).expect("endpoint test directory should be created");
        root
    }

    fn legacy_path_endpoint(root: &Path) -> String {
        let absolute = if root.is_absolute() {
            root.to_path_buf()
        } else {
            std::env::current_dir()
                .expect("current directory should resolve")
                .join(root)
        };
        let mut hasher = Sha256::new();
        hasher.update(absolute.to_string_lossy().as_bytes());
        let digest = hasher.finalize();
        format!("synapse-v1-{}", hex_lower(&digest[..16]))
    }

    #[test]
    fn endpoint_name_is_stable_short_and_preserves_the_legacy_name_on_bootstrap() {
        let root = endpoint_test_root("stable");
        let legacy = legacy_path_endpoint(&root);
        let first = endpoint_name(&root).expect("endpoint should resolve");
        let second = endpoint_name(&root).expect("endpoint should resolve consistently");
        assert_eq!(first, legacy);
        assert_eq!(first, second);
        assert!(first.starts_with("synapse-v1-"));
        assert!(first.len() < 107);
        fs::remove_dir_all(root).expect("endpoint test directory should be removed");
    }

    #[test]
    fn endpoint_name_uses_store_identity_across_path_aliases() {
        let root = endpoint_test_root("alias");
        let aliased = root.join(".");
        let direct = endpoint_name(&root).expect("direct endpoint should resolve");
        let through_alias = endpoint_name(&aliased).expect("aliased endpoint should resolve");
        assert_eq!(direct, through_alias);
        fs::remove_dir_all(root).expect("endpoint test directory should be removed");
    }

    #[test]
    fn endpoint_name_survives_moving_the_store_directory() {
        let root = endpoint_test_root("move-source");
        let moved = root.with_file_name(format!(
            "{}-moved",
            root.file_name()
                .and_then(|name| name.to_str())
                .expect("test root should have a UTF-8 file name")
        ));
        let _ = fs::remove_dir_all(&moved);

        let before = endpoint_name(&root).expect("endpoint should initialize before move");
        fs::rename(&root, &moved).expect("store directory should move");
        let after = endpoint_name(&moved).expect("endpoint should survive store move");
        assert_eq!(before, after);
        fs::remove_dir_all(moved).expect("moved endpoint test directory should be removed");
    }

    #[test]
    fn protocol_frames_include_the_explicit_version() {
        let frame = ProtocolEnvelope::new(IpcRequest::Ping);
        let value = serde_json::to_value(&frame).expect("frame should serialize");
        assert_eq!(value["protocol_version"], IPC_PROTOCOL_VERSION);
        assert_eq!(value["op"], "ping");
    }

    #[test]
    fn legacy_unversioned_protocol_frames_are_treated_as_v1() {
        let frame: ProtocolEnvelope<IpcRequest> =
            serde_json::from_str(r#"{"op":"ping"}"#).expect("legacy frame should deserialize");
        frame
            .validate()
            .expect("legacy v1 frame should remain valid");
        assert_eq!(frame.protocol_version, LEGACY_IPC_PROTOCOL_VERSION);
        assert_eq!(frame.payload, IpcRequest::Ping);
    }

    #[test]
    fn versioned_frames_remain_decodable_by_the_legacy_payload_shapes() {
        let request_value =
            serde_json::to_value(ProtocolEnvelope::new(IpcRequest::Ping)).expect("request frame");
        let legacy_request: IpcRequest = serde_json::from_value(request_value)
            .expect("legacy request shape should ignore version");
        assert_eq!(legacy_request, IpcRequest::Ping);

        let response_value = serde_json::to_value(ProtocolEnvelope::new(IpcResponse::Ok {
            value: Box::new(IpcValue::Pong),
        }))
        .expect("response frame");
        let legacy_response: IpcResponse = serde_json::from_value(response_value)
            .expect("legacy response shape should ignore version");
        assert_eq!(
            legacy_response,
            IpcResponse::Ok {
                value: Box::new(IpcValue::Pong)
            }
        );
    }

    #[test]
    fn future_protocol_versions_are_rejected() {
        let frame: ProtocolEnvelope<IpcRequest> =
            serde_json::from_str(r#"{"protocol_version":3,"op":"ping"}"#)
                .expect("future frame should deserialize structurally");
        assert!(matches!(
            frame.validate(),
            Err(IpcError::UnsupportedProtocolVersion {
                received: 3,
                supported: IPC_PROTOCOL_VERSION
            })
        ));
    }

    #[test]
    fn legacy_protocol_rejects_scope_key_requests_instead_of_dropping_the_address() {
        let record = KnowledgeRecord::new(
            "addressed",
            "compiler path",
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
        .expect("fixture should be valid")
        .with_address("machine", "toolchain.rust.compiler_path")
        .expect("address should be valid");
        let request = IpcRequest::InsertRecord { record };

        assert!(matches!(
            ensure_request_supported(LEGACY_IPC_PROTOCOL_VERSION, &request),
            Err(IpcError::ProtocolUpgradeRequired {
                required: IPC_PROTOCOL_VERSION
            })
        ));
        ensure_request_supported(IPC_PROTOCOL_VERSION, &request)
            .expect("v2 must support addressed records");
    }

    #[test]
    fn outgoing_addressed_requests_use_v2_only_operation_names() {
        let addressed = KnowledgeRecord::new(
            "addressed",
            "compiler path",
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
        .expect("fixture should be valid")
        .with_address("machine", "toolchain.rust.compiler_path")
        .expect("address should be valid");
        let addressed_wire = wire_request(&IpcRequest::InsertRecord { record: addressed });
        let addressed_json =
            serde_json::to_value(addressed_wire).expect("addressed wire request should serialize");
        assert_eq!(addressed_json["op"], "insert_addressed_record");

        let plain = KnowledgeRecord::new(
            "plain",
            "plain knowledge",
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
        .expect("plain fixture should be valid");
        let plain_wire = wire_request(&IpcRequest::InsertRecord { record: plain });
        let plain_json =
            serde_json::to_value(plain_wire).expect("plain wire request should serialize");
        assert_eq!(plain_json["op"], "insert_record");

        let addressed_query = wire_request(&IpcRequest::Query {
            query: KnowledgeQuery {
                scope: Some("machine".to_owned()),
                key: Some("toolchain.rust.compiler_path".to_owned()),
                ..KnowledgeQuery::default()
            },
        });
        let query_json =
            serde_json::to_value(addressed_query).expect("addressed query should serialize");
        assert_eq!(query_json["op"], "query_addressed");
    }

    #[test]
    fn legacy_protocol_rejects_addressed_results_instead_of_silently_losing_fields() {
        let record = KnowledgeRecord::new(
            "addressed",
            "compiler path",
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
        .expect("fixture should be valid")
        .with_address("machine", "toolchain.rust.compiler_path")
        .expect("address should be valid");
        let value = IpcValue::Record {
            record: Some(record),
        };

        assert!(matches!(
            ensure_value_supported(LEGACY_IPC_PROTOCOL_VERSION, &value),
            Err(IpcError::ProtocolUpgradeRequired {
                required: IPC_PROTOCOL_VERSION
            })
        ));
        ensure_value_supported(IPC_PROTOCOL_VERSION, &value)
            .expect("v2 must return addressed records");
    }

    #[test]
    fn protocol_round_trip_preserves_successor_request() {
        let record = KnowledgeRecord::new(
            "new",
            "new value",
            "fact",
            "writer",
            2,
            synapse_core::knowledge::Confidence::High,
            synapse_core::knowledge::KnowledgeState::Active,
            synapse_core::knowledge::Provenance {
                basis: synapse_core::knowledge::ProvenanceBasis::Observed,
                detail: None,
            },
        )
        .expect("record fixture should be valid");
        let relation = KnowledgeRelation::new(
            "new-over-old",
            "new",
            synapse_core::knowledge::KnowledgeRelationKind::Supersedes,
            "old",
            "writer",
            2,
            synapse_core::knowledge::Provenance {
                basis: synapse_core::knowledge::ProvenanceBasis::Observed,
                detail: None,
            },
        )
        .expect("relation fixture should be valid");
        let request = IpcRequest::InsertSuccessor { record, relation };
        let bytes = serde_json::to_vec(&request).expect("request should serialize");
        let decoded: IpcRequest =
            serde_json::from_slice(&bytes).expect("request should deserialize");
        assert_eq!(decoded, request);
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

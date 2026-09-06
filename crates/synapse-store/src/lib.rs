//! Durable local storage for Synapse knowledge records.

mod authorization;
mod evolution;
mod index;

pub use authorization::{AuthorizationPolicy, ClientAuthorization, InvalidAuthorizationPolicy};

use std::{
    error::Error,
    ffi::OsStr,
    fmt, fs,
    fs::{File, OpenOptions},
    io::{self, Read, Write},
    ops::Deref,
    path::{Path, PathBuf},
    process,
    sync::atomic::{AtomicU64, Ordering},
};

use serde::{Deserialize, Serialize};
use synapse_core::knowledge::{
    KnowledgeRecord, KnowledgeRelation, KnowledgeState, ProvenanceBasis,
};

const MAX_ID_BYTES: usize = 100;
const MAX_RECORD_BYTES: usize = 1024 * 1024;
const MAX_QUERY_RESULTS: usize = 10;
const MAX_QUERY_TEXT_BYTES: usize = 256;
const MAX_QUERY_SCAN_RECORDS: usize = 256;
const MAX_QUERY_CANDIDATES: usize = 256;
static NEXT_TEMP_FILE: AtomicU64 = AtomicU64::new(0);

/// A simple local store that publishes each knowledge record as one validated JSON file.
#[derive(Debug, Clone)]
pub struct FileStore {
    root: PathBuf,
}

/// A bounded lexical query over persisted knowledge.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct KnowledgeQuery {
    pub text: Option<String>,
    pub kind: Option<String>,
    pub source: Option<String>,
    pub state: Option<KnowledgeState>,
    pub provenance_basis: Option<ProvenanceBasis>,
    pub limit: usize,
}

impl Default for KnowledgeQuery {
    fn default() -> Self {
        Self {
            text: None,
            kind: None,
            source: None,
            state: Some(KnowledgeState::Active),
            provenance_basis: None,
            limit: 5,
        }
    }
}

/// A query result containing the immutable stored record and its relation-derived lifecycle state.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct KnowledgeHit {
    #[serde(flatten)]
    pub record: KnowledgeRecord,
    pub effective_state: KnowledgeState,
}

impl Deref for KnowledgeHit {
    type Target = KnowledgeRecord;

    fn deref(&self) -> &Self::Target {
        &self.record
    }
}

/// Detailed currentness information for one immutable knowledge record.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct KnowledgeStatus {
    #[serde(flatten)]
    pub record: KnowledgeRecord,
    pub effective_state: KnowledgeState,
    pub superseded_by: Vec<KnowledgeRelation>,
    pub conflicts_with: Vec<KnowledgeRelation>,
}

#[derive(Debug)]
pub enum StoreError {
    Io(io::Error),
    AlreadyExists {
        id: String,
    },
    InvalidId {
        reason: &'static str,
    },
    IdTooLong {
        max_bytes: usize,
    },
    RecordTooLarge {
        max_bytes: usize,
    },
    InvalidQueryLimit {
        max: usize,
    },
    QueryTextTooLong {
        max_bytes: usize,
    },
    QueryScanLimitExceeded {
        max_records: usize,
    },
    IndexRebuildRequired {
        legacy_scan_limit: usize,
    },
    IndexCapacityExceeded {
        max_records: usize,
    },
    IndexEntryCapacityExceeded {
        max_entries: usize,
    },
    QueryCandidateLimitExceeded {
        max_candidates: usize,
    },
    AuthorizationNotConfigured,
    AuthorizationAlreadyConfigured,
    AuthorizationPolicyTooLarge {
        max_bytes: usize,
    },
    InvalidAuthorizationPolicy {
        reason: String,
    },
    AuthorizationDenied {
        client_id: String,
        capability: &'static str,
    },
    RelationAlreadyExists {
        id: String,
    },
    RelationTooLarge {
        max_bytes: usize,
    },
    RelationEndpointMissing {
        id: String,
    },
    RelationCapacityExceeded {
        max_relations: usize,
    },
    SupersessionCycle,
    CorruptRelation {
        id: String,
        reason: String,
    },
    CorruptIndex {
        reason: String,
    },
    CorruptRecord {
        id: String,
        reason: String,
    },
}

impl fmt::Display for StoreError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Io(error) => write!(formatter, "storage I/O error: {error}"),
            Self::AlreadyExists { id } => {
                write!(formatter, "knowledge record '{id}' already exists")
            }
            Self::InvalidId { reason } => {
                write!(formatter, "invalid knowledge record id: {reason}")
            }
            Self::IdTooLong { max_bytes } => write!(
                formatter,
                "knowledge record id exceeds the storage limit of {max_bytes} bytes"
            ),
            Self::RecordTooLarge { max_bytes } => write!(
                formatter,
                "knowledge record exceeds the storage limit of {max_bytes} bytes"
            ),
            Self::InvalidQueryLimit { max } => write!(
                formatter,
                "knowledge query limit must be between 1 and {max}"
            ),
            Self::QueryTextTooLong { max_bytes } => write!(
                formatter,
                "knowledge query text exceeds the limit of {max_bytes} bytes"
            ),
            Self::QueryScanLimitExceeded { max_records } => write!(
                formatter,
                "knowledge query would scan more than {max_records} records; narrower or indexed retrieval is required"
            ),
            Self::IndexRebuildRequired { legacy_scan_limit } => write!(
                formatter,
                "legacy knowledge store exceeds the {legacy_scan_limit}-record scan limit; rebuild the retrieval index"
            ),
            Self::IndexCapacityExceeded { max_records } => write!(
                formatter,
                "knowledge retrieval index exceeds its bounded capacity of {max_records} records"
            ),
            Self::IndexEntryCapacityExceeded { max_entries } => write!(
                formatter,
                "knowledge retrieval index contains more than {max_entries} bounded entries"
            ),
            Self::QueryCandidateLimitExceeded { max_candidates } => write!(
                formatter,
                "knowledge query matches more than {max_candidates} index candidates; narrow the query"
            ),
            Self::AuthorizationNotConfigured => write!(
                formatter,
                "Synapse authorization is not configured; initialize authorization before authoritative writes"
            ),
            Self::AuthorizationAlreadyConfigured => {
                write!(formatter, "Synapse authorization is already configured")
            }
            Self::AuthorizationPolicyTooLarge { max_bytes } => write!(
                formatter,
                "Synapse authorization policy exceeds the limit of {max_bytes} bytes"
            ),
            Self::InvalidAuthorizationPolicy { reason } => {
                write!(formatter, "Synapse authorization policy is invalid: {reason}")
            }
            Self::AuthorizationDenied {
                client_id,
                capability,
            } => write!(
                formatter,
                "client '{client_id}' is not authorized for {capability}"
            ),
            Self::RelationAlreadyExists { id } => {
                write!(formatter, "knowledge relation '{id}' already exists")
            }
            Self::RelationTooLarge { max_bytes } => write!(
                formatter,
                "knowledge relation exceeds the storage limit of {max_bytes} bytes"
            ),
            Self::RelationEndpointMissing { id } => {
                write!(formatter, "knowledge relation endpoint '{id}' does not exist")
            }
            Self::RelationCapacityExceeded { max_relations } => write!(
                formatter,
                "knowledge evolution graph exceeds its bounded capacity of {max_relations} relations"
            ),
            Self::SupersessionCycle => {
                write!(formatter, "knowledge supersession would create a cycle")
            }
            Self::CorruptRelation { id, reason } => write!(
                formatter,
                "stored knowledge relation '{id}' is invalid: {reason}"
            ),
            Self::CorruptIndex { reason } => {
                write!(formatter, "knowledge retrieval index is invalid: {reason}")
            }
            Self::CorruptRecord { id, reason } => {
                write!(
                    formatter,
                    "stored knowledge record '{id}' is invalid: {reason}"
                )
            }
        }
    }
}

impl Error for StoreError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::Io(error) => Some(error),
            _ => None,
        }
    }
}

impl From<io::Error> for StoreError {
    fn from(error: io::Error) -> Self {
        Self::Io(error)
    }
}

impl FileStore {
    pub fn new(root: impl Into<PathBuf>) -> Self {
        Self { root: root.into() }
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    /// Persist a record exactly once. Existing IDs are never overwritten.
    pub fn insert(&self, record: &KnowledgeRecord) -> Result<(), StoreError> {
        authorization::require(
            self,
            &record.source,
            authorization::AuthorizationCapability::WriteRecords,
        )?;
        let file_name = record_file_name(&record.id)?;
        let serialized = serde_json::to_vec(record).map_err(|error| StoreError::CorruptRecord {
            id: record.id.clone(),
            reason: error.to_string(),
        })?;
        if serialized.len() > MAX_RECORD_BYTES {
            return Err(StoreError::RecordTooLarge {
                max_bytes: MAX_RECORD_BYTES,
            });
        }

        index::ensure_ready(self)?;
        index::publish_record_entry(self, record, &serialized)?;

        let records_dir = self.records_dir();
        create_private_dir_all(&records_dir)?;
        let final_path = records_dir.join(file_name);
        let (temporary_path, mut temporary_file) = create_temporary_file(&records_dir)?;

        let write_result = (|| -> Result<(), StoreError> {
            temporary_file.write_all(&serialized)?;
            temporary_file.sync_all()?;
            drop(temporary_file);

            match fs::hard_link(&temporary_path, &final_path) {
                Ok(()) => Ok(()),
                Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {
                    Err(StoreError::AlreadyExists {
                        id: record.id.clone(),
                    })
                }
                Err(error) => Err(StoreError::Io(error)),
            }
        })();

        let _ = fs::remove_file(&temporary_path);
        write_result
    }

    /// Read one record by ID. Missing IDs return `Ok(None)`.
    pub fn get(&self, id: &str) -> Result<Option<KnowledgeRecord>, StoreError> {
        let file_name = record_file_name(id)?;
        let path = self.records_dir().join(file_name);
        let file = match File::open(&path) {
            Ok(file) => file,
            Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
            Err(error) => return Err(StoreError::Io(error)),
        };

        if file.metadata()?.len() > MAX_RECORD_BYTES as u64 {
            return Err(StoreError::RecordTooLarge {
                max_bytes: MAX_RECORD_BYTES,
            });
        }

        let mut serialized = Vec::new();
        file.take((MAX_RECORD_BYTES + 1) as u64)
            .read_to_end(&mut serialized)?;
        if serialized.len() > MAX_RECORD_BYTES {
            return Err(StoreError::RecordTooLarge {
                max_bytes: MAX_RECORD_BYTES,
            });
        }

        let record: KnowledgeRecord =
            serde_json::from_slice(&serialized).map_err(|error| StoreError::CorruptRecord {
                id: id.to_owned(),
                reason: error.to_string(),
            })?;
        if record.id != id {
            return Err(StoreError::CorruptRecord {
                id: id.to_owned(),
                reason: format!("record contains mismatched id '{}'", record.id),
            });
        }

        Ok(Some(record))
    }

    /// Persist one append-only lifecycle relation after validating both record endpoints.
    pub fn insert_relation(&self, relation: &KnowledgeRelation) -> Result<(), StoreError> {
        authorization::require(
            self,
            &relation.source,
            authorization::AuthorizationCapability::WriteRelations,
        )?;
        evolution::insert_relation(self, relation)
    }

    /// Create the store-local authorization policy exactly once.
    pub fn initialize_authorization(&self, policy: &AuthorizationPolicy) -> Result<(), StoreError> {
        authorization::initialize(self, policy)
    }

    /// Read and validate the current store-local authorization policy.
    pub fn authorization_policy(&self) -> Result<AuthorizationPolicy, StoreError> {
        authorization::load(self)
    }

    /// Resolve one record's effective lifecycle state and relation evidence.
    pub fn status(&self, id: &str) -> Result<Option<KnowledgeStatus>, StoreError> {
        let Some(record) = self.get(id)? else {
            return Ok(None);
        };
        let graph = evolution::RelationGraph::load(self)?;
        Ok(Some(graph.status(record)))
    }

    /// Discover persisted knowledge using bounded lexical matching and effective lifecycle state.
    pub fn query(&self, query: &KnowledgeQuery) -> Result<Vec<KnowledgeHit>, StoreError> {
        validate_query(query)?;
        let normalized_text = query.text.as_ref().map(|text| text.to_lowercase());
        let graph = evolution::RelationGraph::load(self)?;

        if index::is_ready(self)? {
            let ids = index::candidate_ids(self, query, normalized_text.as_deref())?;
            return self.query_ids(ids, query, normalized_text.as_deref(), &graph);
        }

        self.query_legacy_store(query, normalized_text.as_deref(), &graph)
    }

    /// Build the durable derived retrieval index for an existing file store.
    pub fn rebuild_index(&self) -> Result<usize, StoreError> {
        index::rebuild(self)
    }

    fn query_ids(
        &self,
        ids: Vec<String>,
        query: &KnowledgeQuery,
        normalized_text: Option<&str>,
        graph: &evolution::RelationGraph,
    ) -> Result<Vec<KnowledgeHit>, StoreError> {
        let mut matches = Vec::with_capacity(query.limit);
        for id in ids {
            let Some(record) = self.get(&id)? else {
                continue;
            };
            let effective_state = graph.effective_state(&record);
            if record_matches_query(&record, effective_state, query, normalized_text) {
                push_bounded_match(
                    &mut matches,
                    KnowledgeHit {
                        record,
                        effective_state,
                    },
                    query.limit,
                );
            }
        }
        Ok(matches)
    }

    fn query_legacy_store(
        &self,
        query: &KnowledgeQuery,
        normalized_text: Option<&str>,
        graph: &evolution::RelationGraph,
    ) -> Result<Vec<KnowledgeHit>, StoreError> {
        let records_dir = self.records_dir();
        let entries = match fs::read_dir(&records_dir) {
            Ok(entries) => entries,
            Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(Vec::new()),
            Err(error) => return Err(StoreError::Io(error)),
        };

        let mut scanned_records = 0usize;
        let mut matches = Vec::with_capacity(query.limit);
        for entry in entries {
            let entry = entry?;
            if !entry.file_type()?.is_file() {
                continue;
            }
            let Some(id) = record_id_from_file_name(&entry.file_name()) else {
                continue;
            };

            scanned_records += 1;
            if scanned_records > MAX_QUERY_SCAN_RECORDS {
                return Err(StoreError::IndexRebuildRequired {
                    legacy_scan_limit: MAX_QUERY_SCAN_RECORDS,
                });
            }

            let Some(record) = self.get(&id)? else {
                continue;
            };
            let effective_state = graph.effective_state(&record);
            if record_matches_query(&record, effective_state, query, normalized_text) {
                push_bounded_match(
                    &mut matches,
                    KnowledgeHit {
                        record,
                        effective_state,
                    },
                    query.limit,
                );
            }
        }
        Ok(matches)
    }

    fn records_dir(&self) -> PathBuf {
        self.root.join("records")
    }

    fn relations_dir(&self) -> PathBuf {
        self.root.join("relations")
    }
}

fn push_bounded_match(matches: &mut Vec<KnowledgeHit>, hit: KnowledgeHit, limit: usize) {
    matches.push(hit);
    matches.sort_by(|left, right| {
        right
            .record
            .created_at_unix_ms
            .cmp(&left.record.created_at_unix_ms)
            .then_with(|| left.record.id.cmp(&right.record.id))
    });
    if matches.len() > limit {
        matches.pop();
    }
}

fn validate_query(query: &KnowledgeQuery) -> Result<(), StoreError> {
    if query.limit == 0 || query.limit > MAX_QUERY_RESULTS {
        return Err(StoreError::InvalidQueryLimit {
            max: MAX_QUERY_RESULTS,
        });
    }
    if query
        .text
        .as_ref()
        .is_some_and(|text| text.len() > MAX_QUERY_TEXT_BYTES)
    {
        return Err(StoreError::QueryTextTooLong {
            max_bytes: MAX_QUERY_TEXT_BYTES,
        });
    }
    Ok(())
}

fn record_matches_query(
    record: &KnowledgeRecord,
    effective_state: KnowledgeState,
    query: &KnowledgeQuery,
    normalized_text: Option<&str>,
) -> bool {
    if query.kind.as_ref().is_some_and(|kind| &record.kind != kind) {
        return false;
    }
    if query
        .source
        .as_ref()
        .is_some_and(|source| &record.source != source)
    {
        return false;
    }
    if query.state.is_some_and(|state| effective_state != state) {
        return false;
    }
    if query
        .provenance_basis
        .is_some_and(|basis| record.provenance.basis != basis)
    {
        return false;
    }

    normalized_text.is_none_or(|needle| record_contains_text(record, needle))
}

fn record_contains_text(record: &KnowledgeRecord, normalized_text: &str) -> bool {
    [
        record.id.as_str(),
        record.content.as_str(),
        record.kind.as_str(),
        record.source.as_str(),
    ]
    .into_iter()
    .any(|value| value.to_lowercase().contains(normalized_text))
        || record
            .provenance
            .detail
            .as_deref()
            .is_some_and(|detail| detail.to_lowercase().contains(normalized_text))
}

fn record_id_from_file_name(file_name: &OsStr) -> Option<String> {
    let name = file_name.to_str()?;
    let encoded = name.strip_suffix(".json")?;
    if encoded.is_empty() || encoded.len() % 2 != 0 || encoded.len() > MAX_ID_BYTES * 2 {
        return None;
    }

    let mut bytes = Vec::with_capacity(encoded.len() / 2);
    for pair in encoded.as_bytes().chunks_exact(2) {
        let high = lowercase_hex_value(pair[0])?;
        let low = lowercase_hex_value(pair[1])?;
        bytes.push((high << 4) | low);
    }
    let id = String::from_utf8(bytes).ok()?;
    (record_file_name(&id).ok().as_deref() == Some(name)).then_some(id)
}

fn lowercase_hex_value(byte: u8) -> Option<u8> {
    match byte {
        b'0'..=b'9' => Some(byte - b'0'),
        b'a'..=b'f' => Some(byte - b'a' + 10),
        _ => None,
    }
}

fn record_file_name(id: &str) -> Result<String, StoreError> {
    if id.trim().is_empty() {
        return Err(StoreError::InvalidId {
            reason: "id must contain non-whitespace text",
        });
    }
    if id.len() > MAX_ID_BYTES {
        return Err(StoreError::IdTooLong {
            max_bytes: MAX_ID_BYTES,
        });
    }

    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut encoded = String::with_capacity(id.len() * 2 + 5);
    for byte in id.as_bytes() {
        encoded.push(char::from(HEX[usize::from(byte >> 4)]));
        encoded.push(char::from(HEX[usize::from(byte & 0x0f)]));
    }
    encoded.push_str(".json");
    Ok(encoded)
}

fn create_temporary_file(records_dir: &Path) -> Result<(PathBuf, File), StoreError> {
    for _ in 0..32 {
        let sequence = NEXT_TEMP_FILE.fetch_add(1, Ordering::Relaxed);
        let path = records_dir.join(format!(".synapse-tmp-{}-{sequence}", process::id()));
        match open_private_new_file(&path) {
            Ok(file) => return Ok((path, file)),
            Err(error) if error.kind() == io::ErrorKind::AlreadyExists => continue,
            Err(error) => return Err(StoreError::Io(error)),
        }
    }

    Err(StoreError::Io(io::Error::new(
        io::ErrorKind::AlreadyExists,
        "could not allocate a temporary storage file",
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

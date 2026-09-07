use std::{
    collections::HashSet,
    fs::{self, File},
    io::{self, Read, Write},
    path::Path,
};

use sha2::{Digest, Sha256};
use synapse_core::knowledge::{KnowledgeRecord, KnowledgeState, ProvenanceBasis};

use super::{
    create_private_dir_all, create_temporary_file, record_file_name, record_id_from_file_name,
    successor, FileStore, KnowledgeQuery, StoreError, MAX_QUERY_CANDIDATES,
};

const INDEX_DIR_NAME: &str = "index-v1";
const READY_FILE_NAME: &str = "READY";
const READY_CONTENT: &[u8] = b"SYNAPSE_INDEX_V1\n";
const ENTRY_MAGIC: &[u8; 8] = b"SYNIDX1E";
const BLOOM_BYTES: usize = 256;
const BLOOM_BITS: u64 = (BLOOM_BYTES * 8) as u64;
const MAX_INDEX_RECORDS: usize = 16_384;
const MAX_INDEX_ENTRIES: usize = 32_768;
const MAX_INDEX_ENTRY_BYTES: usize = 512;

pub(super) fn ensure_ready(store: &FileStore) -> Result<(), StoreError> {
    if is_ready(store)? {
        return Ok(());
    }

    rebuild(store).map(|_| ())
}

pub(super) fn is_ready(store: &FileStore) -> Result<bool, StoreError> {
    let path = index_dir(store).join(READY_FILE_NAME);
    let file = match File::open(path) {
        Ok(file) => file,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(false),
        Err(error) => return Err(StoreError::Io(error)),
    };

    let mut content = Vec::new();
    file.take((READY_CONTENT.len() + 1) as u64)
        .read_to_end(&mut content)?;
    if content != READY_CONTENT {
        return Err(StoreError::CorruptIndex {
            reason: "index readiness marker is invalid".to_owned(),
        });
    }

    Ok(true)
}

pub(super) fn rebuild(store: &FileStore) -> Result<usize, StoreError> {
    let mut ids = Vec::new();
    match fs::read_dir(store.records_dir()) {
        Ok(entries) => {
            for entry in entries {
                let entry = entry?;
                if !entry.file_type()?.is_file() {
                    continue;
                }
                let Some(id) = record_id_from_file_name(&entry.file_name()) else {
                    continue;
                };
                ids.push(id);
                if ids.len() > MAX_INDEX_RECORDS {
                    return Err(StoreError::IndexCapacityExceeded {
                        max_records: MAX_INDEX_RECORDS,
                    });
                }
            }
        }
        Err(error) if error.kind() == io::ErrorKind::NotFound => {}
        Err(error) => return Err(StoreError::Io(error)),
    }
    for id in successor::record_ids(store)? {
        ids.push(id);
        if ids.len() > MAX_INDEX_RECORDS {
            return Err(StoreError::IndexCapacityExceeded {
                max_records: MAX_INDEX_RECORDS,
            });
        }
    }

    for id in &ids {
        let (record, serialized) =
            store
                .read_record_with_serialized(id)?
                .ok_or_else(|| StoreError::CorruptIndex {
                    reason: format!("record '{id}' disappeared during index rebuild"),
                })?;
        publish_record_entry(store, &record, &serialized)?;
    }

    create_private_dir_all(&index_dir(store))?;
    publish_ready_marker(store)?;
    Ok(ids.len())
}

pub(super) fn publish_record_entry(
    store: &FileStore,
    record: &KnowledgeRecord,
    serialized_record: &[u8],
) -> Result<(), StoreError> {
    let directory = index_dir(store);
    create_private_dir_all(&directory)?;

    let entry = encode_entry(record)?;
    let final_path = directory.join(index_entry_file_name(serialized_record));
    if final_path.exists() {
        return verify_existing_entry(&final_path, &entry);
    }

    let (temporary_path, mut temporary_file) = create_temporary_file(&directory)?;
    let result = (|| -> Result<(), StoreError> {
        temporary_file.write_all(&entry)?;
        temporary_file.sync_all()?;
        drop(temporary_file);

        match fs::hard_link(&temporary_path, &final_path) {
            Ok(()) => Ok(()),
            Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {
                verify_existing_entry(&final_path, &entry)
            }
            Err(error) => Err(StoreError::Io(error)),
        }
    })();

    let _ = fs::remove_file(&temporary_path);
    result
}

pub(super) fn reconcile_duplicate_record_entry(
    store: &FileStore,
    attempted_record: &KnowledgeRecord,
    attempted_serialized: &[u8],
) -> Result<(), StoreError> {
    let (winning_record, winning_serialized) = store
        .read_record_with_serialized(&attempted_record.id)?
        .ok_or_else(|| StoreError::CorruptIndex {
            reason: format!(
                "record '{}' disappeared while reconciling a concurrent duplicate insert",
                attempted_record.id
            ),
        })?;

    publish_record_entry(store, &winning_record, &winning_serialized)?;

    let attempted_path = index_dir(store).join(index_entry_file_name(attempted_serialized));
    let winning_path = index_dir(store).join(index_entry_file_name(&winning_serialized));
    if attempted_path == winning_path {
        return Ok(());
    }

    match fs::remove_file(attempted_path) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(StoreError::Io(error)),
    }
}

pub(super) fn candidate_ids(
    store: &FileStore,
    query: &KnowledgeQuery,
    normalized_text: Option<&str>,
) -> Result<Vec<String>, StoreError> {
    let entries = fs::read_dir(index_dir(store))?;
    let normalized_scope = query.scope.as_ref().map(|scope| scope.to_lowercase());
    let normalized_key = query.key.as_ref().map(|key| key.to_lowercase());
    let mut index_entries = 0usize;
    let mut candidates = HashSet::new();

    for entry in entries {
        let entry = entry?;
        if !entry.file_type()?.is_file()
            || entry.path().extension().and_then(|value| value.to_str()) != Some("idx")
        {
            continue;
        }

        index_entries += 1;
        if index_entries > MAX_INDEX_ENTRIES {
            return Err(StoreError::IndexEntryCapacityExceeded {
                max_entries: MAX_INDEX_ENTRIES,
            });
        }

        let decoded = read_entry(&entry.path())?;
        if !entry_matches_query(
            &decoded,
            query,
            normalized_text,
            normalized_scope.as_deref(),
            normalized_key.as_deref(),
        ) {
            continue;
        }

        candidates.insert(decoded.id);
        if candidates.len() > MAX_QUERY_CANDIDATES {
            return Err(StoreError::QueryCandidateLimitExceeded {
                max_candidates: MAX_QUERY_CANDIDATES,
            });
        }
    }

    Ok(candidates.into_iter().collect())
}

fn verify_existing_entry(path: &Path, expected: &[u8]) -> Result<(), StoreError> {
    let file = File::open(path)?;
    if file.metadata()?.len() > MAX_INDEX_ENTRY_BYTES as u64 {
        return Err(StoreError::CorruptIndex {
            reason: "existing index entry exceeds its size bound".to_owned(),
        });
    }

    let mut bytes = Vec::new();
    file.take((MAX_INDEX_ENTRY_BYTES + 1) as u64)
        .read_to_end(&mut bytes)?;
    if bytes != expected {
        return Err(StoreError::CorruptIndex {
            reason: "existing index entry does not match the authoritative record".to_owned(),
        });
    }
    Ok(())
}

fn index_dir(store: &FileStore) -> std::path::PathBuf {
    store.root().join(INDEX_DIR_NAME)
}

fn publish_ready_marker(store: &FileStore) -> Result<(), StoreError> {
    let directory = index_dir(store);
    create_private_dir_all(&directory)?;
    let final_path = directory.join(READY_FILE_NAME);
    if final_path.exists() {
        return require_ready_marker(store);
    }

    let (temporary_path, mut temporary_file) = create_temporary_file(&directory)?;
    let result = (|| -> Result<(), StoreError> {
        temporary_file.write_all(READY_CONTENT)?;
        temporary_file.sync_all()?;
        drop(temporary_file);

        match fs::hard_link(&temporary_path, &final_path) {
            Ok(()) => Ok(()),
            Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {
                require_ready_marker(store)
            }
            Err(error) => Err(StoreError::Io(error)),
        }
    })();

    let _ = fs::remove_file(&temporary_path);
    result
}

fn require_ready_marker(store: &FileStore) -> Result<(), StoreError> {
    if is_ready(store)? {
        return Ok(());
    }

    Err(StoreError::CorruptIndex {
        reason: "index readiness marker disappeared during publication".to_owned(),
    })
}

fn index_entry_file_name(serialized_record: &[u8]) -> String {
    let digest = Sha256::digest(serialized_record);
    let mut name = String::with_capacity(68);
    for byte in digest {
        use std::fmt::Write as _;
        write!(&mut name, "{byte:02x}").expect("writing to String cannot fail");
    }
    name.push_str(".idx");
    name
}

fn encode_entry(record: &KnowledgeRecord) -> Result<Vec<u8>, StoreError> {
    record_file_name(&record.id)?;
    let id = record.id.as_bytes();
    let id_len: u8 = id.len().try_into().map_err(|_| StoreError::CorruptIndex {
        reason: "record id is too long for index encoding".to_owned(),
    })?;

    let mut entry = Vec::with_capacity(ENTRY_MAGIC.len() + 1 + id.len() + 18 + BLOOM_BYTES);
    entry.extend_from_slice(ENTRY_MAGIC);
    entry.push(id_len);
    entry.extend_from_slice(id);
    entry.extend_from_slice(&stable_hash(record.kind.as_bytes()).to_le_bytes());
    entry.extend_from_slice(&stable_hash(record.source.as_bytes()).to_le_bytes());
    entry.push(state_code(record.state));
    entry.push(basis_code(record.provenance.basis));
    entry.extend_from_slice(&build_bloom(record));
    Ok(entry)
}

struct IndexEntry {
    id: String,
    kind_hash: u64,
    source_hash: u64,
    basis: u8,
    bloom: [u8; BLOOM_BYTES],
}

fn read_entry(path: &Path) -> Result<IndexEntry, StoreError> {
    let file = File::open(path)?;
    if file.metadata()?.len() > MAX_INDEX_ENTRY_BYTES as u64 {
        return Err(StoreError::CorruptIndex {
            reason: "index entry exceeds its size bound".to_owned(),
        });
    }

    let mut bytes = Vec::new();
    file.take((MAX_INDEX_ENTRY_BYTES + 1) as u64)
        .read_to_end(&mut bytes)?;
    decode_entry(&bytes)
}

fn decode_entry(bytes: &[u8]) -> Result<IndexEntry, StoreError> {
    if bytes.len() < ENTRY_MAGIC.len() + 1 || &bytes[..ENTRY_MAGIC.len()] != ENTRY_MAGIC {
        return Err(StoreError::CorruptIndex {
            reason: "index entry has an invalid header".to_owned(),
        });
    }

    let id_len = usize::from(bytes[ENTRY_MAGIC.len()]);
    let expected_len = ENTRY_MAGIC.len() + 1 + id_len + 8 + 8 + 1 + 1 + BLOOM_BYTES;
    if bytes.len() != expected_len {
        return Err(StoreError::CorruptIndex {
            reason: "index entry has an invalid length".to_owned(),
        });
    }

    let mut cursor = ENTRY_MAGIC.len() + 1;
    let id = String::from_utf8(bytes[cursor..cursor + id_len].to_vec()).map_err(|_| {
        StoreError::CorruptIndex {
            reason: "index entry id is not valid UTF-8".to_owned(),
        }
    })?;
    record_file_name(&id).map_err(|error| StoreError::CorruptIndex {
        reason: error.to_string(),
    })?;
    cursor += id_len;

    let kind_hash = read_u64(bytes, &mut cursor);
    let source_hash = read_u64(bytes, &mut cursor);
    cursor += 1; // Stored lifecycle state remains in the v1 format but is not authoritative after relation resolution.
    let basis = bytes[cursor];
    cursor += 1;
    let mut bloom = [0u8; BLOOM_BYTES];
    bloom.copy_from_slice(&bytes[cursor..]);

    Ok(IndexEntry {
        id,
        kind_hash,
        source_hash,
        basis,
        bloom,
    })
}

fn read_u64(bytes: &[u8], cursor: &mut usize) -> u64 {
    let mut value = [0u8; 8];
    value.copy_from_slice(&bytes[*cursor..*cursor + 8]);
    *cursor += 8;
    u64::from_le_bytes(value)
}

fn entry_matches_query(
    entry: &IndexEntry,
    query: &KnowledgeQuery,
    normalized_text: Option<&str>,
    normalized_scope: Option<&str>,
    normalized_key: Option<&str>,
) -> bool {
    if query
        .kind
        .as_ref()
        .is_some_and(|kind| stable_hash(kind.as_bytes()) != entry.kind_hash)
    {
        return false;
    }
    if query
        .source
        .as_ref()
        .is_some_and(|source| stable_hash(source.as_bytes()) != entry.source_hash)
    {
        return false;
    }
    if query
        .provenance_basis
        .is_some_and(|basis| basis_code(basis) != entry.basis)
    {
        return false;
    }
    if normalized_scope.is_some_and(|scope| !bloom_may_contain(&entry.bloom, scope.as_bytes())) {
        return false;
    }
    if normalized_key.is_some_and(|key| !bloom_may_contain(&entry.bloom, key.as_bytes())) {
        return false;
    }

    normalized_text.is_none_or(|text| bloom_may_contain(&entry.bloom, text.as_bytes()))
}

fn build_bloom(record: &KnowledgeRecord) -> [u8; BLOOM_BYTES] {
    let mut bloom = [0u8; BLOOM_BYTES];
    for value in [
        Some(record.id.as_str()),
        Some(record.content.as_str()),
        Some(record.kind.as_str()),
        Some(record.source.as_str()),
        record.scope.as_deref(),
        record.key.as_deref(),
        record.provenance.detail.as_deref(),
    ]
    .into_iter()
    .flatten()
    {
        let normalized = value.to_lowercase();
        add_all_grams(&mut bloom, normalized.as_bytes());
    }
    bloom
}

fn add_all_grams(bloom: &mut [u8; BLOOM_BYTES], bytes: &[u8]) {
    for width in 1..=bytes.len().min(3) {
        for gram in bytes.windows(width) {
            bloom_insert(bloom, gram);
        }
    }
}

fn bloom_may_contain(bloom: &[u8; BLOOM_BYTES], query: &[u8]) -> bool {
    if query.is_empty() {
        return true;
    }

    let width = query.len().min(3);
    query.windows(width).all(|gram| bloom_contains(bloom, gram))
}

fn bloom_insert(bloom: &mut [u8; BLOOM_BYTES], bytes: &[u8]) {
    for position in bloom_positions(bytes) {
        bloom[position / 8] |= 1 << (position % 8);
    }
}

fn bloom_contains(bloom: &[u8; BLOOM_BYTES], bytes: &[u8]) -> bool {
    bloom_positions(bytes)
        .into_iter()
        .all(|position| bloom[position / 8] & (1 << (position % 8)) != 0)
}

fn bloom_positions(bytes: &[u8]) -> [usize; 4] {
    let first = stable_hash_with_seed(0xcbf29ce484222325, bytes);
    let second = stable_hash_with_seed(0x9e3779b97f4a7c15, bytes) | 1;
    std::array::from_fn(|index| {
        first
            .wrapping_add((index as u64).wrapping_mul(second))
            .wrapping_rem(BLOOM_BITS) as usize
    })
}

fn stable_hash(bytes: &[u8]) -> u64 {
    stable_hash_with_seed(0xcbf29ce484222325, bytes)
}

fn stable_hash_with_seed(mut hash: u64, bytes: &[u8]) -> u64 {
    for byte in bytes {
        hash ^= u64::from(*byte);
        hash = hash.wrapping_mul(0x100000001b3);
    }
    hash
}

fn state_code(state: KnowledgeState) -> u8 {
    match state {
        KnowledgeState::Active => 1,
        KnowledgeState::Stale => 2,
        KnowledgeState::Conflicted => 3,
        KnowledgeState::Superseded => 4,
        KnowledgeState::Unknown => 5,
    }
}

fn basis_code(basis: ProvenanceBasis) -> u8 {
    match basis {
        ProvenanceBasis::Observed => 1,
        ProvenanceBasis::Inferred => 2,
    }
}

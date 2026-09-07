use std::{fs, fs::File, io, io::Read, io::Write};

use synapse_core::knowledge::{KnowledgeRecord, KnowledgeRelation, KnowledgeRelationKind};

use super::{
    create_private_dir_all, create_temporary_file, evolution, format, index, record_file_name,
    record_id_from_file_name, FileStore, StoreError, MAX_RECORD_BYTES,
};

pub(super) const SUCCESSOR_DIRECTORY: &str = "successors-v1";
const MAX_SUCCESSOR_BYTES: usize = MAX_RECORD_BYTES + 32 * 1024;
const MAX_SUCCESSOR_COMMITS: usize = 4_096;

#[derive(Debug, Clone)]
pub(super) struct SuccessorCommit {
    pub record: KnowledgeRecord,
    pub relation: KnowledgeRelation,
}

pub(super) fn validate_shape(
    record: &KnowledgeRecord,
    relation: &KnowledgeRelation,
) -> Result<(), StoreError> {
    if relation.kind != KnowledgeRelationKind::Supersedes {
        return Err(StoreError::InvalidSuccessor {
            reason: "atomic successor relation must use kind 'supersedes'".to_owned(),
        });
    }
    if relation.subject_id != record.id {
        return Err(StoreError::InvalidSuccessor {
            reason: "atomic successor relation subject must equal the new record id".to_owned(),
        });
    }
    if relation.source != record.source {
        return Err(StoreError::InvalidSuccessor {
            reason: "atomic successor record and relation must use the same source".to_owned(),
        });
    }
    validate_component_sizes(record, relation)
}

fn validate_component_sizes(
    record: &KnowledgeRecord,
    relation: &KnowledgeRelation,
) -> Result<(), StoreError> {
    if format::serialize_record(record)?.len() > MAX_RECORD_BYTES {
        return Err(StoreError::RecordTooLarge {
            max_bytes: MAX_RECORD_BYTES,
        });
    }
    if format::serialize_relation(relation)?.len() > evolution::MAX_RELATION_BYTES {
        return Err(StoreError::RelationTooLarge {
            max_bytes: evolution::MAX_RELATION_BYTES,
        });
    }
    Ok(())
}

pub(super) fn insert(
    store: &FileStore,
    record: &KnowledgeRecord,
    relation: &KnowledgeRelation,
) -> Result<(), StoreError> {
    validate_shape(record, relation)?;
    record_file_name(&record.id)?;
    record_file_name(&relation.id)?;

    if store.read_record_with_serialized(&record.id)?.is_some() {
        return Err(StoreError::AlreadyExists {
            id: record.id.clone(),
        });
    }
    if store.get(&relation.object_id)?.is_none() {
        return Err(StoreError::RelationEndpointMissing {
            id: relation.object_id.clone(),
        });
    }

    let serialized = format::serialize_successor(record, relation)?;
    if serialized.len() > MAX_SUCCESSOR_BYTES {
        return Err(StoreError::SuccessorTooLarge {
            max_bytes: MAX_SUCCESSOR_BYTES,
        });
    }

    let relations_dir = store.relations_dir();
    create_private_dir_all(&relations_dir)?;
    let _relation_lock = evolution::acquire_relation_mutation_lock(&relations_dir)?;
    let graph = evolution::RelationGraph::load(store)?;
    if graph.contains_relation_id(&relation.id) {
        return Err(StoreError::RelationAlreadyExists {
            id: relation.id.clone(),
        });
    }
    if graph.relation_count() >= evolution::MAX_RELATIONS {
        return Err(StoreError::RelationCapacityExceeded {
            max_relations: evolution::MAX_RELATIONS,
        });
    }
    if graph.would_create_supersession_cycle(&relation.subject_id, &relation.object_id) {
        return Err(StoreError::SupersessionCycle);
    }

    index::ensure_ready(store)?;
    index::publish_record_entry(store, record, &serialized)?;

    let directory = store.successors_dir();
    create_private_dir_all(&directory)?;
    let final_path = directory.join(record_file_name(&record.id)?);
    let (temporary_path, mut temporary_file) = create_temporary_file(&directory)?;
    let result = (|| -> Result<(), StoreError> {
        temporary_file.write_all(&serialized)?;
        temporary_file.sync_all()?;
        drop(temporary_file);

        match fs::hard_link(&temporary_path, &final_path) {
            Ok(()) => Ok(()),
            Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {
                index::reconcile_duplicate_record_entry(store, record, &serialized)?;
                Err(StoreError::AlreadyExists {
                    id: record.id.clone(),
                })
            }
            Err(error) => Err(StoreError::Io(error)),
        }
    })();

    let _ = fs::remove_file(&temporary_path);
    result
}

pub(super) fn read(
    store: &FileStore,
    id: &str,
) -> Result<Option<(SuccessorCommit, Vec<u8>)>, StoreError> {
    let path = store.successors_dir().join(record_file_name(id)?);
    let file = match File::open(path) {
        Ok(file) => file,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(StoreError::Io(error)),
    };

    if file.metadata()?.len() > MAX_SUCCESSOR_BYTES as u64 {
        return Err(StoreError::SuccessorTooLarge {
            max_bytes: MAX_SUCCESSOR_BYTES,
        });
    }
    let mut serialized = Vec::new();
    file.take((MAX_SUCCESSOR_BYTES + 1) as u64)
        .read_to_end(&mut serialized)?;
    if serialized.len() > MAX_SUCCESSOR_BYTES {
        return Err(StoreError::SuccessorTooLarge {
            max_bytes: MAX_SUCCESSOR_BYTES,
        });
    }

    let (record, relation) = format::deserialize_successor(id, &serialized)?;
    validate_shape(&record, &relation).map_err(|error| StoreError::CorruptSuccessor {
        id: id.to_owned(),
        reason: error.to_string(),
    })?;
    if record.id != id {
        return Err(StoreError::CorruptSuccessor {
            id: id.to_owned(),
            reason: format!(
                "successor commit contains mismatched record id '{}'",
                record.id
            ),
        });
    }

    Ok(Some((SuccessorCommit { record, relation }, serialized)))
}

pub(super) fn record_ids(store: &FileStore) -> Result<Vec<String>, StoreError> {
    let entries = match fs::read_dir(store.successors_dir()) {
        Ok(entries) => entries,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(error) => return Err(StoreError::Io(error)),
    };

    let mut ids = Vec::new();
    for entry in entries {
        let entry = entry?;
        if !entry.file_type()?.is_file() {
            continue;
        }
        let Some(id) = record_id_from_file_name(&entry.file_name()) else {
            continue;
        };
        ids.push(id);
        if ids.len() > MAX_SUCCESSOR_COMMITS {
            return Err(StoreError::RelationCapacityExceeded {
                max_relations: MAX_SUCCESSOR_COMMITS,
            });
        }
    }
    Ok(ids)
}

pub(super) fn relations(store: &FileStore) -> Result<Vec<KnowledgeRelation>, StoreError> {
    let ids = record_ids(store)?;
    let mut relations = Vec::with_capacity(ids.len());
    for id in ids {
        let commit = read(store, &id)?.ok_or_else(|| StoreError::CorruptSuccessor {
            id: id.clone(),
            reason: "successor commit disappeared while loading evolution".to_owned(),
        })?;
        relations.push(commit.0.relation);
    }
    Ok(relations)
}

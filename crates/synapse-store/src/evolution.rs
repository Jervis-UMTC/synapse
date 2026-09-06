use std::{
    collections::{HashMap, HashSet},
    fs::{self, File},
    io::{self, Read, Write},
};

use synapse_core::knowledge::{
    KnowledgeRecord, KnowledgeRelation, KnowledgeRelationKind, KnowledgeState,
};

use super::{
    create_private_dir_all, create_temporary_file, record_file_name, record_id_from_file_name,
    FileStore, KnowledgeStatus, StoreError,
};

const MAX_RELATION_BYTES: usize = 16 * 1024;
const MAX_RELATIONS: usize = 4_096;

#[derive(Debug, Default)]
pub(super) struct RelationGraph {
    superseded_by: HashMap<String, Vec<KnowledgeRelation>>,
    conflicts_with: HashMap<String, Vec<KnowledgeRelation>>,
    supersedes: HashMap<String, Vec<String>>,
}

impl RelationGraph {
    pub(super) fn load(store: &FileStore) -> Result<Self, StoreError> {
        let entries = match fs::read_dir(store.relations_dir()) {
            Ok(entries) => entries,
            Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(Self::default()),
            Err(error) => return Err(StoreError::Io(error)),
        };

        let mut graph = Self::default();
        let mut count = 0usize;
        for entry in entries {
            let entry = entry?;
            if !entry.file_type()?.is_file() {
                continue;
            }
            let Some(id) = record_id_from_file_name(&entry.file_name()) else {
                continue;
            };

            count += 1;
            if count > MAX_RELATIONS {
                return Err(StoreError::RelationCapacityExceeded {
                    max_relations: MAX_RELATIONS,
                });
            }

            let relation =
                read_relation(store, &id)?.ok_or_else(|| StoreError::CorruptRelation {
                    id: id.clone(),
                    reason: "relation disappeared while loading the evolution graph".to_owned(),
                })?;
            require_endpoint_path(store, &relation.subject_id, &relation.id)?;
            require_endpoint_path(store, &relation.object_id, &relation.id)?;
            graph.add(relation);
        }

        if graph.has_supersession_cycle() {
            return Err(StoreError::SupersessionCycle);
        }
        Ok(graph)
    }

    pub(super) fn effective_state(&self, record: &KnowledgeRecord) -> KnowledgeState {
        if self
            .superseded_by
            .get(&record.id)
            .is_some_and(|relations| !relations.is_empty())
        {
            return KnowledgeState::Superseded;
        }
        if self
            .conflicts_with
            .get(&record.id)
            .is_some_and(|relations| !relations.is_empty())
        {
            return KnowledgeState::Conflicted;
        }
        record.state
    }

    pub(super) fn status(&self, record: KnowledgeRecord) -> KnowledgeStatus {
        let effective_state = self.effective_state(&record);
        let mut superseded_by = self
            .superseded_by
            .get(&record.id)
            .cloned()
            .unwrap_or_default();
        let mut conflicts_with = self
            .conflicts_with
            .get(&record.id)
            .cloned()
            .unwrap_or_default();
        sort_relations(&mut superseded_by);
        sort_relations(&mut conflicts_with);

        KnowledgeStatus {
            record,
            effective_state,
            superseded_by,
            conflicts_with,
        }
    }

    pub(super) fn would_create_supersession_cycle(
        &self,
        subject_id: &str,
        object_id: &str,
    ) -> bool {
        let mut pending = vec![object_id];
        let mut visited = HashSet::new();
        while let Some(id) = pending.pop() {
            if id == subject_id {
                return true;
            }
            if !visited.insert(id) {
                continue;
            }
            if let Some(targets) = self.supersedes.get(id) {
                pending.extend(targets.iter().map(String::as_str));
            }
        }
        false
    }

    fn has_supersession_cycle(&self) -> bool {
        self.supersedes.iter().any(|(subject, objects)| {
            objects
                .iter()
                .any(|object| self.would_create_supersession_cycle(subject, object))
        })
    }

    fn add(&mut self, relation: KnowledgeRelation) {
        match relation.kind {
            KnowledgeRelationKind::Supersedes => {
                self.supersedes
                    .entry(relation.subject_id.clone())
                    .or_default()
                    .push(relation.object_id.clone());
                self.superseded_by
                    .entry(relation.object_id.clone())
                    .or_default()
                    .push(relation);
            }
            KnowledgeRelationKind::ConflictsWith => {
                self.conflicts_with
                    .entry(relation.subject_id.clone())
                    .or_default()
                    .push(relation.clone());
                self.conflicts_with
                    .entry(relation.object_id.clone())
                    .or_default()
                    .push(relation);
            }
        }
    }
}

pub(super) fn insert_relation(
    store: &FileStore,
    relation: &KnowledgeRelation,
) -> Result<(), StoreError> {
    record_file_name(&relation.id)?;
    require_endpoint_record(store, &relation.subject_id)?;
    require_endpoint_record(store, &relation.object_id)?;

    let graph = RelationGraph::load(store)?;
    if relation.kind == KnowledgeRelationKind::Supersedes
        && graph.would_create_supersession_cycle(&relation.subject_id, &relation.object_id)
    {
        return Err(StoreError::SupersessionCycle);
    }

    let serialized = serde_json::to_vec(relation).map_err(|error| StoreError::CorruptRelation {
        id: relation.id.clone(),
        reason: error.to_string(),
    })?;
    if serialized.len() > MAX_RELATION_BYTES {
        return Err(StoreError::RelationTooLarge {
            max_bytes: MAX_RELATION_BYTES,
        });
    }

    let directory = store.relations_dir();
    create_private_dir_all(&directory)?;
    let final_path = directory.join(record_file_name(&relation.id)?);
    let (temporary_path, mut temporary_file) = create_temporary_file(&directory)?;
    let result = (|| -> Result<(), StoreError> {
        temporary_file.write_all(&serialized)?;
        temporary_file.sync_all()?;
        drop(temporary_file);

        match fs::hard_link(&temporary_path, &final_path) {
            Ok(()) => Ok(()),
            Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {
                Err(StoreError::RelationAlreadyExists {
                    id: relation.id.clone(),
                })
            }
            Err(error) => Err(StoreError::Io(error)),
        }
    })();

    let _ = fs::remove_file(&temporary_path);
    result
}

fn read_relation(store: &FileStore, id: &str) -> Result<Option<KnowledgeRelation>, StoreError> {
    let path = store.relations_dir().join(record_file_name(id)?);
    let file = match File::open(path) {
        Ok(file) => file,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(StoreError::Io(error)),
    };

    if file.metadata()?.len() > MAX_RELATION_BYTES as u64 {
        return Err(StoreError::RelationTooLarge {
            max_bytes: MAX_RELATION_BYTES,
        });
    }

    let mut serialized = Vec::new();
    file.take((MAX_RELATION_BYTES + 1) as u64)
        .read_to_end(&mut serialized)?;
    if serialized.len() > MAX_RELATION_BYTES {
        return Err(StoreError::RelationTooLarge {
            max_bytes: MAX_RELATION_BYTES,
        });
    }

    let relation: KnowledgeRelation =
        serde_json::from_slice(&serialized).map_err(|error| StoreError::CorruptRelation {
            id: id.to_owned(),
            reason: error.to_string(),
        })?;
    if relation.id != id {
        return Err(StoreError::CorruptRelation {
            id: id.to_owned(),
            reason: format!("relation contains mismatched id '{}'", relation.id),
        });
    }

    Ok(Some(relation))
}

fn require_endpoint_record(store: &FileStore, id: &str) -> Result<(), StoreError> {
    if store.get(id)?.is_some() {
        return Ok(());
    }
    Err(StoreError::RelationEndpointMissing { id: id.to_owned() })
}

fn require_endpoint_path(
    store: &FileStore,
    endpoint_id: &str,
    relation_id: &str,
) -> Result<(), StoreError> {
    let path = store.records_dir().join(record_file_name(endpoint_id)?);
    match fs::metadata(path) {
        Ok(metadata) if metadata.is_file() => Ok(()),
        Ok(_) => Err(StoreError::CorruptRelation {
            id: relation_id.to_owned(),
            reason: format!("endpoint '{endpoint_id}' is not a record file"),
        }),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Err(StoreError::CorruptRelation {
            id: relation_id.to_owned(),
            reason: format!("endpoint '{endpoint_id}' does not exist"),
        }),
        Err(error) => Err(StoreError::Io(error)),
    }
}

fn sort_relations(relations: &mut [KnowledgeRelation]) {
    relations.sort_by(|left, right| {
        right
            .created_at_unix_ms
            .cmp(&left.created_at_unix_ms)
            .then_with(|| left.id.cmp(&right.id))
    });
}

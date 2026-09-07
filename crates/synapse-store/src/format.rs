use serde::{Deserialize, Serialize};
use serde_json::Value;
use synapse_core::knowledge::{KnowledgeRecord, KnowledgeRelation};

use super::StoreError;

const LEGACY_KNOWLEDGE_SCHEMA_VERSION: u64 = 1;
const ADDRESSED_RECORD_SCHEMA_VERSION: u64 = 2;
const RELATION_SCHEMA_VERSION: u64 = 1;

#[derive(Serialize)]
struct RecordEnvelope<'a> {
    schema_version: u64,
    record: &'a KnowledgeRecord,
}

#[derive(Deserialize)]
struct OwnedRecordEnvelope {
    #[serde(rename = "schema_version")]
    _schema_version: u64,
    record: KnowledgeRecord,
}

#[derive(Serialize)]
struct RelationEnvelope<'a> {
    schema_version: u64,
    relation: &'a KnowledgeRelation,
}

#[derive(Deserialize)]
struct OwnedRelationEnvelope {
    #[serde(rename = "schema_version")]
    _schema_version: u64,
    relation: KnowledgeRelation,
}

#[derive(Serialize)]
struct SuccessorEnvelope<'a> {
    schema_version: u64,
    record: &'a KnowledgeRecord,
    relation: &'a KnowledgeRelation,
}

#[derive(Deserialize)]
struct OwnedSuccessorEnvelope {
    #[serde(rename = "schema_version")]
    _schema_version: u64,
    record: KnowledgeRecord,
    relation: KnowledgeRelation,
}

pub(super) fn serialize_record(record: &KnowledgeRecord) -> Result<Vec<u8>, StoreError> {
    serde_json::to_vec(&RecordEnvelope {
        schema_version: record_schema_version(record),
        record,
    })
    .map_err(|error| StoreError::CorruptRecord {
        id: record.id.clone(),
        reason: error.to_string(),
    })
}

pub(super) fn deserialize_record(id: &str, bytes: &[u8]) -> Result<KnowledgeRecord, StoreError> {
    let value: Value =
        serde_json::from_slice(bytes).map_err(|error| StoreError::CorruptRecord {
            id: id.to_owned(),
            reason: error.to_string(),
        })?;

    if let Some(raw_version) = value.get("schema_version") {
        let version = raw_version
            .as_u64()
            .ok_or_else(|| StoreError::CorruptRecord {
                id: id.to_owned(),
                reason: "record schema_version must be an unsigned integer".to_owned(),
            })?;
        if !matches!(
            version,
            LEGACY_KNOWLEDGE_SCHEMA_VERSION | ADDRESSED_RECORD_SCHEMA_VERSION
        ) {
            return Err(StoreError::CorruptRecord {
                id: id.to_owned(),
                reason: format!(
                    "unsupported record schema version {version}; expected {LEGACY_KNOWLEDGE_SCHEMA_VERSION} or {ADDRESSED_RECORD_SCHEMA_VERSION}"
                ),
            });
        }
        let envelope: OwnedRecordEnvelope =
            serde_json::from_value(value).map_err(|error| StoreError::CorruptRecord {
                id: id.to_owned(),
                reason: error.to_string(),
            })?;
        validate_record_version(id, version, &envelope.record)?;
        return Ok(envelope.record);
    }

    let record: KnowledgeRecord =
        serde_json::from_value(value).map_err(|error| StoreError::CorruptRecord {
            id: id.to_owned(),
            reason: error.to_string(),
        })?;
    validate_record_version(id, LEGACY_KNOWLEDGE_SCHEMA_VERSION, &record)?;
    Ok(record)
}

pub(super) fn serialize_relation(relation: &KnowledgeRelation) -> Result<Vec<u8>, StoreError> {
    serde_json::to_vec(&RelationEnvelope {
        schema_version: RELATION_SCHEMA_VERSION,
        relation,
    })
    .map_err(|error| StoreError::CorruptRelation {
        id: relation.id.clone(),
        reason: error.to_string(),
    })
}

pub(super) fn deserialize_relation(
    id: &str,
    bytes: &[u8],
) -> Result<KnowledgeRelation, StoreError> {
    let value: Value =
        serde_json::from_slice(bytes).map_err(|error| StoreError::CorruptRelation {
            id: id.to_owned(),
            reason: error.to_string(),
        })?;

    if let Some(raw_version) = value.get("schema_version") {
        let version = raw_version
            .as_u64()
            .ok_or_else(|| StoreError::CorruptRelation {
                id: id.to_owned(),
                reason: "relation schema_version must be an unsigned integer".to_owned(),
            })?;
        if version != RELATION_SCHEMA_VERSION {
            return Err(StoreError::CorruptRelation {
                id: id.to_owned(),
                reason: format!(
                    "unsupported relation schema version {version}; expected {RELATION_SCHEMA_VERSION}"
                ),
            });
        }
        let envelope: OwnedRelationEnvelope =
            serde_json::from_value(value).map_err(|error| StoreError::CorruptRelation {
                id: id.to_owned(),
                reason: error.to_string(),
            })?;
        return Ok(envelope.relation);
    }

    serde_json::from_value(value).map_err(|error| StoreError::CorruptRelation {
        id: id.to_owned(),
        reason: error.to_string(),
    })
}

pub(super) fn serialize_successor(
    record: &KnowledgeRecord,
    relation: &KnowledgeRelation,
) -> Result<Vec<u8>, StoreError> {
    serde_json::to_vec(&SuccessorEnvelope {
        schema_version: record_schema_version(record),
        record,
        relation,
    })
    .map_err(|error| StoreError::CorruptSuccessor {
        id: record.id.clone(),
        reason: error.to_string(),
    })
}

pub(super) fn deserialize_successor(
    id: &str,
    bytes: &[u8],
) -> Result<(KnowledgeRecord, KnowledgeRelation), StoreError> {
    let value: Value =
        serde_json::from_slice(bytes).map_err(|error| StoreError::CorruptSuccessor {
            id: id.to_owned(),
            reason: error.to_string(),
        })?;
    let raw_version = value
        .get("schema_version")
        .ok_or_else(|| StoreError::CorruptSuccessor {
            id: id.to_owned(),
            reason: "successor commit must include schema_version".to_owned(),
        })?;
    let version = raw_version
        .as_u64()
        .ok_or_else(|| StoreError::CorruptSuccessor {
            id: id.to_owned(),
            reason: "successor schema_version must be an unsigned integer".to_owned(),
        })?;
    if !matches!(
        version,
        LEGACY_KNOWLEDGE_SCHEMA_VERSION | ADDRESSED_RECORD_SCHEMA_VERSION
    ) {
        return Err(StoreError::CorruptSuccessor {
            id: id.to_owned(),
            reason: format!(
                "unsupported successor schema version {version}; expected {LEGACY_KNOWLEDGE_SCHEMA_VERSION} or {ADDRESSED_RECORD_SCHEMA_VERSION}"
            ),
        });
    }

    let envelope: OwnedSuccessorEnvelope =
        serde_json::from_value(value).map_err(|error| StoreError::CorruptSuccessor {
            id: id.to_owned(),
            reason: error.to_string(),
        })?;
    validate_record_version(id, version, &envelope.record).map_err(|error| {
        StoreError::CorruptSuccessor {
            id: id.to_owned(),
            reason: error.to_string(),
        }
    })?;
    Ok((envelope.record, envelope.relation))
}

fn record_schema_version(record: &KnowledgeRecord) -> u64 {
    if record.scope.is_some() {
        ADDRESSED_RECORD_SCHEMA_VERSION
    } else {
        LEGACY_KNOWLEDGE_SCHEMA_VERSION
    }
}

fn validate_record_version(
    id: &str,
    version: u64,
    record: &KnowledgeRecord,
) -> Result<(), StoreError> {
    if version == LEGACY_KNOWLEDGE_SCHEMA_VERSION
        && (record.scope.is_some() || record.key.is_some())
    {
        return Err(StoreError::CorruptRecord {
            id: id.to_owned(),
            reason: "schema version 1 records cannot contain scope/key addressing fields"
                .to_owned(),
        });
    }
    Ok(())
}

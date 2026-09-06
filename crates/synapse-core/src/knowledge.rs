use serde::{Deserialize, Serialize};
use std::fmt;

/// A durable unit of machine knowledge that can be exchanged between local clients.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(try_from = "UncheckedKnowledgeRecord")]
pub struct KnowledgeRecord {
    pub id: String,
    pub content: String,
    pub kind: String,
    pub source: String,
    pub created_at_unix_ms: u64,
    pub confidence: Confidence,
    pub state: KnowledgeState,
    pub provenance: Provenance,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Confidence {
    Unknown,
    Low,
    Medium,
    High,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum KnowledgeState {
    Active,
    Stale,
    Conflicted,
    Superseded,
    Unknown,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Provenance {
    pub basis: ProvenanceBasis,
    pub detail: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ProvenanceBasis {
    Observed,
    Inferred,
}

/// An append-only assertion describing how two knowledge records relate over time.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(try_from = "UncheckedKnowledgeRelation")]
pub struct KnowledgeRelation {
    pub id: String,
    pub subject_id: String,
    pub kind: KnowledgeRelationKind,
    pub object_id: String,
    pub source: String,
    pub created_at_unix_ms: u64,
    pub provenance: Provenance,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum KnowledgeRelationKind {
    Supersedes,
    ConflictsWith,
}

#[derive(Debug, Deserialize)]
struct UncheckedKnowledgeRelation {
    id: String,
    subject_id: String,
    kind: KnowledgeRelationKind,
    object_id: String,
    source: String,
    created_at_unix_ms: u64,
    provenance: Provenance,
}

#[derive(Debug, Deserialize)]
struct UncheckedKnowledgeRecord {
    id: String,
    content: String,
    kind: String,
    source: String,
    created_at_unix_ms: u64,
    confidence: Confidence,
    state: KnowledgeState,
    provenance: Provenance,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct InvalidKnowledgeRecord {
    pub field: &'static str,
}

impl fmt::Display for InvalidKnowledgeRecord {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            formatter,
            "knowledge record field '{}' must not be empty",
            self.field
        )
    }
}

impl std::error::Error for InvalidKnowledgeRecord {}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct InvalidKnowledgeRelation {
    pub field: &'static str,
    pub reason: &'static str,
}

impl fmt::Display for InvalidKnowledgeRelation {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            formatter,
            "knowledge relation field '{}' {}",
            self.field, self.reason
        )
    }
}

impl std::error::Error for InvalidKnowledgeRelation {}

impl TryFrom<UncheckedKnowledgeRecord> for KnowledgeRecord {
    type Error = InvalidKnowledgeRecord;

    fn try_from(record: UncheckedKnowledgeRecord) -> Result<Self, Self::Error> {
        Self::new(
            record.id,
            record.content,
            record.kind,
            record.source,
            record.created_at_unix_ms,
            record.confidence,
            record.state,
            record.provenance,
        )
    }
}

impl TryFrom<UncheckedKnowledgeRelation> for KnowledgeRelation {
    type Error = InvalidKnowledgeRelation;

    fn try_from(relation: UncheckedKnowledgeRelation) -> Result<Self, Self::Error> {
        Self::new(
            relation.id,
            relation.subject_id,
            relation.kind,
            relation.object_id,
            relation.source,
            relation.created_at_unix_ms,
            relation.provenance,
        )
    }
}

impl KnowledgeRelation {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        id: impl Into<String>,
        subject_id: impl Into<String>,
        kind: KnowledgeRelationKind,
        object_id: impl Into<String>,
        source: impl Into<String>,
        created_at_unix_ms: u64,
        provenance: Provenance,
    ) -> Result<Self, InvalidKnowledgeRelation> {
        let relation = Self {
            id: id.into(),
            subject_id: subject_id.into(),
            kind,
            object_id: object_id.into(),
            source: source.into(),
            created_at_unix_ms,
            provenance,
        };

        for (field, value) in [
            ("id", relation.id.as_str()),
            ("subject_id", relation.subject_id.as_str()),
            ("object_id", relation.object_id.as_str()),
            ("source", relation.source.as_str()),
        ] {
            if value.trim().is_empty() {
                return Err(InvalidKnowledgeRelation {
                    field,
                    reason: "must not be empty",
                });
            }
        }

        if relation.subject_id == relation.object_id {
            return Err(InvalidKnowledgeRelation {
                field: "object_id",
                reason: "must reference a different record than subject_id",
            });
        }

        Ok(relation)
    }
}

impl KnowledgeRecord {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        id: impl Into<String>,
        content: impl Into<String>,
        kind: impl Into<String>,
        source: impl Into<String>,
        created_at_unix_ms: u64,
        confidence: Confidence,
        state: KnowledgeState,
        provenance: Provenance,
    ) -> Result<Self, InvalidKnowledgeRecord> {
        let record = Self {
            id: id.into(),
            content: content.into(),
            kind: kind.into(),
            source: source.into(),
            created_at_unix_ms,
            confidence,
            state,
            provenance,
        };

        for (field, value) in [
            ("id", record.id.as_str()),
            ("content", record.content.as_str()),
            ("kind", record.kind.as_str()),
            ("source", record.source.as_str()),
        ] {
            if value.trim().is_empty() {
                return Err(InvalidKnowledgeRecord { field });
            }
        }

        Ok(record)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn observed_record() -> KnowledgeRecord {
        KnowledgeRecord::new(
            "record-1",
            "Rust toolchain is installed at /usr/bin/rustc",
            "fact",
            "system-inspector",
            1_788_707_200_000,
            Confidence::High,
            KnowledgeState::Active,
            Provenance {
                basis: ProvenanceBasis::Observed,
                detail: Some("rustc path read from the local machine".to_owned()),
            },
        )
        .expect("fixture should be valid")
    }

    #[test]
    fn creates_a_knowledge_record_with_provenance() {
        let record = observed_record();

        assert_eq!(record.id, "record-1");
        assert_eq!(record.kind, "fact");
        assert_eq!(record.source, "system-inspector");
        assert_eq!(record.confidence, Confidence::High);
        assert_eq!(record.state, KnowledgeState::Active);
        assert_eq!(record.provenance.basis, ProvenanceBasis::Observed);
    }

    #[test]
    fn rejects_empty_required_fields() {
        let result = KnowledgeRecord::new(
            "record-1",
            "   ",
            "fact",
            "system-inspector",
            1_788_707_200_000,
            Confidence::High,
            KnowledgeState::Active,
            Provenance {
                basis: ProvenanceBasis::Observed,
                detail: None,
            },
        );

        assert_eq!(result, Err(InvalidKnowledgeRecord { field: "content" }));
    }

    #[test]
    fn distinguishes_observed_from_inferred_knowledge() {
        let mut inferred = observed_record();
        inferred.provenance.basis = ProvenanceBasis::Inferred;

        assert_ne!(inferred.provenance.basis, ProvenanceBasis::Observed);
    }

    #[test]
    fn serializes_and_deserializes_without_losing_information() {
        let record = observed_record();

        let json = serde_json::to_string(&record).expect("knowledge record should serialize");
        let decoded: KnowledgeRecord =
            serde_json::from_str(&json).expect("knowledge record should deserialize");

        assert_eq!(decoded, record);
        assert!(json.contains("\"basis\":\"observed\""));
        assert!(json.contains("\"state\":\"active\""));
    }

    #[test]
    fn relation_preserves_evolution_provenance() {
        let relation = KnowledgeRelation::new(
            "relation-1",
            "record-2",
            KnowledgeRelationKind::Supersedes,
            "record-1",
            "machine-inspector",
            1_788_707_300_000,
            Provenance {
                basis: ProvenanceBasis::Observed,
                detail: Some("toolchain was re-read from disk".to_owned()),
            },
        )
        .expect("relation fixture should be valid");

        let json = serde_json::to_string(&relation).expect("relation should serialize");
        let decoded: KnowledgeRelation =
            serde_json::from_str(&json).expect("relation should deserialize");

        assert_eq!(decoded, relation);
        assert_eq!(decoded.kind, KnowledgeRelationKind::Supersedes);
        assert_eq!(decoded.provenance.basis, ProvenanceBasis::Observed);
    }

    #[test]
    fn relation_rejects_self_reference() {
        let error = KnowledgeRelation::new(
            "relation-1",
            "record-1",
            KnowledgeRelationKind::ConflictsWith,
            "record-1",
            "reviewer",
            1_788_707_300_000,
            Provenance {
                basis: ProvenanceBasis::Inferred,
                detail: None,
            },
        )
        .expect_err("self-relation should be rejected");

        assert_eq!(
            error,
            InvalidKnowledgeRelation {
                field: "object_id",
                reason: "must reference a different record than subject_id"
            }
        );
    }

    #[test]
    fn deserialization_rejects_invalid_records() {
        let json = r#"{
            "id":"record-1",
            "content":" ",
            "kind":"fact",
            "source":"test-client",
            "created_at_unix_ms":1788707200000,
            "confidence":"low",
            "state":"active",
            "provenance":{"basis":"inferred","detail":null}
        }"#;

        let error = serde_json::from_str::<KnowledgeRecord>(json)
            .expect_err("invalid serialized records must be rejected");

        assert!(error
            .to_string()
            .contains("field 'content' must not be empty"));
    }
}

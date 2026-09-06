use std::{collections::HashSet, fmt, fs::File, io::Read, io::Write};

use serde::{Deserialize, Serialize};

use super::{create_private_dir_all, create_temporary_file, FileStore, StoreError};

const AUTHORIZATION_FILE_NAME: &str = "authorization-v1.json";
const AUTHORIZATION_VERSION: u8 = 1;
const MAX_AUTHORIZATION_BYTES: usize = 64 * 1024;
const MAX_AUTHORIZATION_CLIENTS: usize = 64;
const MAX_CLIENT_ID_BYTES: usize = 100;

/// Store-local cooperative authorization policy for authoritative knowledge writes.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(try_from = "UncheckedAuthorizationPolicy")]
pub struct AuthorizationPolicy {
    pub version: u8,
    pub clients: Vec<ClientAuthorization>,
}

/// Permissions associated with one claimed local client identity.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(try_from = "UncheckedClientAuthorization")]
pub struct ClientAuthorization {
    pub id: String,
    pub write_records: bool,
    pub write_relations: bool,
}

#[derive(Debug, Deserialize)]
struct UncheckedAuthorizationPolicy {
    version: u8,
    clients: Vec<ClientAuthorization>,
}

#[derive(Debug, Deserialize)]
struct UncheckedClientAuthorization {
    id: String,
    write_records: bool,
    write_relations: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InvalidAuthorizationPolicy {
    reason: String,
}

impl fmt::Display for InvalidAuthorizationPolicy {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.reason)
    }
}

impl std::error::Error for InvalidAuthorizationPolicy {}

impl ClientAuthorization {
    pub fn new(
        id: impl Into<String>,
        write_records: bool,
        write_relations: bool,
    ) -> Result<Self, InvalidAuthorizationPolicy> {
        let client = Self {
            id: id.into(),
            write_records,
            write_relations,
        };
        validate_client(&client)?;
        Ok(client)
    }
}

impl AuthorizationPolicy {
    pub fn new(clients: Vec<ClientAuthorization>) -> Result<Self, InvalidAuthorizationPolicy> {
        let policy = Self {
            version: AUTHORIZATION_VERSION,
            clients,
        };
        validate_policy(&policy)?;
        Ok(policy)
    }
}

impl TryFrom<UncheckedClientAuthorization> for ClientAuthorization {
    type Error = InvalidAuthorizationPolicy;

    fn try_from(client: UncheckedClientAuthorization) -> Result<Self, Self::Error> {
        Self::new(client.id, client.write_records, client.write_relations)
    }
}

impl TryFrom<UncheckedAuthorizationPolicy> for AuthorizationPolicy {
    type Error = InvalidAuthorizationPolicy;

    fn try_from(policy: UncheckedAuthorizationPolicy) -> Result<Self, Self::Error> {
        let policy = Self {
            version: policy.version,
            clients: policy.clients,
        };
        validate_policy(&policy)?;
        Ok(policy)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum AuthorizationCapability {
    WriteRecords,
    WriteRelations,
}

impl AuthorizationCapability {
    fn name(self) -> &'static str {
        match self {
            Self::WriteRecords => "write_records",
            Self::WriteRelations => "write_relations",
        }
    }
}

pub(super) fn require(
    store: &FileStore,
    client_id: &str,
    capability: AuthorizationCapability,
) -> Result<(), StoreError> {
    let policy = load(store)?;
    let authorized = policy.clients.iter().any(|client| {
        client.id == client_id
            && match capability {
                AuthorizationCapability::WriteRecords => client.write_records,
                AuthorizationCapability::WriteRelations => client.write_relations,
            }
    });

    if authorized {
        return Ok(());
    }

    Err(StoreError::AuthorizationDenied {
        client_id: client_id.to_owned(),
        capability: capability.name(),
    })
}

pub(super) fn initialize(
    store: &FileStore,
    policy: &AuthorizationPolicy,
) -> Result<(), StoreError> {
    validate_policy(policy).map_err(|error| StoreError::InvalidAuthorizationPolicy {
        reason: error.to_string(),
    })?;
    let serialized =
        serde_json::to_vec(policy).map_err(|error| StoreError::InvalidAuthorizationPolicy {
            reason: error.to_string(),
        })?;
    if serialized.len() > MAX_AUTHORIZATION_BYTES {
        return Err(StoreError::AuthorizationPolicyTooLarge {
            max_bytes: MAX_AUTHORIZATION_BYTES,
        });
    }

    create_private_dir_all(store.root())?;
    let final_path = store.root().join(AUTHORIZATION_FILE_NAME);
    let (temporary_path, mut temporary_file) = create_temporary_file(store.root())?;
    let result = (|| -> Result<(), StoreError> {
        temporary_file.write_all(&serialized)?;
        temporary_file.sync_all()?;
        drop(temporary_file);

        match std::fs::hard_link(&temporary_path, &final_path) {
            Ok(()) => Ok(()),
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {
                Err(StoreError::AuthorizationAlreadyConfigured)
            }
            Err(error) => Err(StoreError::Io(error)),
        }
    })();

    let _ = std::fs::remove_file(&temporary_path);
    result
}

pub(super) fn load(store: &FileStore) -> Result<AuthorizationPolicy, StoreError> {
    let path = store.root().join(AUTHORIZATION_FILE_NAME);
    let file = match File::open(path) {
        Ok(file) => file,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return Err(StoreError::AuthorizationNotConfigured)
        }
        Err(error) => return Err(StoreError::Io(error)),
    };

    if file.metadata()?.len() > MAX_AUTHORIZATION_BYTES as u64 {
        return Err(StoreError::AuthorizationPolicyTooLarge {
            max_bytes: MAX_AUTHORIZATION_BYTES,
        });
    }

    let mut serialized = Vec::new();
    file.take((MAX_AUTHORIZATION_BYTES + 1) as u64)
        .read_to_end(&mut serialized)?;
    if serialized.len() > MAX_AUTHORIZATION_BYTES {
        return Err(StoreError::AuthorizationPolicyTooLarge {
            max_bytes: MAX_AUTHORIZATION_BYTES,
        });
    }

    serde_json::from_slice(&serialized).map_err(|error| StoreError::InvalidAuthorizationPolicy {
        reason: error.to_string(),
    })
}

fn validate_client(client: &ClientAuthorization) -> Result<(), InvalidAuthorizationPolicy> {
    if client.id.trim().is_empty() {
        return Err(invalid(
            "authorization client id must contain non-whitespace text",
        ));
    }
    if client.id.len() > MAX_CLIENT_ID_BYTES {
        return Err(invalid(format!(
            "authorization client id exceeds the limit of {MAX_CLIENT_ID_BYTES} bytes"
        )));
    }
    if !client.write_records && !client.write_relations {
        return Err(invalid(format!(
            "authorization client '{}' must have at least one write capability",
            client.id
        )));
    }
    Ok(())
}

fn validate_policy(policy: &AuthorizationPolicy) -> Result<(), InvalidAuthorizationPolicy> {
    if policy.version != AUTHORIZATION_VERSION {
        return Err(invalid(format!(
            "unsupported authorization policy version {}; expected {AUTHORIZATION_VERSION}",
            policy.version
        )));
    }
    if policy.clients.is_empty() {
        return Err(invalid(
            "authorization policy must contain at least one client",
        ));
    }
    if policy.clients.len() > MAX_AUTHORIZATION_CLIENTS {
        return Err(invalid(format!(
            "authorization policy exceeds the limit of {MAX_AUTHORIZATION_CLIENTS} clients"
        )));
    }

    let mut ids = HashSet::with_capacity(policy.clients.len());
    for client in &policy.clients {
        validate_client(client)?;
        if !ids.insert(client.id.as_str()) {
            return Err(invalid(format!(
                "authorization policy contains duplicate client id '{}'",
                client.id
            )));
        }
    }
    Ok(())
}

fn invalid(reason: impl Into<String>) -> InvalidAuthorizationPolicy {
    InvalidAuthorizationPolicy {
        reason: reason.into(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rejects_duplicate_client_ids() {
        let error = AuthorizationPolicy::new(vec![
            ClientAuthorization::new("writer", true, false).expect("grant should be valid"),
            ClientAuthorization::new("writer", false, true).expect("grant should be valid"),
        ])
        .expect_err("duplicate identities must be rejected");

        assert!(error.to_string().contains("duplicate client id"));
    }

    #[test]
    fn deserialization_rejects_unknown_policy_versions() {
        let error = serde_json::from_str::<AuthorizationPolicy>(
            r#"{"version":2,"clients":[{"id":"writer","write_records":true,"write_relations":false}]}"#,
        )
        .expect_err("unknown versions must fail closed");

        assert!(error
            .to_string()
            .contains("unsupported authorization policy version"));
    }
}

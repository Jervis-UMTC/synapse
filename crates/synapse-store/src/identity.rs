use std::{fs::File, io::Read, io::Write};

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use super::{create_private_dir_all, create_temporary_file, FileStore, StoreError};

const STORE_IDENTITY_FILE_NAME: &str = "store-identity-v1.json";
const STORE_IDENTITY_VERSION: u8 = 1;
const MAX_STORE_IDENTITY_BYTES: usize = 256;
const STORE_ID_HEX_BYTES: usize = 64;

#[derive(Debug, Serialize, Deserialize)]
struct StoreIdentityFile {
    version: u8,
    id: String,
}

pub(super) fn load_or_initialize(store: &FileStore) -> Result<String, StoreError> {
    if let Some(identity) = load(store)? {
        return Ok(identity.id);
    }

    create_private_dir_all(store.root())?;
    let candidate = StoreIdentityFile {
        version: STORE_IDENTITY_VERSION,
        id: legacy_path_seed(store)?,
    };
    validate(&candidate)?;
    let serialized =
        serde_json::to_vec(&candidate).map_err(|error| StoreError::InvalidStoreIdentity {
            reason: error.to_string(),
        })?;
    if serialized.len() > MAX_STORE_IDENTITY_BYTES {
        return Err(StoreError::StoreIdentityTooLarge {
            max_bytes: MAX_STORE_IDENTITY_BYTES,
        });
    }

    let final_path = store.root().join(STORE_IDENTITY_FILE_NAME);
    let (temporary_path, mut temporary_file) = create_temporary_file(store.root())?;
    let result = (|| -> Result<String, StoreError> {
        temporary_file.write_all(&serialized)?;
        temporary_file.sync_all()?;
        drop(temporary_file);

        match std::fs::hard_link(&temporary_path, &final_path) {
            Ok(()) => Ok(candidate.id.clone()),
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => load(store)?
                .map(|identity| identity.id)
                .ok_or_else(|| StoreError::InvalidStoreIdentity {
                    reason: "store identity disappeared during concurrent initialization"
                        .to_owned(),
                }),
            Err(error) => Err(StoreError::Io(error)),
        }
    })();

    let _ = std::fs::remove_file(&temporary_path);
    result
}

fn load(store: &FileStore) -> Result<Option<StoreIdentityFile>, StoreError> {
    let path = store.root().join(STORE_IDENTITY_FILE_NAME);
    let file = match File::open(path) {
        Ok(file) => file,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(StoreError::Io(error)),
    };

    if file.metadata()?.len() > MAX_STORE_IDENTITY_BYTES as u64 {
        return Err(StoreError::StoreIdentityTooLarge {
            max_bytes: MAX_STORE_IDENTITY_BYTES,
        });
    }

    let mut serialized = Vec::new();
    file.take((MAX_STORE_IDENTITY_BYTES + 1) as u64)
        .read_to_end(&mut serialized)?;
    if serialized.len() > MAX_STORE_IDENTITY_BYTES {
        return Err(StoreError::StoreIdentityTooLarge {
            max_bytes: MAX_STORE_IDENTITY_BYTES,
        });
    }

    let identity: StoreIdentityFile =
        serde_json::from_slice(&serialized).map_err(|error| StoreError::InvalidStoreIdentity {
            reason: error.to_string(),
        })?;
    validate(&identity)?;
    Ok(Some(identity))
}

fn validate(identity: &StoreIdentityFile) -> Result<(), StoreError> {
    if identity.version != STORE_IDENTITY_VERSION {
        return Err(StoreError::InvalidStoreIdentity {
            reason: format!(
                "unsupported store identity version {}; expected {STORE_IDENTITY_VERSION}",
                identity.version
            ),
        });
    }
    if identity.id.len() != STORE_ID_HEX_BYTES
        || !identity
            .id
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
    {
        return Err(StoreError::InvalidStoreIdentity {
            reason: "store id must contain exactly 64 lowercase hexadecimal characters".to_owned(),
        });
    }
    Ok(())
}

fn legacy_path_seed(store: &FileStore) -> Result<String, StoreError> {
    let absolute = if store.root().is_absolute() {
        store.root().to_path_buf()
    } else {
        std::env::current_dir()?.join(store.root())
    };
    let mut hasher = Sha256::new();
    hasher.update(absolute.to_string_lossy().as_bytes());
    Ok(hex_lower(&hasher.finalize()))
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

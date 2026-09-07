use std::{
    fs::{File, OpenOptions},
    io,
    path::Path,
    thread,
    time::{Duration, Instant},
};

use fs2::FileExt;

use super::{create_private_dir_all, FileStore, StoreError};

const STATE_LOCK_FILE_NAME: &str = ".state.lock";
const STATE_LOCK_TIMEOUT: Duration = Duration::from_secs(5);
const STATE_LOCK_RETRY: Duration = Duration::from_millis(5);

pub(super) fn shared(store: &FileStore) -> Result<File, StoreError> {
    acquire(store, LockKind::Shared)
}

pub(super) fn exclusive(store: &FileStore) -> Result<File, StoreError> {
    acquire(store, LockKind::Exclusive)
}

#[derive(Clone, Copy)]
enum LockKind {
    Shared,
    Exclusive,
}

fn acquire(store: &FileStore, kind: LockKind) -> Result<File, StoreError> {
    create_private_dir_all(store.root())?;
    let file = open_state_lock_file(&store.root().join(STATE_LOCK_FILE_NAME))?;
    let deadline = Instant::now() + STATE_LOCK_TIMEOUT;

    loop {
        let result = match kind {
            LockKind::Shared => FileExt::try_lock_shared(&file),
            LockKind::Exclusive => FileExt::try_lock_exclusive(&file),
        };
        match result {
            Ok(()) => return Ok(file),
            Err(error) if lock_is_contended(&error) => {
                if Instant::now() >= deadline {
                    return Err(StoreError::Io(io::Error::new(
                        io::ErrorKind::TimedOut,
                        "timed out waiting for the Synapse state lock",
                    )));
                }
                thread::sleep(STATE_LOCK_RETRY);
            }
            Err(error) => return Err(StoreError::Io(error)),
        }
    }
}

fn lock_is_contended(error: &io::Error) -> bool {
    if error.kind() == io::ErrorKind::WouldBlock {
        return true;
    }

    #[cfg(windows)]
    if matches!(error.raw_os_error(), Some(32 | 33)) {
        return true;
    }

    false
}

#[cfg(unix)]
fn open_state_lock_file(path: &Path) -> io::Result<File> {
    use std::os::unix::fs::OpenOptionsExt;

    let mut options = OpenOptions::new();
    options
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .mode(0o600);
    options.open(path)
}

#[cfg(not(unix))]
fn open_state_lock_file(path: &Path) -> io::Result<File> {
    OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(path)
}

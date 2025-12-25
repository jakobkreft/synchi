use crate::roots::Root;
use anyhow::Result;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Mutex, OnceLock};

pub struct Lock<'a> {
    root: &'a dyn Root,
    path: PathBuf,
    id: usize,
}

impl<'a> Lock<'a> {
    pub fn acquire(root: &'a dyn Root, lock_name: &str, info: &str) -> Result<Self> {
        let path = PathBuf::from(format!(".synchi/{}", lock_name));

        root.mkdirs(Path::new(".synchi"))?;
        root.try_lock(&path, info)?;
        let id = register_active_lock(root, &path);

        Ok(Self { root, path, id })
    }
}

impl<'a> Drop for Lock<'a> {
    fn drop(&mut self) {
        unregister_active_lock(self.id);
        let _ = self.root.unlock(&self.path);
    }
}

struct ActiveLock {
    id: usize,
    cleanup: Box<dyn Fn() + Send + Sync>,
}

fn lock_registry() -> &'static Mutex<Vec<ActiveLock>> {
    static REGISTRY: OnceLock<Mutex<Vec<ActiveLock>>> = OnceLock::new();
    REGISTRY.get_or_init(|| Mutex::new(Vec::new()))
}

static NEXT_LOCK_ID: AtomicUsize = AtomicUsize::new(1);

fn register_active_lock(root: &dyn Root, path: &Path) -> usize {
    let id = NEXT_LOCK_ID.fetch_add(1, Ordering::SeqCst);
    let path_buf = path.to_path_buf();
    let cleanup_root = root.box_clone();
    let cleanup = Box::new(move || {
        if let Err(err) = cleanup_root.unlock(&path_buf) {
            tracing::warn!(
                "Failed to release lock at {}: {err}",
                path_buf.display()
            );
        }
    });
    let mut registry = lock_registry().lock().unwrap();
    registry.push(ActiveLock { id, cleanup });
    id
}

fn unregister_active_lock(id: usize) {
    let mut registry = lock_registry().lock().unwrap();
    if let Some(pos) = registry.iter().position(|entry| entry.id == id) {
        registry.remove(pos);
    }
}

pub fn force_unlock_all() {
    let registry = lock_registry();
    let active = {
        let mut guard = registry.lock().unwrap();
        guard.drain(..).collect::<Vec<_>>()
    };

    for entry in active {
        (entry.cleanup)();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::roots::LocalRoot;
    use tempfile::TempDir;

    #[test]
    fn test_local_lock() -> Result<()> {
        let tmp_dir = TempDir::new()?;
        let root = LocalRoot::new(tmp_dir.path())?;

        {
            let _lock = Lock::acquire(&root, "lock", "mypid")?;
            // Verify lock file exists
            assert!(tmp_dir.path().join(".synchi/lock").exists());

            // Try acquire again
            let res = Lock::acquire(&root, "lock", "mypid2");
            assert!(res.is_err());
        }

        // Lock should be released
        assert!(!tmp_dir.path().join(".synchi/lock").exists());

        // Acquire again
        let _lock2 = Lock::acquire(&root, "lock", "mypid3")?;

        Ok(())
    }
}

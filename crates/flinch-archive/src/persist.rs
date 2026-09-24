//! Replacing state files so no reader ever sees half of one.
//!
//! Every state file is rewritten whole each cycle while the web UI reads them.
//! An interrupted plain overwrite leaves a truncated file, and readers take a
//! file that does not parse as "nothing yet" — for the eviction ledger that is
//! a cycle without recycle-bin credit, which evicts more than the disk needs.
//! Writing a sibling and renaming it over the target makes each replacement
//! all-or-nothing, on a local disk and on the NFS share behind a ReadWriteMany
//! volume alike.
//!
//! State files hold the Plex token and the household's viewing history, so
//! each one is created readable by its owner only.

use std::io::Write;
use std::path::Path;
use std::sync::atomic::{AtomicU64, Ordering};

/// Tells apart the temp files of concurrent writers in one process (the web
/// server writes settings from several requests at once); the pid tells apart
/// the daemon and the web server sharing one state volume.
static NEXT_TEMP: AtomicU64 = AtomicU64::new(0);

/// Replace `path` with `bytes`: write a sibling temp file no other writer can
/// share, flush it to disk, rename it over `path`, then flush the directory so
/// the rename itself survives a crash.
pub fn replace(path: &Path, bytes: &[u8]) -> std::io::Result<()> {
    let name = path.file_name().ok_or_else(|| std::io::Error::other(format!("{} names no file", path.display())))?;
    let mut temp_name = name.to_owned();
    temp_name.push(format!(".{}.{}.tmp", std::process::id(), NEXT_TEMP.fetch_add(1, Ordering::Relaxed)));
    let temp = path.with_file_name(temp_name);
    let written = write_new(&temp, bytes).and_then(|()| std::fs::rename(&temp, path));
    if written.is_err() {
        // Best effort: a leftover temp file is litter, never a state file.
        std::fs::remove_file(&temp).ok();
    }
    written?;
    sync_parent(path);
    Ok(())
}

/// Create `temp` (never reusing an existing file, so a planted symlink is not
/// followed), owner-only on unix, and write `bytes` through to disk.
fn write_new(temp: &Path, bytes: &[u8]) -> std::io::Result<()> {
    let mut options = std::fs::OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let mut file = options.open(temp)?;
    file.write_all(bytes)?;
    file.sync_all()
}

/// Flush the directory entry the rename changed. Best effort: some shares
/// refuse to open a directory, and the file itself is already on disk.
#[cfg(unix)]
fn sync_parent(path: &Path) {
    let dir = match path.parent() {
        Some(dir) if !dir.as_os_str().is_empty() => dir,
        _ => Path::new("."),
    };
    if let Ok(dir) = std::fs::File::open(dir) {
        dir.sync_all().ok();
    }
}

/// Elsewhere a directory cannot be opened for flushing; the rename stands.
#[cfg(not(unix))]
fn sync_parent(_path: &Path) {}

#[cfg(test)]
mod tests {
    use super::replace;

    fn scratch(name: &str) -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!("flinch-persist-{name}-{}", std::process::id()));
        std::fs::remove_dir_all(&dir).ok();
        std::fs::create_dir_all(&dir).expect("scratch dir");
        dir
    }

    #[test]
    fn a_replacement_overwrites_the_target_and_leaves_no_temp_file_behind() {
        let dir = scratch("replace");
        let path = dir.join("settings.json");
        replace(&path, b"first").expect("first write");
        replace(&path, b"second").expect("replacing write");
        assert_eq!(std::fs::read(&path).expect("read back"), b"second");
        let names: Vec<_> = std::fs::read_dir(&dir).expect("list").map(|entry| entry.expect("entry").file_name()).collect();
        assert_eq!(names, ["settings.json"], "every temp file is renamed away");
        std::fs::remove_dir_all(&dir).ok();
    }

    #[cfg(unix)]
    #[test]
    fn a_state_file_is_readable_by_its_owner_only_even_over_a_wider_file() {
        use std::os::unix::fs::PermissionsExt;
        let dir = scratch("mode");
        let path = dir.join("settings.json");
        std::fs::write(&path, b"{}").expect("an older, world-readable file");
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o644)).expect("chmod");
        replace(&path, br#"{"plex_token":"secret"}"#).expect("replace");
        let mode = std::fs::metadata(&path).expect("stat").permissions().mode() & 0o777;
        assert_eq!(mode, 0o600, "the Plex token must not be readable by other users");
        std::fs::remove_dir_all(&dir).ok();
    }
}

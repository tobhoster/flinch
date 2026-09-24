//! Replacing state files so no reader ever sees half of one.
//!
//! Every state file is rewritten whole each cycle while the web UI reads them.
//! An interrupted plain overwrite leaves a truncated file, and readers take a
//! file that does not parse as "nothing yet" — for the eviction ledger that is
//! a cycle without recycle-bin credit, which evicts more than the disk needs.
//! Writing a sibling and renaming it over the target makes each replacement
//! all-or-nothing, on a local disk and on the NFS share behind a ReadWriteMany
//! volume alike.

use std::io::Write;
use std::path::{Path, PathBuf};

/// Replace `path` with `bytes`: write `<path>.tmp`, flush it to disk, then
/// rename it over `path`.
pub fn replace(path: &Path, bytes: &[u8]) -> std::io::Result<()> {
    let mut temp = path.as_os_str().to_owned();
    temp.push(".tmp");
    let temp = PathBuf::from(temp);
    let mut file = std::fs::File::create(&temp)?;
    file.write_all(bytes)?;
    file.sync_all()?;
    std::fs::rename(&temp, path)
}

//! Which filesystems hold the library, and which one an item lives on.
//!
//! `/api/v3/diskspace` lists every mount in the *arr container — `/`,
//! `/config`, the media share. Only the mounts that host a root folder are the
//! library; governing anything else would evict media to fix a disk that
//! deleting media cannot relieve.

use serde::{Deserialize, Serialize};

/// One mount as an *arr reports it.
#[derive(Debug, Clone, PartialEq)]
pub struct Volume {
    pub path: String,
    pub total_bytes: u64,
    pub free_bytes: u64,
}

impl Volume {
    pub fn used_bytes(&self) -> u64 {
        self.total_bytes.saturating_sub(self.free_bytes)
    }

    /// Bytes above `fraction` of this volume, rounded up so freeing them lands
    /// at or under the line; 0 when already there.
    pub(super) fn excess_over(&self, fraction: f64) -> u64 {
        let line = self.total_bytes as f64 * fraction;
        (self.used_bytes() as f64 - line).max(0.0).ceil() as u64
    }

    pub(super) fn budget(&self, fraction: f64) -> u64 {
        (self.total_bytes as f64 * fraction).floor() as u64
    }
}

/// Which *arr reported a mount. Paths are container-local, so they are only
/// comparable within one app.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum App {
    Radarr,
    Sonarr,
}

impl App {
    pub fn label(self) -> &'static str {
        match self {
            App::Radarr => "radarr",
            App::Sonarr => "sonarr",
        }
    }
}

/// What one app says about its disks.
#[derive(Debug, Clone, PartialEq)]
pub struct AppDisks {
    pub app: App,
    pub diskspace: Vec<Volume>,
    pub root_folders: Vec<RootFolder>,
    /// How long a delete stays on disk in the app's recycle bin.
    pub recycle: RecycleBin,
}

/// Where an app keeps its library, with the free space it measured there.
#[derive(Debug, Clone, PartialEq)]
pub struct RootFolder {
    pub path: String,
    /// Free bytes of the filesystem under the folder, as the app measured it at
    /// the folder itself; `None` when the app did not say.
    pub free_bytes: Option<u64>,
}

impl RootFolder {
    /// Whether `mount` can be the filesystem this folder is on. A path prefix
    /// alone cannot tell: an app that lists none of its media mounts (Sonarr,
    /// seen live with three) leaves only `/`, which holds every path. The
    /// folder's own free space can: a mount showing different free space is
    /// another filesystem. One app's readings agree within 0.5%, the same
    /// window as [`same_filesystem`]. Without a reading the prefix stands.
    fn is_on(&self, mount: &Volume) -> bool {
        self.free_bytes.is_none_or(|free| free.abs_diff(mount.free_bytes) <= mount.total_bytes / 200)
    }
}

/// What an app's recycle bin does to freed space (`/api/v3/config/mediamanagement`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RecycleBin {
    /// No recycle bin: a delete frees its space immediately.
    Disabled,
    /// Deletes sit in the bin for this many days, on the same volume.
    Days(u32),
    /// Enabled with cleanup `0`: the app never empties it, so its deletes never
    /// free space on their own. Credited for the stale-tracking horizon and
    /// reported, because no amount of eviction relieves such a disk.
    NeverEmptied,
    /// The app's settings could not be read: assume the default 7 days — a
    /// longer credit window means fewer extra deletes, the safe direction.
    Unknown,
}

impl RecycleBin {
    pub const DEFAULT_DAYS: u32 = 7;

    pub fn from_settings(path: &str, cleanup_days: u32) -> Self {
        match (path.trim().is_empty(), cleanup_days) {
            (true, _) => RecycleBin::Disabled,
            (false, 0) => RecycleBin::NeverEmptied,
            (false, days) => RecycleBin::Days(days),
        }
    }

    /// Seconds a deleted item may still occupy its volume.
    pub fn hold_secs(self) -> u64 {
        match self {
            RecycleBin::Disabled => 0,
            RecycleBin::Days(days) => u64::from(days) * 86_400,
            RecycleBin::NeverEmptied => super::STALE_ON_DISK_SECS,
            RecycleBin::Unknown => u64::from(Self::DEFAULT_DAYS) * 86_400,
        }
    }
}

fn trim_separators(path: &str) -> &str {
    path.trim_end_matches(['/', '\\'])
}

/// Does `path` live under the mount at `mount`? Whole components only:
/// `/media` holds `/media/tv` but not `/media2`. The root mount holds all.
fn is_under(path: &str, mount: &str) -> bool {
    let (path, mount) = (trim_separators(path), trim_separators(mount));
    mount.is_empty() || path == mount || (path.starts_with(mount) && path[mount.len()..].starts_with(['/', '\\']))
}

/// The deepest mount holding `path`.
fn deepest_mount<'a>(path: &str, mounts: &'a [Volume]) -> Option<&'a Volume> {
    mounts.iter().filter(|mount| is_under(path, &mount.path)).max_by_key(|mount| trim_separators(&mount.path).len())
}

/// Two mounts showing the same filesystem: a shared media share mounted at
/// `/movies` in Radarr and `/tv` in Sonarr. Identical totals and free space
/// within 0.5% (the two apps are sampled seconds apart). Merging errs safe:
/// the unmerged mistake would double the eviction goal for one disk.
fn same_filesystem(a: &Volume, b: &Volume) -> bool {
    a.total_bytes > 0 && a.total_bytes == b.total_bytes && a.free_bytes.abs_diff(b.free_bytes) <= a.total_bytes / 200
}

/// The filesystems that host the libraries, and how to attribute an item to one.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct LibraryVolumes {
    /// One entry per distinct filesystem hosting a root folder, keyed by `path`.
    pub volumes: Vec<Volume>,
    /// (app, mount path, key of the filesystem it shows).
    mounts: Vec<(App, String, String)>,
    /// Root folders no reported mount holds: their items can never be evicted,
    /// so the operator must see them rather than find out when a disk fills.
    pub unmatched_roots: Vec<(App, String)>,
    /// Each reporting app's recycle bin; an app that never answered is `Unknown`.
    recycle: Vec<(App, RecycleBin)>,
}

impl LibraryVolumes {
    /// Keep only the mounts that host a root folder, merging mounts of one
    /// filesystem, so `/config` or the container's `/` never drives eviction.
    pub fn build(disks: &[AppDisks]) -> Self {
        let mut library = Self::default();
        for disk in disks {
            library.recycle.push((disk.app, disk.recycle));
            for root in &disk.root_folders {
                let Some(mount) = deepest_mount(&root.path, &disk.diskspace).filter(|mount| root.is_on(mount)) else {
                    library.unmatched_roots.push((disk.app, root.path.clone()));
                    continue;
                };
                if library.mounts.iter().any(|(app, path, _)| *app == disk.app && *path == mount.path) {
                    continue;
                }
                let key = match library.volumes.iter().find(|known| same_filesystem(known, mount)) {
                    Some(known) => known.path.clone(),
                    None => {
                        // A different filesystem at an already-used path (two
                        // apps, two shares, both at `/media`) needs its own key.
                        let key = if library.volumes.iter().any(|known| known.path == mount.path) {
                            format!("{} ({})", mount.path, disk.app.label())
                        } else {
                            mount.path.clone()
                        };
                        library.volumes.push(Volume { path: key.clone(), ..mount.clone() });
                        key
                    }
                };
                library.mounts.push((disk.app, mount.path.clone(), key));
            }
        }
        library.volumes.sort_by(|a, b| a.path.cmp(&b.path));
        library
    }

    /// The volume key an item's files live on, from the app that owns it.
    pub fn volume_of(&self, app: App, item_path: &str) -> Option<&str> {
        self.mounts
            .iter()
            .filter(|(owner, mount, _)| *owner == app && is_under(item_path, mount))
            .max_by_key(|(_, mount, _)| trim_separators(mount).len())
            .map(|(_, _, key)| key.as_str())
    }

    /// How long a delete by `app` may still occupy its volume.
    pub fn recycle_secs(&self, app: App) -> u64 {
        self.recycle_bin(app).hold_secs()
    }

    pub fn recycle_bin(&self, app: App) -> RecycleBin {
        self.recycle.iter().find(|(owner, _)| *owner == app).map_or(RecycleBin::Unknown, |(_, bin)| *bin)
    }
}

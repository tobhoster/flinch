//! Which filesystems hold the library, and which one an item lives on.
//!
//! `/api/v3/diskspace` lists every mount in the *arr container — `/`,
//! `/config`, the media share. Only the mounts that host a root folder are the
//! library; governing anything else would evict media to fix a disk that
//! deleting media cannot relieve.

use serde::{Deserialize, Serialize};

/// One mount as an *arr reports it, or as FLINCH measured it itself.
#[derive(Debug, Clone, PartialEq)]
pub struct Volume {
    pub path: String,
    pub total_bytes: u64,
    pub free_bytes: u64,
    /// The filesystem's identity (`st_dev`) where FLINCH measured it in its
    /// own container; `None` for a mount an app reported, which says nothing
    /// about identity.
    pub device: Option<u64>,
}

impl Volume {
    pub fn used_bytes(&self) -> u64 {
        self.total_bytes.saturating_sub(self.free_bytes)
    }
}

/// Which *arr reported a mount. Paths are container-local, so they are only
/// comparable within one instance of one app.
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

/// What one instance says about its disks.
#[derive(Debug, Clone, PartialEq)]
pub struct AppDisks {
    pub app: App,
    /// The instance ([`crate::ids`]); empty for the default.
    pub instance: String,
    pub diskspace: Vec<Volume>,
    pub root_folders: Vec<RootFolder>,
    /// How long a delete stays on disk in the app's recycle bin.
    pub recycle: RecycleBin,
    /// The recycle bin's folder as the app sees it; empty when it has none.
    pub recycle_path: String,
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
/// `/movies` in Radarr and `/tv` in Sonarr. Measured here, both carry their
/// device and that decides. Otherwise: identical totals and free space within
/// 0.5% (the two apps are sampled seconds apart). Merging errs safe: the
/// unmerged mistake would double the eviction goal for one disk. The guess
/// cannot tell two same-size disks that filled alike, which is why a device,
/// when both have one, overrides it.
fn same_filesystem(a: &Volume, b: &Volume) -> bool {
    match (a.device, b.device) {
        (Some(a), Some(b)) => a == b,
        _ => a.total_bytes > 0 && a.total_bytes == b.total_bytes && a.free_bytes.abs_diff(b.free_bytes) <= a.total_bytes / 200,
    }
}

/// The filesystems that host the libraries, and how to attribute an item to one.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct LibraryVolumes {
    /// One entry per distinct filesystem hosting a root folder, keyed by `path`.
    pub volumes: Vec<Volume>,
    /// (instance key, mount path, key of the filesystem it shows); the
    /// instance key is [`crate::ids::instance_key`]'s `radarr` or `radarr@4k`.
    mounts: Vec<(String, String, String)>,
    /// Root folders no reported mount holds, by instance key: their items can
    /// never be evicted, so the operator must see them rather than find out
    /// when a disk fills.
    pub unmatched_roots: Vec<(String, String)>,
    /// Each reporting instance's recycle bin; one that never answered is `Unknown`.
    recycle: Vec<(String, RecycleBin)>,
    /// Where each instance's bin sits, by instance key, when it could be
    /// measured: the bin folder and the filesystem under it.
    bins: Vec<(String, String, Volume)>,
}

/// A recycle bin on another filesystem than the library it deletes from.
/// Such a delete frees the library disk at once and fills the bin's disk for
/// the bin's window instead, so it is credited as freed and reported.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BinElsewhere {
    /// The instance key (`sonarr`, `radarr@4k`).
    pub owner: String,
    pub bin: String,
    /// The governed disk the bin is on, when it is one.
    pub on: Option<String>,
    /// The governed disks it deletes from.
    pub from: Vec<String>,
    pub bin_days: Option<u32>,
}

impl BinElsewhere {
    /// One sentence for the log and the problem notifications.
    pub fn describe(&self) -> String {
        let on = self.on.as_deref().map_or_else(|| "another disk".to_string(), |disk| format!("disk {disk}"));
        let held = self.bin_days.map_or_else(|| "until it is emptied".to_string(), |days| format!("for {days} days"));
        format!(
            "{}'s recycle bin {} is on {on}, not on {}: each delete moves its files there {held} instead of freeing space where it is short. Clear the recycle bin folder (Settings › Media Management) or give each disk its own",
            self.owner,
            self.bin,
            self.from.join(", "),
        )
    }
}

impl LibraryVolumes {
    /// Keep only the mounts that host a root folder, merging mounts of one
    /// filesystem, so `/config` or the container's `/` never drives eviction.
    ///
    /// A root the app reports no mount for (Sonarr lists none of its NFS media
    /// mounts) is measured by `probe` instead: the daemon's `statvfs` on the
    /// same share where FLINCH has it mounted. A root neither can measure is
    /// unmatched: its items are never evicted, and the operator is told.
    pub fn build(disks: &[AppDisks], probe: impl Fn(&str) -> Option<Volume>) -> Self {
        let mut library = Self::default();
        for disk in disks {
            let owner = crate::ids::instance_key(disk.app, &disk.instance);
            library.recycle.push((owner.clone(), disk.recycle));
            if disk.recycle != RecycleBin::Disabled && !disk.recycle_path.trim().is_empty() {
                // FLINCH's own mount answers with a device; a reported mount
                // only counts when it is not the container's `/`, which holds
                // every path and so proves nothing (Sonarr reports no NFS mount).
                let measured = probe(&disk.recycle_path)
                    .or_else(|| deepest_mount(&disk.recycle_path, &disk.diskspace).filter(|mount| !trim_separators(&mount.path).is_empty()).cloned());
                if let Some(fs) = measured {
                    library.bins.push((owner.clone(), disk.recycle_path.clone(), fs));
                }
            }
            for root in &disk.root_folders {
                let reported = deepest_mount(&root.path, &disk.diskspace).filter(|mount| root.is_on(mount)).cloned();
                let Some(mount) = reported.or_else(|| probe(&root.path).filter(|found| root.is_on(found))) else {
                    library.unmatched_roots.push((owner.clone(), root.path.clone()));
                    continue;
                };
                let mount = &mount;
                if library.mounts.iter().any(|(known, path, _)| *known == owner && *path == mount.path) {
                    continue;
                }
                let key = match library.volumes.iter().find(|known| same_filesystem(known, mount)) {
                    Some(known) => known.path.clone(),
                    None => {
                        // A different filesystem at an already-used path (two
                        // instances, two shares, both at `/media`) needs its own key.
                        let key = if library.volumes.iter().any(|known| known.path == mount.path) {
                            format!("{} ({owner})", mount.path)
                        } else {
                            mount.path.clone()
                        };
                        library.volumes.push(Volume { path: key.clone(), ..mount.clone() });
                        key
                    }
                };
                library.mounts.push((owner.clone(), mount.path.clone(), key));
            }
        }
        library.volumes.sort_by(|a, b| a.path.cmp(&b.path));
        library
    }

    /// The volume key an item's files live on, from the instance that owns it.
    pub fn volume_of(&self, app: App, instance: &str, item_path: &str) -> Option<&str> {
        self.mounts
            .iter()
            .filter(|(owner, mount, _)| owns(owner, app, instance) && is_under(item_path, mount))
            .max_by_key(|(_, mount, _)| trim_separators(mount).len())
            .map(|(_, _, key)| key.as_str())
    }

    /// How long a delete by the instance may still occupy its volume.
    pub fn recycle_secs(&self, app: App, instance: &str) -> u64 {
        self.recycle_bin(app, instance).hold_secs()
    }

    pub fn recycle_bin(&self, app: App, instance: &str) -> RecycleBin {
        self.recycle.iter().find(|(owner, _)| owns(owner, app, instance)).map_or(RecycleBin::Unknown, |(_, bin)| *bin)
    }

    /// How long a delete of `card_id` from `volume` may still occupy that
    /// volume: its instance's bin window ([`crate::ids::ArrRef`]; an id naming
    /// none is `Unknown`), or nothing when the bin is on another filesystem,
    /// where the delete moves the files at once.
    pub fn recycle_secs_of(&self, card_id: &str, volume: &str) -> u64 {
        let Some(item) = crate::ids::ArrRef::card(card_id) else {
            return RecycleBin::Unknown.hold_secs();
        };
        let owner = crate::ids::instance_key(item.app, item.instance);
        if self.bin_elsewhere(&owner, volume) {
            return 0;
        }
        self.recycle_bin(item.app, item.instance).hold_secs()
    }

    /// Whether `owner`'s bin is measured on a filesystem other than `volume`'s.
    fn bin_elsewhere(&self, owner: &str, volume: &str) -> bool {
        let Some((_, _, fs)) = self.bins.iter().find(|(known, _, _)| known == owner) else { return false };
        self.volumes.iter().find(|known| known.path == volume).is_some_and(|disk| !same_filesystem(disk, fs))
    }

    /// The instance keys whose bin is never emptied.
    pub fn never_emptied(&self) -> impl Iterator<Item = &str> {
        self.recycle.iter().filter(|(_, bin)| *bin == RecycleBin::NeverEmptied).map(|(owner, _)| owner.as_str())
    }

    /// Each instance whose bin sits on another filesystem than some of the
    /// governed disks it deletes from.
    pub fn bins_elsewhere(&self) -> Vec<BinElsewhere> {
        self.bins
            .iter()
            .filter_map(|(owner, bin, fs)| {
                let mut from: Vec<String> = self
                    .mounts
                    .iter()
                    .filter(|(known, _, key)| known == owner && self.bin_elsewhere(owner, key))
                    .map(|(_, _, key)| key.clone())
                    .collect();
                from.sort();
                from.dedup();
                if from.is_empty() {
                    return None;
                }
                let on = self.volumes.iter().find(|disk| same_filesystem(disk, fs)).map(|disk| disk.path.clone());
                let bin_days = match self.recycle.iter().find(|(known, _)| known == owner).map(|(_, bin)| *bin) {
                    Some(RecycleBin::Days(days)) => Some(days),
                    _ => None,
                };
                Some(BinElsewhere { owner: owner.clone(), bin: bin.clone(), on, from, bin_days })
            })
            .collect()
    }
}

/// Whether `owner` ([`crate::ids::instance_key`]) is `app`'s `instance`.
fn owns(owner: &str, app: App, instance: &str) -> bool {
    owner.strip_prefix(app.label()).is_some_and(|rest| match rest.strip_prefix('@') {
        Some(name) => name == instance,
        None => rest.is_empty() && instance.is_empty(),
    })
}

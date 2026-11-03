//! Where a recycle bin sits decides what a delete frees: on the library's
//! own disk it holds the bytes for its window; on another disk the delete
//! moves them there at once, freeing the library disk and filling the other.

use super::{App, AppDisks, LibraryVolumes, RecycleBin, RootFolder, Volume};

const GB: u64 = 1_000_000_000;
const DAYS3: u64 = 3 * 86_400;

fn vol(path: &str, total_gb: u64, used_gb: u64, device: Option<u64>) -> Volume {
    Volume { path: path.to_string(), total_bytes: total_gb * GB, free_bytes: (total_gb - used_gb) * GB, device }
}

fn root(path: &str, free_gb: u64) -> RootFolder {
    RootFolder { path: path.into(), free_bytes: Some(free_gb * GB) }
}

/// Live shape: Radarr reports the movies share as `/data`; Sonarr lists only
/// `/` and keeps its libraries on the tv and tv-b shares, which FLINCH
/// measures where it has them mounted, with its bin at `bin`.
fn sonarr_with_bin(bin: &str, bin_mounted_here: bool) -> LibraryVolumes {
    let radarr = AppDisks {
        app: App::Radarr,
        instance: String::new(),
        diskspace: vec![vol("/data", 834, 617, None)],
        root_folders: vec![root("/data/media/movies", 217)],
        recycle: RecycleBin::Days(3),
        recycle_path: String::new(),
    };
    let sonarr = AppDisks {
        app: App::Sonarr,
        instance: String::new(),
        diskspace: vec![vol("/", 510, 57, None)],
        root_folders: vec![root("/data/media/tv", 56), root("/data/media/tv-b", 54)],
        recycle: RecycleBin::Days(3),
        recycle_path: bin.into(),
    };
    LibraryVolumes::build(&[radarr, sonarr], |path| match path {
        _ if path.starts_with("/data/media/tv-b") => Some(vol("/data/media/tv-b", 834, 780, Some(2))),
        _ if path.starts_with("/data/media/tv") => Some(vol("/data/media/tv", 834, 778, Some(1))),
        _ if bin_mounted_here && path.starts_with("/data/downloads") => Some(vol(path, 834, 617, Some(3))),
        _ => None,
    })
}

#[test]
fn a_recycle_bin_on_another_disk_frees_the_library_at_once_and_is_reported() {
    let library = sonarr_with_bin("/data/downloads/.RecycleBin", true);
    assert_eq!(library.recycle_secs_of("sonarr-30-s1", "/data/media/tv-b"), 0, "the delete moves the files off tv-b at once");
    assert_eq!(library.recycle_secs_of("sonarr-20-s4", "/data/media/tv"), 0);
    let elsewhere = library.bins_elsewhere();
    assert_eq!(elsewhere.len(), 1);
    assert_eq!(elsewhere[0].on.as_deref(), Some("/data"), "found to be the share Radarr reports, by size and free space");
    assert_eq!(elsewhere[0].from, ["/data/media/tv", "/data/media/tv-b"]);
    let said = elsewhere[0].describe();
    assert!(said.contains("on disk /data, not on /data/media/tv, /data/media/tv-b") && said.contains("for 3 days"), "{said}");
}

#[test]
fn a_bin_flinch_cannot_measure_keeps_its_window_and_raises_nothing() {
    // Not mounted here, and Sonarr reports only `/`, which holds every path and proves nothing.
    let library = sonarr_with_bin("/data/downloads/.RecycleBin", false);
    assert_eq!(library.recycle_secs_of("sonarr-30-s1", "/data/media/tv-b"), DAYS3);
    assert!(library.bins_elsewhere().is_empty());
}

#[test]
fn a_bin_on_one_library_disk_holds_there_and_frees_the_other_at_once() {
    let library = sonarr_with_bin("/data/media/tv/.RecycleBin", true);
    assert_eq!(library.recycle_secs_of("sonarr-20-s4", "/data/media/tv"), DAYS3, "same device: the bin holds the bytes on tv");
    assert_eq!(library.recycle_secs_of("sonarr-30-s1", "/data/media/tv-b"), 0, "tv-b's deletes still leave tv-b at once");
    let elsewhere = library.bins_elsewhere();
    assert_eq!(elsewhere[0].from, ["/data/media/tv-b"]);
    assert_eq!(elsewhere[0].on.as_deref(), Some("/data/media/tv"));
}

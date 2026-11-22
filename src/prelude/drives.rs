//! The music step's look around: every drive here (its mount point or drive letter, label, size, free space,
//! whether it can be unplugged) and the music already on it, so you can choose where the library lives.
//!
//! Linux: `lsblk -J -b …`, else /proc/mounts (and `df` for the sizes). Windows: the logical drives (PowerShell),
//! USB ones marked. System and pseudo filesystems (tmpfs, overlay, squashfs, /boot, /proc…) are left out.

use std::{
    path::{Path, PathBuf},
    process::Command,
    time::{Duration, Instant},
};

use serde_json::Value;

#[derive(Debug, Clone, PartialEq, Default)]
pub struct Drive {
    /// where it's mounted (Windows: "D:\")
    pub mount: PathBuf,
    pub label: Option<String>,
    pub size: u64,
    pub free: u64,
    /// it can be unplugged (USB, a card…)
    pub removable: bool,
    /// "USB", "network"…, for the list
    pub kind: Option<String>,
    /// the one the system runs from (/, C:\)
    pub system: bool,
}

impl Drive {
    /// "Seagate, USB" (empty: nothing to say)
    pub fn describe(&self) -> String {
        let mut parts: Vec<String> = self.label.iter().cloned().collect();
        if let Some(k) = &self.kind {
            parts.push(k.clone());
        } else if self.removable {
            parts.push("removable".into());
        }
        if self.system {
            parts.push("system".into());
        }
        parts.join(", ")
    }
}

/// A music folder found on a drive.
#[derive(Debug, Clone, PartialEq)]
pub struct Found {
    pub path: PathBuf,
    /// which of the drives
    pub drive: usize,
    pub songs: usize,
    /// counting stopped (the cap or the time) before the end: at least `songs`
    pub more: bool,
}

/// The drives here.
pub fn detect() -> Vec<Drive> {
    #[cfg(windows)]
    {
        let script = "$usb = @(try { Get-Disk -ErrorAction Stop | Where-Object BusType -eq 'USB' | Get-Partition | Where-Object DriveLetter | ForEach-Object { [string]$_.DriveLetter } } catch { });
$d = @(Get-CimInstance Win32_LogicalDisk | Select-Object DeviceID, VolumeName, Size, FreeSpace, DriveType);
@{ usb = $usb; disks = $d } | ConvertTo-Json -Depth 3 -Compress";
        let sys = std::env::var("SystemDrive").unwrap_or_else(|_| "C:".into());
        Command::new("powershell")
            .args(["-NoProfile", "-NonInteractive", "-Command", script])
            .output()
            .ok()
            .map(|o| parse_windows(&String::from_utf8_lossy(&o.stdout), &sys))
            .unwrap_or_default()
    }
    #[cfg(not(windows))]
    {
        let o = Command::new("lsblk").args(["-J", "-b", "-o", "NAME,LABEL,SIZE,FSAVAIL,MOUNTPOINTS,RM,HOTPLUG,TRAN,FSTYPE"]).output();
        if let Ok(o) = o
            && o.status.success()
        {
            let d = parse_lsblk(&String::from_utf8_lossy(&o.stdout));
            if !d.is_empty() {
                return d;
            }
        }
        let mut d = parse_proc_mounts(&std::fs::read_to_string("/proc/mounts").unwrap_or_default());
        for x in &mut d {
            (x.size, x.free) = df(&x.mount).unwrap_or_default();
        }
        d
    }
}

const SKIP_FS: &[&str] = &[
    "tmpfs", "devtmpfs", "overlay", "squashfs", "proc", "sysfs", "cgroup", "cgroup2", "devpts", "mqueue", "debugfs", "tracefs", "securityfs",
    "pstore", "efivarfs", "bpf", "autofs", "fusectl", "configfs", "hugetlbfs", "ramfs", "nsfs", "swap", "iso9660", "udf", "erofs", "zram",
    "binfmt_misc", "rpc_pipefs", "fuse.portal", "fuse.gvfsd-fuse",
];
const NETWORK_FS: &[&str] = &["nfs", "nfs4", "cifs", "smb3", "smbfs", "fuse.sshfs"];

/// Places that aren't where music lives (Linux).
fn skip_mount(m: &str) -> bool {
    m.is_empty()
        || m.starts_with('[')
        || ["/boot", "/efi", "/snap", "/var/lib", "/proc", "/sys", "/dev", "/tmp", "/var/tmp", "/var/log", "/var/cache"]
            .iter()
            .any(|p| m == *p || m.starts_with(&format!("{p}/")))
        || (m.starts_with("/run") && !m.starts_with("/run/media/"))
}

/// lsblk's JSON (-J -b, NAME,LABEL,SIZE,FSAVAIL,MOUNTPOINTS,RM,HOTPLUG,TRAN,FSTYPE): one drive per mounted
/// filesystem. A partition takes its disk's transport (USB) and removability.
pub fn parse_lsblk(json: &str) -> Vec<Drive> {
    let Ok(v) = serde_json::from_str::<Value>(json) else { return vec![] };
    let mut out = vec![];
    for dev in v["blockdevices"].as_array().into_iter().flatten() {
        walk_lsblk(dev, None, false, &mut out);
    }
    out
}

fn flag(v: &Value) -> bool {
    v.as_bool().unwrap_or_else(|| v.as_str().is_some_and(|s| s == "1") || v.as_u64() == Some(1))
}

fn number(v: &Value) -> Option<u64> {
    v.as_u64().or_else(|| v.as_str().and_then(|s| s.parse().ok()))
}

fn walk_lsblk(dev: &Value, tran: Option<&str>, parent_removable: bool, out: &mut Vec<Drive>) {
    let tran = dev["tran"].as_str().or(tran);
    let removable = parent_removable || flag(&dev["rm"]) || flag(&dev["hotplug"]);
    let fstype = dev["fstype"].as_str().unwrap_or("");
    // MOUNTPOINTS (a list; lsblk 2.37+) or MOUNTPOINT
    let mounts: Vec<&str> = match dev["mountpoints"].as_array() {
        Some(a) => a.iter().filter_map(|m| m.as_str()).collect(),
        None => dev["mountpoint"].as_str().into_iter().collect(),
    };
    let mounts: Vec<&str> = mounts.into_iter().filter(|m| !skip_mount(m)).collect();
    // one filesystem mounted at several places (btrfs subvolumes): "/" if it's one of them, else the first
    let mount = mounts.iter().find(|m| **m == "/").or(mounts.first());
    if let Some(m) = mount
        && !fstype.is_empty()
        && !SKIP_FS.contains(&fstype)
    {
        let label = dev["label"].as_str().map(String::from).filter(|l| !l.is_empty());
        let kind = if NETWORK_FS.contains(&fstype) {
            Some("network".to_string())
        } else {
            tran.filter(|t| *t == "usb").map(|_| "USB".to_string())
        };
        out.push(Drive {
            mount: PathBuf::from(m),
            label,
            size: number(&dev["size"]).unwrap_or(0),
            free: number(&dev["fsavail"]).unwrap_or(0),
            removable: removable || tran == Some("usb"),
            kind,
            system: *m == "/",
        });
    }
    for c in dev["children"].as_array().into_iter().flatten() {
        walk_lsblk(c, tran, removable, out);
    }
}

/// /proc/mounts: the real filesystems (sizes come from `df`). Under /media or /run/media counts as removable.
pub fn parse_proc_mounts(text: &str) -> Vec<Drive> {
    let mut out: Vec<Drive> = vec![];
    for line in text.lines() {
        let f: Vec<&str> = line.split_whitespace().collect();
        let [dev, mount, fstype, ..] = f[..] else { continue };
        let mount = mount.replace("\\040", " ").replace("\\011", "\t");
        let network = NETWORK_FS.contains(&fstype);
        if SKIP_FS.contains(&fstype) || skip_mount(&mount) || !(dev.starts_with("/dev/") || network) || out.iter().any(|d| d.mount == Path::new(&mount)) {
            continue;
        }
        let removable = mount.starts_with("/media/") || mount.starts_with("/run/media/");
        out.push(Drive {
            system: mount == "/",
            label: None,
            kind: network.then(|| "network".to_string()),
            removable,
            mount: PathBuf::from(mount),
            ..Default::default()
        });
    }
    out
}

#[cfg(not(windows))]
fn df(mount: &Path) -> Option<(u64, u64)> {
    let o = Command::new("df").args(["-P", "-B1"]).arg(mount).output().ok()?;
    let text = String::from_utf8_lossy(&o.stdout).into_owned();
    let f: Vec<&str> = text.lines().nth(1)?.split_whitespace().collect();
    Some((f.get(1)?.parse().ok()?, f.get(3)?.parse().ok()?))
}

/// PowerShell's answer (Win32_LogicalDisk, and the letters on USB disks): fixed, removable and network drives.
pub fn parse_windows(json: &str, system_drive: &str) -> Vec<Drive> {
    let Ok(v) = serde_json::from_str::<Value>(json.trim()) else { return vec![] };
    let list = |v: &Value| -> Vec<Value> {
        match v {
            Value::Array(a) => a.clone(),
            Value::Null => vec![],
            one => vec![one.clone()],
        }
    };
    let usb: Vec<String> = list(&v["usb"]).iter().filter_map(|l| l.as_str().map(|s| s.to_uppercase())).collect();
    let mut out = vec![];
    for d in list(&v["disks"]) {
        let Some(id) = d["DeviceID"].as_str() else { continue };
        let (Some(size), kind) = (number(&d["Size"]), number(&d["DriveType"]).unwrap_or(0)) else { continue };
        if !matches!(kind, 2..=4) {
            continue; // 5: CD/DVD, 6: RAM disk
        }
        let on_usb = usb.iter().any(|u| id.to_uppercase().starts_with(u.as_str()));
        out.push(Drive {
            mount: PathBuf::from(format!("{id}\\")),
            label: d["VolumeName"].as_str().map(String::from).filter(|l| !l.is_empty()),
            size,
            free: number(&d["FreeSpace"]).unwrap_or(0),
            removable: kind == 2 || on_usb,
            kind: if on_usb { Some("USB".into()) } else if kind == 4 { Some("network".into()) } else { None },
            system: id.eq_ignore_ascii_case(system_drive.trim_end_matches('\\')),
        });
    }
    out
}

/// What a music folder is called, in a few languages.
const MUSIC_NAMES: &[&str] = &["music", "my music", "musik", "música", "musica", "musique", "muziek", "muzyka", "musiikki", "hudba", "zene", "музыка"];
/// Folders that never hold your music (and take long to look through).
const SKIP_DIRS: &[&str] = &[
    "proc", "sys", "dev", "run", "tmp", "usr", "var", "etc", "boot", "snap", "lib", "lib64", "bin", "sbin", "opt", "srv", "lost+found",
    "windows", "program files", "program files (x86)", "programdata", "$recycle.bin", "system volume information", "appdata", "node_modules",
    "library", "applications",
];
const SEARCH_DEPTH: usize = 3;
/// per drive: looking and counting
const TIME: Duration = Duration::from_secs(5);
const MAX_SONGS: usize = 100_000;

/// Music folders on these drives (a shallow look near the top of each, up to 5 seconds a drive), with their songs
/// counted (up to 100,000).
pub fn find_music(drives: &[Drive]) -> Vec<Found> {
    let mut out = vec![];
    for (i, d) in drives.iter().enumerate() {
        if d.kind.as_deref() == Some("network") {
            continue; // slow to walk; type its path if the music is there
        }
        let until = Instant::now() + TIME;
        let mut dirs = vec![];
        look(&d.mount, 0, until, &mut dirs);
        for path in dirs {
            // a drive's own other mounts (a separate /home) are drives of their own
            if drives.iter().any(|o| o.mount != d.mount && o.mount.starts_with(&d.mount) && path.starts_with(&o.mount)) {
                continue;
            }
            let (songs, more) = count_songs(&path, MAX_SONGS, until);
            if songs > 0 {
                out.push(Found { path, drive: i, songs, more });
            }
        }
    }
    out
}

fn look(dir: &Path, depth: usize, until: Instant, out: &mut Vec<PathBuf>) {
    if depth >= SEARCH_DEPTH || Instant::now() > until {
        return;
    }
    let Ok(rd) = std::fs::read_dir(dir) else { return };
    let mut subs: Vec<(PathBuf, String)> = rd
        .filter_map(|e| e.ok())
        .filter(|e| e.file_type().is_ok_and(|t| t.is_dir()))
        .map(|e| (e.path(), e.file_name().to_string_lossy().to_lowercase()))
        .filter(|(_, n)| !n.starts_with('.') && !SKIP_DIRS.contains(&n.as_str()))
        .collect();
    subs.sort();
    for (p, name) in subs {
        if MUSIC_NAMES.contains(&name.as_str()) {
            out.push(p); // and not inside it: it's one library
        } else {
            look(&p, depth + 1, until, out);
        }
    }
}

/// Songs in a folder, up to `cap` or until `until`. (n, stopped before the end)
pub fn count_songs(dir: &Path, cap: usize, until: Instant) -> (usize, bool) {
    let mut n = 0;
    for (i, e) in walkdir::WalkDir::new(dir).into_iter().filter_map(|e| e.ok()).enumerate() {
        // the time is checked from the 256th entry on: a folder found just as time ran out is still counted a bit
        if n >= cap || (i > 0 && i % 256 == 0 && Instant::now() > until) {
            return (n, true);
        }
        if e.file_type().is_file() && e.path().extension().is_some_and(|x| crate::library::AUDIO.contains(&x.to_string_lossy().to_lowercase().as_str())) {
            n += 1;
        }
    }
    (n, false)
}

/// What the music step offers first.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Pick {
    /// the music folder found with the most songs
    Found(usize),
    /// a new folder on this drive
    NewOn(usize),
    /// no drive to offer: type a path
    Type,
}

/// A data drive this big (free) is chosen over the system drive for a new library.
pub const BIG: u64 = 32 << 30;

pub fn default_choice(drives: &[Drive], found: &[Found]) -> Pick {
    if let Some((i, _)) = found.iter().enumerate().max_by_key(|(i, f)| (f.songs, std::cmp::Reverse(*i))) {
        return Pick::Found(i);
    }
    let usable: Vec<usize> = (0..drives.len()).filter(|i| drives[*i].kind.as_deref() != Some("network")).collect();
    let most_free = |c: &mut dyn Iterator<Item = usize>| c.max_by_key(|i| drives[*i].free);
    let data = most_free(&mut usable.iter().copied().filter(|i| !drives[*i].system && drives[*i].free >= BIG));
    match data.or_else(|| most_free(&mut usable.iter().copied())) {
        Some(i) => Pick::NewOn(i),
        None => Pick::Type,
    }
}

/// A path as the drives list has them: Windows' canonical paths start with \\?\ (\\?\D:\Music), which never
/// starts with D:\ as a path, so that's taken off.
pub fn plain(p: PathBuf) -> PathBuf {
    let s = p.to_string_lossy();
    if let Some(rest) = s.strip_prefix(r"\\?\") {
        if let Some(unc) = rest.strip_prefix(r"UNC\") {
            return PathBuf::from(format!(r"\\{unc}"));
        }
        if rest.as_bytes().get(1) == Some(&b':') {
            return PathBuf::from(rest);
        }
    }
    p
}

/// The drive a path is on (the longest mount point it's under).
pub fn drive_of<'a>(drives: &'a [Drive], path: &Path) -> Option<&'a Drive> {
    drives.iter().filter(|d| path.starts_with(&d.mount)).max_by_key(|d| d.mount.as_os_str().len())
}

/// Where a new library goes on a drive: Music at its top, or in your home on the system drive (Linux, where / isn't
/// yours to write in).
pub fn new_folder(d: &Drive) -> PathBuf {
    if d.system
        && cfg!(unix)
        && let Some(h) = std::env::var_os("HOME").map(PathBuf::from).filter(|h| h.starts_with(&d.mount))
    {
        return h.join("Music");
    }
    d.mount.join("Music")
}

/// 1.2 TB, 640 GB, 12 MB (powers of 1000, as drives are sold).
pub fn size(n: u64) -> String {
    let units = [("TB", 1e12), ("GB", 1e9), ("MB", 1e6)];
    for (u, f) in units {
        if n as f64 >= f {
            let v = n as f64 / f;
            return if v < 10.0 { format!("{v:.1} {u}") } else { format!("{v:.0} {u}") };
        }
    }
    format!("{n} bytes")
}

/// 2,140
pub fn thousands(n: usize) -> String {
    let s = n.to_string();
    let mut out = String::new();
    for (i, c) in s.chars().enumerate() {
        if i > 0 && (s.len() - i).is_multiple_of(3) {
            out.push(',');
        }
        out.push(c);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    const LSBLK: &str = r#"{"blockdevices": [
      {"name":"nvme0n1", "label":null, "size":512110190592, "fsavail":null, "mountpoints":[null], "rm":false, "hotplug":false, "tran":"nvme", "fstype":null,
       "children": [
          {"name":"nvme0n1p1", "label":null, "size":629145600, "fsavail":601767936, "mountpoints":["/boot/efi"], "rm":false, "hotplug":false, "tran":null, "fstype":"vfat"},
          {"name":"nvme0n1p3", "label":"fedora", "size":510405738496, "fsavail":201234567890, "mountpoints":["/home", "/"], "rm":false, "hotplug":false, "tran":null, "fstype":"btrfs"}
       ]},
      {"name":"sda", "label":null, "size":2000398934016, "fsavail":null, "mountpoints":[null], "rm":false, "hotplug":true, "tran":"usb", "fstype":null,
       "children": [
          {"name":"sda1", "label":"Seagate", "size":2000397885440, "fsavail":1200000000000, "mountpoints":["/run/media/me/Seagate"], "rm":false, "hotplug":true, "tran":null, "fstype":"exfat"}
       ]},
      {"name":"loop0", "label":null, "size":4096, "fsavail":0, "mountpoints":["/var/lib/snapd/snap/core/1"], "rm":false, "hotplug":false, "tran":null, "fstype":"squashfs"},
      {"name":"zram0", "label":null, "size":8589934592, "fsavail":null, "mountpoints":["[SWAP]"], "rm":false, "hotplug":false, "tran":null, "fstype":"swap"},
      {"name":"sdb", "label":null, "size":"4000787030016", "fsavail":null, "mountpoint":null, "rm":"0", "hotplug":"0", "tran":"sata", "fstype":null,
       "children": [{"name":"sdb1", "label":"Data", "size":"4000785104896", "fsavail":"3500000000000", "mountpoint":"/mnt/data", "rm":"0", "hotplug":"0", "tran":null, "fstype":"ext4"}]}
    ]}"#;

    #[test]
    fn lsblk_drives() {
        let d = parse_lsblk(LSBLK);
        let mounts: Vec<&str> = d.iter().map(|x| x.mount.to_str().unwrap()).collect();
        assert_eq!(mounts, ["/", "/run/media/me/Seagate", "/mnt/data"], "no /boot/efi, snaps or swap");
        assert!(d[0].system && !d[0].removable && d[0].label.as_deref() == Some("fedora"));
        assert_eq!((d[1].removable, d[1].kind.as_deref(), d[1].free), (true, Some("USB"), 1_200_000_000_000), "the disk's USB, on its partition");
        assert_eq!(d[1].describe(), "Seagate, USB");
        assert_eq!((d[2].size, d[2].free, d[2].removable), (4_000_785_104_896, 3_500_000_000_000, false), "older lsblk: strings, MOUNTPOINT");
        assert!(parse_lsblk("not json").is_empty());
    }

    #[test]
    fn proc_mounts_and_windows() {
        let m = "proc /proc proc rw 0 0\n/dev/sda2 / ext4 rw 0 0\ntmpfs /tmp tmpfs rw 0 0\n/dev/sda1 /boot ext4 rw 0 0\n\
                 /dev/sdb1 /media/me/My\\040Drive vfat rw 0 0\nnas:/music /mnt/nas nfs4 rw 0 0\n/dev/loop1 /snap/x/1 squashfs ro 0 0\n";
        let d = parse_proc_mounts(m);
        let mounts: Vec<&str> = d.iter().map(|x| x.mount.to_str().unwrap()).collect();
        assert_eq!(mounts, ["/", "/media/me/My Drive", "/mnt/nas"]);
        assert!(d[0].system && d[1].removable && d[2].kind.as_deref() == Some("network"));
        let w = r#"{"usb":"E","disks":[{"DeviceID":"C:","VolumeName":"","Size":511000000000,"FreeSpace":90000000000,"DriveType":3},
            {"DeviceID":"D:","VolumeName":"Data","Size":4000000000000,"FreeSpace":3000000000000,"DriveType":3},
            {"DeviceID":"E:","VolumeName":"Seagate","Size":2000000000000,"FreeSpace":1200000000000,"DriveType":3},
            {"DeviceID":"F:","VolumeName":null,"Size":null,"FreeSpace":null,"DriveType":5}]}"#;
        let d = parse_windows(w, "C:");
        assert_eq!(d.len(), 3, "no empty CD drive");
        assert_eq!(d[0].mount, PathBuf::from("C:\\"));
        assert!(d[0].system && !d[1].system && !d[1].removable);
        assert_eq!((d[2].removable, d[2].describe().as_str()), (true, "Seagate, USB"));
        let one = r#"{"usb":[],"disks":{"DeviceID":"C:","Size":100,"FreeSpace":50,"DriveType":3}}"#;
        assert_eq!(parse_windows(one, "C:").len(), 1, "one drive: PowerShell gives an object, not a list");
    }

    #[test]
    fn what_is_offered_first() {
        let drive = |m: &str, free: u64, system: bool| Drive { mount: m.into(), free, size: free * 2, system, ..Default::default() };
        let drives = [drive("/", 200 << 30, true), drive("/mnt/data", 100 << 30, false), drive("/run/media/me/USB", 10 << 30, false)];
        let found = |drive: usize, songs: usize| Found { path: format!("{}/Music", drives[drive].mount.display()).into(), drive, songs, more: false };
        assert_eq!(default_choice(&drives, &[found(0, 20), found(2, 2140)]), Pick::Found(1), "the most songs");
        assert_eq!(default_choice(&drives, &[]), Pick::NewOn(1), "a big data drive over the system drive, though it has less free");
        let small = [drive("/", 200 << 30, true), drive("/mnt/small", 8 << 30, false)];
        assert_eq!(default_choice(&small, &[]), Pick::NewOn(0), "no big data drive: the most free space");
        assert_eq!(default_choice(&[], &[]), Pick::Type);
        assert_eq!(drive_of(&drives, Path::new("/run/media/me/USB/Music")).map(|d| d.free), Some(10 << 30));
        assert_eq!((size(1_234_000_000_000), size(640_000_000_000), thousands(2140), thousands(100_000)), ("1.2 TB".into(), "640 GB".into(), "2,140".into(), "100,000".into()));
    }

    #[test]
    fn music_found_near_the_top() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        for (dir, songs) in [("a/Music", 3), ("b/c/Musik/x", 2), ("d/e/f/Music", 5), (".hidden/Music", 4), ("Windows/Music", 1)] {
            std::fs::create_dir_all(root.join(dir)).unwrap();
            for n in 0..songs {
                std::fs::write(root.join(dir).join(format!("{n}.flac")), b"x").unwrap();
            }
        }
        std::fs::create_dir_all(root.join("empty/Music")).unwrap();
        let drives = [Drive { mount: root.to_path_buf(), ..Default::default() }];
        let mut f = find_music(&drives);
        f.sort_by(|a, b| a.path.cmp(&b.path));
        let got: Vec<(String, usize)> = f.iter().map(|x| (x.path.strip_prefix(root).unwrap().to_string_lossy().replace('\\', "/"), x.songs)).collect();
        assert_eq!(got, [("a/Music".to_string(), 3), ("b/c/Musik".to_string(), 2)], "three levels down at most; not hidden or system folders; not empty ones");
        let (n, more) = count_songs(&root.join("d"), 3, Instant::now() + TIME);
        assert_eq!((n, more), (3, true), "stops at the cap");
        // the looking took all the time: what's found is still counted a little, not dropped as empty
        let late = Instant::now().checked_sub(Duration::from_secs(1)).unwrap();
        assert_eq!(count_songs(&root.join("a/Music"), MAX_SONGS, late), (3, false));
    }

    #[test]
    fn windows_canonical_paths() {
        assert_eq!(plain(PathBuf::from(r"\\?\D:\Music")), PathBuf::from(r"D:\Music"));
        assert_eq!(plain(PathBuf::from(r"\\?\UNC\nas\share\Music")), PathBuf::from(r"\\nas\share\Music"));
        assert_eq!(plain(PathBuf::from("/run/media/me/Music")), PathBuf::from("/run/media/me/Music"));
    }
}

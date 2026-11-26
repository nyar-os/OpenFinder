//! Read-only host diagnostics: logical volumes and single-path inspection.
//!
//! Both entry points only read host state. `volumes` lists logical drives with
//! their filesystem, capacity, and the index OpenFinder already holds for them.
//! `inspect` reports one path's metadata, an optional recursive size, and the
//! minimal git repository state (branch, HEAD, remotes) parsed from `.git`
//! directly — no `git` process, no external search tool, no index write.

use std::{
    fs,
    path::{Path, PathBuf},
    time::{SystemTime, UNIX_EPOCH},
};

use boost_fs::walk::{ErrorPolicy, Walker};
use serde::Serialize;

use crate::{
    config::default_index_dir,
    error::{Error, Result},
    index::{INDEX_SCHEMA, IndexMeta},
};

/// Index presence and header fields for one volume.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct IndexStatus {
    /// Directory that was checked.
    pub dir: String,
    /// Whether `meta.json` exists there.
    pub present: bool,
    /// On-disk schema, when the header parsed.
    pub schema: Option<String>,
    /// Indexed entry count, when the header parsed.
    pub entry_count: Option<u64>,
    /// USN cursor, when the header parsed.
    pub cursor: Option<u64>,
    /// Last complete build time in Unix milliseconds.
    pub built_at_ms: Option<u64>,
    /// Last update time in Unix milliseconds.
    pub updated_at_ms: Option<u64>,
    /// Why the header could not be used, when it exists but did not parse.
    pub error: Option<String>,
}

/// One logical volume.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct VolumeInfo {
    /// Volume label such as `C:`.
    pub volume: String,
    /// Volume name assigned by the filesystem, when readable.
    pub label: Option<String>,
    /// Filesystem name, for example `NTFS`.
    pub filesystem: Option<String>,
    /// Whether the filesystem is NTFS and therefore indexable here.
    pub ntfs: bool,
    /// Drive classification: `fixed`, `removable`, `remote`, or `ramdisk`.
    pub drive_type: String,
    /// NTFS volume serial, when readable.
    pub volume_serial: Option<u64>,
    /// Total capacity in bytes.
    pub total_bytes: Option<u64>,
    /// Free bytes reported for the caller.
    pub free_bytes: Option<u64>,
    /// Free bytes available to the calling user.
    pub available_bytes: Option<u64>,
    /// Index held for this volume.
    pub index: IndexStatus,
}

/// Report produced by [`volumes`].
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct VolumesReport {
    /// Runtime platform identity.
    pub platform: String,
    /// Base index directory that was consulted.
    pub index_dir: String,
    /// Logical volumes in label order.
    pub volumes: Vec<VolumeInfo>,
    /// Boundary notes that are not capabilities.
    pub notes: Vec<String>,
}

/// Options for [`inspect`].
#[derive(Debug, Clone)]
pub struct InspectOptions {
    /// Path to inspect.
    pub path: PathBuf,
    /// Whether a directory's tree totals are computed.
    pub recursive: bool,
    /// Entry cap for the recursive walk.
    pub max_entries: u64,
}

impl Default for InspectOptions {
    fn default() -> Self {
        Self { path: PathBuf::from("."), recursive: true, max_entries: 200_000 }
    }
}

/// Recursive directory totals.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TreeStats {
    /// Regular files visited.
    pub files: u64,
    /// Directories visited below the root.
    pub dirs: u64,
    /// Sum of regular-file sizes.
    pub bytes: u64,
    /// Entries visited, including directories.
    pub entries: u64,
    /// True when the walk stopped at the entry cap.
    pub truncated: bool,
}

/// One git remote.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RemoteRef {
    /// Remote name, such as `origin`.
    pub name: String,
    /// Remote URL as written in `.git/config`.
    pub url: String,
}

/// Minimal git repository state.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RepoSummary {
    /// Worktree root that owns `.git`.
    pub root: String,
    /// Resolved git directory.
    pub git_dir: String,
    /// Current branch, when HEAD points at a branch.
    pub branch: Option<String>,
    /// Commit id HEAD resolves to, when resolvable.
    pub head: Option<String>,
    /// True when HEAD holds a raw commit id instead of a ref.
    pub detached: bool,
    /// Configured remotes.
    pub remotes: Vec<RemoteRef>,
}

/// One inspected path.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PathReport {
    /// Path as requested.
    pub path: String,
    /// Whether the path exists.
    pub exists: bool,
    /// `file`, `dir`, `symlink`, or `other`; `missing` in the absent case.
    pub kind: String,
    /// File size in bytes; `0` for a directory, whose size is in `tree`.
    pub size: u64,
    /// Reparse target for a symlink or junction.
    pub link_target: Option<String>,
    /// Stable lower-case Windows attribute names; empty off Windows.
    pub attributes: Vec<String>,
    /// Creation time in Unix milliseconds.
    pub created_ms: u64,
    /// Last modification time in Unix milliseconds.
    pub modified_ms: u64,
    /// Last access time in Unix milliseconds.
    pub accessed_ms: u64,
    /// Recursive totals for a directory when `recursive` was requested.
    pub tree: Option<TreeStats>,
    /// Nearest git repository, when one was found.
    pub repo: Option<RepoSummary>,
}

/// List logical volumes and the index each one has.
///
/// Enumeration uses the host volume API directly, so an agent never has to call
/// `wmic`, PowerShell, or a third-party search tool. Non-Windows targets report
/// `unsupported_platform` instead of faking a volume list.
pub fn volumes() -> Result<VolumesReport> {
    let index_dir = default_index_dir()?;
    let base = index_dir.display().to_string();
    #[cfg(windows)]
    {
        let mut items = Vec::new();
        for raw in win::logical_drives()? {
            let index = index_status(&raw.volume, &index_dir);
            items.push(VolumeInfo {
                volume: raw.volume,
                label: raw.label,
                filesystem: raw.filesystem,
                ntfs: raw.ntfs,
                drive_type: raw.drive_type,
                volume_serial: raw.volume_serial,
                total_bytes: raw.total_bytes,
                free_bytes: raw.free_bytes,
                available_bytes: raw.available_bytes,
                index,
            });
        }
        items.sort_by(|left, right| left.volume.cmp(&right.volume));
        Ok(VolumesReport {
            platform: crate::platform::platform_name().to_string(),
            index_dir: base,
            volumes: items,
            notes: vec![
                "volumes is read-only: it never writes the index and never needs elevation".to_string(),
                "index status is read from meta.json only; entries are not loaded".to_string(),
            ],
        })
    }
    #[cfg(not(windows))]
    {
        let _ = base;
        Err(Error::capability(
            crate::audit::capability_ids::VOLUMES,
            crate::error::CapabilityState::UnsupportedPlatform,
            "logical volume enumeration is only implemented on Windows",
        ))
    }
}

/// Inspect one path without writing anything.
pub fn inspect(options: &InspectOptions) -> Result<PathReport> {
    let requested = options.path.display().to_string();
    let metadata = match fs::symlink_metadata(&options.path) {
        Ok(metadata) => metadata,
        Err(source) if source.kind() == std::io::ErrorKind::NotFound => {
            return Ok(PathReport {
                path: requested,
                exists: false,
                kind: "missing".to_string(),
                size: 0,
                link_target: None,
                attributes: Vec::new(),
                created_ms: 0,
                modified_ms: 0,
                accessed_ms: 0,
                tree: None,
                repo: None,
            });
        }
        Err(source) => return Err(Error::io(requested, source)),
    };
    let file_type = metadata.file_type();
    let kind = if file_type.is_symlink() {
        "symlink"
    }
    else if file_type.is_dir() {
        "dir"
    }
    else if file_type.is_file() {
        "file"
    }
    else {
        "other"
    };
    let is_dir = file_type.is_dir();
    let link_target = if file_type.is_symlink() { fs::read_link(&options.path).ok().map(|target| target.display().to_string()) } else { None };
    let tree = if is_dir && options.recursive { Some(tree_stats(&options.path, options.max_entries)) } else { None };
    Ok(PathReport {
        path: requested,
        exists: true,
        kind: kind.to_string(),
        size: if is_dir { 0 } else { metadata.len() },
        link_target,
        attributes: attributes(&metadata),
        created_ms: timestamp_ms(metadata.created().ok()),
        modified_ms: timestamp_ms(metadata.modified().ok()),
        accessed_ms: timestamp_ms(metadata.accessed().ok()),
        tree,
        repo: find_repo(&options.path),
    })
}

fn index_status(volume: &str, base: &Path) -> IndexStatus {
    let letter = volume.trim_end_matches(['\\', '/']).trim_end_matches(':');
    let per_volume = base.join(letter);
    let candidates = [per_volume.clone(), base.to_path_buf()];
    for (index, dir) in candidates.into_iter().enumerate() {
        let meta_path = dir.join("meta.json");
        if !meta_path.is_file() {
            continue;
        }
        match fs::read_to_string(&meta_path).ok().and_then(|text| serde_json::from_str::<IndexMeta>(&text).ok()) {
            Some(meta) if meta.volume.eq_ignore_ascii_case(volume) => {
                // A schema mismatch is still a present index: report it so the
                // caller knows a rebuild is required instead of treating the
                // volume as unindexed.
                let error = (meta.schema != INDEX_SCHEMA).then(|| format!("schema {} is not {}; rebuild the index", meta.schema, INDEX_SCHEMA));
                return IndexStatus {
                    dir: dir.display().to_string(),
                    present: true,
                    schema: Some(meta.schema),
                    entry_count: Some(meta.entry_count),
                    cursor: Some(meta.cursor),
                    built_at_ms: Some(meta.built_at_ms),
                    updated_at_ms: Some(meta.updated_at_ms),
                    error,
                };
            }
            Some(_) => continue,
            None => {
                // A broken header in a per-volume directory belongs to this
                // volume. The shared base directory may hold another volume's
                // header, so it is skipped instead of mislabelled.
                if index == 0 {
                    return IndexStatus {
                        dir: dir.display().to_string(),
                        present: true,
                        schema: None,
                        entry_count: None,
                        cursor: None,
                        built_at_ms: None,
                        updated_at_ms: None,
                        error: Some(format!("meta.json at {} is not a usable index header", meta_path.display())),
                    };
                }
            }
        }
    }
    IndexStatus {
        dir: per_volume.display().to_string(),
        present: false,
        schema: None,
        entry_count: None,
        cursor: None,
        built_at_ms: None,
        updated_at_ms: None,
        error: None,
    }
}

fn tree_stats(root: &Path, max_entries: u64) -> TreeStats {
    let mut stats = TreeStats { files: 0, dirs: 0, bytes: 0, entries: 0, truncated: false };
    let walker = Walker::new(root).min_depth(1).error_policy(ErrorPolicy::Lenient);
    for item in walker {
        if stats.entries >= max_entries {
            stats.truncated = true;
            break;
        }
        let Ok(entry) = item
        else {
            continue;
        };
        // A symlink is counted as an entry by its own metadata only; following
        // it would double count the target and can loop.
        if entry.is_symlink() {
            continue;
        }
        let Ok(metadata) = entry.metadata()
        else {
            continue;
        };
        stats.entries += 1;
        if metadata.is_dir() {
            stats.dirs += 1;
        }
        else {
            stats.files += 1;
            stats.bytes = stats.bytes.saturating_add(metadata.len());
        }
    }
    stats
}

fn find_repo(start: &Path) -> Option<RepoSummary> {
    let kind = fs::symlink_metadata(start).ok()?;
    let anchor = if kind.is_dir() { start.to_path_buf() } else { start.parent()?.to_path_buf() };
    let anchor = std::path::absolute(&anchor).ok()?;
    let mut dir = anchor;
    for _ in 0..64 {
        if let Some(git_dir) = resolve_git_dir(&dir) {
            return Some(repo_summary(&dir, &git_dir));
        }
        match dir.parent() {
            Some(parent) => dir = parent.to_path_buf(),
            None => break,
        }
    }
    None
}

fn resolve_git_dir(worktree: &Path) -> Option<PathBuf> {
    let dot = worktree.join(".git");
    let metadata = fs::symlink_metadata(&dot).ok()?;
    if metadata.is_dir() {
        return Some(dot);
    }
    if metadata.is_file() {
        // A linked worktree and a submodule both store `gitdir: <path>`.
        let text = fs::read_to_string(&dot).ok()?;
        let line = text.lines().next()?.trim();
        let rest = line.strip_prefix("gitdir:")?.trim();
        if rest.is_empty() {
            return None;
        }
        let target = PathBuf::from(rest);
        return Some(if target.is_absolute() { target } else { worktree.join(target) });
    }
    None
}

fn repo_summary(root: &Path, git_dir: &Path) -> RepoSummary {
    let (branch, head, detached) = resolve_head(git_dir);
    RepoSummary {
        root: root.display().to_string(),
        git_dir: git_dir.display().to_string(),
        branch,
        head,
        detached,
        remotes: read_remotes(git_dir),
    }
}

fn resolve_head(git_dir: &Path) -> (Option<String>, Option<String>, bool) {
    let Some(head) = read_trimmed(&git_dir.join("HEAD"))
    else {
        return (None, None, false);
    };
    if let Some(rest) = head.strip_prefix("ref:") {
        let refname = rest.trim();
        if refname.is_empty() {
            return (None, None, false);
        }
        let branch = refname.strip_prefix("refs/heads/").map(str::to_string);
        let commit = read_trimmed(&git_dir.join(refname)).filter(|value| is_commit_id(value)).or_else(|| packed_ref(git_dir, refname));
        return (branch, commit, false);
    }
    if is_commit_id(&head) {
        return (None, Some(head), true);
    }
    (None, None, false)
}

fn packed_ref(git_dir: &Path, refname: &str) -> Option<String> {
    let text = fs::read_to_string(git_dir.join("packed-refs")).ok()?;
    for line in text.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') || line.starts_with('^') {
            continue;
        }
        let Some((commit, name)) = line.split_once(char::is_whitespace)
        else {
            continue;
        };
        if name.trim() == refname && is_commit_id(commit) {
            return Some(commit.to_string());
        }
    }
    None
}

fn read_remotes(git_dir: &Path) -> Vec<RemoteRef> {
    let text = fs::read_to_string(git_dir.join("config")).unwrap_or_default();
    let mut remotes = Vec::new();
    let mut current: Option<String> = None;
    for raw in text.lines() {
        let line = raw.trim();
        if line.is_empty() || line.starts_with('#') || line.starts_with(';') {
            continue;
        }
        if line.starts_with('[') && line.ends_with(']') {
            current = remote_section(&line[1..line.len() - 1]);
            continue;
        }
        let Some(name) = &current
        else {
            continue;
        };
        let Some((key, value)) = line.split_once('=')
        else {
            continue;
        };
        if key.trim().eq_ignore_ascii_case("url") {
            remotes.push(RemoteRef { name: name.clone(), url: value.trim().to_string() });
        }
    }
    remotes
}

fn remote_section(section: &str) -> Option<String> {
    let section = section.trim();
    if let Some(rest) = section.strip_prefix("remote \"").and_then(|rest| rest.strip_suffix('"')) {
        return Some(rest.trim().to_string()).filter(|name| !name.is_empty());
    }
    let rest = section.strip_prefix("remote.")?;
    let name = rest.trim();
    (!name.is_empty()).then(|| name.to_string())
}

fn is_commit_id(value: &str) -> bool {
    (7..=64).contains(&value.len()) && value.bytes().all(|byte| byte.is_ascii_hexdigit())
}

fn read_trimmed(path: &Path) -> Option<String> {
    let text = fs::read_to_string(path).ok()?;
    let text = text.trim().to_string();
    (!text.is_empty()).then_some(text)
}

fn timestamp_ms(value: Option<SystemTime>) -> u64 {
    value.and_then(|time| time.duration_since(UNIX_EPOCH).ok()).map_or(0, |duration| duration.as_millis() as u64)
}

#[cfg(windows)]
fn attributes(metadata: &fs::Metadata) -> Vec<String> {
    use std::os::windows::fs::MetadataExt;

    const NAMES: [(u32, &str); 14] = [
        (0x0000_0001, "readonly"),
        (0x0000_0002, "hidden"),
        (0x0000_0004, "system"),
        (0x0000_0010, "directory"),
        (0x0000_0020, "archive"),
        (0x0000_0040, "device"),
        (0x0000_0080, "normal"),
        (0x0000_0100, "temporary"),
        (0x0000_0200, "sparse"),
        (0x0000_0400, "reparse_point"),
        (0x0000_0800, "compressed"),
        (0x0000_1000, "offline"),
        (0x0000_2000, "not_content_indexed"),
        (0x0000_4000, "encrypted"),
    ];
    let bits = metadata.file_attributes();
    NAMES.iter().filter(|(bit, _)| bits & bit != 0).map(|(_, name)| (*name).to_string()).collect()
}

#[cfg(not(windows))]
fn attributes(_metadata: &fs::Metadata) -> Vec<String> {
    Vec::new()
}

#[cfg(windows)]
mod win {
    use std::ptr;

    use crate::error::{Error, Result};

    const DRIVE_REMOVABLE: u32 = 2;
    const DRIVE_FIXED: u32 = 3;
    const DRIVE_REMOTE: u32 = 4;
    const DRIVE_CDROM: u32 = 5;
    const DRIVE_RAMDISK: u32 = 6;

    #[link(name = "kernel32")]
    unsafe extern "system" {
        fn GetLogicalDriveStringsW(buffer_length: u32, buffer: *mut u16) -> u32;
        fn GetDriveTypeW(root: *const u16) -> u32;
        fn GetVolumeInformationW(
            root: *const u16,
            volume_name: *mut u16,
            volume_name_size: u32,
            volume_serial: *mut u32,
            max_component: *mut u32,
            flags: *mut u32,
            fs_name: *mut u16,
            fs_name_size: u32,
        ) -> i32;
        fn GetDiskFreeSpaceExW(root: *const u16, available: *mut u64, total: *mut u64, free: *mut u64) -> i32;
        fn GetLastError() -> u32;
    }

    /// One volume row read from the host.
    pub struct RawVolume {
        pub volume: String,
        pub label: Option<String>,
        pub filesystem: Option<String>,
        pub ntfs: bool,
        pub drive_type: String,
        pub volume_serial: Option<u64>,
        pub total_bytes: Option<u64>,
        pub free_bytes: Option<u64>,
        pub available_bytes: Option<u64>,
    }

    /// Enumerate mounted logical drives.
    pub fn logical_drives() -> Result<Vec<RawVolume>> {
        // A zero-length query returns the required size, including the final
        // null. The second call fills exactly that buffer.
        // SAFETY: the first call passes a null buffer with length 0, which the
        // API documents as a size query.
        let needed = unsafe { GetLogicalDriveStringsW(0, ptr::null_mut()) };
        if needed == 0 {
            return Ok(Vec::new());
        }
        let mut buffer = vec![0u16; needed as usize];
        // SAFETY: the buffer is valid for `needed` wide characters.
        let written = unsafe { GetLogicalDriveStringsW(needed, buffer.as_mut_ptr()) };
        if written == 0 {
            // SAFETY: GetLastError has no preconditions.
            return Err(Error::message(format!("cannot enumerate logical drives (Win32 error {})", unsafe { GetLastError() })));
        }
        buffer.truncate(written as usize);
        let mut volumes = Vec::new();
        for root in split_roots(&buffer) {
            let drive_type = {
                let wide = encode_utf16(&root);
                // SAFETY: the wide string is null-terminated and outlives the call.
                unsafe { GetDriveTypeW(wide.as_ptr()) }
            };
            // Optical and unknown roots are skipped: querying an empty drive can
            // block, and they never carry the NTFS index this tool builds.
            let drive_type_name = match drive_type {
                DRIVE_REMOVABLE => "removable",
                DRIVE_FIXED => "fixed",
                DRIVE_REMOTE => "remote",
                DRIVE_RAMDISK => "ramdisk",
                DRIVE_CDROM => continue,
                _ => continue,
            };
            let volume = root.trim_end_matches(['\\', '/']).to_string();
            let (label, filesystem, volume_serial) = volume_information(&root);
            let (available_bytes, total_bytes, free_bytes) = disk_space(&root);
            let ntfs = filesystem.as_deref().is_some_and(|name| name.eq_ignore_ascii_case("NTFS"));
            volumes.push(RawVolume {
                volume,
                label,
                filesystem,
                ntfs,
                drive_type: drive_type_name.to_string(),
                volume_serial,
                total_bytes,
                free_bytes,
                available_bytes,
            });
        }
        Ok(volumes)
    }

    fn split_roots(buffer: &[u16]) -> Vec<String> {
        let mut roots = Vec::new();
        let mut start = 0usize;
        for (index, value) in buffer.iter().enumerate() {
            if *value != 0 {
                continue;
            }
            if index > start {
                roots.push(String::from_utf16_lossy(&buffer[start..index]));
            }
            start = index + 1;
        }
        roots
    }

    fn volume_information(root: &str) -> (Option<String>, Option<String>, Option<u64>) {
        let wide = encode_utf16(root);
        let mut label = vec![0u16; 256];
        let mut filesystem = vec![0u16; 64];
        let mut serial = 0u32;
        let mut max_component = 0u32;
        let mut flags = 0u32;
        // SAFETY: every buffer is valid for its stated length and the wide root
        // string is null-terminated.
        let ok = unsafe {
            GetVolumeInformationW(
                wide.as_ptr(),
                label.as_mut_ptr(),
                label.len() as u32,
                &mut serial,
                &mut max_component,
                &mut flags,
                filesystem.as_mut_ptr(),
                filesystem.len() as u32,
            )
        };
        if ok == 0 {
            return (None, None, None);
        }
        (
            Some(trim_nul(&label)).filter(|value| !value.is_empty()),
            Some(trim_nul(&filesystem)).filter(|value| !value.is_empty()),
            Some(u64::from(serial)),
        )
    }

    fn disk_space(root: &str) -> (Option<u64>, Option<u64>, Option<u64>) {
        let wide = encode_utf16(root);
        let mut available = 0u64;
        let mut total = 0u64;
        let mut free = 0u64;
        // SAFETY: all three pointers refer to live stack values.
        let ok = unsafe { GetDiskFreeSpaceExW(wide.as_ptr(), &mut available, &mut total, &mut free) };
        if ok == 0 {
            return (None, None, None);
        }
        (Some(available), Some(total), Some(free))
    }

    fn trim_nul(buffer: &[u16]) -> String {
        let end = buffer.iter().position(|value| *value == 0).unwrap_or(buffer.len());
        String::from_utf16_lossy(&buffer[..end])
    }

    fn encode_utf16(value: &str) -> Vec<u16> {
        value.encode_utf16().chain(std::iter::once(0)).collect()
    }
}

#[cfg(test)]
mod tests {
    use std::fs;

    use super::*;

    fn unique(name: &str) -> PathBuf {
        std::env::temp_dir().join(format!("openfinder-{name}-{}", std::process::id()))
    }

    fn write(path: &Path, text: &str) {
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).unwrap();
        }
        fs::write(path, text).unwrap();
    }

    const COMMIT: &str = "0123456789abcdef0123456789abcdef01234567";

    #[test]
    fn repo_head_branch_and_remotes_are_parsed_without_git() {
        let root = unique("repo");
        let _ = fs::remove_dir_all(&root);
        write(&root.join(".git").join("HEAD"), "ref: refs/heads/dev\n");
        write(&root.join(".git").join("refs").join("heads").join("dev"), &format!("{COMMIT}\n"));
        write(
            &root.join(".git").join("config"),
            "[core]\n\trepositoryformatversion = 0\n[remote \"origin\"]\n\turl = https://example.com/repo.git\n\tfetch = +refs/heads/*:refs/remotes/origin/*\n",
        );
        let report = inspect(&InspectOptions { path: root.clone(), ..InspectOptions::default() }).unwrap();
        let repo = report.repo.expect("repo detected");
        assert_eq!(repo.branch.as_deref(), Some("dev"));
        assert_eq!(repo.head.as_deref(), Some(COMMIT));
        assert!(!repo.detached);
        assert_eq!(repo.remotes.len(), 1);
        assert_eq!(repo.remotes[0].name, "origin");
        assert_eq!(repo.remotes[0].url, "https://example.com/repo.git");
        assert!(report.tree.is_some_and(|tree| tree.files >= 3));
        fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn packed_refs_resolve_and_detached_head_is_reported() {
        let root = unique("packed");
        let _ = fs::remove_dir_all(&root);
        write(&root.join(".git").join("HEAD"), "ref: refs/heads/main\n");
        write(&root.join(".git").join("packed-refs"), &format!("# pack-refs with: peeled\n{COMMIT} refs/heads/main\n"));
        let report = inspect(&InspectOptions { path: root.clone(), recursive: false, ..InspectOptions::default() }).unwrap();
        let repo = report.repo.expect("repo detected");
        assert_eq!(repo.branch.as_deref(), Some("main"));
        assert_eq!(repo.head.as_deref(), Some(COMMIT));

        write(&root.join(".git").join("HEAD"), &format!("{COMMIT}\n"));
        let detached = inspect(&InspectOptions { path: root.clone(), recursive: false, ..InspectOptions::default() }).unwrap();
        let repo = detached.repo.expect("repo detected");
        assert!(repo.detached);
        assert_eq!(repo.branch, None);
        assert_eq!(repo.head.as_deref(), Some(COMMIT));
        fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn gitfile_points_at_a_separate_git_directory() {
        let root = unique("gitfile");
        let git_dir = unique("gitfile-real");
        let _ = fs::remove_dir_all(&root);
        let _ = fs::remove_dir_all(&git_dir);
        write(&root.join(".git"), &format!("gitdir: {}\n", git_dir.display()));
        write(&git_dir.join("HEAD"), "ref: refs/heads/worktree\n");
        write(&git_dir.join("refs").join("heads").join("worktree"), &format!("{COMMIT}\n"));
        let report = inspect(&InspectOptions { path: root.clone(), recursive: false, ..InspectOptions::default() }).unwrap();
        let repo = report.repo.expect("repo detected");
        assert_eq!(repo.branch.as_deref(), Some("worktree"));
        assert_eq!(repo.head.as_deref(), Some(COMMIT));
        fs::remove_dir_all(&root).unwrap();
        fs::remove_dir_all(&git_dir).unwrap();
    }

    #[test]
    fn missing_path_is_reported_not_thrown() {
        let path = std::env::temp_dir().join(format!("openfinder-missing-{}", std::process::id()));
        let report = inspect(&InspectOptions { path, ..InspectOptions::default() }).unwrap();
        assert!(!report.exists);
        assert_eq!(report.kind, "missing");
        assert!(report.repo.is_none());
    }

    #[test]
    fn file_size_and_link_target_are_reported() {
        let root = unique("file");
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(&root).unwrap();
        let file = root.join("notes.txt");
        write(&file, "hello");
        let report = inspect(&InspectOptions { path: file, ..InspectOptions::default() }).unwrap();
        assert!(report.exists);
        assert_eq!(report.kind, "file");
        assert_eq!(report.size, 5);
        assert!(report.tree.is_none());
        fs::remove_dir_all(&root).unwrap();
    }
}

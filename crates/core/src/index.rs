//! Index model, persistence, path reconstruction, and USN-based updates.
//!
//! Storage format (schema `open-finder.index/v0`):
//!
//! ```text
//! <index-dir>/meta.json        one JSON object: volume, geometry, cursor, counts
//! <index-dir>/entries.ndjson   one JSON entry per line, keyed by FRN
//! ```
//!
//! The default directory is `<config-dir>/index`; `--index-dir` or the
//! `OPEN_FINDER_INDEX_DIR` environment variable override it.

use std::{
    collections::HashMap,
    fs,
    io::{BufRead, BufReader, Write},
    path::{Path, PathBuf},
    time::{SystemTime, UNIX_EPOCH},
};

use serde::{Deserialize, Serialize};

use crate::{
    error::{Error, Result},
    mft::MftReader,
    usn,
    volume::Volume,
};

/// Index schema identifier. Bump only with a breaking on-disk change.
pub const INDEX_SCHEMA: &str = "open-finder.index/v0";

/// One indexed file or directory.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct IndexEntry {
    /// File record number.
    pub frn: u64,
    /// Parent directory file record number.
    pub parent_frn: u64,
    /// File name without a path.
    pub name: String,
    /// Whether the entry is a directory.
    pub is_dir: bool,
    /// Logical size in bytes.
    pub size: u64,
    /// Creation time in Unix milliseconds.
    pub created_ms: u64,
    /// Last modification time in Unix milliseconds.
    pub modified_ms: u64,
    /// Last access time in Unix milliseconds.
    pub accessed_ms: u64,
}

/// Index header persisted beside the entries.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct IndexMeta {
    /// On-disk schema.
    pub schema: String,
    /// Volume label such as `C:`.
    pub volume: String,
    /// NTFS volume serial.
    pub volume_serial: u64,
    /// File record size.
    pub record_size: u64,
    /// Build or update method: `mft` or `usn`.
    pub method: String,
    /// Last complete build time in Unix milliseconds.
    pub built_at_ms: u64,
    /// Last update time in Unix milliseconds.
    pub updated_at_ms: u64,
    /// USN cursor for the next incremental read; `0` means no baseline.
    pub cursor: u64,
    /// Number of MFT record slots scanned at build time.
    pub record_count: u64,
    /// Number of entries in this index.
    pub entry_count: u64,
}

/// In-memory index.
#[derive(Debug, Clone)]
pub struct Index {
    /// Header.
    pub meta: IndexMeta,
    /// Entries keyed by FRN.
    pub entries: HashMap<u64, IndexEntry>,
}

impl Index {
    /// Build a full path from the volume root for one entry.
    pub fn path_of(&self, frn: u64) -> String {
        let root = volume_root(&self.meta.volume);
        let Some(entry) = self.entries.get(&frn)
        else {
            return root;
        };
        let mut parts = Vec::new();
        let mut current = entry;
        for _ in 0..64 {
            if current.frn == 5 || current.parent_frn == current.frn || current.name == "." {
                break;
            }
            parts.push(current.name.clone());
            let Some(parent) = self.entries.get(&current.parent_frn)
            else {
                parts.push("<missing>".to_string());
                break;
            };
            current = parent;
        }
        if parts.is_empty() {
            root
        }
        else {
            parts.reverse();
            format!("{}\\{}", root.trim_end_matches('\\'), parts.join("\\"))
        }
    }

    /// Resolve an entry's full path.
    pub fn hit(&self, entry: &IndexEntry) -> crate::query::SearchHit {
        crate::query::SearchHit {
            frn: entry.frn,
            name: entry.name.clone(),
            path: self.path_of(entry.frn),
            is_dir: entry.is_dir,
            size: entry.size,
            created_ms: entry.created_ms,
            modified_ms: entry.modified_ms,
            accessed_ms: entry.accessed_ms,
        }
    }

    /// Remove one entry and every indexed descendant.
    pub fn remove_subtree(&mut self, frn: u64) -> u64 {
        let mut targets = vec![frn];
        let mut removed = 0u64;
        while let Some(target) = targets.pop() {
            if self.entries.remove(&target).is_some() {
                removed += 1;
            }
            let children: Vec<u64> = self.entries.iter().filter(|(_, entry)| entry.parent_frn == target).map(|(child, _)| *child).collect();
            targets.extend(children);
        }
        self.meta.entry_count = self.entries.len() as u64;
        removed
    }

    /// Persist the index directory.
    pub fn save(&self, dir: &Path) -> Result<()> {
        fs::create_dir_all(dir).map_err(|source| Error::io(dir.display().to_string(), source))?;
        let entries_path = dir.join("entries.ndjson");
        let temp_entries = dir.join("entries.ndjson.tmp");
        {
            let file = fs::File::create(&temp_entries).map_err(|source| Error::io(temp_entries.display().to_string(), source))?;
            let mut writer = std::io::BufWriter::new(file);
            for entry in self.entries.values() {
                serde_json::to_writer(&mut writer, entry).map_err(|error| Error::message(format!("cannot encode index entry: {error}")))?;
                writer.write_all(b"\n").map_err(|source| Error::io(temp_entries.display().to_string(), source))?;
            }
            writer.flush().map_err(|source| Error::io(temp_entries.display().to_string(), source))?;
        }
        let meta_path = dir.join("meta.json");
        let temp_meta = dir.join("meta.json.tmp");
        let encoded = serde_json::to_vec_pretty(&self.meta).map_err(|error| Error::message(format!("cannot encode index meta: {error}")))?;
        fs::write(&temp_meta, encoded).map_err(|source| Error::io(temp_meta.display().to_string(), source))?;
        replace_file(&temp_entries, &entries_path)?;
        replace_file(&temp_meta, &meta_path)?;
        Ok(())
    }
}

/// Load an index directory.
pub fn load_index(dir: &Path) -> Result<Index> {
    let meta_path = dir.join("meta.json");
    let meta_text = fs::read_to_string(&meta_path).map_err(|source| Error::io(meta_path.display().to_string(), source))?;
    let meta: IndexMeta = serde_json::from_str(&meta_text)
        .map_err(|error| Error::message(format!("index meta at {} is not valid JSON: {error}", meta_path.display())))?;
    if meta.schema != INDEX_SCHEMA {
        return Err(Error::message(format!("index schema {} is not {}; rebuild the index", meta.schema, INDEX_SCHEMA)));
    }
    let entries_path = dir.join("entries.ndjson");
    let file = fs::File::open(&entries_path).map_err(|source| Error::io(entries_path.display().to_string(), source))?;
    let mut entries = HashMap::with_capacity(meta.entry_count as usize);
    for (line_index, line) in BufReader::new(file).lines().enumerate() {
        let line = line.map_err(|source| Error::io(entries_path.display().to_string(), source))?;
        if line.trim().is_empty() {
            continue;
        }
        let entry: IndexEntry = serde_json::from_str(&line)
            .map_err(|error| Error::message(format!("index entry on line {} is not valid JSON: {error}", line_index + 1)))?;
        entries.insert(entry.frn, entry);
    }
    Ok(Index { meta, entries })
}

/// Options for a full index build.
#[derive(Debug, Clone)]
pub struct BuildOptions {
    /// Volume label, such as `C:`.
    pub volume: String,
    /// Destination directory.
    pub index_dir: PathBuf,
    /// Optional scan cap for a partial build.
    pub record_limit: Option<u64>,
}

/// Result of a full index build.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct BuildReport {
    /// Volume label.
    pub volume: String,
    /// Index directory written.
    pub index_dir: String,
    /// NTFS volume serial.
    pub volume_serial: u64,
    /// File record size.
    pub record_size: u64,
    /// Record slots in the MFT data stream.
    pub record_count: u64,
    /// Record slots actually scanned.
    pub scanned: u64,
    /// Scanned slots that are unallocated or carry no `FILE` signature.
    pub unallocated: u64,
    /// Scanned slots that are allocated but have no `$FILE_NAME`.
    pub nameless: u64,
    /// Entries retained.
    pub entry_count: u64,
    /// USN cursor captured before scanning.
    pub cursor: u64,
    /// Elapsed milliseconds.
    pub took_ms: u64,
    /// Method, always `mft` for a full build.
    pub method: String,
}

/// Volume geometry and journal state, produced by a dry-run inspection.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct VolumeReport {
    /// Volume label.
    pub volume: String,
    /// NTFS volume serial.
    pub volume_serial: u64,
    /// Bytes per sector.
    pub bytes_per_sector: u64,
    /// Bytes per cluster.
    pub cluster_size: u64,
    /// File record size.
    pub record_size: u64,
    /// Record slots in the MFT data stream.
    pub record_count: u64,
    /// Journal id when available.
    pub journal_id: Option<u64>,
    /// Oldest available USN.
    pub first_usn: Option<u64>,
    /// Next USN.
    pub next_usn: Option<u64>,
    /// Whether indexing this volume needs an elevated process.
    pub requires_elevation: bool,
}

/// Result of an incremental update.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct UpdateReport {
    /// USN records consumed.
    pub applied: u64,
    /// Entries inserted or refreshed.
    pub reloaded: u64,
    /// Entries removed.
    pub removed: u64,
    /// New cursor saved into the index.
    pub cursor: u64,
    /// `true` when the journal no longer covers the saved cursor.
    pub needs_rescan: bool,
    /// Change source: `usn-record` for `FSCTL_READ_USN_JOURNAL` or `usn-enum`
    /// for the `FSCTL_ENUM_USN_DATA` fallback.
    pub strategy: String,
    /// Whether deleted files are observable through the change source.
    pub deletions_tracked: bool,
    /// Number of journal batches read.
    pub batches: u64,
    /// Elapsed milliseconds.
    pub took_ms: u64,
}

/// Inspect a volume without writing an index.
pub fn inspect_volume(volume: &str) -> Result<VolumeReport> {
    let mut handle = Volume::open(volume)?;
    let boot = handle.boot_sector()?;
    let record_size = boot.record_size()? as u64;
    let journal = usn::query(&mut handle).ok();
    let reader = MftReader::bootstrap(handle)?;
    let record_count = reader.record_count();
    Ok(VolumeReport {
        volume: volume.to_string(),
        volume_serial: boot.volume_serial,
        bytes_per_sector: u64::from(boot.bytes_per_sector),
        cluster_size: boot.cluster_size(),
        record_size,
        record_count,
        journal_id: journal.map(|state| state.id),
        first_usn: journal.map(|state| state.first_usn),
        next_usn: journal.map(|state| state.next_usn),
        requires_elevation: true,
    })
}

/// Build a full index by reading `$MFT` records.
pub fn build_index(options: &BuildOptions) -> Result<BuildReport> {
    let started = SystemTime::now();
    let mut handle = Volume::open(&options.volume)?;
    let journal = usn::query(&mut handle).ok();
    let cursor = journal.map_or(0, |state| state.next_usn);
    let boot = handle.boot_sector()?;
    let mut reader = MftReader::bootstrap(handle)?;
    let record_count = reader.record_count();
    let mut entries = HashMap::with_capacity(record_count.min(1_000_000) as usize);
    let stats = reader.scan(options.record_limit, |entry| {
        entries.insert(entry.frn, entry);
    })?;
    let now = now_ms();
    let index = Index {
        meta: IndexMeta {
            schema: INDEX_SCHEMA.to_string(),
            volume: options.volume.clone(),
            volume_serial: boot.volume_serial,
            record_size: boot.record_size()? as u64,
            method: "mft".to_string(),
            built_at_ms: now,
            updated_at_ms: now,
            cursor,
            record_count,
            entry_count: entries.len() as u64,
        },
        entries,
    };
    index.save(&options.index_dir)?;
    Ok(BuildReport {
        volume: options.volume.clone(),
        index_dir: options.index_dir.display().to_string(),
        volume_serial: boot.volume_serial,
        record_size: boot.record_size()? as u64,
        record_count,
        scanned: stats.scanned,
        unallocated: stats.unallocated,
        nameless: stats.nameless,
        entry_count: index.meta.entry_count,
        cursor,
        took_ms: elapsed_ms(started),
        method: "mft".to_string(),
    })
}

/// Apply USN journal changes to an existing index.
///
/// The primary source is `FSCTL_READ_USN_JOURNAL`, which reports every change
/// including deletions. When the volume rejects that control — some hosts
/// return `ERROR_INVALID_USER_BUFFER` regardless of request shape — the caller
/// falls back to `FSCTL_ENUM_USN_DATA` over the same USN window. That fallback
/// is reported as `strategy: usn-enum` with `deletionsTracked: false`, because
/// deleted records no longer exist in the MFT.
pub fn update_index(index_dir: &Path, max_records: u64) -> Result<UpdateReport> {
    let started = SystemTime::now();
    let mut index = load_index(index_dir)?;
    let mut handle = Volume::open(&index.meta.volume)?;
    let journal = usn::query(&mut handle)
        .map_err(|error| Error::capability("index-update", crate::error::CapabilityState::DependencyBlocked, error.to_string()))?;
    let cursor = index.meta.cursor;
    if cursor == 0 || cursor < journal.first_usn {
        return Ok(UpdateReport {
            applied: 0,
            reloaded: 0,
            removed: 0,
            cursor,
            needs_rescan: true,
            strategy: "usn-record".to_string(),
            deletions_tracked: true,
            batches: 0,
            took_ms: elapsed_ms(started),
        });
    }
    let mut reader = MftReader::bootstrap(handle)?;
    let mut report = UpdateReport {
        applied: 0,
        reloaded: 0,
        removed: 0,
        cursor,
        needs_rescan: false,
        strategy: "usn-record".to_string(),
        deletions_tracked: true,
        batches: 0,
        took_ms: 0,
    };
    const CHUNK: u32 = 65_536;
    let mut position = cursor;
    match usn::read(reader.volume_mut(), &journal, position, CHUNK) {
        Ok((next, records)) => {
            apply_batch(&mut index, &mut reader, &records, &mut report, max_records, false)?;
            report.batches += 1;
            if next > position && !records.is_empty() {
                position = next;
            }
            while report.applied < max_records {
                let (next, records) = usn::read(reader.volume_mut(), &journal, position, CHUNK)?;
                report.batches += 1;
                if records.is_empty() {
                    if next > position {
                        position = next;
                    }
                    break;
                }
                apply_batch(&mut index, &mut reader, &records, &mut report, max_records, false)?;
                if next <= position {
                    break;
                }
                position = next;
            }
        }
        Err(_) => {
            report.strategy = "usn-enum".to_string();
            report.deletions_tracked = false;
            // The MFT enumeration is keyed by file reference number, so the whole
            // window is consumed before the cursor advances. Stopping early would
            // silently drop changes, and `max_records` therefore only bounds the
            // journal-record path.
            let mut start_frn = 0u64;
            while let Some((next_frn, records)) = usn::enumerate(reader.volume_mut(), cursor, journal.next_usn, start_frn, CHUNK)? {
                report.batches += 1;
                if records.is_empty() || next_frn <= start_frn {
                    break;
                }
                apply_batch(&mut index, &mut reader, &records, &mut report, u64::MAX, true)?;
                start_frn = next_frn;
            }
            position = journal.next_usn;
        }
    }
    report.cursor = position;
    index.meta.cursor = position;
    index.meta.updated_at_ms = now_ms();
    index.meta.method = "usn".to_string();
    index.meta.entry_count = index.entries.len() as u64;
    index.save(index_dir)?;
    report.took_ms = elapsed_ms(started);
    Ok(report)
}

/// Apply one page of journal records to the index, honouring `max_records`.
///
/// `reload_all` refreshes every listed record regardless of its reason flags.
/// The MFT enumeration path needs that: it returns one current-state record per
/// file, so the reason mask may only carry a trailing flag such as a close.
fn apply_batch(
    index: &mut Index,
    reader: &mut MftReader<Volume>,
    records: &[usn::UsnRecord],
    report: &mut UpdateReport,
    max_records: u64,
    reload_all: bool,
) -> Result<()> {
    for record in records {
        if report.applied >= max_records {
            break;
        }
        report.applied += 1;
        if record.is_delete() {
            report.removed += index.remove_subtree(record.frn);
        }
        else if reload_all || record.requires_reload() {
            match reader.read_record(record.frn)? {
                Some(entry) => {
                    report.reloaded += 1;
                    index.entries.insert(entry.frn, entry);
                }
                None => report.removed += index.remove_subtree(record.frn),
            }
        }
    }
    Ok(())
}

fn replace_file(from: &Path, to: &Path) -> Result<()> {
    if to.exists() {
        fs::remove_file(to).map_err(|source| Error::io(to.display().to_string(), source))?;
    }
    fs::rename(from, to).map_err(|source| Error::io(to.display().to_string(), source))
}

fn volume_root(volume: &str) -> String {
    if volume.contains('\\') { volume.trim_end_matches(['\\', '/']).to_string() } else { format!("{}\\", volume.trim_end_matches(['\\', '/'])) }
}

fn now_ms() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map_or(0, |duration| duration.as_millis() as u64)
}

fn elapsed_ms(started: SystemTime) -> u64 {
    SystemTime::now().duration_since(started).map_or(0, |duration| duration.as_millis() as u64)
}

#[cfg(test)]
mod tests {
    use std::fs;

    use super::*;

    fn entry(frn: u64, parent_frn: u64, name: &str, is_dir: bool, size: u64) -> IndexEntry {
        IndexEntry { frn, parent_frn, name: name.to_string(), is_dir, size, created_ms: 1, modified_ms: 2, accessed_ms: 3 }
    }

    fn sample_index() -> Index {
        let entries = [
            entry(5, 5, ".", true, 0),
            entry(10, 5, "Users", true, 0),
            entry(11, 10, "alice", true, 0),
            entry(12, 11, "notes.txt", false, 900),
        ]
        .into_iter()
        .map(|entry| (entry.frn, entry))
        .collect();
        Index {
            meta: IndexMeta {
                schema: INDEX_SCHEMA.to_string(),
                volume: "C:".to_string(),
                volume_serial: 7,
                record_size: 1024,
                method: "mft".to_string(),
                built_at_ms: 1,
                updated_at_ms: 1,
                cursor: 100,
                record_count: 4,
                entry_count: 4,
            },
            entries,
        }
    }

    #[test]
    fn paths_are_rebuilt_from_parent_links() {
        let index = sample_index();
        assert_eq!(index.path_of(12), "C:\\Users\\alice\\notes.txt");
        assert_eq!(index.path_of(5), "C:\\");
        assert_eq!(index.path_of(999), "C:\\");
    }

    #[test]
    fn subtree_removal_removes_descendants() {
        let mut index = sample_index();
        let removed = index.remove_subtree(10);
        assert_eq!(removed, 3);
        assert_eq!(index.meta.entry_count, 1);
    }

    #[test]
    fn index_round_trips_through_disk() {
        let index = sample_index();
        let dir = std::env::temp_dir().join(format!("openfinder-index-test-{}", std::process::id()));
        index.save(&dir).unwrap();
        let loaded = load_index(&dir).unwrap();
        assert_eq!(loaded.entries.len(), 4);
        assert_eq!(loaded.path_of(12), "C:\\Users\\alice\\notes.txt");
        fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn unknown_schema_is_rejected() {
        let dir = std::env::temp_dir().join(format!("openfinder-schema-test-{}", std::process::id()));
        fs::create_dir_all(&dir).unwrap();
        fs::write(dir.join("meta.json"), br#"{"schema":"open-finder.index/v9","volume":"C:","volumeSerial":1,"recordSize":1024,"method":"mft","builtAtMs":0,"updatedAtMs":0,"cursor":0,"recordCount":0,"entryCount":0}"#)
            .unwrap();
        fs::write(dir.join("entries.ndjson"), "").unwrap();
        let error = load_index(&dir).err().unwrap();
        assert!(error.to_string().contains("rebuild the index"));
        fs::remove_dir_all(&dir).unwrap();
    }
}

//! Explicit non-index directory walk.
//!
//! This mode exists for platforms without an NTFS index and for "search this
//! folder now" requests. It is labelled `indexed: false` everywhere and never
//! runs implicitly when an index operation fails.

use std::{
    fs,
    path::PathBuf,
    time::{SystemTime, UNIX_EPOCH},
};

use serde::Deserialize;

use crate::{
    error::{Error, Result},
    query::{EntryKind, MAX_LIMIT, QuerySpec, SearchHit, SearchPage, SortKey, compile_matcher},
};

/// Options for a portable walk.
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct WalkOptions {
    /// Directory to start from.
    pub root: PathBuf,
    /// Maximum directory depth below the root.
    pub max_depth: u32,
    /// Maximum number of entries inspected.
    pub max_entries: u64,
    /// Filters and paging.
    pub spec: QuerySpec,
}

impl Default for WalkOptions {
    fn default() -> Self {
        Self { root: PathBuf::from("."), max_depth: 8, max_entries: 100_000, spec: QuerySpec::default() }
    }
}

/// Walk a directory tree explicitly.
pub fn walk(options: &WalkOptions) -> Result<SearchPage> {
    let started = SystemTime::now();
    let metadata = fs::metadata(&options.root).map_err(|source| Error::io(options.root.display().to_string(), source))?;
    if !metadata.is_dir() {
        return Err(Error::message(format!("{} is not a directory", options.root.display())));
    }
    let matcher = compile_matcher(&options.spec)?;
    let path_filter = options.spec.path_filter.as_ref().map(|filter| filter.to_lowercase());
    let mut hits = Vec::new();
    let mut skipped = 0u64;
    let mut inspected = 0u64;
    let mut stack = vec![(options.root.clone(), 0u32)];
    let mut truncated = false;
    'walk: while let Some((directory, depth)) = stack.pop() {
        let Ok(reader) = fs::read_dir(&directory)
        else {
            skipped += 1;
            continue;
        };
        for entry in reader {
            if inspected >= options.max_entries {
                // Stop the whole walk, not just this directory, and mark the page
                // so `total` is never read as the size of the full tree.
                truncated = true;
                break 'walk;
            }
            let Ok(entry) = entry
            else {
                skipped += 1;
                continue;
            };
            inspected += 1;
            let Ok(file_type) = entry.file_type()
            else {
                skipped += 1;
                continue;
            };
            if file_type.is_symlink() {
                skipped += 1;
                continue;
            }
            let Ok(metadata) = entry.metadata()
            else {
                skipped += 1;
                continue;
            };
            let is_dir = metadata.is_dir();
            let name = entry.file_name().to_string_lossy().to_string();
            let path = entry.path();
            let path_text = path.display().to_string();
            if depth < options.max_depth && is_dir {
                stack.push((path.clone(), depth + 1));
            }
            if let Some(kind) = options.spec.kind {
                let wanted = match kind {
                    EntryKind::File => !is_dir,
                    EntryKind::Directory => is_dir,
                };
                if !wanted {
                    continue;
                }
            }
            let size = if is_dir { 0 } else { metadata.len() };
            if options.spec.min_size.is_some_and(|minimum| size < minimum) || options.spec.max_size.is_some_and(|maximum| size > maximum) {
                continue;
            }
            let modified_ms = timestamp_ms(metadata.modified().ok());
            if options.spec.modified_after_ms.is_some_and(|after| modified_ms < after)
                || options.spec.modified_before_ms.is_some_and(|before| modified_ms > before)
            {
                continue;
            }
            if let Some(filter) = &path_filter
                && !path_text.to_lowercase().contains(filter)
            {
                continue;
            }
            if let Some(matcher) = &matcher {
                let subject = if options.spec.match_path { path_text.as_str() } else { name.as_str() };
                if !matcher.is_match(subject) {
                    continue;
                }
            }
            hits.push(SearchHit {
                frn: 0,
                name,
                path: path_text,
                is_dir,
                size,
                created_ms: timestamp_ms(metadata.created().ok()),
                modified_ms,
                accessed_ms: timestamp_ms(metadata.accessed().ok()),
            });
        }
    }
    let total = hits.len() as u64;
    match options.spec.sort {
        SortKey::Name => hits.sort_by_key(|hit| hit.name.to_lowercase()),
        SortKey::Path => hits.sort_by_key(|hit| hit.path.to_lowercase()),
        SortKey::Size => hits.sort_by_key(|hit| hit.size),
        SortKey::Modified => hits.sort_by_key(|hit| hit.modified_ms),
    }
    if options.spec.descending {
        hits.reverse();
    }
    let limit = options.spec.limit.clamp(1, MAX_LIMIT);
    let offset = options.spec.offset;
    let page: Vec<SearchHit> = hits.iter().skip(offset).take(limit).cloned().collect();
    let has_more = offset.saturating_add(page.len()) < hits.len();
    Ok(SearchPage {
        total,
        offset,
        limit,
        has_more,
        hits: page,
        mode: "portable-walk".to_string(),
        indexed: false,
        root: Some(options.root.display().to_string()),
        skipped,
        truncated,
        took_ms: SystemTime::now().duration_since(started).map_or(0, |duration| duration.as_millis() as u64),
    })
}

fn timestamp_ms(value: Option<SystemTime>) -> u64 {
    value.and_then(|time| time.duration_since(UNIX_EPOCH).ok()).map_or(0, |duration| duration.as_millis() as u64)
}

#[cfg(test)]
mod tests {
    use std::fs;

    use super::*;

    #[test]
    fn walk_finds_matching_files_and_labels_itself_non_indexed() {
        let root = std::env::temp_dir().join(format!("openfinder-walk-{}", std::process::id()));
        fs::create_dir_all(root.join("nested")).unwrap();
        fs::write(root.join("alpha.txt"), b"alpha").unwrap();
        fs::write(root.join("nested").join("beta.log"), b"beta").unwrap();
        let page = walk(&WalkOptions {
            root: root.clone(),
            spec: QuerySpec { pattern: Some("*.log".to_string()), ..QuerySpec::default() },
            ..WalkOptions::default()
        })
        .unwrap();
        assert_eq!(page.total, 1);
        assert_eq!(page.hits[0].name, "beta.log");
        assert!(!page.indexed);
        assert_eq!(page.mode, "portable-walk");
        assert!(!page.truncated);
        fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn walk_reports_truncation_at_the_entry_cap() {
        let root = std::env::temp_dir().join(format!("openfinder-cap-{}", std::process::id()));
        fs::create_dir_all(&root).unwrap();
        for index in 0..12 {
            fs::write(root.join(format!("file-{index}.txt")), b"x").unwrap();
        }
        let page = walk(&WalkOptions {
            root: root.clone(),
            max_entries: 5,
            spec: QuerySpec { limit: 50, ..QuerySpec::default() },
            ..WalkOptions::default()
        })
        .unwrap();
        assert_eq!(page.total, 5);
        assert!(page.truncated, "a run stopped by `maxEntries` must say so");
        assert!(!page.has_more, "truncation is not a page with more rows");
        fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn walk_respects_depth() {
        let root = std::env::temp_dir().join(format!("openfinder-depth-{}", std::process::id()));
        fs::create_dir_all(root.join("a").join("b")).unwrap();
        fs::write(root.join("a").join("b").join("deep.txt"), b"deep").unwrap();
        let page = walk(&WalkOptions {
            root,
            max_depth: 1,
            spec: QuerySpec { pattern: Some("deep.txt".to_string()), ..QuerySpec::default() },
            ..WalkOptions::default()
        })
        .unwrap();
        assert_eq!(page.total, 0);
        fs::remove_dir_all(std::env::temp_dir().join(format!("openfinder-depth-{}", std::process::id()))).unwrap();
    }
}

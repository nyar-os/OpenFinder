//! Query matching, filtering, ordering, and pagination over an index.

use std::time::SystemTime;

use regex::{Regex, RegexBuilder};
use serde::{Deserialize, Serialize};

use crate::{
    error::{Error, Result},
    index::Index,
};

/// Entry kind filter.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum EntryKind {
    /// Regular files only.
    File,
    /// Directories only.
    #[serde(rename = "dir")]
    Directory,
}

/// Sort key.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, Serialize, Default)]
#[serde(rename_all = "lowercase")]
pub enum SortKey {
    /// Sort by file name.
    #[default]
    Name,
    /// Sort by full path.
    Path,
    /// Sort by size.
    Size,
    /// Sort by modification time.
    Modified,
}

/// One search request. Every field has a documented default.
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct QuerySpec {
    /// Glob pattern (`*` and `?`), or a regular expression when `regex` is true.
    pub pattern: Option<String>,
    /// Interpret `pattern` as a regular expression.
    pub regex: bool,
    /// Match the pattern against the full path instead of the file name.
    pub match_path: bool,
    /// Case-insensitive path substring filter.
    pub path_filter: Option<String>,
    /// Minimum size in bytes.
    pub min_size: Option<u64>,
    /// Maximum size in bytes.
    pub max_size: Option<u64>,
    /// Only entries modified at or after this Unix millisecond timestamp.
    pub modified_after_ms: Option<u64>,
    /// Only entries modified at or before this Unix millisecond timestamp.
    pub modified_before_ms: Option<u64>,
    /// File or directory.
    pub kind: Option<EntryKind>,
    /// Sort key.
    pub sort: SortKey,
    /// Descending order.
    pub descending: bool,
    /// Page size, clamped to `1..=MAX_LIMIT`.
    pub limit: usize,
    /// Page offset.
    pub offset: usize,
}

/// Largest page size accepted by [`search`].
pub const MAX_LIMIT: usize = 1000;

impl Default for QuerySpec {
    fn default() -> Self {
        Self {
            pattern: None,
            regex: false,
            match_path: false,
            path_filter: None,
            min_size: None,
            max_size: None,
            modified_after_ms: None,
            modified_before_ms: None,
            kind: None,
            sort: SortKey::Name,
            descending: false,
            limit: 50,
            offset: 0,
        }
    }
}

/// One search result.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SearchHit {
    /// File record number.
    pub frn: u64,
    /// File name.
    pub name: String,
    /// Full path from the volume root.
    pub path: String,
    /// Whether the hit is a directory.
    pub is_dir: bool,
    /// Size in bytes.
    pub size: u64,
    /// Creation time in Unix milliseconds.
    pub created_ms: u64,
    /// Modification time in Unix milliseconds.
    pub modified_ms: u64,
    /// Access time in Unix milliseconds.
    pub accessed_ms: u64,
}

/// One page of results.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SearchPage {
    /// Total matches after filtering, before pagination.
    pub total: u64,
    /// Requested offset.
    pub offset: usize,
    /// Effective page size.
    pub limit: usize,
    /// Whether more entries follow this page.
    pub has_more: bool,
    /// Hits on this page.
    pub hits: Vec<SearchHit>,
    /// Query mode: `index` or `portable-walk`.
    pub mode: String,
    /// Whether results came from an index.
    pub indexed: bool,
    /// Volume label or walk root.
    pub root: Option<String>,
    /// Entries that could not be inspected during a walk.
    pub skipped: u64,
    /// Elapsed milliseconds.
    pub took_ms: u64,
}

/// Run one indexed query.
pub fn search(index: &Index, spec: &QuerySpec) -> Result<SearchPage> {
    let started = SystemTime::now();
    let matcher = compile_matcher(spec)?;
    let path_filter = spec.path_filter.as_ref().map(|filter| filter.to_lowercase());
    let mut hits: Vec<SearchHit> = Vec::new();
    for entry in index.entries.values() {
        if let Some(kind) = spec.kind {
            let matches_kind = match kind {
                EntryKind::File => !entry.is_dir,
                EntryKind::Directory => entry.is_dir,
            };
            if !matches_kind {
                continue;
            }
        }
        if spec.min_size.is_some_and(|minimum| entry.size < minimum) || spec.max_size.is_some_and(|maximum| entry.size > maximum) {
            continue;
        }
        if spec.modified_after_ms.is_some_and(|after| entry.modified_ms < after)
            || spec.modified_before_ms.is_some_and(|before| entry.modified_ms > before)
        {
            continue;
        }
        let path = index.path_of(entry.frn);
        if let Some(filter) = &path_filter
            && !path.to_lowercase().contains(filter)
        {
            continue;
        }
        if let Some(matcher) = &matcher {
            let subject = if spec.match_path { path.as_str() } else { entry.name.as_str() };
            if !matcher.is_match(subject) {
                continue;
            }
        }
        hits.push(SearchHit {
            frn: entry.frn,
            name: entry.name.clone(),
            path,
            is_dir: entry.is_dir,
            size: entry.size,
            created_ms: entry.created_ms,
            modified_ms: entry.modified_ms,
            accessed_ms: entry.accessed_ms,
        });
    }
    let total = hits.len() as u64;
    // The index map iterates in hash order, so equal sort keys need the FRN as a
    // tie-break. Without it the same query can return different rows on each
    // call and pages would overlap or skip entries.
    match spec.sort {
        SortKey::Name => hits.sort_by_cached_key(|hit| (hit.name.to_lowercase(), hit.frn)),
        SortKey::Path => hits.sort_by_cached_key(|hit| (hit.path.to_lowercase(), hit.frn)),
        SortKey::Size => hits.sort_by_cached_key(|hit| (hit.size, hit.frn)),
        SortKey::Modified => hits.sort_by_cached_key(|hit| (hit.modified_ms, hit.frn)),
    }
    if spec.descending {
        hits.reverse();
    }
    let limit = spec.limit.clamp(1, MAX_LIMIT);
    let offset = spec.offset;
    let page = hits.iter().skip(offset).take(limit).cloned().collect::<Vec<_>>();
    let has_more = offset.saturating_add(page.len()) < hits.len();
    Ok(SearchPage {
        total,
        offset,
        limit,
        has_more,
        hits: page,
        mode: "index".to_string(),
        indexed: true,
        root: Some(index.meta.volume.clone()),
        skipped: 0,
        took_ms: SystemTime::now().duration_since(started).map_or(0, |duration| duration.as_millis() as u64),
    })
}

/// Compile the name or path matcher for a request.
pub fn compile_matcher(spec: &QuerySpec) -> Result<Option<Regex>> {
    let Some(pattern) = spec.pattern.as_ref().filter(|pattern| !pattern.is_empty())
    else {
        return Ok(None);
    };
    let expression = if spec.regex { pattern.clone() } else { glob_to_regex(pattern) };
    RegexBuilder::new(&expression)
        .case_insensitive(true)
        .size_limit(4 * 1024 * 1024)
        .build()
        .map(Some)
        .map_err(|error| Error::message(format!("invalid pattern: {error}")))
}

fn glob_to_regex(pattern: &str) -> String {
    let mut expression = String::from("^");
    for character in pattern.chars() {
        match character {
            '*' => expression.push_str(".*"),
            '?' => expression.push('.'),
            other => expression.push_str(&regex::escape(&other.to_string())),
        }
    }
    expression.push('$');
    expression
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;

    use crate::index::{INDEX_SCHEMA, IndexEntry, IndexMeta};

    use super::*;

    fn index() -> Index {
        let entries = [
            ("readme.md", false, 120, 1_000),
            ("main.RS", false, 400, 2_000),
            ("notes.txt", false, 900, 3_000),
            ("src", true, 0, 1_500),
            ("assets", true, 0, 500),
        ];
        let mut map = HashMap::new();
        map.insert(
            5,
            IndexEntry { frn: 5, parent_frn: 5, name: ".".to_string(), is_dir: true, size: 0, created_ms: 0, modified_ms: 0, accessed_ms: 0 },
        );
        for (position, (name, is_dir, size, modified)) in entries.iter().enumerate() {
            let frn = 10 + position as u64;
            map.insert(
                frn,
                IndexEntry {
                    frn,
                    parent_frn: 5,
                    name: name.to_string(),
                    is_dir: *is_dir,
                    size: *size,
                    created_ms: 0,
                    modified_ms: *modified,
                    accessed_ms: 0,
                },
            );
        }
        Index {
            meta: IndexMeta {
                schema: INDEX_SCHEMA.to_string(),
                volume: "C:".to_string(),
                volume_serial: 1,
                record_size: 1024,
                method: "mft".to_string(),
                built_at_ms: 0,
                updated_at_ms: 0,
                cursor: 0,
                record_count: 6,
                entry_count: 6,
            },
            entries: map,
        }
    }

    #[test]
    fn glob_is_case_insensitive() {
        let page = search(&index(), &QuerySpec { pattern: Some("*.rs".to_string()), ..QuerySpec::default() }).unwrap();
        assert_eq!(page.total, 1);
        assert_eq!(page.hits[0].name, "main.RS");
    }

    #[test]
    fn regex_and_path_filters_combine() {
        let page = search(
            &index(),
            &QuerySpec { pattern: Some("^ma.n\\.rs$".to_string()), regex: true, path_filter: Some("main".to_string()), ..QuerySpec::default() },
        )
        .unwrap();
        assert_eq!(page.total, 1);
    }

    #[test]
    fn size_and_time_filters_apply() {
        let page = search(&index(), &QuerySpec { min_size: Some(200), modified_after_ms: Some(2_500), ..QuerySpec::default() }).unwrap();
        assert_eq!(page.total, 1);
        assert_eq!(page.hits[0].name, "notes.txt");
    }

    #[test]
    fn sort_and_pagination_are_stable() {
        let page = search(&index(), &QuerySpec { sort: SortKey::Size, descending: true, limit: 2, offset: 1, ..QuerySpec::default() }).unwrap();
        assert_eq!(page.total, 6);
        assert_eq!(page.hits.len(), 2);
        assert_eq!(page.hits[0].name, "main.RS");
        assert!(page.has_more);
    }

    #[test]
    fn equal_sort_keys_keep_a_deterministic_order() {
        let mut entries = HashMap::new();
        entries.insert(
            5,
            IndexEntry { frn: 5, parent_frn: 5, name: ".".to_string(), is_dir: true, size: 0, created_ms: 0, modified_ms: 0, accessed_ms: 0 },
        );
        for frn in [40u64, 41, 42] {
            entries.insert(
                frn,
                IndexEntry {
                    frn,
                    parent_frn: 5,
                    name: "same.dll".to_string(),
                    is_dir: false,
                    size: 10,
                    created_ms: 0,
                    modified_ms: 0,
                    accessed_ms: 0,
                },
            );
        }
        let index = Index {
            meta: IndexMeta {
                schema: INDEX_SCHEMA.to_string(),
                volume: "C:".to_string(),
                volume_serial: 1,
                record_size: 1024,
                method: "mft".to_string(),
                built_at_ms: 0,
                updated_at_ms: 0,
                cursor: 0,
                record_count: 4,
                entry_count: 4,
            },
            entries,
        };
        let first = search(&index, &QuerySpec { sort: SortKey::Size, limit: 3, ..QuerySpec::default() }).unwrap();
        let second = search(&index, &QuerySpec { sort: SortKey::Size, limit: 3, ..QuerySpec::default() }).unwrap();
        let frns = first.hits.iter().map(|hit| hit.frn).collect::<Vec<_>>();
        assert_eq!(frns, vec![5, 40, 41]);
        assert_eq!(second.hits.iter().map(|hit| hit.frn).collect::<Vec<_>>(), frns);
    }

    #[test]
    fn directory_kind_filter_works() {
        let page = search(&index(), &QuerySpec { kind: Some(EntryKind::Directory), ..QuerySpec::default() }).unwrap();
        assert_eq!(page.total, 3);
    }

    #[test]
    fn invalid_regex_surfaces_a_message() {
        let error = search(&index(), &QuerySpec { pattern: Some("(".to_string()), regex: true, ..QuerySpec::default() }).err().unwrap();
        assert!(error.to_string().contains("invalid pattern"));
    }
}

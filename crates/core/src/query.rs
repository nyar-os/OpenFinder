//! Query matching, filtering, ordering, and pagination over an index.

use std::{
    cmp::Ordering,
    collections::{BinaryHeap, HashMap},
    time::SystemTime,
};

use regex::{Regex, RegexBuilder};
use serde::{Deserialize, Serialize};

use crate::{
    error::{Error, Result},
    index::{Index, IndexEntry},
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
///
/// Matching runs on index fields first. A full path is rebuilt only when a
/// filter or sort key reads it, and only the best `offset + limit` rows are
/// kept while scanning, so memory is bounded by the page size instead of the
/// match count. `total` stays exact because the scan itself is cheap once the
/// per-entry parent-chain walk is gone.
pub fn search(index: &Index, spec: &QuerySpec) -> Result<SearchPage> {
    let started = SystemTime::now();
    let matcher = compile_matcher(spec)?;
    let path_filter = compile_path_filter(spec.path_filter.as_deref())?;
    let limit = spec.limit.clamp(1, MAX_LIMIT);
    let offset = spec.offset;
    let page_end = offset.saturating_add(limit);
    // Path is needed up front only for the filters and sort keys that read it.
    // Otherwise the parent-chain walk is paid for the rows that make the page.
    let needs_path = path_filter.is_some() || spec.match_path || spec.sort == SortKey::Path;
    let mut paths = needs_path.then(|| PathCache::new(index));
    let mut total = 0u64;
    let mut best: BinaryHeap<Candidate<'_>> = BinaryHeap::new();
    for entry in index.entries.values() {
        if !passes_scalar_filters(entry, spec) {
            continue;
        }
        let path = paths.as_mut().map(|cache| cache.entry_path(entry));
        if let Some(filter) = &path_filter
            && !filter.is_match(path.as_deref().unwrap_or_default())
        {
            continue;
        }
        if let Some(matcher) = &matcher {
            let subject = if spec.match_path { path.as_deref().unwrap_or_default() } else { entry.name.as_str() };
            if !matcher.is_match(subject) {
                continue;
            }
        }
        total += 1;
        let candidate = Candidate { entry, path, sort: spec.sort, descending: spec.descending };
        if best.len() < page_end {
            best.push(candidate);
        }
        else if best.peek().is_some_and(|worst| candidate < *worst) {
            best.pop();
            best.push(candidate);
        }
    }
    // The index map iterates in hash order, so equal sort keys need the FRN as a
    // tie-break. Without it the same query could return different rows on each
    // call and pages would overlap or skip entries.
    let mut kept = best.into_vec();
    kept.sort_unstable();
    let hits = kept
        .into_iter()
        .skip(offset)
        .map(|candidate| SearchHit {
            frn: candidate.entry.frn,
            name: candidate.entry.name.clone(),
            path: candidate.path.unwrap_or_else(|| index.path_of(candidate.entry.frn)),
            is_dir: candidate.entry.is_dir,
            size: candidate.entry.size,
            created_ms: candidate.entry.created_ms,
            modified_ms: candidate.entry.modified_ms,
            accessed_ms: candidate.entry.accessed_ms,
        })
        .collect::<Vec<_>>();
    let has_more = (offset.saturating_add(hits.len()) as u64) < total;
    Ok(SearchPage {
        total,
        offset,
        limit,
        has_more,
        hits,
        mode: "index".to_string(),
        indexed: true,
        root: Some(index.meta.volume.clone()),
        skipped: 0,
        took_ms: SystemTime::now().duration_since(started).map_or(0, |duration| duration.as_millis() as u64),
    })
}

/// Rebuild full paths for one query while remembering directory paths.
///
/// Every file path ends in its parent directory's path, so walking the parent
/// chain once per directory instead of once per entry removes most of the
/// repeated work without storing paths in the index.
struct PathCache<'a> {
    index: &'a Index,
    dirs: HashMap<u64, String>,
}

impl<'a> PathCache<'a> {
    fn new(index: &'a Index) -> Self {
        Self { index, dirs: HashMap::new() }
    }

    fn entry_path(&mut self, entry: &IndexEntry) -> String {
        if entry.is_dir {
            return self.dir_path(entry.frn);
        }
        let parent = self.dir_path(entry.parent_frn);
        format!("{}\\{}", parent.trim_end_matches('\\'), entry.name)
    }

    fn dir_path(&mut self, frn: u64) -> String {
        if let Some(path) = self.dirs.get(&frn) {
            return path.clone();
        }
        let path = self.index.path_of(frn);
        self.dirs.insert(frn, path.clone());
        path
    }
}

/// One row that may still make the requested page.
struct Candidate<'a> {
    entry: &'a IndexEntry,
    path: Option<String>,
    sort: SortKey,
    descending: bool,
}

impl Ord for Candidate<'_> {
    fn cmp(&self, other: &Self) -> Ordering {
        let ordering = match self.sort {
            SortKey::Name => compare_case_insensitive(&self.entry.name, &other.entry.name),
            SortKey::Path => compare_case_insensitive(self.path.as_deref().unwrap_or_default(), other.path.as_deref().unwrap_or_default()),
            SortKey::Size => self.entry.size.cmp(&other.entry.size),
            SortKey::Modified => self.entry.modified_ms.cmp(&other.entry.modified_ms),
        }
        .then_with(|| self.entry.frn.cmp(&other.entry.frn));
        if self.descending { ordering.reverse() } else { ordering }
    }
}

impl PartialOrd for Candidate<'_> {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

impl PartialEq for Candidate<'_> {
    fn eq(&self, other: &Self) -> bool {
        self.cmp(other) == Ordering::Equal
    }
}

impl Eq for Candidate<'_> {}

/// Compare names and paths the way the index sorts them, without allocating a
/// lowercased copy for every candidate.
fn compare_case_insensitive(left: &str, right: &str) -> Ordering {
    let mut left = left.chars().flat_map(char::to_lowercase);
    let mut right = right.chars().flat_map(char::to_lowercase);
    loop {
        match (left.next(), right.next()) {
            (Some(left), Some(right)) => match left.cmp(&right) {
                Ordering::Equal => {}
                other => return other,
            },
            (Some(_), None) => return Ordering::Greater,
            (None, Some(_)) => return Ordering::Less,
            (None, None) => return Ordering::Equal,
        }
    }
}

/// Filters that only read fields already stored on the entry.
fn passes_scalar_filters(entry: &IndexEntry, spec: &QuerySpec) -> bool {
    if let Some(kind) = spec.kind {
        let matches_kind = match kind {
            EntryKind::File => !entry.is_dir,
            EntryKind::Directory => entry.is_dir,
        };
        if !matches_kind {
            return false;
        }
    }
    if spec.min_size.is_some_and(|minimum| entry.size < minimum) || spec.max_size.is_some_and(|maximum| entry.size > maximum) {
        return false;
    }
    if spec.modified_after_ms.is_some_and(|after| entry.modified_ms < after)
        || spec.modified_before_ms.is_some_and(|before| entry.modified_ms > before)
    {
        return false;
    }
    true
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

/// Compile the case-insensitive path substring filter once per query.
fn compile_path_filter(filter: Option<&str>) -> Result<Option<Regex>> {
    let Some(filter) = filter.filter(|filter| !filter.is_empty())
    else {
        return Ok(None);
    };
    RegexBuilder::new(&regex::escape(filter))
        .case_insensitive(true)
        .size_limit(4 * 1024 * 1024)
        .build()
        .map(Some)
        .map_err(|error| Error::message(format!("invalid path filter: {error}")))
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
    fn paging_matches_the_full_sort_order() {
        let index = index();
        for sort in [SortKey::Name, SortKey::Path, SortKey::Size, SortKey::Modified] {
            for descending in [false, true] {
                let spec = QuerySpec { sort, descending, ..QuerySpec::default() };
                let all = search(&index, &QuerySpec { limit: MAX_LIMIT, ..spec.clone() }).unwrap();
                let page = search(&index, &QuerySpec { limit: 2, offset: 3, ..spec }).unwrap();
                let expected = all.hits.iter().skip(3).take(2).map(|hit| hit.frn).collect::<Vec<_>>();
                assert_eq!(page.hits.iter().map(|hit| hit.frn).collect::<Vec<_>>(), expected);
                assert_eq!(page.total, all.total);
                assert_eq!(page.has_more, 5 < all.total as usize);
            }
        }
    }

    #[test]
    fn path_filter_is_case_insensitive() {
        let page = search(&index(), &QuerySpec { path_filter: Some("README".to_string()), ..QuerySpec::default() }).unwrap();
        assert_eq!(page.total, 1);
        assert_eq!(page.hits[0].name, "readme.md");
    }

    #[test]
    fn cached_paths_match_the_index_walk() {
        let mut entries = HashMap::new();
        let mut push = |frn: u64, parent_frn: u64, name: &str, is_dir: bool| {
            entries.insert(
                frn,
                IndexEntry {
                    frn,
                    parent_frn,
                    name: name.to_string(),
                    is_dir,
                    size: 0,
                    created_ms: 0,
                    modified_ms: 0,
                    accessed_ms: 0,
                },
            );
        };
        push(5, 5, ".", true);
        push(6, 5, "src", true);
        push(7, 6, "inner", true);
        push(8, 7, "main.rs", false);
        push(9, 5, "top.txt", false);
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
                record_count: 5,
                entry_count: 5,
            },
            entries,
        };
        let mut cache = PathCache::new(&index);
        for entry in index.entries.values() {
            assert_eq!(cache.entry_path(entry), index.path_of(entry.frn));
        }
        assert_eq!(cache.entry_path(index.entries.get(&8).unwrap()), "C:\\src\\inner\\main.rs");
    }

    #[test]
    fn invalid_regex_surfaces_a_message() {
        let error = search(&index(), &QuerySpec { pattern: Some("(".to_string()), regex: true, ..QuerySpec::default() }).err().unwrap();
        assert!(error.to_string().contains("invalid pattern"));
    }
}

//! Content-condition scanning.
//!
//! A scan is deliberately a different entry point from [`crate::query::search`]:
//! metadata queries answer from index fields in microseconds per entry and never
//! open a file, while a content predicate must read bytes — milliseconds per
//! file, with permission, binary, and size failure modes. Folding line filters
//! into `QuerySpec` would let `search` silently turn into a whole-volume file
//! read, which is a performance trap for scripted callers.
//!
//! The pipeline is three stages:
//!
//! 1. **Prune** — `exclude` globs and the `code` preset remove whole directory
//!    subtrees before anything inside them is enumerated. This is what makes
//!    `node_modules` filtering cheap.
//! 2. **Pre-filter** — the candidate [`QuerySpec`] (pattern, size, time window)
//!    rejects files from their directory metadata, so bytes are only read for
//!    files that already passed the metadata conditions.
//! 3. **Evaluate** — surviving files are streamed in fixed chunks and the
//!    content predicate is applied. A `maxLines` breach rejects a file without
//!    reading the rest.
//!
//! Every budget (`maxFiles`, `maxBytes`, `timeoutMs`) stops the scan early and
//! says so through `budgetExceeded`; results are never silently partial.

use std::{
    cmp::Ordering,
    collections::BinaryHeap,
    fs::File,
    io::Read,
    path::PathBuf,
    time::{Instant, SystemTime, UNIX_EPOCH},
};

use boost_fs::walk::{ErrorPolicy, Filter, Walker};
use regex::RegexBuilder;
use serde::{Deserialize, Serialize};

use crate::{
    error::{Error, Result},
    query::{MAX_LIMIT, QuerySpec, SearchHit, compile_matcher, glob_to_regex},
};

/// Chunk size for streaming line counts. No line buffer is ever allocated.
const READ_CHUNK: usize = 64 * 1024;
/// Bytes sniffed for a NUL before a file is treated as binary.
const BINARY_SNIFF: u64 = 8 * 1024;

/// Directory names removed by the `code` exclude preset.
pub const CODE_PRESET_DIRS: &[&str] = &["node_modules", ".git", "dist", "build", "target", "__pycache__", ".venv", "vendor"];

/// Content predicate. At least one bound must be set; that requirement is what
/// distinguishes a scan from a metadata query.
#[derive(Debug, Clone, Default, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", default)]
pub struct ContentFilter {
    /// Minimum number of lines, inclusive.
    pub min_lines: Option<u64>,
    /// Maximum number of lines, inclusive. Checked early so oversized files
    /// are rejected without reading them to the end.
    pub max_lines: Option<u64>,
}

impl ContentFilter {
    /// Whether the filter accepts an exact line count.
    fn passes(&self, lines: u64) -> bool {
        self.min_lines.is_none_or(|minimum| lines >= minimum) && self.max_lines.is_none_or(|maximum| lines <= maximum)
    }

    /// Whether the newline count seen so far already proves the file is over
    /// `maxLines`, allowing an early reject mid-read.
    fn exceeds(&self, newlines: u64) -> bool {
        self.max_lines.is_some_and(|maximum| newlines > maximum)
    }
}

/// Resource bounds for one scan. Exhausting any of them stops the scan and
/// marks `budgetExceeded`; it never truncates silently.
#[derive(Debug, Clone, Default, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", default)]
pub struct ScanBudget {
    /// Maximum files whose content is read.
    pub max_files: Option<u64>,
    /// Maximum cumulative bytes read.
    pub max_bytes: Option<u64>,
    /// Maximum wall-clock milliseconds before the scan stops.
    pub timeout_ms: Option<u64>,
}

/// Which directory names are excluded before traversal.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Deserialize, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum ExcludePreset {
    /// Skip the directories a code tree never wants to read: `node_modules`,
    /// `.git`, `dist`, `build`, `target`, `__pycache__`, `.venv`, `vendor`.
    #[default]
    Code,
    /// Honor only the explicit `exclude` globs.
    None,
}

/// What to do with files that sniff as binary.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Deserialize, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum BinaryPolicy {
    /// Do not read or count them; report through `skipped`.
    #[default]
    Skip,
    /// Count lines anyway and include them when they pass the filter.
    Count,
}

/// Sort key for scan results. `lines` exists only here; metadata sorts are
/// shared with [`crate::query::SortKey`] semantics.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Deserialize, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum ScanSort {
    /// File name.
    Name,
    /// Full path.
    Path,
    /// Size in bytes.
    Size,
    /// Modification time.
    Modified,
    /// Line count.
    #[default]
    Lines,
}

/// One content scan request.
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct ScanOptions {
    /// Directory to scan. Required.
    pub root: PathBuf,
    /// Metadata pre-filter applied before any file is opened.
    pub candidates: QuerySpec,
    /// Glob allow-list for files. Empty means every file passes. Matched
    /// against the file name and the full path.
    pub include: Vec<String>,
    /// Glob deny-list. Matched against the file name and the full path; a
    /// matching directory is pruned without traversing it.
    pub exclude: Vec<String>,
    /// Directory-name preset applied in addition to `exclude`.
    pub exclude_preset: ExcludePreset,
    /// Content predicate. At least one bound is required.
    pub content: ContentFilter,
    /// Sort key.
    pub sort: ScanSort,
    /// Descending order.
    pub descending: bool,
    /// Number of best results to keep, clamped to `1..=MAX_LIMIT`.
    pub limit: usize,
    /// Binary file handling.
    pub binary_policy: BinaryPolicy,
    /// Follow symbolic links during traversal. Loop detection belongs to the
    /// walk kernel; a loop is reported through `skipped`.
    pub follow_symlinks: bool,
    /// Maximum directory depth below the root.
    pub max_depth: usize,
    /// Maximum entries inspected before the scan reports `truncated`.
    pub max_entries: u64,
    /// Resource bounds.
    pub budget: ScanBudget,
}

impl Default for ScanOptions {
    fn default() -> Self {
        Self {
            root: PathBuf::from("."),
            candidates: QuerySpec::default(),
            include: Vec::new(),
            exclude: Vec::new(),
            exclude_preset: ExcludePreset::Code,
            content: ContentFilter::default(),
            sort: ScanSort::Lines,
            descending: false,
            limit: 50,
            binary_policy: BinaryPolicy::Skip,
            follow_symlinks: false,
            max_depth: 64,
            max_entries: 100_000,
            budget: ScanBudget::default(),
        }
    }
}

/// One scan result: a search hit plus the measured line count.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ScanHit {
    /// Metadata fields, flattened so the shape stays compatible with a
    /// [`SearchHit`].
    #[serde(flatten)]
    pub hit: SearchHit,
    /// Exact line count, or `0` for an empty file.
    pub lines: u64,
}

/// Outcome of a content scan.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ScanPage {
    /// Total matches of the content predicate, before the top-K cut.
    pub total: u64,
    /// Best `limit` hits in sort order.
    pub hits: Vec<ScanHit>,
    /// Files whose content was read (fully or until an early reject).
    pub scanned: u64,
    /// Entries skipped without a content read: pruned failures, symlinks,
    /// permission errors, binary files under the skip policy.
    pub skipped: u64,
    /// Whether a budget (`maxFiles`, `maxBytes`, or `timeoutMs`) stopped the
    /// scan before the tree was exhausted. The result is partial.
    pub budget_exceeded: bool,
    /// Whether the scan stopped at `maxEntries` before the tree was exhausted.
    pub truncated: bool,
    /// Scan root.
    pub root: Option<String>,
    /// Elapsed milliseconds.
    pub took_ms: u64,
    /// Always `content-scan`.
    pub mode: String,
}

/// Outcome of reading one file for its line count.
enum FileOutcome {
    /// Exact line count.
    Lines(u64),
    /// Newline count already proved the file exceeds `maxLines`.
    OverMax,
    /// NUL seen in the sniff window under the skip policy.
    Binary,
    /// The file could not be opened or read.
    Failed,
}

/// Stream a file and count lines.
///
/// Lines are `\n` counts plus a trailing line when the last byte is not a
/// newline. The read never allocates per line, and a file that already exceeds
/// `maxLines` is abandoned mid-stream.
fn count_lines(path: &std::path::Path, content: &ContentFilter, sniff_binary: bool) -> (FileOutcome, u64) {
    let Ok(mut file) = File::open(path)
    else {
        return (FileOutcome::Failed, 0);
    };
    let mut buffer = vec![0u8; READ_CHUNK];
    let mut newlines = 0u64;
    let mut total = 0u64;
    let mut last_byte: Option<u8> = None;
    loop {
        let Ok(read) = file.read(&mut buffer)
        else {
            return (FileOutcome::Failed, total);
        };
        if read == 0 {
            break;
        }
        let chunk = &buffer[..read];
        if sniff_binary && total < BINARY_SNIFF {
            let window = (BINARY_SNIFF - total).min(read as u64) as usize;
            if chunk[..window].contains(&0) {
                return (FileOutcome::Binary, total + read as u64);
            }
        }
        newlines += chunk.iter().filter(|byte| **byte == b'\n').count() as u64;
        if content.exceeds(newlines) {
            return (FileOutcome::OverMax, total + read as u64);
        }
        total += read as u64;
        last_byte = chunk.last().copied();
    }
    let mut lines = newlines;
    if total > 0 && last_byte != Some(b'\n') {
        lines += 1;
    }
    (FileOutcome::Lines(lines), total)
}

/// Compile glob patterns into case-insensitive path/name matchers.
fn compile_globs(patterns: &[String]) -> Result<Vec<regex::Regex>> {
    patterns
        .iter()
        .filter(|pattern| !pattern.trim().is_empty())
        .map(|pattern| {
            RegexBuilder::new(&glob_to_regex(pattern))
                .case_insensitive(true)
                .size_limit(4 * 1024 * 1024)
                .build()
                .map_err(|error| Error::message(format!("invalid glob `{pattern}`: {error}")))
        })
        .collect()
}

/// Whether any matcher hits the file name or the full path.
fn any_match(matchers: &[regex::Regex], name: &str, path: &str) -> bool {
    matchers.iter().any(|matcher| matcher.is_match(name) || matcher.is_match(path))
}

/// One kept candidate. Ordering is defined so that `greater` means `worse`:
/// the binary heap keeps the best `limit` rows with the worst one on top, the
/// same scheme the indexed query uses.
struct Kept {
    hit: ScanHit,
    sort: ScanSort,
    descending: bool,
}

impl Ord for Kept {
    fn cmp(&self, other: &Self) -> Ordering {
        let ordering = match self.sort {
            ScanSort::Lines => self.hit.lines.cmp(&other.hit.lines),
            ScanSort::Size => self.hit.hit.size.cmp(&other.hit.hit.size),
            ScanSort::Modified => self.hit.hit.modified_ms.cmp(&other.hit.hit.modified_ms),
            ScanSort::Name => self.hit.hit.name.to_lowercase().cmp(&other.hit.hit.name.to_lowercase()),
            ScanSort::Path => self.hit.hit.path.to_lowercase().cmp(&other.hit.hit.path.to_lowercase()),
        }
        .then_with(|| self.hit.hit.path.cmp(&other.hit.hit.path));
        if self.descending { ordering.reverse() } else { ordering }
    }
}

impl PartialOrd for Kept {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

impl PartialEq for Kept {
    fn eq(&self, other: &Self) -> bool {
        self.cmp(other) == Ordering::Equal
    }
}

impl Eq for Kept {}

/// Run one content scan.
pub fn scan(options: &ScanOptions) -> Result<ScanPage> {
    let started = Instant::now();
    let metadata = std::fs::metadata(&options.root).map_err(|source| Error::io(options.root.display().to_string(), source))?;
    if !metadata.is_dir() {
        return Err(Error::message(format!("{} is not a directory", options.root.display())));
    }
    if options.content.min_lines.is_none() && options.content.max_lines.is_none() {
        return Err(Error::message("a scan requires a content condition: set `minLines` or `maxLines`"));
    }
    if let (Some(minimum), Some(maximum)) = (options.content.min_lines, options.content.max_lines)
        && minimum > maximum
    {
        return Err(Error::message(format!("`minLines` ({minimum}) exceeds `maxLines` ({maximum})")));
    }
    let matcher = compile_matcher(&options.candidates)?;
    let include = compile_globs(&options.include)?;
    let exclude = compile_globs(&options.exclude)?;
    let limit = options.limit.clamp(1, MAX_LIMIT);
    let timeout = options.budget.timeout_ms.map(std::time::Duration::from_millis);

    let mut best: BinaryHeap<Kept> = BinaryHeap::new();
    let mut total = 0u64;
    let mut scanned = 0u64;
    let mut skipped = 0u64;
    let mut inspected = 0u64;
    let mut bytes_read = 0u64;
    let mut truncated = false;
    let mut budget_exceeded = false;

    // The predicate prunes directories before traversal, which is what keeps
    // `node_modules` filtering at directory cost instead of file cost.
    let exclude_for_filter = exclude.clone();
    let include_for_filter = include.clone();
    let preset_clone = options.exclude_preset;
    let walker = Walker::new(&options.root)
        .min_depth(1)
        .max_depth(options.max_depth)
        .follow_links(options.follow_symlinks)
        .error_policy(ErrorPolicy::Lenient)
        .filter(move |entry| {
            // The walk kernel asks the predicate about the root itself. An
            // explicit `--root node_modules` is a deliberate request, so the
            // root is never pruned by name; only its children are.
            if entry.depth() == 0 {
                return Filter::Continue;
            }
            let name = entry.file_name().to_string_lossy().to_lowercase();
            let is_dir = entry.is_folder();
            let excluded = preset_clone == ExcludePreset::Code && CODE_PRESET_DIRS.contains(&name.as_str())
                || any_match(&exclude_for_filter, &name, &entry.path().display().to_string());
            if excluded {
                return if is_dir { Filter::IgnoreFolder } else { Filter::Ignore };
            }
            if !is_dir && !include_for_filter.is_empty() && !any_match(&include_for_filter, &name, &entry.path().display().to_string()) {
                return Filter::Ignore;
            }
            Filter::Continue
        });

    for item in walker {
        if inspected >= options.max_entries {
            truncated = true;
            break;
        }
        let Ok(entry) = item
        else {
            skipped += 1;
            continue;
        };
        inspected += 1;
        if entry.is_symlink() {
            skipped += 1;
            continue;
        }
        if entry.is_folder() {
            continue;
        }
        let Ok(metadata) = entry.metadata()
        else {
            skipped += 1;
            continue;
        };
        let name = entry.file_name().to_string_lossy().to_string();
        let path_text = entry.path().display().to_string();
        // Metadata pre-filter: reject before paying for a single byte.
        let size = metadata.len();
        if options.candidates.min_size.is_some_and(|minimum| size < minimum)
            || options.candidates.max_size.is_some_and(|maximum| size > maximum)
        {
            continue;
        }
        let modified_ms = timestamp_ms(metadata.modified().ok());
        if options.candidates.modified_after_ms.is_some_and(|after| modified_ms < after)
            || options.candidates.modified_before_ms.is_some_and(|before| modified_ms > before)
        {
            continue;
        }
        if let Some(matcher) = &matcher {
            let subject = if options.candidates.match_path { path_text.as_str() } else { name.as_str() };
            if !matcher.is_match(subject) {
                continue;
            }
        }
        if options
            .candidates
            .path_filter
            .as_ref()
            .filter(|filter| !filter.is_empty())
            .is_some_and(|filter| !path_text.to_lowercase().contains(&filter.to_lowercase()))
        {
            continue;
        }
        // Budgets stop the scan and say so; they never hide a partial result.
        if options.budget.max_files.is_some_and(|maximum| scanned >= maximum)
            || options.budget.max_bytes.is_some_and(|maximum| bytes_read >= maximum)
            || timeout.is_some_and(|limit| started.elapsed() >= limit)
        {
            budget_exceeded = true;
            break;
        }
        let sniff_binary = options.binary_policy == BinaryPolicy::Skip;
        let (outcome, read) = count_lines(entry.path(), &options.content, sniff_binary);
        scanned += 1;
        bytes_read += read;
        match outcome {
            FileOutcome::Failed | FileOutcome::Binary => {
                skipped += 1;
            }
            FileOutcome::OverMax => {}
            FileOutcome::Lines(lines) if options.content.passes(lines) => {
                total += 1;
                let hit = ScanHit {
                    hit: SearchHit {
                        frn: 0,
                        name: name.clone(),
                        path: path_text.clone(),
                        is_dir: false,
                        size,
                        created_ms: timestamp_ms(metadata.created().ok()),
                        modified_ms,
                        accessed_ms: timestamp_ms(metadata.accessed().ok()),
                    },
                    lines,
                };
                let candidate = Kept { hit, sort: options.sort, descending: options.descending };
                if best.len() < limit {
                    best.push(candidate);
                }
                else if best.peek().is_some_and(|worst| candidate < *worst) {
                    best.pop();
                    best.push(candidate);
                }
            }
            FileOutcome::Lines(_) => {}
        }
    }

    let mut kept = best.into_vec();
    kept.sort_unstable();
    let hits = kept.into_iter().map(|candidate| candidate.hit).collect::<Vec<_>>();
    Ok(ScanPage {
        total,
        hits,
        scanned,
        skipped,
        budget_exceeded,
        truncated,
        root: Some(options.root.display().to_string()),
        took_ms: started.elapsed().as_millis() as u64,
        mode: "content-scan".to_string(),
    })
}

fn timestamp_ms(value: Option<SystemTime>) -> u64 {
    value.and_then(|time| time.duration_since(UNIX_EPOCH).ok()).map_or(0, |duration| duration.as_millis() as u64)
}

#[cfg(test)]
mod tests {
    use std::fs;

    use super::*;

    fn temp_root(tag: &str) -> PathBuf {
        std::env::temp_dir().join(format!("openfinder-scan-{tag}-{}", std::process::id()))
    }

    /// Write `file` with exactly `lines` newline-terminated lines.
    fn write_lines(path: &std::path::Path, lines: u64) {
        let mut content = String::new();
        for index in 0..lines {
            content.push_str(&format!("line {index}\n"));
        }
        fs::write(path, content).unwrap();
    }

    fn fixture(tag: &str) -> PathBuf {
        let root = temp_root(tag);
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(root.join("src")).unwrap();
        fs::create_dir_all(root.join("node_modules/pkg")).unwrap();
        write_lines(&root.join("src/small.rs"), 10);
        write_lines(&root.join("src/big.rs"), 120);
        write_lines(&root.join("src/huge.rs"), 1_200);
        write_lines(&root.join("node_modules/pkg/vendored.rs"), 5_000);
        fs::write(root.join("src/notes.txt"), b"hello").unwrap();
        fs::write(root.join("src/blob.bin"), [0u8, 1, 2, 3, 0, 5]).unwrap();
        root
    }

    fn options(root: PathBuf, content: ContentFilter) -> ScanOptions {
        ScanOptions { root, content, ..ScanOptions::default() }
    }

    #[test]
    fn min_lines_reports_exact_counts() {
        let root = fixture("min");
        let page = scan(&options(root.clone(), ContentFilter { min_lines: Some(100), max_lines: None })).unwrap();
        let names = page.hits.iter().map(|hit| hit.hit.name.as_str()).collect::<Vec<_>>();
        // vendored.rs lives under node_modules and must never be read.
        assert_eq!(names, vec!["big.rs", "huge.rs"]);
        let huge = page.hits.iter().find(|hit| hit.hit.name == "huge.rs").unwrap();
        assert_eq!(huge.lines, 1_200);
        assert_eq!(page.total, 2);
        assert_eq!(page.mode, "content-scan");
        fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn preset_prunes_node_modules_without_reading_it() {
        let root = fixture("prune");
        let page = scan(&options(root.clone(), ContentFilter { min_lines: Some(4_000), max_lines: None })).unwrap();
        assert_eq!(page.total, 0, "the only 4k-line file sits inside node_modules");
        fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn exclude_glob_prunes_custom_directories() {
        let root = fixture("exclude");
        let mut opts = options(root.clone(), ContentFilter { min_lines: Some(4_000), max_lines: None });
        opts.exclude_preset = ExcludePreset::None;
        opts.exclude = vec!["node_modules".to_string()];
        let page = scan(&opts).unwrap();
        assert_eq!(page.total, 0);
        fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn max_lines_rejects_oversized_files() {
        let root = fixture("max");
        let page = scan(&options(root.clone(), ContentFilter { min_lines: None, max_lines: Some(50) })).unwrap();
        let names = page.hits.iter().map(|hit| hit.hit.name.as_str()).collect::<Vec<_>>();
        assert_eq!(names, vec!["notes.txt", "small.rs"], "notes.txt has one trailing line without a newline");
        assert!(page.hits.iter().find(|hit| hit.hit.name == "notes.txt").is_some_and(|hit| hit.lines == 1));
        fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn include_restricts_by_extension() {
        let root = fixture("include");
        let mut opts = options(root.clone(), ContentFilter { min_lines: Some(1), max_lines: None });
        opts.include = vec!["*.rs".to_string()];
        let page = scan(&opts).unwrap();
        assert!(page.hits.iter().all(|hit| hit.hit.name.ends_with(".rs")));
        assert!(!page.hits.iter().any(|hit| hit.hit.name == "notes.txt"));
        fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn binary_files_are_skipped_by_default() {
        let root = fixture("binary");
        let mut opts = options(root.clone(), ContentFilter { min_lines: Some(1), max_lines: None });
        opts.exclude = vec!["node_modules".to_string()];
        opts.exclude_preset = ExcludePreset::None;
        let page = scan(&opts).unwrap();
        assert!(!page.hits.iter().any(|hit| hit.hit.name == "blob.bin"));
        assert!(page.skipped >= 1);
        fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn budget_max_files_marks_partial_results() {
        let root = fixture("budget");
        let mut opts = options(root.clone(), ContentFilter { min_lines: Some(1), max_lines: None });
        opts.budget.max_files = Some(1);
        let page = scan(&opts).unwrap();
        assert!(page.budget_exceeded);
        assert_eq!(page.scanned, 1);
        fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn sort_lines_descending_keeps_top_k() {
        let root = fixture("topk");
        let mut opts = options(root.clone(), ContentFilter { min_lines: Some(1), max_lines: None });
        opts.sort = ScanSort::Lines;
        opts.descending = true;
        opts.limit = 2;
        let page = scan(&opts).unwrap();
        assert_eq!(page.hits.len(), 2);
        assert_eq!(page.hits[0].hit.name, "huge.rs", "node_modules is excluded, so huge.rs leads");
        assert_eq!(page.hits[1].hit.name, "big.rs");
        assert_eq!(page.total, 4, "notes.txt, small.rs, big.rs, huge.rs all pass minLines=1");
        fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn candidate_pattern_prevents_content_reads() {
        let root = fixture("candidate");
        let mut opts = options(root.clone(), ContentFilter { min_lines: Some(1), max_lines: None });
        opts.candidates.pattern = Some("*.rs".to_string());
        let page = scan(&opts).unwrap();
        assert!(page.hits.iter().all(|hit| hit.hit.name.ends_with(".rs")));
        fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn explicit_root_named_like_the_preset_is_never_pruned() {
        let root = temp_root("explicit").join("node_modules");
        let _ = fs::remove_dir_all(temp_root("explicit"));
        fs::create_dir_all(&root).unwrap();
        write_lines(&root.join("pkg.json"), 42);
        let page = scan(&options(root.clone(), ContentFilter { min_lines: Some(40), max_lines: None })).unwrap();
        assert_eq!(page.total, 1, "scanning `node_modules` directly must read it");
        assert_eq!(page.hits[0].hit.name, "pkg.json");
        fs::remove_dir_all(temp_root("explicit")).unwrap();
    }

    #[test]
    fn empty_content_filter_is_rejected() {
        let root = fixture("empty");
        let error = scan(&options(root.clone(), ContentFilter::default())).err().unwrap();
        assert!(error.to_string().contains("content condition"));
        fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn inverted_bounds_are_rejected() {
        let root = fixture("inverted");
        let error = scan(&options(root.clone(), ContentFilter { min_lines: Some(10), max_lines: Some(5) })).err().unwrap();
        assert!(error.to_string().contains("exceeds"));
        fs::remove_dir_all(&root).unwrap();
    }
}

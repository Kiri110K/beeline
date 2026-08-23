//! The Recents collection (SPEC §7, research #3): a system-derived list of recently
//! used files, sourced from Spotlight — never a filesystem crawl.
//!
//! Mechanism (research #3, amended by measurement): Finder's public metadata predicate
//! over `kMDItemLastUsedDate` plus a content-type tree, run through a single unbounded
//! `mdfind -attr kMDItemLastUsedDate` call — paths and dates in one process. Research
//! #3's progressive widening and batched `mdls` dating were measured on the reference
//! machine to cost 2-4 process launches per refresh and blow the 150 ms budget (SPEC
//! §10), while the bare unbounded query is no slower than a windowed one. Results are
//! sorted newest-first, passed through the documented post-filter (hidden and support
//! files), and only the kept top slice pays a per-file metadata read.
//!
//! Serving is cache-first: [`get_recents`] returns a slice of the last-success cache
//! instantly and kicks off one background refresh (never more than one in flight). The
//! refresh persists the cache atomically and emits `beeline://recents-updated` so the
//! frontend re-pulls. Degraded states are honest: an empty result is empty, a disabled
//! or privacy-excluded or still-indexing Spotlight is reported as such — there is no
//! silent crawl fallback.

use std::{
    ffi::OsStr,
    fs, io,
    os::unix::ffi::OsStrExt,
    path::{Path, PathBuf},
    process::Command,
    sync::{
        atomic::{AtomicBool, Ordering},
        Mutex,
    },
    thread,
    time::Instant,
};

use serde::{Deserialize, Serialize};
use serde_json::json;
use tauri::{AppHandle, Emitter, Manager, State};

use crate::listing::kind_label;
use crate::telemetry::Telemetry;

const CACHE_FILE: &str = "recents_cache.json";
const CACHE_VERSION: u32 = 1;
const RECENTS_UPDATED_EVENT: &str = "beeline://recents-updated";

// Dates are fetched from `mdls` in batches of this size — only on the default
// `find_dated` composition path (test fakes); the system adapter gets dates from
// `mdfind -attr` directly.
const DATE_BATCH: usize = 100;
// A refresh keeps this many newest candidates after the sort; only they pay the
// per-file metadata read. A gather bound, not a product cap on the view.
const REFRESH_TARGET: usize = 200;

// Finder's public Recents predicate (research #3): a non-null last-used date and a
// user-facing content type (content tree, a Microsoft type, or an archive). Directories
// are excluded because none of these content types match a folder.
const PREDICATE: &str = "(kMDItemLastUsedDate = \"*\") && ((kMDItemContentTypeTree = public.content) || (kMDItemContentTypeTree = \"com.microsoft.*\"cdw) || (kMDItemContentTypeTree = public.archive))";

// Support-file basenames rejected by the post-filter in addition to hidden entries.
// Most macOS support files are already dot-hidden; `Icon\r` is the notable exception.
const SUPPORT_NAMES: &[&str] = &[".DS_Store", ".localized", "Icon\r"];

/// Why the Recents collection could not be produced (SPEC §7). Serialized snake_case so
/// the frontend discriminated union can match on it.
#[derive(Serialize, Deserialize, Clone, Copy, Debug, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum UnavailableReason {
    /// Spotlight indexing is turned off (`mdutil` reports disabled).
    Disabled,
    /// The queried scope is excluded from Spotlight / has no index.
    PrivacyExcluded,
    /// Spotlight is present but not answering (mid-index, or `mdfind` failed).
    Indexing,
}

/// Health of the Spotlight index, as classified from `mdutil -s`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum IndexHealth {
    Enabled,
    Unavailable(UnavailableReason),
}

/// Map a `mdutil -s` status line to index health. Best-effort per research #3: a
/// "disabled" line is a disabled index, a "no index" line is a privacy-excluded or
/// unindexed scope, anything else is treated as healthy and left for `mdfind` to prove.
fn classify_mdutil(output: &str) -> IndexHealth {
    let lower = output.to_lowercase();
    if lower.contains("disabled") {
        IndexHealth::Unavailable(UnavailableReason::Disabled)
    } else if lower.contains("no index") {
        IndexHealth::Unavailable(UnavailableReason::PrivacyExcluded)
    } else {
        IndexHealth::Enabled
    }
}

/// Parse a `+HHMM` / `-HHMM` timezone offset into seconds east of UTC.
fn parse_tz_offset(tz: &str) -> Option<i64> {
    let bytes = tz.as_bytes();
    if bytes.len() != 5 {
        return None;
    }
    let sign = match bytes[0] {
        b'+' => 1,
        b'-' => -1,
        _ => return None,
    };
    let hours: i64 = tz.get(1..3)?.parse().ok()?;
    let minutes: i64 = tz.get(3..5)?.parse().ok()?;
    Some(sign * (hours * 3600 + minutes * 60))
}

/// Days since the Unix epoch for a civil date (Howard Hinnant's algorithm), so date
/// parsing needs no chrono/time parsing feature.
fn days_from_civil(year: i64, month: i64, day: i64) -> i64 {
    let year = if month <= 2 { year - 1 } else { year };
    let era = (if year >= 0 { year } else { year - 399 }) / 400;
    let year_of_era = year - era * 400;
    let day_of_year = (153 * (if month > 2 { month - 3 } else { month + 9 }) + 2) / 5 + day - 1;
    let day_of_era = year_of_era * 365 + year_of_era / 4 - year_of_era / 100 + day_of_year;
    era * 146097 + day_of_era - 719468
}

/// Parse a `mdls -raw` last-used value (e.g. `2026-08-01 12:34:56 +0900`) into epoch ms,
/// or `None` for a missing value. The timezone offset is honored so the stored instant
/// is UTC; sorting is unaffected either way.
fn parse_mdls_date(value: &str) -> Option<i64> {
    let value = value.trim();
    if value.is_empty() || value == "(null)" {
        return None;
    }
    let mut parts = value.split_whitespace();
    let mut date = parts.next()?.split('-');
    let year: i64 = date.next()?.parse().ok()?;
    let month: i64 = date.next()?.parse().ok()?;
    let day: i64 = date.next()?.parse().ok()?;
    let mut time = parts.next()?.split(':');
    let hours: i64 = time.next()?.parse().ok()?;
    let minutes: i64 = time.next()?.parse().ok()?;
    let seconds: i64 = time.next().unwrap_or("0").parse().ok()?;
    let offset = parts.next().and_then(parse_tz_offset).unwrap_or(0);
    let local = days_from_civil(year, month, day) * 86400 + hours * 3600 + minutes * 60 + seconds;
    Some((local - offset) * 1000)
}

/// The documented post-filter (SPEC §7, research #3): reject hidden entries (a dot-named
/// component anywhere in the path) and known support files. Everything else is kept.
fn passes_post_filter(path: &Path) -> bool {
    for component in path.components() {
        if let std::path::Component::Normal(name) = component {
            if let Some(text) = name.to_str() {
                if text.starts_with('.') {
                    return false;
                }
            }
        }
    }
    let basename = path.file_name().and_then(OsStr::to_str).unwrap_or("");
    !SUPPORT_NAMES.contains(&basename)
}

/// The process seam. The real implementation shells out to Spotlight tools; tests inject
/// a fake so the pure gather/filter/sort logic runs without a live Spotlight.
trait Spotlight {
    /// `mdutil -s /` classified into index health, or `Err` if the tool cannot run.
    fn index_status(&self) -> Result<IndexHealth, ()>;
    /// `mdfind -0 <query>` split into paths, or `Err` on a failed invocation.
    fn find(&self, query: &str) -> Result<Vec<PathBuf>, ()>;
    /// `mdls -raw -name kMDItemLastUsedDate <paths...>` parsed to epoch ms per path.
    fn last_used(&self, paths: &[PathBuf]) -> Result<Vec<Option<i64>>, ()>;

    /// Paths with their last-used dates in one gather. The default composes `find` plus
    /// batched `last_used` (which the test fakes implement); the system adapter overrides
    /// it with a single `mdfind -attr` invocation — the per-batch `mdls` round-trips
    /// alone cost more than the whole 150 ms refresh budget (SPEC §10) on the reference
    /// machine.
    fn find_dated(&self, query: &str) -> Result<Vec<(PathBuf, Option<i64>)>, ()> {
        let paths = self.find(query)?;
        let mut dated = Vec::with_capacity(paths.len());
        for batch in paths.chunks(DATE_BATCH) {
            match self.last_used(batch) {
                Ok(dates) => dated.extend(batch.iter().cloned().zip(dates)),
                // A failed date batch keeps its paths, undated; they drop later.
                Err(()) => dated.extend(batch.iter().cloned().map(|path| (path, None))),
            }
        }
        Ok(dated)
    }
}

/// The real Spotlight adapter: `std::process::Command`, no shell, no argument string
/// interpolation beyond the query the caller built.
struct SystemSpotlight;

impl Spotlight for SystemSpotlight {
    fn index_status(&self) -> Result<IndexHealth, ()> {
        let output = Command::new("mdutil")
            .arg("-s")
            .arg("/")
            .output()
            .map_err(|_| ())?;
        if !output.status.success() {
            return Err(());
        }
        Ok(classify_mdutil(&String::from_utf8_lossy(&output.stdout)))
    }

    fn find(&self, query: &str) -> Result<Vec<PathBuf>, ()> {
        let output = Command::new("mdfind")
            .arg("-0")
            .arg(query)
            .output()
            .map_err(|_| ())?;
        if !output.status.success() {
            return Err(());
        }
        Ok(output
            .stdout
            .split(|&byte| byte == 0)
            .filter(|segment| !segment.is_empty())
            .map(|segment| PathBuf::from(OsStr::from_bytes(segment)))
            .collect())
    }

    fn last_used(&self, paths: &[PathBuf]) -> Result<Vec<Option<i64>>, ()> {
        let output = Command::new("mdls")
            .arg("-raw")
            .arg("-name")
            .arg("kMDItemLastUsedDate")
            .args(paths)
            .output()
            .map_err(|_| ())?;
        if !output.status.success() {
            return Err(());
        }
        // `-raw` emits one value per file delimited by NUL, no labels.
        let text = String::from_utf8_lossy(&output.stdout);
        Ok(text.split('\0').map(parse_mdls_date).collect())
    }

    fn find_dated(&self, query: &str) -> Result<Vec<(PathBuf, Option<i64>)>, ()> {
        let output = Command::new("mdfind")
            .arg("-attr")
            .arg("kMDItemLastUsedDate")
            .arg(query)
            .output()
            .map_err(|_| ())?;
        if !output.status.success() {
            return Err(());
        }
        // One line per hit: `<path>\t kMDItemLastUsedDate = <date>` (missing attribute
        // prints `(null)`, which the date parser rejects into `None`). Split on the label
        // from the right so any path content survives.
        let text = String::from_utf8_lossy(&output.stdout);
        Ok(text
            .lines()
            .filter(|line| !line.is_empty())
            .map(|line| match line.rsplit_once("kMDItemLastUsedDate = ") {
                Some((path, value)) => (
                    PathBuf::from(path.trim_end()),
                    parse_mdls_date(value.trim()),
                ),
                None => (PathBuf::from(line), None),
            })
            .collect())
    }
}

/// One Recents row, shaped like the dense listing's `Item` (camelCase across IPC) so the
/// frontend parses both with the same schema. Always a file; `modifiedMs` is the
/// last-used date (SPEC §7).
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct RecentItem {
    name: String,
    path: String,
    is_directory: bool,
    is_hidden: bool,
    kind: String,
    modified_ms: Option<i64>,
    size_bytes: Option<u64>,
}

/// Outcome of a single refresh, before it is folded into the cache.
enum RefreshResult {
    Ok(Vec<RecentItem>),
    Empty,
    Unavailable(UnavailableReason),
}

/// Turn a dated path into a Recents row, or `None` if it vanished during the query or is
/// not a file. Inaccessible-but-present files are kept (size unknown), per SPEC §7.
fn build_item(path: &Path, modified_ms: i64) -> Option<RecentItem> {
    let name = path.file_name().and_then(OsStr::to_str)?.to_owned();
    let (is_directory, size_bytes) = match fs::metadata(path) {
        Ok(metadata) => {
            if metadata.is_dir() {
                return None; // Recents is files only (defensive; the predicate excludes dirs).
            }
            (false, Some(metadata.len()))
        }
        // NotFound means it disappeared during the query — drop it (research #3). Any
        // other error (e.g. permission) keeps the row visible with an unknown size.
        Err(error) if error.kind() == io::ErrorKind::NotFound => return None,
        Err(_) => (false, None),
    };
    Some(RecentItem {
        kind: kind_label(&name, is_directory),
        name,
        path: path.to_string_lossy().into_owned(),
        is_directory,
        is_hidden: false,
        modified_ms: Some(modified_ms),
        size_bytes,
    })
}

/// Gather the Recents collection through the seam with the production gather target.
fn compute_recents(spotlight: &dyn Spotlight) -> RefreshResult {
    compute_recents_with(spotlight, REFRESH_TARGET)
}

/// Gather the Recents collection through the seam: one unbounded query, drop dateless
/// hits, post-filter, sort newest-first, keep the `target` top slice, and only then read
/// per-file metadata. Index health is consulted only when the gather fails or comes back
/// empty — a successful `mdfind` proves Spotlight is alive, and the extra `mdutil`
/// process on every refresh cost real budget (SPEC §10: refresh ≤150 ms). Pure with
/// respect to the process boundary, so tests drive it with a fake Spotlight and a small
/// target.
fn compute_recents_with(spotlight: &dyn Spotlight, target: usize) -> RefreshResult {
    // Classify an unhealthy-looking gather honestly: a disabled or excluded index if
    // `mdutil` says so, otherwise "indexing" (present but not answering).
    fn classify_failure(spotlight: &dyn Spotlight) -> RefreshResult {
        match spotlight.index_status() {
            Ok(IndexHealth::Unavailable(reason)) => RefreshResult::Unavailable(reason),
            Ok(IndexHealth::Enabled) | Err(()) => {
                RefreshResult::Unavailable(UnavailableReason::Indexing)
            }
        }
    }

    let found = match spotlight.find_dated(PREDICATE) {
        Ok(found) => found,
        Err(()) => return classify_failure(spotlight),
    };

    // Everything up to the truncate is string-level work — no filesystem IO.
    let mut seen = std::collections::HashSet::new();
    let mut candidates: Vec<(i64, PathBuf)> = found
        .into_iter()
        .filter(|(path, _)| seen.insert(path.clone()))
        .filter_map(|(path, date)| date.map(|modified_ms| (modified_ms, path)))
        .filter(|(_, path)| passes_post_filter(path))
        .collect();
    candidates.sort_by(|a, b| b.0.cmp(&a.0).then_with(|| a.1.cmp(&b.1)));
    candidates.truncate(target);

    let mut dated: Vec<(i64, RecentItem)> = Vec::new();
    for (modified_ms, path) in candidates {
        if let Some(item) = build_item(&path, modified_ms) {
            dated.push((modified_ms, item));
        }
    }

    if dated.is_empty() {
        // Distinguish an honestly empty collection from a sick index (§7).
        return match spotlight.index_status() {
            Ok(IndexHealth::Enabled) => RefreshResult::Empty,
            Ok(IndexHealth::Unavailable(reason)) => RefreshResult::Unavailable(reason),
            Err(()) => RefreshResult::Unavailable(UnavailableReason::Indexing),
        };
    }

    // Newest last-used first; localized-name-ish then full path as stable tie-breakers.
    dated.sort_by(|a, b| {
        b.0.cmp(&a.0)
            .then_with(|| a.1.name.cmp(&b.1.name))
            .then_with(|| a.1.path.cmp(&b.1.path))
    });
    RefreshResult::Ok(dated.into_iter().map(|(_, item)| item).collect())
}

/// The live, in-memory cache and its last known degraded state (SPEC §7, §11). The
/// `unavailable` reason only surfaces when there are no cached items to show; a stale
/// non-empty cache is always shown in preference to a degraded line (last-success wins).
#[derive(Serialize, Deserialize, Clone, Debug, Default, PartialEq, Eq)]
struct CacheState {
    items: Vec<RecentItem>,
    unavailable: Option<UnavailableReason>,
}

/// The persisted cache envelope.
#[derive(Serialize, Deserialize)]
struct PersistedCache {
    version: u32,
    #[serde(flatten)]
    state: CacheState,
}

fn read_cache(path: &Path) -> Option<CacheState> {
    let text = fs::read_to_string(path).ok()?;
    let persisted: PersistedCache = serde_json::from_str(&text).ok()?;
    if persisted.version != CACHE_VERSION {
        return None;
    }
    Some(persisted.state)
}

fn write_cache(path: &Path, state: &CacheState) -> io::Result<()> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    let persisted = PersistedCache {
        version: CACHE_VERSION,
        state: state.clone(),
    };
    let json = serde_json::to_string(&persisted).map_err(io::Error::other)?;
    // Atomic write: sibling temp file, then rename over the target.
    let temp_path = path.with_extension("json.tmp");
    fs::write(&temp_path, json.as_bytes())?;
    fs::rename(&temp_path, path)
}

/// The Recents response across IPC (SPEC §7). Tagged by `state` so the frontend matches
/// honest-empty against spotlight-unavailable.
#[derive(Serialize, Debug, PartialEq, Eq)]
#[serde(tag = "state", rename_all = "snake_case")]
pub enum RecentsResponse {
    /// A slice of the cache; `total` is the full cached length for progressive paging.
    Ok {
        items: Vec<RecentItem>,
        total: usize,
    },
    /// Spotlight is healthy but there are no recent files.
    Empty,
    /// Spotlight cannot serve Recents; the reason explains why.
    SpotlightUnavailable { reason: UnavailableReason },
}

/// Managed Tauri state: the last-success cache plus the single-refresh guard.
pub struct RecentsCache {
    state: Mutex<CacheState>,
    refreshing: AtomicBool,
    cache_file: PathBuf,
}

impl RecentsCache {
    /// Load the persisted cache on start (SPEC §7, §11); an absent or corrupt file starts
    /// empty. No refresh happens here — the first `get_recents` triggers it.
    pub fn load(app_data_dir: &Path) -> Self {
        let cache_file = app_data_dir.join(CACHE_FILE);
        let state = read_cache(&cache_file).unwrap_or_default();
        Self {
            state: Mutex::new(state),
            refreshing: AtomicBool::new(false),
            cache_file,
        }
    }

    /// Serve a slice of the cache instantly (SPEC §10: cached paint ≤50 ms).
    fn serve(&self, offset: usize, limit: usize) -> RecentsResponse {
        let cache = self.state.lock().expect("recents cache lock poisoned");
        if cache.items.is_empty() {
            return match cache.unavailable {
                Some(reason) => RecentsResponse::SpotlightUnavailable { reason },
                None => RecentsResponse::Empty,
            };
        }
        let total = cache.items.len();
        let items = cache
            .items
            .iter()
            .skip(offset)
            .take(limit)
            .cloned()
            .collect();
        RecentsResponse::Ok { items, total }
    }

    /// Fold a refresh outcome into the cache, returning the resulting item count and
    /// whether anything actually changed. An unavailable outcome preserves any existing
    /// items (last-success wins). The `changed` flag gates the update event so a stable
    /// refresh does not re-notify the frontend into an endless re-pull loop.
    fn apply(&self, result: RefreshResult) -> (usize, bool) {
        let mut cache = self.state.lock().expect("recents cache lock poisoned");
        let changed = match result {
            RefreshResult::Ok(items) => {
                let changed = cache.items != items || cache.unavailable.is_some();
                cache.items = items;
                cache.unavailable = None;
                changed
            }
            RefreshResult::Empty => {
                let changed = !cache.items.is_empty() || cache.unavailable.is_some();
                cache.items.clear();
                cache.unavailable = None;
                changed
            }
            RefreshResult::Unavailable(reason) => {
                let changed = cache.unavailable != Some(reason);
                cache.unavailable = Some(reason);
                changed
            }
        };
        (cache.items.len(), changed)
    }

    fn persist(&self) -> io::Result<()> {
        let state = self
            .state
            .lock()
            .expect("recents cache lock poisoned")
            .clone();
        write_cache(&self.cache_file, &state)
    }

    /// Start one background refresh if none is in flight (SPEC §7: one at a time). The
    /// thread recomputes through the real Spotlight, persists, records telemetry, and
    /// emits `beeline://recents-updated` so the frontend re-pulls.
    fn spawn_refresh(&self, app: AppHandle) {
        if self.refreshing.swap(true, Ordering::AcqRel) {
            return; // A refresh is already running.
        }
        thread::spawn(move || {
            let started = Instant::now();
            let result = compute_recents(&SystemSpotlight);
            let cache = app.state::<RecentsCache>();
            let (count, changed) = cache.apply(result);
            let duration_ms = u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX);
            if let Err(error) = app.state::<Telemetry>().record(
                "recents_refresh",
                json!({ "count": count, "duration_ms": duration_ms, "changed": changed }),
            ) {
                eprintln!("telemetry event recents_refresh failed: {error}");
            }
            // Persist and notify only on a real change: an unchanged refresh writing and
            // emitting would trigger the frontend to re-pull, refreshing forever.
            if changed {
                if let Err(error) = cache.persist() {
                    eprintln!("recents cache persist failed: {error}");
                }
                if let Err(error) = app.emit(RECENTS_UPDATED_EVENT, json!({ "count": count })) {
                    eprintln!("failed to emit recents-updated event: {error}");
                }
            }
            cache.refreshing.store(false, Ordering::Release);
        });
    }
}

/// Serve a page of Recents from the cache and trigger a background refresh (SPEC §7).
/// Returns instantly; the refresh notifies via `beeline://recents-updated`.
#[tauri::command]
pub fn get_recents(
    offset: usize,
    limit: usize,
    app: AppHandle,
    state: State<'_, RecentsCache>,
) -> RecentsResponse {
    let response = state.serve(offset, limit);
    state.spawn_refresh(app);
    response
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;
    use std::sync::atomic::{AtomicU64, Ordering};

    static TEMP_COUNTER: AtomicU64 = AtomicU64::new(0);

    /// A unique temp directory removed on drop (no `tempfile` dependency).
    struct TempDir(PathBuf);

    impl TempDir {
        fn new() -> Self {
            let unique = TEMP_COUNTER.fetch_add(1, Ordering::Relaxed);
            let path = std::env::temp_dir().join(format!(
                "beeline_recents_{}_{}",
                std::process::id(),
                unique
            ));
            fs::create_dir_all(&path).expect("create temp dir");
            Self(path)
        }

        fn path(&self) -> &Path {
            &self.0
        }
    }

    impl Drop for TempDir {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    /// A scripted Spotlight: canned health, one path list per successive `find` call
    /// (to exercise widening), and a date per path. Records the queries it was asked.
    struct FakeSpotlight {
        health: Result<IndexHealth, ()>,
        finds: Mutex<Vec<Result<Vec<PathBuf>, ()>>>,
        queries: Mutex<Vec<String>>,
        dates: HashMap<PathBuf, Option<i64>>,
    }

    impl FakeSpotlight {
        fn healthy(
            finds: Vec<Result<Vec<PathBuf>, ()>>,
            dates: HashMap<PathBuf, Option<i64>>,
        ) -> Self {
            Self {
                health: Ok(IndexHealth::Enabled),
                finds: Mutex::new(finds),
                queries: Mutex::new(Vec::new()),
                dates,
            }
        }
    }

    impl Spotlight for FakeSpotlight {
        fn index_status(&self) -> Result<IndexHealth, ()> {
            self.health
        }

        fn find(&self, query: &str) -> Result<Vec<PathBuf>, ()> {
            self.queries.lock().unwrap().push(query.to_owned());
            let mut finds = self.finds.lock().unwrap();
            // Once the script is exhausted, repeat the last result: a wider window is a
            // superset of the narrower one, so it never returns fewer paths.
            if finds.len() > 1 {
                finds.remove(0)
            } else {
                finds.first().cloned().unwrap_or_else(|| Ok(Vec::new()))
            }
        }

        fn last_used(&self, paths: &[PathBuf]) -> Result<Vec<Option<i64>>, ()> {
            Ok(paths
                .iter()
                .map(|path| self.dates.get(path).copied().flatten())
                .collect())
        }
    }

    #[test]
    fn predicate_shape() {
        // Finder's public Recents predicate: last-used date plus user-facing types.
        assert!(PREDICATE.contains("kMDItemLastUsedDate"));
        assert!(PREDICATE.contains("public.content"));
        assert!(PREDICATE.contains("public.archive"));
        assert!(!PREDICATE.contains("$time.today"));
    }

    #[test]
    fn post_filter_rejects_hidden_and_support() {
        assert!(passes_post_filter(Path::new(
            "/Users/k/Documents/report.pdf"
        )));
        // A dot-named component anywhere rejects the whole path.
        assert!(!passes_post_filter(Path::new(
            "/Users/k/.config/app/data.json"
        )));
        assert!(!passes_post_filter(Path::new("/Users/k/.hidden.txt")));
        // Known support files.
        assert!(!passes_post_filter(Path::new("/Users/k/Desktop/.DS_Store")));
        assert!(!passes_post_filter(Path::new("/Users/k/Desktop/Icon\r")));
    }

    #[test]
    fn mdutil_classification() {
        assert_eq!(
            classify_mdutil("/:\n\tIndexing enabled."),
            IndexHealth::Enabled
        );
        assert_eq!(
            classify_mdutil("/:\n\tIndexing and searching disabled."),
            IndexHealth::Unavailable(UnavailableReason::Disabled)
        );
        assert_eq!(
            classify_mdutil("/Volumes/x:\n\tNo index."),
            IndexHealth::Unavailable(UnavailableReason::PrivacyExcluded)
        );
    }

    #[test]
    fn mdls_date_parsing() {
        assert_eq!(parse_mdls_date("1970-01-01 00:00:00 +0000"), Some(0));
        assert_eq!(
            parse_mdls_date("1970-01-02 00:00:00 +0000"),
            Some(86_400_000)
        );
        // A +0900 local time is nine hours ahead of UTC, so it lands back on the epoch.
        assert_eq!(parse_mdls_date("1970-01-01 09:00:00 +0900"), Some(0));
        assert_eq!(parse_mdls_date("(null)"), None);
        assert_eq!(parse_mdls_date(""), None);
    }

    #[test]
    fn degraded_disabled_index() {
        let fake = FakeSpotlight {
            health: Ok(IndexHealth::Unavailable(UnavailableReason::Disabled)),
            finds: Mutex::new(Vec::new()),
            queries: Mutex::new(Vec::new()),
            dates: HashMap::new(),
        };
        assert!(matches!(
            compute_recents(&fake),
            RefreshResult::Unavailable(UnavailableReason::Disabled)
        ));
    }

    #[test]
    fn degraded_mdfind_failure_maps_to_indexing() {
        let fake = FakeSpotlight::healthy(vec![Err(())], HashMap::new());
        assert!(matches!(
            compute_recents(&fake),
            RefreshResult::Unavailable(UnavailableReason::Indexing)
        ));
    }

    #[test]
    fn empty_when_no_dated_matches() {
        let fake = FakeSpotlight::healthy(vec![Ok(Vec::new())], HashMap::new());
        assert!(matches!(compute_recents(&fake), RefreshResult::Empty));
    }

    #[test]
    fn gathers_and_sorts_newest_first() {
        let dir = TempDir::new();
        let older = dir.path().join("older.txt");
        let newer = dir.path().join("newer.txt");
        let hidden = dir.path().join(".secret.txt");
        fs::write(&older, b"x").unwrap();
        fs::write(&newer, b"x").unwrap();
        fs::write(&hidden, b"x").unwrap();

        let mut dates = HashMap::new();
        dates.insert(older.clone(), Some(1_000));
        dates.insert(newer.clone(), Some(2_000));
        dates.insert(hidden.clone(), Some(3_000)); // Newest, but hidden — filtered out.

        let fake = FakeSpotlight::healthy(
            vec![Ok(vec![older.clone(), newer.clone(), hidden.clone()])],
            dates,
        );
        match compute_recents(&fake) {
            RefreshResult::Ok(items) => {
                let names: Vec<&str> = items.iter().map(|item| item.name.as_str()).collect();
                assert_eq!(names, vec!["newer.txt", "older.txt"]);
                assert_eq!(items[0].modified_ms, Some(2_000));
                assert!(!items[0].is_directory);
            }
            other => panic!("expected Ok, got {:?}", other_kind(&other)),
        }
    }

    #[test]
    fn gathers_once_and_truncates_to_target() {
        let dir = TempDir::new();
        let first = dir.path().join("a.txt");
        let second = dir.path().join("b.txt");
        let third = dir.path().join("c.txt");
        for path in [&first, &second, &third] {
            fs::write(path, b"x").unwrap();
        }
        let mut dates = HashMap::new();
        dates.insert(first.clone(), Some(10));
        dates.insert(second.clone(), Some(20));
        dates.insert(third.clone(), Some(30));

        let fake = FakeSpotlight::healthy(vec![Ok(vec![first, second, third])], dates);
        let result = compute_recents_with(&fake, 2);
        // The two newest survive the truncate; the oldest never pays metadata IO.
        match result {
            RefreshResult::Ok(items) => {
                let names: Vec<&str> = items.iter().map(|item| item.name.as_str()).collect();
                assert_eq!(names, vec!["c.txt", "b.txt"]);
            }
            other => panic!("expected Ok, got {:?}", other_kind(&other)),
        }
        // One gather, with the bare unbounded predicate.
        let queries = fake.queries.lock().unwrap();
        assert_eq!(queries.as_slice(), &[PREDICATE.to_owned()]);
    }

    #[test]
    fn cache_round_trip() {
        let dir = TempDir::new();
        let file = dir.path().join(CACHE_FILE);
        let state = CacheState {
            items: vec![RecentItem {
                name: "report.pdf".to_owned(),
                path: "/Users/k/report.pdf".to_owned(),
                is_directory: false,
                is_hidden: false,
                kind: "PDF".to_owned(),
                modified_ms: Some(123),
                size_bytes: Some(456),
            }],
            unavailable: Some(UnavailableReason::PrivacyExcluded),
        };
        write_cache(&file, &state).expect("write");
        let loaded = read_cache(&file).expect("read");
        assert_eq!(loaded, state);

        // A version mismatch is rejected (returns None, i.e. an empty start).
        fs::write(&file, br#"{"version":999,"items":[],"unavailable":null}"#).unwrap();
        assert!(read_cache(&file).is_none());
    }

    #[test]
    fn serve_distinguishes_empty_from_unavailable() {
        let dir = TempDir::new();
        let file = dir.path().join(CACHE_FILE);
        let cache = RecentsCache {
            state: Mutex::new(CacheState::default()),
            refreshing: AtomicBool::new(false),
            cache_file: file,
        };
        // No items, no reason → honest empty.
        assert_eq!(cache.serve(0, 100), RecentsResponse::Empty);
        // No items, a reason → unavailable.
        cache.apply(RefreshResult::Unavailable(UnavailableReason::Disabled));
        assert_eq!(
            cache.serve(0, 100),
            RecentsResponse::SpotlightUnavailable {
                reason: UnavailableReason::Disabled
            }
        );
    }

    #[test]
    fn serve_pages_from_cache() {
        let dir = TempDir::new();
        let cache = RecentsCache {
            state: Mutex::new(CacheState::default()),
            refreshing: AtomicBool::new(false),
            cache_file: dir.path().join(CACHE_FILE),
        };
        let items: Vec<RecentItem> = (0..5)
            .map(|i| RecentItem {
                name: format!("f{i}.txt"),
                path: format!("/x/f{i}.txt"),
                is_directory: false,
                is_hidden: false,
                kind: "TXT".to_owned(),
                modified_ms: Some(i),
                size_bytes: Some(1),
            })
            .collect();
        cache.apply(RefreshResult::Ok(items));
        // A stale unavailable is cleared once items land.
        match cache.serve(2, 2) {
            RecentsResponse::Ok { items, total } => {
                assert_eq!(total, 5);
                let names: Vec<&str> = items.iter().map(|i| i.name.as_str()).collect();
                assert_eq!(names, vec!["f2.txt", "f3.txt"]);
            }
            other => panic!("expected Ok, got {other:?}"),
        }
    }

    fn other_kind(result: &RefreshResult) -> &'static str {
        match result {
            RefreshResult::Ok(_) => "Ok",
            RefreshResult::Empty => "Empty",
            RefreshResult::Unavailable(_) => "Unavailable",
        }
    }
}

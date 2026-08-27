use std::{
    collections::{HashMap, HashSet, VecDeque},
    fs, io,
    path::{Path, PathBuf},
    sync::{
        atomic::{AtomicU64, Ordering},
        Arc, Mutex,
    },
    time::{Instant, UNIX_EPOCH},
};

use serde::{Deserialize, Serialize};
use serde_json::json;
use tauri::State;

use crate::telemetry::Telemetry;

// A large directory paints from this sorted prefix, then the frontend asks for the full
// metadata-rich listing in the background. This is comfortably above one viewport while
// keeping the first-screen path free of tens of thousands of stat calls (SPEC §10).
const INITIAL_LIST_ITEMS: usize = 64;
const MAX_WINDOW_ITEMS: usize = 4_096;
const MAX_LISTING_SESSIONS: usize = 16;

// One directory entry, shaped for the dense listing. Field names cross the IPC
// boundary as camelCase to match the zod schema on the TypeScript side.
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ListedItem {
    name: String,
    path: String,
    is_directory: bool,
    is_hidden: bool,
    kind: String,
    modified_ms: Option<i64>,
    size_bytes: Option<u64>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ListLocationWindow {
    path: String,
    session_id: String,
    items: Vec<ListedItem>,
    offset: usize,
    total: usize,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct InitialListLocation {
    path: String,
    session_id: String,
    items: Vec<ListedItem>,
    offset: usize,
    total: usize,
    complete: bool,
    focus_index: Option<usize>,
    selected: Vec<ResolvedPath>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct InitialListRequest {
    path: String,
    owner_id: String,
    owner_generation: u64,
    focus_path: Option<String>,
    selected_paths: Vec<String>,
    preferred_index: Option<usize>,
}

struct SortableEntry {
    name: String,
    path: PathBuf,
    sort_key: String,
}

struct SessionEntry {
    name: String,
    path: PathBuf,
}

struct ListingSession {
    path: String,
    entries: Vec<SessionEntry>,
}

struct SessionStore {
    sessions: HashMap<u64, Arc<ListingSession>>,
    owners: HashMap<String, (u64, u64)>,
    insertion_order: VecDeque<u64>,
}

pub struct ListingSessions {
    next_id: AtomicU64,
    store: Mutex<SessionStore>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ResolvedPath {
    path: String,
    index: usize,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ListingFileNeighbor {
    #[serde(flatten)]
    window: ListLocationWindow,
    focus_index: usize,
}

// A tagged, code-only error: raw io::Error text never reaches the UI.
#[derive(Debug, Serialize)]
#[serde(tag = "code", rename_all = "kebab-case")]
pub enum ListError {
    NotFound,
    NotADirectory,
    PermissionDenied,
    SessionExpired,
    Io,
}

impl ListingSessions {
    pub fn new() -> Self {
        Self {
            next_id: AtomicU64::new(1),
            store: Mutex::new(SessionStore {
                sessions: HashMap::new(),
                owners: HashMap::new(),
                insertion_order: VecDeque::new(),
            }),
        }
    }

    fn insert(
        &self,
        owner_id: String,
        owner_generation: u64,
        session: Arc<ListingSession>,
    ) -> Result<String, ListError> {
        let id = self.next_id.fetch_add(1, Ordering::Relaxed);
        let mut store = self.store.lock().map_err(|_| ListError::Io)?;
        if store
            .owners
            .get(&owner_id)
            .is_some_and(|(generation, _)| *generation > owner_generation)
        {
            return Err(ListError::SessionExpired);
        }
        if let Some((_, previous)) = store.owners.insert(owner_id, (owner_generation, id)) {
            store.sessions.remove(&previous);
            store
                .insertion_order
                .retain(|candidate| *candidate != previous);
        }
        store.sessions.insert(id, session);
        store.insertion_order.push_back(id);
        while store.insertion_order.len() > MAX_LISTING_SESSIONS {
            if let Some(expired) = store.insertion_order.pop_front() {
                store.sessions.remove(&expired);
                store
                    .owners
                    .retain(|_, (_, session_id)| *session_id != expired);
            }
        }
        Ok(id.to_string())
    }

    fn get(&self, id: &str) -> Result<Arc<ListingSession>, ListError> {
        let id = id.parse::<u64>().map_err(|_| ListError::SessionExpired)?;
        self.store
            .lock()
            .map_err(|_| ListError::Io)?
            .sessions
            .get(&id)
            .cloned()
            .ok_or(ListError::SessionExpired)
    }
}

fn map_io_error(error: &io::Error) -> ListError {
    match error.kind() {
        io::ErrorKind::NotFound => ListError::NotFound,
        io::ErrorKind::PermissionDenied => ListError::PermissionDenied,
        _ => ListError::Io,
    }
}

pub(crate) fn kind_label(name: &str, is_directory: bool) -> String {
    if is_directory {
        return "Folder".to_owned();
    }
    match Path::new(name).extension().and_then(|ext| ext.to_str()) {
        Some(ext) if !ext.is_empty() => ext.to_uppercase(),
        _ => "File".to_owned(),
    }
}

fn modified_ms(metadata: &fs::Metadata) -> Option<i64> {
    let modified = metadata.modified().ok()?;
    let duration = modified.duration_since(UNIX_EPOCH).ok()?;
    i64::try_from(duration.as_millis()).ok()
}

fn describe_entry(entry: &SessionEntry) -> ListedItem {
    let name = entry.name.clone();
    let entry_path = &entry.path;
    // metadata() follows symlinks so a symlinked directory is navigable.
    let metadata = fs::metadata(entry_path).ok();
    let is_directory = metadata.as_ref().is_some_and(fs::Metadata::is_dir);
    let is_hidden = name.starts_with('.');
    let kind = kind_label(&name, is_directory);
    let modified_ms = metadata.as_ref().and_then(modified_ms);
    let size_bytes = match &metadata {
        Some(meta) if !is_directory => Some(meta.len()),
        _ => None,
    };

    ListedItem {
        name,
        path: entry_path.to_string_lossy().into_owned(),
        is_directory,
        is_hidden,
        kind,
        modified_ms,
        size_bytes,
    }
}

fn build_session(path: &str) -> Result<ListingSession, ListError> {
    let metadata = fs::metadata(path).map_err(|error| map_io_error(&error))?;
    if !metadata.is_dir() {
        return Err(ListError::NotADirectory);
    }

    let mut entries = Vec::new();
    for entry in fs::read_dir(path).map_err(|error| map_io_error(&error))? {
        // Skip entries that error mid-scan rather than failing the whole listing.
        let Ok(entry) = entry else { continue };
        let name = entry.file_name().to_string_lossy().into_owned();
        let sort_key = name.to_lowercase();
        entries.push(SortableEntry {
            name,
            path: entry.path(),
            sort_key,
        });
    }

    // Directories and files intermixed, case-insensitive by name.
    entries.sort_by(|a, b| a.sort_key.cmp(&b.sort_key));
    Ok(ListingSession {
        path: path.to_owned(),
        entries: entries
            .into_iter()
            .map(|entry| SessionEntry {
                name: entry.name,
                path: entry.path,
            })
            .collect(),
    })
}

// Validate a proposed Location for the Default Entry Point (SPEC §4, §12) using a single
// metadata call — no directory enumeration or sort. An empty, missing, or file path is
// rejected with the same tagged cause as a listing, so the UI can never persist a
// non-directory entry point. On success the accepted path is echoed back.
fn validate_directory_path(path: &str) -> Result<String, ListError> {
    let metadata = fs::metadata(path).map_err(|error| map_io_error(&error))?;
    if !metadata.is_dir() {
        return Err(ListError::NotADirectory);
    }
    Ok(path.to_owned())
}

fn centered_offset(total: usize, preferred_index: usize, limit: usize) -> usize {
    let count = total.min(limit);
    preferred_index
        .saturating_sub(count / 2)
        .min(total.saturating_sub(count))
}

fn read_window(session: &ListingSession, offset: usize, limit: usize) -> (usize, Vec<ListedItem>) {
    let limit = limit.clamp(1, MAX_WINDOW_ITEMS);
    let offset = offset.min(session.entries.len().saturating_sub(limit));
    let items = session
        .entries
        .iter()
        .skip(offset)
        .take(limit)
        .map(describe_entry)
        .collect();
    (offset, items)
}

fn find_index(session: &ListingSession, path: &str) -> Option<usize> {
    let target = Path::new(path);
    session
        .entries
        .iter()
        .position(|entry| entry.path == target)
}

fn resolve_selected(session: &ListingSession, paths: &[String]) -> Vec<ResolvedPath> {
    let requested: HashSet<&Path> = paths.iter().map(|path| Path::new(path.as_str())).collect();
    let mut found = HashMap::<&Path, usize>::new();
    for (index, entry) in session.entries.iter().enumerate() {
        if requested.contains(entry.path.as_path()) {
            found.insert(entry.path.as_path(), index);
        }
    }
    paths
        .iter()
        .filter_map(|path| {
            found
                .get(Path::new(path.as_str()))
                .map(|index| ResolvedPath {
                    path: path.clone(),
                    index: *index,
                })
        })
        .collect()
}

fn resolve_indices(session: &ListingSession, indices: &[usize]) -> Vec<ResolvedPath> {
    indices
        .iter()
        .filter_map(|index| {
            session.entries.get(*index).map(|entry| ResolvedPath {
                path: entry.path.to_string_lossy().into_owned(),
                index: *index,
            })
        })
        .collect()
}

fn read_initial_location(
    session: &ListingSession,
    session_id: String,
    focus_path: Option<&str>,
    selected_paths: &[String],
    preferred_index: Option<usize>,
) -> InitialListLocation {
    let total = session.entries.len();
    let complete = total <= INITIAL_LIST_ITEMS;
    let focus_index = focus_path.and_then(|path| find_index(session, path));
    let preferred = preferred_index.or(focus_index).unwrap_or(0);
    let requested_offset = centered_offset(total, preferred, INITIAL_LIST_ITEMS);
    let (offset, items) = read_window(session, requested_offset, INITIAL_LIST_ITEMS);

    InitialListLocation {
        path: session.path.clone(),
        session_id,
        items,
        offset,
        total,
        complete,
        focus_index,
        selected: resolve_selected(session, selected_paths),
    }
}

fn list_window(
    session: &ListingSession,
    session_id: String,
    offset: usize,
    limit: usize,
) -> ListLocationWindow {
    let (offset, items) = read_window(session, offset, limit);
    ListLocationWindow {
        path: session.path.clone(),
        session_id,
        items,
        offset,
        total: session.entries.len(),
    }
}

fn find_file_neighbor(
    session: &ListingSession,
    session_id: String,
    index: usize,
    delta: i32,
    limit: usize,
) -> Option<ListingFileNeighbor> {
    let mut candidate = i64::try_from(index).unwrap_or(i64::MAX) + i64::from(delta);
    let total = i64::try_from(session.entries.len()).unwrap_or(i64::MAX);
    while candidate >= 0 && candidate < total {
        let candidate_index = usize::try_from(candidate).ok()?;
        let item = describe_entry(session.entries.get(candidate_index)?);
        if !item.is_directory {
            let offset = centered_offset(session.entries.len(), candidate_index, limit);
            return Some(ListingFileNeighbor {
                window: list_window(session, session_id, offset, limit),
                focus_index: candidate_index,
            });
        }
        candidate += i64::from(delta);
    }
    None
}

#[tauri::command]
pub async fn list_location_initial(
    request: InitialListRequest,
    sessions: State<'_, ListingSessions>,
    telemetry: State<'_, Telemetry>,
) -> Result<InitialListLocation, ListError> {
    let started = Instant::now();
    let telemetry_path = request.path.clone();
    let task_path = request.path.clone();
    let session = tauri::async_runtime::spawn_blocking(move || build_session(&task_path))
        .await
        .map_err(|_| ListError::Io)?;
    let session = Arc::new(session?);
    let session_id = sessions.insert(
        request.owner_id.clone(),
        request.owner_generation,
        Arc::clone(&session),
    )?;
    let task_session_id = session_id.clone();
    let listing = tauri::async_runtime::spawn_blocking(move || {
        read_initial_location(
            &session,
            task_session_id,
            request.focus_path.as_deref(),
            &request.selected_paths,
            request.preferred_index,
        )
    })
    .await
    .map_err(|_| ListError::Io)?;

    let duration_ms = u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX);
    if let Err(error) = telemetry.record(
        "location_listed",
        json!({
            "path": telemetry_path,
            "count": listing.total,
            "returned": listing.items.len(),
            "complete": listing.complete,
            "phase": "initial",
            "duration_ms": duration_ms,
        }),
    ) {
        eprintln!("telemetry event location_listed failed: {error}");
    }

    Ok(listing)
}

#[tauri::command]
pub async fn list_location_window(
    session_id: String,
    offset: usize,
    limit: usize,
    sessions: State<'_, ListingSessions>,
) -> Result<ListLocationWindow, ListError> {
    let session = sessions.get(&session_id)?;
    let task_session_id = session_id.clone();
    tauri::async_runtime::spawn_blocking(move || {
        list_window(&session, task_session_id, offset, limit)
    })
    .await
    .map_err(|_| ListError::Io)
}

#[tauri::command]
pub async fn list_location_selection_paths(
    session_id: String,
    indices: Vec<usize>,
    sessions: State<'_, ListingSessions>,
) -> Result<Vec<ResolvedPath>, ListError> {
    let session = sessions.get(&session_id)?;
    tauri::async_runtime::spawn_blocking(move || resolve_indices(&session, &indices))
        .await
        .map_err(|_| ListError::Io)
}

#[tauri::command]
pub async fn list_location_file_neighbor(
    session_id: String,
    index: usize,
    delta: i32,
    limit: usize,
    sessions: State<'_, ListingSessions>,
) -> Result<Option<ListingFileNeighbor>, ListError> {
    if delta != -1 && delta != 1 {
        return Err(ListError::Io);
    }
    let session = sessions.get(&session_id)?;
    let task_session_id = session_id.clone();
    tauri::async_runtime::spawn_blocking(move || {
        find_file_neighbor(&session, task_session_id, index, delta, limit)
    })
    .await
    .map_err(|_| ListError::Io)
}

// Validate a Default Entry Point candidate off the IPC thread (SPEC §4, §12). Cheap: a
// single metadata call, no listing, so it can gate a Settings keystroke without cost.
#[tauri::command]
pub async fn validate_directory(path: String) -> Result<String, ListError> {
    tauri::async_runtime::spawn_blocking(move || validate_directory_path(&path))
        .await
        .map_err(|_| ListError::Io)?
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicU64, Ordering};

    use super::*;

    static NEXT_TEMP: AtomicU64 = AtomicU64::new(0);

    struct TestDir(std::path::PathBuf);

    impl TestDir {
        fn new() -> Self {
            let id = NEXT_TEMP.fetch_add(1, Ordering::Relaxed);
            let path = std::env::temp_dir()
                .join(format!("beeline-listing-test-{}-{id}", std::process::id()));
            fs::create_dir(&path).expect("create listing test directory");
            Self(path)
        }
    }

    impl Drop for TestDir {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    #[test]
    fn large_initial_listing_is_sorted_and_bounded() {
        let dir = TestDir::new();
        for index in (0..INITIAL_LIST_ITEMS + 2).rev() {
            fs::write(dir.0.join(format!("file_{index:04}.txt")), b"x")
                .expect("write listing fixture");
        }

        let session =
            build_session(dir.0.to_str().expect("utf-8 test path")).expect("build listing session");
        let initial = read_initial_location(&session, "1".to_owned(), None, &[], None);

        assert_eq!(initial.total, INITIAL_LIST_ITEMS + 2);
        assert_eq!(initial.items.len(), INITIAL_LIST_ITEMS);
        assert!(!initial.complete);
        assert_eq!(
            initial.items.first().map(|item| item.name.as_str()),
            Some("file_0000.txt")
        );
        let expected_last = format!("file_{:04}.txt", INITIAL_LIST_ITEMS - 1);
        assert_eq!(
            initial.items.last().map(|item| item.name.as_str()),
            Some(expected_last.as_str())
        );
    }

    #[test]
    fn initial_listing_resolves_focus_and_selection_outside_first_prefix() {
        let dir = TestDir::new();
        for index in 0..200 {
            fs::write(dir.0.join(format!("file_{index:04}.txt")), b"x")
                .expect("write listing fixture");
        }
        let focus = dir.0.join("file_0150.txt").to_string_lossy().into_owned();
        let selected = dir.0.join("file_0149.txt").to_string_lossy().into_owned();
        let session =
            build_session(dir.0.to_str().expect("utf-8 test path")).expect("build listing session");

        let initial = read_initial_location(
            &session,
            "2".to_owned(),
            Some(&focus),
            std::slice::from_ref(&selected),
            None,
        );

        assert_eq!(initial.focus_index, Some(150));
        assert_eq!(initial.selected.len(), 1);
        assert_eq!(initial.selected[0].index, 149);
        assert!(initial.offset > 0);
        assert!(initial.items.iter().any(|item| item.path == focus));
    }

    #[test]
    fn window_is_bounded_and_keeps_global_offset() {
        let dir = TestDir::new();
        for index in 0..100 {
            fs::write(dir.0.join(format!("file_{index:04}.txt")), b"x")
                .expect("write listing fixture");
        }
        let session =
            build_session(dir.0.to_str().expect("utf-8 test path")).expect("build listing session");

        let window = list_window(&session, "3".to_owned(), 40, 10);

        assert_eq!(window.offset, 40);
        assert_eq!(window.items.len(), 10);
        assert_eq!(window.total, 100);
        assert_eq!(window.items[0].name, "file_0040.txt");
    }

    #[test]
    fn quick_look_neighbor_skips_directories() {
        let dir = TestDir::new();
        fs::write(dir.0.join("file_0000.txt"), b"x").expect("write first file");
        fs::create_dir(dir.0.join("file_0001.txt")).expect("create intervening directory");
        fs::write(dir.0.join("file_0002.txt"), b"x").expect("write second file");
        let session =
            build_session(dir.0.to_str().expect("utf-8 test path")).expect("build listing session");

        let neighbor =
            find_file_neighbor(&session, "4".to_owned(), 0, 1, 2).expect("next file exists");

        assert_eq!(neighbor.focus_index, 2);
        let local = neighbor.focus_index - neighbor.window.offset;
        assert_eq!(neighbor.window.items[local].name, "file_0002.txt");
    }

    #[test]
    fn selection_paths_resolve_without_describing_the_whole_listing() {
        let dir = TestDir::new();
        for index in 0..10 {
            fs::write(dir.0.join(format!("file_{index:04}.txt")), b"x")
                .expect("write listing fixture");
        }
        let session =
            build_session(dir.0.to_str().expect("utf-8 test path")).expect("build listing session");

        let resolved = resolve_indices(&session, &[1, 8]);

        assert_eq!(resolved.len(), 2);
        assert!(resolved[0].path.ends_with("file_0001.txt"));
        assert!(resolved[1].path.ends_with("file_0008.txt"));
    }

    #[test]
    fn a_tab_revalidation_replaces_its_previous_session() {
        let sessions = ListingSessions::new();
        let first = Arc::new(ListingSession {
            path: "/first".to_owned(),
            entries: Vec::new(),
        });
        let second = Arc::new(ListingSession {
            path: "/second".to_owned(),
            entries: Vec::new(),
        });
        let first_id = sessions
            .insert("tab".to_owned(), 1, first)
            .expect("insert first session");
        let second_id = sessions
            .insert("tab".to_owned(), 2, second)
            .expect("replace session");
        let stale = Arc::new(ListingSession {
            path: "/stale".to_owned(),
            entries: Vec::new(),
        });

        assert!(matches!(
            sessions.get(&first_id),
            Err(ListError::SessionExpired)
        ));
        assert_eq!(
            sessions.get(&second_id).expect("new session remains").path,
            "/second"
        );
        assert!(matches!(
            sessions.insert("tab".to_owned(), 1, stale),
            Err(ListError::SessionExpired)
        ));
        assert_eq!(
            sessions
                .get(&second_id)
                .expect("newer session survives stale completion")
                .path,
            "/second"
        );
    }

    #[test]
    fn validate_directory_accepts_a_directory() {
        let dir = TestDir::new();
        let path = dir.0.to_str().expect("utf-8 test path");
        let accepted = validate_directory_path(path).expect("directory accepted");
        assert_eq!(accepted, path);
    }

    #[test]
    fn validate_directory_rejects_a_regular_file() {
        let dir = TestDir::new();
        let file = dir.0.join("entry.txt");
        fs::write(&file, b"x").expect("write file fixture");
        let error = validate_directory_path(file.to_str().expect("utf-8 test path"))
            .expect_err("regular file rejected");
        assert!(matches!(error, ListError::NotADirectory));
    }

    #[test]
    fn validate_directory_rejects_a_missing_path() {
        let dir = TestDir::new();
        let missing = dir.0.join("does-not-exist");
        let error = validate_directory_path(missing.to_str().expect("utf-8 test path"))
            .expect_err("missing path rejected");
        assert!(matches!(error, ListError::NotFound));
    }
}

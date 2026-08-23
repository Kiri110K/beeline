//! The operations engine (SPEC §8, §10, §13): the Tauri commands and events behind
//! Copy/Move-Paste, Rename, New Folder, Trash, Delete Permanently, Reveal, and Open With.
//! The UI (Action Menu, Status Strip, key wiring) is a later chunk; this layer only moves
//! bytes and reports outcomes.
//!
//! Async class (SPEC §10): every command runs its filesystem work off the IPC threads.
//! Single actions use `spawn_blocking`; multi-item actions become a background-QoS
//! **batch job** that streams progress and continues past individual failures.
//!
//! Batch protocol:
//! - The command validates up front (target is a directory, sources exist) and, on a
//!   validation failure, returns an [`OpError`] immediately with no job started (SPEC §13).
//! - Otherwise it registers a job, spawns a background-QoS worker, and returns the
//!   `job_id`. The worker emits `beeline://operation-progress` per item and one final
//!   `beeline://operation-finished` carrying the ok count and concrete per-item failures.
//! - `cancel_operation(job_id)` sets a flag the worker checks between items (best-effort
//!   universal cancellation, SPEC §10).

mod cause;
mod fsops;

use std::{
    collections::HashMap,
    fs,
    path::{Path, PathBuf},
    process::Command,
    sync::{
        atomic::{AtomicBool, AtomicU64, Ordering},
        Arc, Mutex,
    },
    thread,
    time::Instant,
};

use serde::Serialize;
use serde_json::json;
use tauri::{AppHandle, Emitter, Manager};

use crate::qos;
use crate::telemetry::Telemetry;

const PROGRESS_EVENT: &str = "beeline://operation-progress";
const FINISHED_EVENT: &str = "beeline://operation-finished";

/// An immediate, pre-dispatch failure (SPEC §13): validation of the request itself, or a
/// single action that could not even launch. Tagged `code` (kebab-case) like
/// [`crate::listing::ListError`] so the frontend matches on a discriminated union; raw
/// `io::Error` text never rides along.
#[derive(Serialize, Debug, PartialEq, Eq)]
#[serde(tag = "code", rename_all = "kebab-case")]
pub enum OpError {
    /// A source Item named for the operation does not exist.
    SourceNotFound { path: String },
    /// The paste/new-folder target directory does not exist.
    TargetNotFound { path: String },
    /// The paste/new-folder target exists but is not a directory.
    TargetNotADirectory { path: String },
    /// A proposed name is empty, a directory reference, or holds a path separator.
    InvalidName { name: String, reason: String },
    /// A rename would land on an existing entry (explicit rename never auto-suffixes).
    NameCollision { path: String },
    /// A launch-only action (`open`) could not start or exited non-zero.
    CommandFailed { detail: String },
    /// A filesystem failure during a single (non-batch) action, already made concrete.
    Io { detail: String },
}

/// One item's failure inside a batch, carried in the finished event (SPEC §8: concrete
/// cause, never a raw error dump).
#[derive(Serialize, Debug, Clone, PartialEq, Eq)]
pub struct Failure {
    path: String,
    cause: String,
}

/// `beeline://operation-progress`. Field names are the wire contract (snake_case, as the
/// ticket specifies) — not the app's usual camelCase — so chunk B binds them verbatim.
#[derive(Serialize, Clone)]
struct ProgressPayload {
    job_id: u64,
    done: usize,
    total: usize,
    current_path: String,
}

/// `beeline://operation-finished`.
#[derive(Serialize, Clone)]
struct FinishedPayload {
    job_id: u64,
    ok_count: usize,
    failures: Vec<Failure>,
}

/// The kind of batch, for the `operation_finished` telemetry line.
#[derive(Clone, Copy)]
enum OpKind {
    PasteCopy,
    PasteMove,
    Trash,
    Delete,
}

impl OpKind {
    fn as_str(self) -> &'static str {
        match self {
            OpKind::PasteCopy => "paste_copy",
            OpKind::PasteMove => "paste_move",
            OpKind::Trash => "trash",
            OpKind::Delete => "delete_permanently",
        }
    }
}

/// The system Trash seam (SPEC §8: Move to Trash routes to the system Trash, recoverable
/// via Finder's Put Back). Trait-injected so unit tests never touch the real Trash; only
/// the ignored integration test uses [`SystemTrash`].
trait TrashBin {
    /// Move one path to the system Trash, or return a concrete Status Strip cause.
    fn trash(&self, path: &Path) -> Result<(), String>;
}

/// The real Trash adapter over the `trash` crate.
struct SystemTrash;

impl TrashBin for SystemTrash {
    fn trash(&self, path: &Path) -> Result<(), String> {
        trash::delete(path)
            .map_err(|error| format!("Could not move to Trash: {} ({error})", path.display()))
    }
}

/// One planned unit of a batch. The paste variants carry the target directory and resolve
/// their collision-free destination at execution time, so a same-Location duplicate and a
/// within-batch name clash both climb the numeric suffix correctly (SPEC §8).
enum UnitOp {
    Copy { src: PathBuf, target_dir: PathBuf },
    Move { src: PathBuf, target_dir: PathBuf },
    Trash { path: PathBuf },
    Delete { path: PathBuf },
}

impl UnitOp {
    /// The subject path reported in progress and attached to a failure.
    fn path(&self) -> &Path {
        match self {
            UnitOp::Copy { src, .. } | UnitOp::Move { src, .. } => src,
            UnitOp::Trash { path } | UnitOp::Delete { path } => path,
        }
    }

    /// Perform this unit, returning a concrete cause string on failure.
    fn execute(&self, trash: &dyn TrashBin) -> Result<(), String> {
        match self {
            UnitOp::Copy { src, target_dir } => {
                let name = file_name_str(src)?;
                let dst = fsops::resolve_collision(target_dir, &name);
                fsops::copy_tree(src, &dst).map_err(|error| cause::describe("copying", src, &error))
            }
            UnitOp::Move { src, target_dir } => {
                let name = file_name_str(src)?;
                let dst = fsops::resolve_collision(target_dir, &name);
                fsops::move_item(src, &dst).map_err(|error| cause::describe("moving", src, &error))
            }
            UnitOp::Trash { path } => trash.trash(path),
            UnitOp::Delete { path } => {
                fsops::remove_tree(path).map_err(|error| cause::describe("deleting", path, &error))
            }
        }
    }
}

/// The final tally of a batch run.
struct BatchOutcome {
    ok_count: usize,
    failures: Vec<Failure>,
}

/// Run the planned units in order, reporting progress before each and collecting failures
/// without stopping (SPEC §8). Cancellation is checked between items only — an in-flight
/// item always finishes (SPEC §10, best-effort). Pure with respect to Tauri, so tests
/// drive it directly with a fake Trash and a hand-held cancel flag.
fn run_units(
    units: Vec<UnitOp>,
    trash: &dyn TrashBin,
    cancel: &AtomicBool,
    mut on_progress: impl FnMut(usize, usize, &Path),
) -> BatchOutcome {
    let total = units.len();
    let mut ok_count = 0;
    let mut failures = Vec::new();

    for (index, unit) in units.into_iter().enumerate() {
        if cancel.load(Ordering::Acquire) {
            break;
        }
        on_progress(index, total, unit.path());
        match unit.execute(trash) {
            Ok(()) => ok_count += 1,
            Err(cause) => failures.push(Failure {
                path: unit.path().display().to_string(),
                cause,
            }),
        }
    }

    BatchOutcome { ok_count, failures }
}

/// The UTF-8 basename of a path, or a concrete cause when it has none (the filesystem
/// root) or is non-UTF-8 (rare on macOS; that item fails cleanly rather than crashing).
fn file_name_str(path: &Path) -> Result<String, String> {
    path.file_name()
        .and_then(|name| name.to_str())
        .map(str::to_owned)
        .ok_or_else(|| format!("Cannot resolve a name for {}", path.display()))
}

/// Managed Tauri state: the job-id counter and the live cancel flags keyed by job. The map
/// is an `Arc` so a worker thread can deregister itself on finish.
pub struct Operations {
    next_job_id: AtomicU64,
    jobs: Arc<Mutex<HashMap<u64, Arc<AtomicBool>>>>,
}

impl Operations {
    pub fn new() -> Self {
        Self {
            next_job_id: AtomicU64::new(1),
            jobs: Arc::new(Mutex::new(HashMap::new())),
        }
    }

    /// Register a job and spawn its background-QoS worker, returning the `job_id` at once.
    /// The worker streams progress, runs every unit, deregisters, records telemetry, and
    /// emits the finished event.
    fn start_job(&self, app: AppHandle, kind: OpKind, units: Vec<UnitOp>) -> u64 {
        let job_id = self.next_job_id.fetch_add(1, Ordering::Relaxed);
        let cancel = Arc::new(AtomicBool::new(false));
        self.jobs
            .lock()
            .expect("operations jobs lock poisoned")
            .insert(job_id, cancel.clone());
        let jobs = self.jobs.clone();

        thread::spawn(move || {
            qos::set_background_qos();
            let started = Instant::now();
            let progress_app = app.clone();
            let outcome = run_units(units, &SystemTrash, &cancel, |done, total, current| {
                let _ = progress_app.emit(
                    PROGRESS_EVENT,
                    ProgressPayload {
                        job_id,
                        done,
                        total,
                        current_path: current.to_string_lossy().into_owned(),
                    },
                );
            });

            jobs.lock()
                .expect("operations jobs lock poisoned")
                .remove(&job_id);

            let duration_ms = u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX);
            if let Err(error) = app.state::<Telemetry>().record(
                "operation_finished",
                json!({
                    "kind": kind.as_str(),
                    "ok": outcome.ok_count,
                    "failed": outcome.failures.len(),
                    "duration_ms": duration_ms,
                }),
            ) {
                eprintln!("telemetry event operation_finished failed: {error}");
            }

            if let Err(error) = app.emit(
                FINISHED_EVENT,
                FinishedPayload {
                    job_id,
                    ok_count: outcome.ok_count,
                    failures: outcome.failures,
                },
            ) {
                eprintln!("failed to emit operation-finished event: {error}");
            }
        });

        job_id
    }
}

impl Default for Operations {
    fn default() -> Self {
        Self::new()
    }
}

/// Validate that a directory exists and is a directory (following symlinks so a symlinked
/// folder is a valid target).
fn validate_dir(path: &str) -> Result<(), OpError> {
    match fs::metadata(path) {
        Ok(meta) if meta.is_dir() => Ok(()),
        Ok(_) => Err(OpError::TargetNotADirectory {
            path: path.to_owned(),
        }),
        Err(_) => Err(OpError::TargetNotFound {
            path: path.to_owned(),
        }),
    }
}

/// Validate that every source exists (as a link or a real entry) and collect them as
/// paths. A missing source is a pre-dispatch failure — no job starts (SPEC §13).
fn validate_sources(sources: &[String]) -> Result<Vec<PathBuf>, OpError> {
    let mut paths = Vec::with_capacity(sources.len());
    for source in sources {
        if fs::symlink_metadata(source).is_err() {
            return Err(OpError::SourceNotFound {
                path: source.clone(),
            });
        }
        paths.push(PathBuf::from(source));
    }
    Ok(paths)
}

/// Reject an unusable Item/folder name before it touches the filesystem (SPEC §8: rename
/// rejects path separators; New Folder shares the rule).
fn validate_name(name: &str) -> Result<(), OpError> {
    let invalid = |reason: &str| OpError::InvalidName {
        name: name.to_owned(),
        reason: reason.to_owned(),
    };
    if name.is_empty() {
        return Err(invalid("name must not be empty"));
    }
    if name == "." || name == ".." {
        return Err(invalid("name must not be a directory reference"));
    }
    if name.contains('/') {
        return Err(invalid("name must not contain a path separator"));
    }
    if name.contains('\0') {
        return Err(invalid("name must not contain a null byte"));
    }
    Ok(())
}

/// Run a blocking closure off the IPC threads, mapping a join failure to a concrete
/// [`OpError`].
async fn off_thread<T, F>(task: F) -> Result<T, OpError>
where
    T: Send + 'static,
    F: FnOnce() -> Result<T, OpError> + Send + 'static,
{
    tauri::async_runtime::spawn_blocking(task)
        .await
        .map_err(|_| OpError::Io {
            detail: "operation task failed to run".to_owned(),
        })?
}

/// Paste copies of `sources` into `target_dir` (SPEC §8: `Cmd+V`). Same-Location paste
/// duplicates; collisions climb the numeric suffix. Returns the batch `job_id`.
#[tauri::command]
pub async fn paste_copy(
    sources: Vec<String>,
    target_dir: String,
    app: AppHandle,
) -> Result<u64, OpError> {
    let target = target_dir.clone();
    let srcs = off_thread(move || {
        validate_dir(&target)?;
        validate_sources(&sources)
    })
    .await?;

    let target = PathBuf::from(target_dir);
    let units = srcs
        .into_iter()
        .map(|src| UnitOp::Copy {
            src,
            target_dir: target.clone(),
        })
        .collect();
    Ok(app
        .state::<Operations>()
        .start_job(app.clone(), OpKind::PasteCopy, units))
}

/// Move `sources` into `target_dir` (SPEC §8: `Cmd+Opt+V`). Cross-volume moves fall back
/// to copy + delete per item. Returns the batch `job_id`.
#[tauri::command]
pub async fn paste_move(
    sources: Vec<String>,
    target_dir: String,
    app: AppHandle,
) -> Result<u64, OpError> {
    let target = target_dir.clone();
    let srcs = off_thread(move || {
        validate_dir(&target)?;
        validate_sources(&sources)
    })
    .await?;

    let target = PathBuf::from(target_dir);
    let units = srcs
        .into_iter()
        .map(|src| UnitOp::Move {
            src,
            target_dir: target.clone(),
        })
        .collect();
    Ok(app
        .state::<Operations>()
        .start_job(app.clone(), OpKind::PasteMove, units))
}

/// Move `paths` to the system Trash (SPEC §8: `Cmd+Delete`, no confirmation at any layer).
/// Recovery is Finder's Put Back. Rows disappear only after success, so results arrive
/// per item in the finished event. Returns the batch `job_id`.
#[tauri::command]
pub async fn trash_items(paths: Vec<String>, app: AppHandle) -> Result<u64, OpError> {
    let sources = paths.clone();
    let srcs = off_thread(move || validate_sources(&sources)).await?;
    let units = srcs
        .into_iter()
        .map(|path| UnitOp::Trash { path })
        .collect();
    Ok(app
        .state::<Operations>()
        .start_job(app.clone(), OpKind::Trash, units))
}

/// Permanently remove `paths` from disk (SPEC §8: `Opt+Cmd+Delete`). There is no undo and
/// no system Trash recovery.
///
/// IMPORTANT: this layer performs the removal with no confirmation. The always-on,
/// no-"don't ask again" confirmation is the UI's responsibility (chunk B) and must gate
/// every invocation of this command.
#[tauri::command]
pub async fn delete_items_permanently(paths: Vec<String>, app: AppHandle) -> Result<u64, OpError> {
    let sources = paths.clone();
    let srcs = off_thread(move || validate_sources(&sources)).await?;
    let units = srcs
        .into_iter()
        .map(|path| UnitOp::Delete { path })
        .collect();
    Ok(app
        .state::<Operations>()
        .start_job(app.clone(), OpKind::Delete, units))
}

/// Rename an Item in place (SPEC §8). Same-directory only: `new_name` is a bare name with
/// no path separators, and a collision fails rather than auto-suffixing (an explicit
/// rename is a deliberate target). Returns the new absolute path.
#[tauri::command]
pub async fn rename_item(path: String, new_name: String) -> Result<String, OpError> {
    off_thread(move || {
        validate_name(&new_name)?;
        let source = PathBuf::from(&path);
        if fs::symlink_metadata(&source).is_err() {
            return Err(OpError::SourceNotFound { path });
        }
        let parent = source.parent().ok_or_else(|| OpError::InvalidName {
            name: new_name.clone(),
            reason: "cannot rename the filesystem root".to_owned(),
        })?;
        let destination = parent.join(&new_name);
        if fs::symlink_metadata(&destination).is_ok() {
            return Err(OpError::NameCollision {
                path: destination.to_string_lossy().into_owned(),
            });
        }
        fs::rename(&source, &destination).map_err(|error| OpError::Io {
            detail: cause::describe("renaming", &source, &error),
        })?;
        Ok(destination.to_string_lossy().into_owned())
    })
    .await
}

/// Create a directory `name` inside `parent_dir` (SPEC §8: New Folder), applying the same
/// suffix-on-collision rule as paste. Returns the created absolute path.
#[tauri::command]
pub async fn create_folder(parent_dir: String, name: String) -> Result<String, OpError> {
    off_thread(move || {
        validate_name(&name)?;
        validate_dir(&parent_dir)?;
        let destination = fsops::resolve_collision(Path::new(&parent_dir), &name);
        fs::create_dir(&destination).map_err(|error| OpError::Io {
            detail: cause::describe("creating folder", &destination, &error),
        })?;
        Ok(destination.to_string_lossy().into_owned())
    })
    .await
}

/// Reveal a path in Finder (SPEC §8: Reveal in Finder).
///
/// Implemented with `open -R` via `std::process::Command` rather than the opener plugin's
/// `reveal_item_in_dir`: it keeps this module free of an `AppHandle`-threaded plugin call,
/// matches [`open_in_app`]'s `open -b` approach, and passes the path as a separate argument
/// (no shell, no interpolation).
#[tauri::command]
pub async fn reveal_in_finder(path: String) -> Result<(), OpError> {
    off_thread(move || {
        if fs::symlink_metadata(&path).is_err() {
            return Err(OpError::SourceNotFound { path });
        }
        run_open(&["-R", &path])
    })
    .await
}

/// Open a path with a specific application by bundle id (SPEC §8: Open With / configured
/// slots). Slot resolution (Terminal/Editor auto-seeding) is chunk B; this opens exactly
/// the bundle id given, `open -b <bundle_id> <path>`, no shell.
#[tauri::command]
pub async fn open_in_app(path: String, bundle_id: String) -> Result<(), OpError> {
    off_thread(move || {
        if bundle_id.trim().is_empty() {
            return Err(OpError::CommandFailed {
                detail: "bundle id must not be empty".to_owned(),
            });
        }
        if fs::symlink_metadata(&path).is_err() {
            return Err(OpError::SourceNotFound { path });
        }
        run_open(&["-b", &bundle_id, &path])
    })
    .await
}

/// Run `/usr/bin/open` with the given arguments (each a separate arg, never a shell
/// string), mapping a launch failure or non-zero exit to a concrete cause.
fn run_open(args: &[&str]) -> Result<(), OpError> {
    let status =
        Command::new("open")
            .args(args)
            .status()
            .map_err(|error| OpError::CommandFailed {
                detail: format!("could not launch `open`: {error}"),
            })?;
    if status.success() {
        Ok(())
    } else {
        Err(OpError::CommandFailed {
            detail: format!("`open` exited with {status}"),
        })
    }
}

/// Request cancellation of a running batch (SPEC §10: universal cancellation). Best-effort
/// — the worker stops before its next item; an in-flight item still finishes. Unknown or
/// already-finished ids are a no-op.
#[tauri::command]
pub fn cancel_operation(job_id: u64, ops: tauri::State<'_, Operations>) {
    if let Some(flag) = ops
        .jobs
        .lock()
        .expect("operations jobs lock poisoned")
        .get(&job_id)
    {
        flag.store(true, Ordering::Release);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::AtomicU64;

    static TEMP_COUNTER: AtomicU64 = AtomicU64::new(0);

    struct TempDir(PathBuf);

    impl TempDir {
        fn new() -> Self {
            let unique = TEMP_COUNTER.fetch_add(1, Ordering::Relaxed);
            let path =
                std::env::temp_dir().join(format!("beeline_ops_{}_{}", std::process::id(), unique));
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

    /// A Trash that records what it was asked to trash and fails on a chosen basename, so
    /// tests exercise continue-past-failure and cancellation without touching the real
    /// Trash.
    struct FakeTrash {
        trashed: Mutex<Vec<PathBuf>>,
        fail_on: Option<String>,
    }

    impl FakeTrash {
        fn new() -> Self {
            Self {
                trashed: Mutex::new(Vec::new()),
                fail_on: None,
            }
        }

        fn failing_on(name: &str) -> Self {
            Self {
                trashed: Mutex::new(Vec::new()),
                fail_on: Some(name.to_owned()),
            }
        }

        fn trashed(&self) -> Vec<PathBuf> {
            self.trashed.lock().unwrap().clone()
        }
    }

    impl TrashBin for FakeTrash {
        fn trash(&self, path: &Path) -> Result<(), String> {
            let basename = path.file_name().and_then(|n| n.to_str()).unwrap_or("");
            if self.fail_on.as_deref() == Some(basename) {
                return Err(format!("fake trash refused {}", path.display()));
            }
            self.trashed.lock().unwrap().push(path.to_path_buf());
            Ok(())
        }
    }

    fn no_cancel() -> AtomicBool {
        AtomicBool::new(false)
    }

    #[test]
    fn same_dir_duplicate_climbs_the_suffix() {
        let dir = TempDir::new();
        let original = dir.path().join("doc.txt");
        fs::write(&original, b"content").unwrap();

        // Two copies of the same file into its own directory: the first suffixes to
        // "doc 2.txt", the second to "doc 3.txt".
        let units = vec![
            UnitOp::Copy {
                src: original.clone(),
                target_dir: dir.path().to_path_buf(),
            },
            UnitOp::Copy {
                src: original.clone(),
                target_dir: dir.path().to_path_buf(),
            },
        ];
        let cancel = no_cancel();
        let outcome = run_units(units, &FakeTrash::new(), &cancel, |_, _, _| {});

        assert_eq!(outcome.ok_count, 2);
        assert!(outcome.failures.is_empty());
        assert!(dir.path().join("doc 2.txt").exists());
        assert!(dir.path().join("doc 3.txt").exists());
        assert_eq!(fs::read(dir.path().join("doc 2.txt")).unwrap(), b"content");
    }

    #[test]
    fn continues_past_failure_with_concrete_causes() {
        let dir = TempDir::new();
        let present = dir.path().join("here.txt");
        fs::write(&present, b"x").unwrap();
        let missing = dir.path().join("gone.txt"); // never created

        // A delete of a present file (ok) and a vanished file (NotFound → concrete cause).
        let units = vec![
            UnitOp::Delete {
                path: present.clone(),
            },
            UnitOp::Delete {
                path: missing.clone(),
            },
        ];
        let cancel = no_cancel();
        let outcome = run_units(units, &FakeTrash::new(), &cancel, |_, _, _| {});

        assert_eq!(outcome.ok_count, 1);
        assert_eq!(outcome.failures.len(), 1);
        assert_eq!(outcome.failures[0].path, missing.display().to_string());
        assert!(outcome.failures[0].cause.starts_with("No longer exists"));
        // The good item really was deleted despite the sibling failure.
        assert!(!present.exists());
    }

    #[test]
    fn trash_seam_records_and_reports_failures() {
        let dir = TempDir::new();
        let good = dir.path().join("keep.txt");
        let boom = dir.path().join("boom.txt");
        fs::write(&good, b"x").unwrap();
        fs::write(&boom, b"x").unwrap();

        let fake = FakeTrash::failing_on("boom.txt");
        let units = vec![
            UnitOp::Trash { path: good.clone() },
            UnitOp::Trash { path: boom.clone() },
        ];
        let cancel = no_cancel();
        let outcome = run_units(units, &fake, &cancel, |_, _, _| {});

        assert_eq!(outcome.ok_count, 1);
        assert_eq!(outcome.failures.len(), 1);
        assert!(outcome.failures[0].cause.contains("fake trash refused"));
        assert_eq!(fake.trashed(), vec![good]);
    }

    #[test]
    fn cancellation_stops_between_items() {
        let dir = TempDir::new();
        let paths: Vec<PathBuf> = (0..3)
            .map(|i| {
                let p = dir.path().join(format!("f{i}.txt"));
                fs::write(&p, b"x").unwrap();
                p
            })
            .collect();

        let fake = FakeTrash::new();
        let cancel = no_cancel();
        let units = paths
            .iter()
            .cloned()
            .map(|path| UnitOp::Trash { path })
            .collect();
        // Cancel once the first item has been reported as done; the third must never run.
        let outcome = run_units(units, &fake, &cancel, |done, _, _| {
            if done == 1 {
                cancel.store(true, Ordering::Release);
            }
        });

        assert_eq!(outcome.ok_count, 2);
        assert_eq!(fake.trashed(), vec![paths[0].clone(), paths[1].clone()]);
    }

    #[test]
    fn rename_rejects_path_separators() {
        assert!(matches!(
            validate_name("a/b"),
            Err(OpError::InvalidName { .. })
        ));
        assert!(matches!(
            validate_name(""),
            Err(OpError::InvalidName { .. })
        ));
        assert!(matches!(
            validate_name(".."),
            Err(OpError::InvalidName { .. })
        ));
        assert!(validate_name("report final.txt").is_ok());
    }

    #[test]
    fn validate_dir_and_sources() {
        let dir = TempDir::new();
        let file = dir.path().join("a.txt");
        fs::write(&file, b"x").unwrap();

        assert!(validate_dir(&dir.path().to_string_lossy()).is_ok());
        assert!(matches!(
            validate_dir(&file.to_string_lossy()),
            Err(OpError::TargetNotADirectory { .. })
        ));
        assert!(matches!(
            validate_dir(&dir.path().join("nope").to_string_lossy()),
            Err(OpError::TargetNotFound { .. })
        ));

        assert!(validate_sources(&[file.to_string_lossy().into_owned()]).is_ok());
        assert!(matches!(
            validate_sources(&[dir.path().join("ghost").to_string_lossy().into_owned()]),
            Err(OpError::SourceNotFound { .. })
        ));
    }

    // The only test that touches the real system Trash (SPEC §8). Ignored by default so
    // the unit suite never mutates the user's Trash; run with `cargo test -- --ignored`.
    #[test]
    #[ignore]
    fn system_trash_removes_file() {
        let dir = TempDir::new();
        let victim = dir.path().join("trash-me.txt");
        fs::write(&victim, b"bye").unwrap();

        SystemTrash.trash(&victim).expect("trash the file");
        assert!(!victim.exists());
    }
}

//! Persistent macOS FSEvents cursor for bounded startup catch-up.
//!
//! The live `notify` watcher starts first. This one-shot stream then asks CoreServices for
//! paths changed since the previous successful startup and stops at HistoryDone. Dropped or
//! wrapped history is never trusted; the caller falls back to the full directory-mtime diff.

use std::{
    collections::BTreeSet,
    ffi::{c_void, CStr, CString},
    fs::{self, File},
    io::{self, Write},
    path::{Path, PathBuf},
    time::{Duration, Instant},
};

#[cfg(target_os = "macos")]
use fsevent_sys::{self as fs_events, core_foundation as cf};
#[cfg(target_os = "macos")]
use std::os::unix::ffi::OsStrExt;

const CHECKPOINT_FILE: &str = "home.fsevent-id";
const HISTORY_TIMEOUT: Duration = Duration::from_secs(5);

pub struct CatchUp {
    pub paths: Vec<PathBuf>,
    pub checkpoint: u64,
    pub complete: bool,
    pub reason: Option<String>,
}

pub fn load_checkpoint(app_data_dir: &Path) -> Option<u64> {
    fs::read_to_string(checkpoint_path(app_data_dir))
        .ok()?
        .trim()
        .parse()
        .ok()
}

pub fn save_checkpoint(app_data_dir: &Path, checkpoint: u64) -> io::Result<()> {
    let path = checkpoint_path(app_data_dir);
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    let temporary = path.with_extension("tmp");
    let mut output = File::create(&temporary)?;
    writeln!(output, "{checkpoint}")?;
    output.sync_all()?;
    fs::rename(temporary, &path)?;
    if let Some(parent) = path.parent() {
        File::open(parent)?.sync_all()?;
    }
    Ok(())
}

fn checkpoint_path(app_data_dir: &Path) -> PathBuf {
    app_data_dir.join("name_index").join(CHECKPOINT_FILE)
}

#[cfg(target_os = "macos")]
struct CallbackState {
    paths: BTreeSet<PathBuf>,
    complete: bool,
    history_done: bool,
    reason: Option<String>,
}

#[cfg(target_os = "macos")]
extern "C" fn callback(
    _stream: fs_events::FSEventStreamRef,
    info: *mut c_void,
    count: usize,
    event_paths: *mut c_void,
    event_flags: *const fs_events::FSEventStreamEventFlags,
    _event_ids: *const fs_events::FSEventStreamEventId,
) {
    // SAFETY: CoreServices invokes this only while `catch_up` keeps CallbackState and all
    // callback arrays alive. The stream uses C-string paths because UseCFTypes is not set.
    unsafe {
        let state = &mut *(info.cast::<CallbackState>());
        let paths = event_paths.cast::<*const i8>();
        for index in 0..count {
            let flags = *event_flags.add(index);
            let unreliable = fs_events::kFSEventStreamEventFlagMustScanSubDirs
                | fs_events::kFSEventStreamEventFlagUserDropped
                | fs_events::kFSEventStreamEventFlagKernelDropped
                | fs_events::kFSEventStreamEventFlagEventIdsWrapped
                | fs_events::kFSEventStreamEventFlagRootChanged;
            if flags & unreliable != 0 {
                state.complete = false;
                state.reason = Some(format!("unreliable FSEvents flags 0x{flags:x}"));
            }
            if flags & fs_events::kFSEventStreamEventFlagHistoryDone != 0 {
                state.history_done = true;
                continue;
            }
            let raw = *paths.add(index);
            if raw.is_null() {
                continue;
            }
            let path = std::ffi::OsStr::from_bytes(CStr::from_ptr(raw).to_bytes());
            state.paths.insert(PathBuf::from(path));
        }
    }
}

#[cfg(target_os = "macos")]
unsafe extern "C" {
    fn CFRunLoopRunInMode(
        mode: cf::CFStringRef,
        seconds: f64,
        return_after_source_handled: cf::Boolean,
    ) -> i32;
}

#[cfg(target_os = "macos")]
pub fn catch_up(root: &Path, since: u64) -> CatchUp {
    let checkpoint = current_event_id().unwrap_or(0);
    if since > checkpoint {
        return unavailable(
            checkpoint,
            "saved FSEvent ID is newer than the system cursor",
        );
    }
    let mut state = Box::new(CallbackState {
        paths: BTreeSet::new(),
        complete: true,
        history_done: false,
        reason: None,
    });
    let Ok(root_c) = CString::new(root.as_os_str().as_bytes()) else {
        return unavailable(checkpoint, "root contains an interior NUL");
    };

    // SAFETY: every CoreFoundation object is checked for null and released after the stream
    // stops. `state` remains pinned in its Box until the callback can no longer run.
    unsafe {
        let root_string = cf::CFStringCreateWithCString(
            cf::kCFAllocatorDefault,
            root_c.as_ptr(),
            cf::kCFStringEncodingUTF8,
        );
        if root_string.is_null() {
            return unavailable(checkpoint, "cannot create root CFString");
        }
        let roots =
            cf::CFArrayCreateMutable(cf::kCFAllocatorDefault, 1, &cf::kCFTypeArrayCallBacks);
        if roots.is_null() {
            cf::CFRelease(root_string);
            return unavailable(checkpoint, "cannot create root CFArray");
        }
        cf::CFArrayAppendValue(roots, root_string);
        let context = fs_events::FSEventStreamContext {
            version: 0,
            info: (&mut *state as *mut CallbackState).cast(),
            retain: None,
            release: None,
            copy_description: None,
        };
        let stream = fs_events::FSEventStreamCreate(
            cf::kCFAllocatorDefault,
            callback,
            &context,
            roots,
            since,
            0.0,
            fs_events::kFSEventStreamCreateFlagFileEvents
                | fs_events::kFSEventStreamCreateFlagNoDefer
                | fs_events::kFSEventStreamCreateFlagWatchRoot,
        );
        if stream.is_null() {
            cf::CFRelease(roots);
            cf::CFRelease(root_string);
            return unavailable(checkpoint, "cannot create FSEvent history stream");
        }
        let run_loop = cf::CFRunLoopGetCurrent();
        fs_events::FSEventStreamScheduleWithRunLoop(stream, run_loop, cf::kCFRunLoopDefaultMode);
        if fs_events::FSEventStreamStart(stream) == 0 {
            fs_events::FSEventStreamInvalidate(stream);
            fs_events::FSEventStreamRelease(stream);
            cf::CFRelease(roots);
            cf::CFRelease(root_string);
            return unavailable(checkpoint, "cannot start FSEvent history stream");
        }
        let deadline = Instant::now() + HISTORY_TIMEOUT;
        while !state.history_done && Instant::now() < deadline {
            CFRunLoopRunInMode(cf::kCFRunLoopDefaultMode, 0.05, 1);
        }
        fs_events::FSEventStreamStop(stream);
        fs_events::FSEventStreamInvalidate(stream);
        fs_events::FSEventStreamRelease(stream);
        cf::CFRelease(roots);
        cf::CFRelease(root_string);
    }

    if !state.history_done {
        state.complete = false;
        state.reason = Some("FSEvents history timed out".to_owned());
    }
    CatchUp {
        paths: state.paths.into_iter().collect(),
        checkpoint,
        complete: state.complete,
        reason: state.reason,
    }
}

#[cfg(target_os = "macos")]
pub fn current_event_id() -> Option<u64> {
    Some(unsafe { fs_events::FSEventsGetCurrentEventId() })
}

#[cfg(not(target_os = "macos"))]
pub fn catch_up(_root: &Path, _since: u64) -> CatchUp {
    unavailable(0, "FSEvents history is available only on macOS")
}

#[cfg(not(target_os = "macos"))]
pub fn current_event_id() -> Option<u64> {
    None
}

fn unavailable(checkpoint: u64, reason: &str) -> CatchUp {
    CatchUp {
        paths: Vec::new(),
        checkpoint,
        complete: false,
        reason: Some(reason.to_owned()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicU64, Ordering};

    static TEMP_COUNTER: AtomicU64 = AtomicU64::new(0);

    #[test]
    fn checkpoint_is_atomic_and_strictly_numeric() {
        let path = std::env::temp_dir().join(format!(
            "beeline_fsevent_checkpoint_{}_{}",
            std::process::id(),
            TEMP_COUNTER.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir_all(&path).expect("temp");
        assert_eq!(load_checkpoint(&path), None);
        save_checkpoint(&path, 42).expect("save");
        assert_eq!(load_checkpoint(&path), Some(42));
        fs::write(checkpoint_path(&path), "invalid\n").expect("corrupt");
        assert_eq!(load_checkpoint(&path), None);
        let _ = fs::remove_dir_all(path);
    }

    #[cfg(target_os = "macos")]
    #[test]
    #[ignore = "exercises the host FSEvents history database"]
    fn live_history_returns_a_file_created_after_the_cursor() {
        let path = std::env::temp_dir().join(format!(
            "beeline_fsevent_history_{}_{}",
            std::process::id(),
            TEMP_COUNTER.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir_all(&path).expect("temp");
        let path = fs::canonicalize(path).expect("canonical temp");
        let since = current_event_id().expect("macOS cursor");
        let marker = path.join("created-after-cursor.txt");
        fs::write(&marker, "test").expect("marker");
        std::thread::sleep(Duration::from_secs(2));

        let result = catch_up(&path, since);
        assert!(result.complete, "history failed: {:?}", result.reason);
        assert!(
            result.paths.contains(&marker),
            "missing {marker:?} in {:?}",
            result.paths
        );
        let _ = fs::remove_dir_all(path);
    }
}

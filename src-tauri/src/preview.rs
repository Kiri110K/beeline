//! Preview data providers for the panel-less Preview Panel (SPEC §9): the metadata,
//! text-excerpt, and thumbnail commands chunk B binds to render the fixed-width right
//! column that follows the Focused Item. The full-document Quick Look panel is a separate
//! bridge ([`crate::quick_look`]); this module never touches AppKit except through the
//! off-main QuickLook thumbnail generator.
//!
//! All three commands are Async class (SPEC §10): filesystem and thumbnail work runs off
//! the IPC threads, a stale request never blocks or overwrites a newer selection, and raw
//! `io::Error` text never reaches the UI — failures are a tagged, code-only union like
//! [`crate::listing::ListError`].

use std::{
    collections::HashMap,
    fs::{self, File},
    hash::{DefaultHasher, Hash, Hasher},
    io::Read,
    path::{Path, PathBuf},
    sync::{
        atomic::{AtomicU64, Ordering},
        Mutex,
    },
    time::{Instant, UNIX_EPOCH},
};

use serde::Serialize;
use serde_json::json;
use tauri::{AppHandle, Manager, State};

use crate::listing::kind_label;
use crate::telemetry::Telemetry;

/// A tagged, code-only preview failure: raw backend error text never reaches the UI
/// (SPEC §6, §9). Chunk B matches on `code` as a discriminated union.
#[derive(Serialize, Debug, PartialEq, Eq)]
#[serde(tag = "code", rename_all = "kebab-case")]
pub enum PreviewError {
    /// The path does not exist (or vanished before the read).
    NotFound,
    /// A text excerpt was requested for a directory.
    IsADirectory,
    /// The head of the file holds a NUL byte, so it is treated as binary and rejected.
    Binary,
    /// Any other filesystem failure, already stripped of its raw message.
    Io,
}

fn map_not_found(error: &std::io::Error) -> PreviewError {
    match error.kind() {
        std::io::ErrorKind::NotFound => PreviewError::NotFound,
        _ => PreviewError::Io,
    }
}

/// Milliseconds since the Unix epoch for a filesystem timestamp, or `None` when the
/// platform does not record it (matches [`crate::listing`]'s modified-time handling).
fn system_time_ms(time: std::io::Result<std::time::SystemTime>) -> Option<i64> {
    let duration = time.ok()?.duration_since(UNIX_EPOCH).ok()?;
    i64::try_from(duration.as_millis()).ok()
}

/// The head of a text file, UTF-8 lossy, with a flag for whether more bytes followed
/// (SPEC §9: the Preview Panel's text/Markdown excerpt).
#[derive(Serialize, Debug, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct TextExcerpt {
    text: String,
    truncated: bool,
}

/// Read at most `max_bytes` of a file's head as text, rejecting binary content by the NUL
/// heuristic (SPEC §9). Pure over the filesystem so it is unit-tested directly.
fn read_text_excerpt(path: &Path, max_bytes: usize) -> Result<TextExcerpt, PreviewError> {
    let metadata = fs::metadata(path).map_err(|error| map_not_found(&error))?;
    if metadata.is_dir() {
        return Err(PreviewError::IsADirectory);
    }

    // Read one byte past the budget so `truncated` is known without a second stat, then
    // keep only the first `max_bytes` for the excerpt itself.
    let mut file = File::open(path).map_err(|error| map_not_found(&error))?;
    let mut buffer = Vec::new();
    file.by_ref()
        .take(max_bytes as u64 + 1)
        .read_to_end(&mut buffer)
        .map_err(|_| PreviewError::Io)?;

    let truncated = buffer.len() > max_bytes;
    buffer.truncate(max_bytes);

    // NUL anywhere in the head marks the file as binary; text previews are rejected rather
    // than rendered as replacement-character noise.
    if buffer.contains(&0) {
        return Err(PreviewError::Binary);
    }

    Ok(TextExcerpt {
        text: String::from_utf8_lossy(&buffer).into_owned(),
        truncated,
    })
}

/// `preview_text_excerpt`: the head of `path` as lossy UTF-8 text (SPEC §9). Binary content
/// (NUL heuristic) and directories are rejected with a concrete code.
#[tauri::command]
pub async fn preview_text_excerpt(
    path: String,
    max_bytes: usize,
) -> Result<TextExcerpt, PreviewError> {
    tauri::async_runtime::spawn_blocking(move || read_text_excerpt(Path::new(&path), max_bytes))
        .await
        .map_err(|_| PreviewError::Io)?
}

/// Lightweight metadata for the Preview Panel header (SPEC §9): size, timestamps, a kind
/// label reusing [`kind_label`], directory flag, and a non-recursive child count for
/// directories (SPEC §9: no recursive size).
#[derive(Serialize, Debug, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct PreviewMetadata {
    /// File byte length; `None` for a directory (no recursive size, SPEC §9).
    size_bytes: Option<u64>,
    created_ms: Option<i64>,
    modified_ms: Option<i64>,
    kind: String,
    is_directory: bool,
    /// Non-recursive entry count for a directory; `None` for a file.
    child_count: Option<usize>,
}

fn read_metadata(path: &Path) -> Result<PreviewMetadata, PreviewError> {
    // metadata() follows symlinks so a symlinked directory previews as a directory, matching
    // the listing layer.
    let metadata = fs::metadata(path).map_err(|error| map_not_found(&error))?;
    let is_directory = metadata.is_dir();

    let name = path
        .file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_default();

    let child_count = if is_directory {
        // Cheap non-recursive count; an unreadable directory reports no count rather than
        // failing the whole preview.
        fs::read_dir(path)
            .map(|entries| entries.filter(|entry| entry.is_ok()).count())
            .ok()
    } else {
        None
    };

    Ok(PreviewMetadata {
        size_bytes: (!is_directory).then_some(metadata.len()),
        created_ms: system_time_ms(metadata.created()),
        modified_ms: system_time_ms(metadata.modified()),
        kind: kind_label(&name, is_directory),
        is_directory,
        child_count,
    })
}

/// `preview_metadata`: size, timestamps, kind, directory flag, and directory child count
/// (SPEC §9).
#[tauri::command]
pub async fn preview_metadata(path: String) -> Result<PreviewMetadata, PreviewError> {
    tauri::async_runtime::spawn_blocking(move || read_metadata(Path::new(&path)))
        .await
        .map_err(|_| PreviewError::Io)?
}

/// A generated thumbnail, delivered as a temp-file path rather than inline base64.
///
/// Justification (SPEC §9, §10): a QuickLook thumbnail can be hundreds of KB; base64 across
/// the IPC bridge inflates it by a third, forces a full copy through the WKWebView JSON
/// channel, and cannot be cached by the webview. A PNG written to the app cache directory is
/// loaded by chunk B through Tauri's asset protocol (`convertFileSrc`), so the bytes never
/// cross the JSON boundary and the browser caches them by URL.
#[derive(Serialize, Debug, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct Thumbnail {
    png_path: String,
}

/// Per-path staleness bookkeeping for thumbnails (SPEC §9, §10: one in-flight per path,
/// drop stale). Each request claims a monotonic generation for its path; when navigation
/// moves on, a newer request supersedes the older, whose result is dropped rather than
/// returned or allowed to overwrite the newer thumbnail. Separated from the AppKit
/// generator so the bookkeeping is unit-tested headless.
#[derive(Default)]
pub struct ThumbnailTracker {
    next_generation: AtomicU64,
    latest: Mutex<HashMap<PathBuf, u64>>,
}

impl ThumbnailTracker {
    pub fn new() -> Self {
        Self::default()
    }

    /// Claim the newest generation for `path`, superseding any in-flight request for it.
    fn begin(&self, path: &Path) -> u64 {
        let generation = self.next_generation.fetch_add(1, Ordering::Relaxed);
        self.latest
            .lock()
            .expect("thumbnail tracker lock poisoned")
            .insert(path.to_path_buf(), generation);
        generation
    }

    /// Is `generation` still the newest claim for `path` (i.e. not superseded)?
    fn is_latest(&self, path: &Path, generation: u64) -> bool {
        self.latest
            .lock()
            .expect("thumbnail tracker lock poisoned")
            .get(path)
            == Some(&generation)
    }

    /// Drop a finished generation's claim so the map does not grow without bound. Only the
    /// claim still equal to `generation` is removed; a newer request keeps its own claim.
    fn finish(&self, path: &Path, generation: u64) {
        let mut latest = self.latest.lock().expect("thumbnail tracker lock poisoned");
        if latest.get(path) == Some(&generation) {
            latest.remove(path);
        }
    }
}

/// A deterministic cache filename for a path/size pair, so repeated previews of the same
/// Focused Item reuse one temp file instead of littering the cache directory.
fn thumbnail_file_name(path: &Path, max_px: u32) -> String {
    let mut hasher = DefaultHasher::new();
    path.hash(&mut hasher);
    format!("thumb_{:016x}_{}.png", hasher.finish(), max_px)
}

/// `preview_thumbnail`: a QuickLook thumbnail PNG for `path` at up to `max_px` on a side,
/// generated off-main at background QoS (SPEC §9, §10). Returns `Ok(None)` when a newer
/// request for the same path superseded this one, so chunk B ignores the stale reply and
/// never overwrites a fresher thumbnail.
#[tauri::command]
pub async fn preview_thumbnail(
    path: String,
    max_px: u32,
    app: AppHandle,
    tracker: State<'_, ThumbnailTracker>,
) -> Result<Option<Thumbnail>, PreviewError> {
    let target = PathBuf::from(&path);
    if fs::symlink_metadata(&target).is_err() {
        return Err(PreviewError::NotFound);
    }

    let generation = tracker.begin(&target);

    let cache_dir = app
        .path()
        .app_cache_dir()
        .map_err(|_| PreviewError::Io)?
        .join("thumbnails");
    let out_path = cache_dir.join(thumbnail_file_name(&target, max_px));

    let job_path = target.clone();
    let job_out = out_path.clone();
    let started = Instant::now();
    let generated = tauri::async_runtime::spawn_blocking(move || {
        crate::qos::set_background_qos();
        generate_thumbnail_png(&job_path, max_px, &job_out)
    })
    .await
    .map_err(|_| PreviewError::Io)?;

    // Record the claim as finished regardless of outcome, and only honor a result that is
    // still the newest request for this path (SPEC §10: stale results are dropped).
    let is_latest = tracker.is_latest(&target, generation);
    tracker.finish(&target, generation);

    let duration_ms = u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX);
    // Sampled telemetry: only the slow thumbnails (SPEC §10 Async class) are worth a line.
    if duration_ms > 50 {
        if let Err(error) = app
            .state::<Telemetry>()
            .record("preview_thumbnail", json!({ "duration_ms": duration_ms }))
        {
            eprintln!("telemetry event preview_thumbnail failed: {error}");
        }
    }

    if !is_latest {
        return Ok(None);
    }

    generated.map(|png_path| Some(Thumbnail { png_path }))
}

/// Generate a QuickLook thumbnail PNG to `out_path` and return its path string. On macOS
/// this drives `QLThumbnailGenerator`; elsewhere it is unsupported.
// `saveBestRepresentationForRequest:toFileAtURL:withContentType:completionHandler:` is
// marked deprecated in the binding (Apple prefers the UTType-typed overload), but the
// NSString-UTI form is still functional and keeps this off the UTType crate. The
// alternative — generate a representation and encode the PNG ourselves — buys nothing for
// v1. See the open questions in the handoff.
#[cfg(target_os = "macos")]
#[allow(deprecated)]
fn generate_thumbnail_png(
    path: &Path,
    max_px: u32,
    out_path: &Path,
) -> Result<String, PreviewError> {
    use std::sync::mpsc;

    use block2::RcBlock;
    use objc2::AnyThread;
    use objc2_core_foundation::CGSize;
    use objc2_foundation::{NSString, NSURL};
    use objc2_quick_look_thumbnailing::{
        QLThumbnailGenerationRequest, QLThumbnailGenerationRequestRepresentationTypes,
        QLThumbnailGenerator,
    };

    if let Some(parent) = out_path.parent() {
        fs::create_dir_all(parent).map_err(|_| PreviewError::Io)?;
    }

    // NSString/NSURL constructors are safe in objc2 (Foundation value types, usable off the
    // main thread); only the generator calls below carry safety obligations.
    let file_url = NSURL::fileURLWithPath(&NSString::from_str(&path.to_string_lossy()));
    let out_url = NSURL::fileURLWithPath(&NSString::from_str(&out_path.to_string_lossy()));
    // A UTI string is the documented `withContentType:` argument; no UTType crate is needed.
    let content_type = NSString::from_str("public.png");

    let side = f64::from(max_px);
    // A 1.0 scale keeps the longest side at `max_px`; the Preview Panel upsamples for the
    // display scale itself, so a device-scale request here would only cost bytes.
    let request = unsafe {
        QLThumbnailGenerationRequest::initWithFileAtURL_size_scale_representationTypes(
            QLThumbnailGenerationRequest::alloc(),
            &file_url,
            CGSize::new(side, side),
            1.0,
            QLThumbnailGenerationRequestRepresentationTypes::All,
        )
    };

    // SAFETY: the shared generator is a process-wide singleton returned as a borrowed
    // reference; no ownership is transferred.
    let generator = unsafe { QLThumbnailGenerator::sharedGenerator() };

    // The completion handler runs on an arbitrary GCD queue; hand its outcome back to this
    // blocked worker thread over a channel. `None` on the channel means success.
    let (tx, rx) = mpsc::channel::<Option<String>>();
    let handler = RcBlock::new(move |error: *mut objc2_foundation::NSError| {
        // SAFETY: `error` is the block's `NSError * _Nullable` argument; null means success,
        // otherwise it is a valid autoreleased error we only read a description from.
        let failure = if error.is_null() {
            None
        } else {
            Some(unsafe { (*error).localizedDescription() }.to_string())
        };
        let _ = tx.send(failure);
    });

    // SAFETY: every argument is a valid retained object or block reference living until the
    // call returns; the generator copies the block and invokes it once when done.
    unsafe {
        generator.saveBestRepresentationForRequest_toFileAtURL_withContentType_completionHandler(
            &request,
            &out_url,
            &content_type,
            &handler,
        );
    }

    // Block this background-QoS worker until the generator's completion handler fires. The
    // `generator` and `handler` locals stay alive across the wait. A disconnected channel
    // (generator dropped the block without calling) is an I/O failure.
    match rx.recv() {
        Ok(None) => Ok(out_path.to_string_lossy().into_owned()),
        Ok(Some(_message)) => Err(PreviewError::Io),
        Err(_) => Err(PreviewError::Io),
    }
}

#[cfg(not(target_os = "macos"))]
fn generate_thumbnail_png(
    _path: &Path,
    _max_px: u32,
    _out_path: &Path,
) -> Result<String, PreviewError> {
    Err(PreviewError::Io)
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
            let path = std::env::temp_dir().join(format!(
                "beeline_preview_{}_{}",
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

    #[test]
    fn text_excerpt_reports_truncation_at_the_byte_budget() {
        let dir = TempDir::new();
        let file = dir.path().join("notes.txt");
        fs::write(&file, b"hello world").unwrap();

        let short = read_text_excerpt(&file, 5).unwrap();
        assert_eq!(short.text, "hello");
        assert!(short.truncated);

        let exact = read_text_excerpt(&file, 11).unwrap();
        assert_eq!(exact.text, "hello world");
        assert!(!exact.truncated);

        let over = read_text_excerpt(&file, 100).unwrap();
        assert_eq!(over.text, "hello world");
        assert!(!over.truncated);
    }

    #[test]
    fn text_excerpt_rejects_binary_by_nul_heuristic() {
        let dir = TempDir::new();
        let file = dir.path().join("blob.bin");
        fs::write(&file, [0x89, b'P', b'N', b'G', 0x00, 0x1a]).unwrap();

        assert_eq!(read_text_excerpt(&file, 64), Err(PreviewError::Binary));
    }

    #[test]
    fn text_excerpt_rejects_directories_and_missing_paths() {
        let dir = TempDir::new();
        assert_eq!(
            read_text_excerpt(dir.path(), 64),
            Err(PreviewError::IsADirectory)
        );
        assert_eq!(
            read_text_excerpt(&dir.path().join("ghost"), 64),
            Err(PreviewError::NotFound)
        );
    }

    #[test]
    fn text_excerpt_does_not_split_a_multibyte_char_into_garbage() {
        // Budget lands mid-"é" (0xC3 0xA9); lossy decode yields a replacement char, never a
        // panic, and truncation is reported.
        let dir = TempDir::new();
        let file = dir.path().join("accent.txt");
        fs::write(&file, "café".as_bytes()).unwrap();

        let excerpt = read_text_excerpt(&file, 4).unwrap();
        assert!(excerpt.truncated);
        assert!(excerpt.text.starts_with("caf"));
    }

    #[test]
    fn metadata_shape_for_file_and_directory() {
        let dir = TempDir::new();
        let file = dir.path().join("report.pdf");
        fs::write(&file, b"%PDF-1.7\n").unwrap();

        let file_meta = read_metadata(&file).unwrap();
        assert!(!file_meta.is_directory);
        assert_eq!(file_meta.size_bytes, Some(9));
        assert_eq!(file_meta.kind, "PDF");
        assert_eq!(file_meta.child_count, None);
        assert!(file_meta.modified_ms.is_some());

        // Two children so the non-recursive count is exercised.
        fs::write(dir.path().join("a.txt"), b"a").unwrap();
        fs::create_dir(dir.path().join("sub")).unwrap();
        let dir_meta = read_metadata(dir.path()).unwrap();
        assert!(dir_meta.is_directory);
        assert_eq!(dir_meta.size_bytes, None);
        assert_eq!(dir_meta.kind, "Folder");
        assert_eq!(dir_meta.child_count, Some(3)); // report.pdf, a.txt, sub
    }

    #[test]
    fn metadata_reports_missing_path() {
        let dir = TempDir::new();
        assert_eq!(
            read_metadata(&dir.path().join("nope")),
            Err(PreviewError::NotFound)
        );
    }

    #[test]
    fn tracker_supersedes_older_generation_per_path() {
        let tracker = ThumbnailTracker::new();
        let a = PathBuf::from("/tmp/a.png");
        let b = PathBuf::from("/tmp/b.png");

        let first = tracker.begin(&a);
        let second = tracker.begin(&a); // navigation moved on, same path re-requested
        assert!(!tracker.is_latest(&a, first), "older claim is superseded");
        assert!(tracker.is_latest(&a, second), "newest claim wins");

        // A different path is tracked independently.
        let other = tracker.begin(&b);
        assert!(tracker.is_latest(&b, other));
        assert!(tracker.is_latest(&a, second));

        // Finishing the newest claim clears the entry; a stale finish is a no-op.
        tracker.finish(&a, first);
        assert!(tracker.is_latest(&a, second), "stale finish keeps newest");
        tracker.finish(&a, second);
        assert!(!tracker.is_latest(&a, second), "newest finish clears entry");
    }

    #[test]
    fn thumbnail_file_name_is_stable_and_size_specific() {
        let path = Path::new("/Users/kiri/Pictures/photo.heic");
        assert_eq!(
            thumbnail_file_name(path, 256),
            thumbnail_file_name(path, 256)
        );
        assert_ne!(
            thumbnail_file_name(path, 256),
            thumbnail_file_name(path, 512)
        );
        assert!(thumbnail_file_name(path, 256).ends_with("_256.png"));
    }

    // Smoke test for real QuickLook thumbnail generation. It needs a logged-in GUI session,
    // so it is ignored by default; run with `cargo test -- --ignored` on the reference
    // machine. Generates a thumbnail of a temp PNG and asserts a non-empty file appears.
    #[cfg(target_os = "macos")]
    #[test]
    #[ignore]
    fn thumbnail_generation_smoke() {
        // A 2x2 red PNG so QuickLook has a real image to thumbnail.
        const RED_PNG: &[u8] = &[
            0x89, 0x50, 0x4e, 0x47, 0x0d, 0x0a, 0x1a, 0x0a, 0x00, 0x00, 0x00, 0x0d, 0x49, 0x48,
            0x44, 0x52, 0x00, 0x00, 0x00, 0x02, 0x00, 0x00, 0x00, 0x02, 0x08, 0x02, 0x00, 0x00,
            0x00, 0xfd, 0xd4, 0x9a, 0x73, 0x00, 0x00, 0x00, 0x16, 0x49, 0x44, 0x41, 0x54, 0x78,
            0x9c, 0x62, 0xf8, 0xcf, 0xc0, 0xf0, 0x1f, 0x8a, 0x81, 0x81, 0x81, 0x11, 0x00, 0x00,
            0x00, 0x00, 0xff, 0xff, 0x03, 0x00, 0x0a, 0xfb, 0x02, 0xfe, 0xa7, 0x8a, 0x0d, 0x2f,
            0x00, 0x00, 0x00, 0x00, 0x49, 0x45, 0x4e, 0x44, 0xae, 0x42, 0x60, 0x82,
        ];
        let dir = TempDir::new();
        let src = dir.path().join("red.png");
        fs::write(&src, RED_PNG).unwrap();
        let out = dir.path().join("red_thumb.png");

        let result = generate_thumbnail_png(&src, 64, &out).expect("generate thumbnail");
        assert_eq!(result, out.to_string_lossy());
        assert!(out.exists());
        assert!(fs::metadata(&out).unwrap().len() > 0);
    }
}

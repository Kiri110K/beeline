//! The native Quick Look bridge (SPEC §9, ADR-0001): an owned, shared `QLPreviewPanel`
//! driven from the main thread. Space toggles the full-document panel over the Focused
//! Item (or the selected files under multi-select); Up/Down move through the list while it
//! is open. Production must own a real `QLPreviewPanel` — the `qlmanage` demo workaround is
//! not acceptable (ADR-0001).
//!
//! Threading (SPEC §10): AppKit and the panel are main-thread only, so every command hops
//! to the main thread with `AppHandle::run_on_main_thread` and returns immediately — the
//! dispatch itself is the ≤50 ms Quick Look budget, never the panel work. `quick_look_is_open`
//! is answered from a `Send` atomic mirror so it needs no main-thread hop at all.
//!
//! Close truthfulness (the ticket's contract): the panel handles its own Escape and steals
//! key focus while open, so the frontend never sees that Escape. When the panel closes by
//! the user we observe `NSWindowWillCloseNotification`, flip the mirror, and emit
//! `beeline://quick-look-closed` so the frontend's Escape-order state (SPEC §5) stays honest.
//! A programmatic hide flips the mirror itself first, so the observer never double-reports it.

use std::sync::{
    atomic::{AtomicBool, Ordering},
    Arc,
};

use serde_json::json;
use tauri::{AppHandle, Manager, State};

use crate::telemetry::Telemetry;

pub const QUICK_LOOK_CLOSED_EVENT: &str = "beeline://quick-look-closed";

/// The `Send` half of the bridge, held in Tauri managed state: a mirror of whether the panel
/// is currently visible. The main-thread controller and the close observer keep it in sync so
/// `quick_look_is_open` is an instant atomic read (SPEC §10), never a main-thread round trip.
pub struct QuickLook {
    open: Arc<AtomicBool>,
}

impl QuickLook {
    pub fn new() -> Self {
        Self {
            open: Arc::new(AtomicBool::new(false)),
        }
    }
}

impl Default for QuickLook {
    fn default() -> Self {
        Self::new()
    }
}

fn record(app: &AppHandle, event: &str, fields: serde_json::Value) {
    if let Err(error) = app.state::<Telemetry>().record(event, fields) {
        eprintln!("telemetry event {event} failed: {error}");
    }
}

/// `quick_look_is_open`: whether the panel is currently showing (SPEC §5, §9). An instant
/// atomic read of the mirror the main-thread code maintains — no AppKit call.
#[tauri::command]
pub fn quick_look_is_open(state: State<'_, QuickLook>) -> bool {
    state.open.load(Ordering::SeqCst)
}

/// `quick_look_show`: open the panel on `paths` focused at `index` (SPEC §9: Space toggles
/// Quick Look). Records `quick_look_shown {count}` and dispatches the AppKit work to the main
/// thread, returning at once so the command meets the ≤50 ms dispatch budget (SPEC §10).
#[tauri::command]
pub fn quick_look_show(
    paths: Vec<String>,
    index: usize,
    app: AppHandle,
    state: State<'_, QuickLook>,
) -> Result<(), String> {
    record(&app, "quick_look_shown", json!({ "count": paths.len() }));
    // Mirror the intent before the async hop so an immediately-following is_open reads true;
    // the observer flips it back the moment the user closes the panel.
    state.open.store(true, Ordering::SeqCst);

    let handle = app.clone();
    let dispatched = dispatch_to_main(&app, move |mtm| imp::present(mtm, &handle, paths, index));
    if dispatched.is_err() {
        // The panel work never reached the main thread, so the mirror must not claim open.
        state.open.store(false, Ordering::SeqCst);
    }
    dispatched
}

/// `quick_look_update`: replace the panel's list/position while it is open (SPEC §9: Up/Down
/// move through the files without reopening). A no-op if the panel is not open.
#[tauri::command]
pub fn quick_look_update(
    paths: Vec<String>,
    index: usize,
    app: AppHandle,
    state: State<'_, QuickLook>,
) -> Result<(), String> {
    if !state.open.load(Ordering::SeqCst) {
        return Ok(());
    }
    dispatch_to_main(&app, move |mtm| imp::update(mtm, paths, index))
}

/// `quick_look_hide`: close the panel programmatically (SPEC §5: the frontend's Escape order
/// may hide it). Flips the mirror and records `quick_look_hidden` up front so this path owns
/// its own bookkeeping; the observer, seeing the mirror already down, stays silent and never
/// double-emits a `beeline://quick-look-closed` the frontend did not need.
#[tauri::command]
pub fn quick_look_hide(app: AppHandle, state: State<'_, QuickLook>) -> Result<(), String> {
    let was_open = state.open.swap(false, Ordering::SeqCst);
    if was_open {
        record(&app, "quick_look_hidden", json!({}));
    }
    dispatch_to_main(&app, imp::hide)
}

/// Run `body` on the main thread with a [`MainThreadMarker`], mapping Tauri's dispatch error
/// to a string.
#[cfg(target_os = "macos")]
fn dispatch_to_main<F>(app: &AppHandle, body: F) -> Result<(), String>
where
    F: FnOnce(objc2::MainThreadMarker) + Send + 'static,
{
    app.run_on_main_thread(move || {
        // SAFETY: `run_on_main_thread` guarantees this closure executes on the main thread,
        // so a marker asserting that invariant is sound.
        let mtm = unsafe { objc2::MainThreadMarker::new_unchecked() };
        body(mtm);
    })
    .map_err(|error| format!("failed to dispatch Quick Look work to the main thread: {error}"))
}

#[cfg(target_os = "macos")]
mod imp {
    //! The main-thread AppKit implementation: the owned data-source object, the shared
    //! panel wiring, and the close observer. Everything here runs on the main thread, so the
    //! non-`Send` `Retained` controller lives in a `thread_local` rather than managed state.

    use std::cell::RefCell;
    use std::sync::atomic::Ordering;
    use std::sync::Arc;

    use objc2::rc::Retained;
    use objc2::runtime::{AnyObject, ProtocolObject};
    use objc2::{define_class, msg_send, sel, DefinedClass, MainThreadMarker, MainThreadOnly};
    use objc2_app_kit::NSWindowWillCloseNotification;
    use objc2_foundation::{
        NSNotification, NSNotificationCenter, NSObject, NSObjectProtocol, NSString, NSURL,
    };
    use objc2_quick_look_ui::{QLPreviewItem, QLPreviewPanel, QLPreviewPanelDataSource};
    use tauri::{AppHandle, Emitter, Manager};

    use super::{record, QuickLook, QUICK_LOOK_CLOSED_EVENT};

    thread_local! {
        /// The one data-source/observer object, created on first show and reused thereafter.
        /// The shared panel does not retain its data source, so we own it here for the life
        /// of the process (main thread only, hence a `thread_local`, not managed state).
        static CONTROLLER: RefCell<Option<Retained<QuickLookController>>> =
            const { RefCell::new(None) };
    }

    /// Instance data for [`QuickLookController`]: the current file list (interior-mutable so
    /// Up/Down replace it in place), an `AppHandle` for emit/telemetry, and the shared-state
    /// `open` mirror the observer flips on a user close.
    struct Ivars {
        urls: RefCell<Vec<Retained<NSURL>>>,
        app: AppHandle,
        open: Arc<std::sync::atomic::AtomicBool>,
    }

    define_class!(
        // SAFETY: `NSObject` is the declared superclass and the type is main-thread only,
        // matching how the shared panel and its data source are always used.
        #[unsafe(super(NSObject))]
        #[thread_kind = MainThreadOnly]
        #[name = "BeelineQuickLookController"]
        #[ivars = Ivars]
        struct QuickLookController;

        unsafe impl NSObjectProtocol for QuickLookController {}

        // SAFETY: the two methods below are the `QLPreviewPanelDataSource` protocol
        // selectors with their documented signatures; they only read the stored URL list.
        unsafe impl QLPreviewPanelDataSource for QuickLookController {
            #[unsafe(method(numberOfPreviewItemsInPreviewPanel:))]
            fn number_of_items(&self, _panel: &QLPreviewPanel) -> isize {
                self.ivars().urls.borrow().len() as isize
            }

            #[unsafe(method_id(previewPanel:previewItemAtIndex:))]
            fn item_at(
                &self,
                _panel: &QLPreviewPanel,
                index: isize,
            ) -> Retained<ProtocolObject<dyn QLPreviewItem>> {
                let urls = self.ivars().urls.borrow();
                // The panel only asks for indices it was told exist; clamp defensively so a
                // race can never panic across the FFI boundary (that would be UB).
                let clamped = index.clamp(0, urls.len().saturating_sub(1) as isize) as usize;
                let url = urls
                    .get(clamped)
                    .cloned()
                    .unwrap_or_else(|| NSURL::fileURLWithPath(&NSString::from_str("/")));
                ProtocolObject::from_retained(url)
            }
        }

        // SAFETY: the close hook is a plain notification selector taking the posted
        // `NSNotification`; it only reads ivars and emits a Tauri event.
        impl QuickLookController {
            #[unsafe(method(beelineQuickLookWillClose:))]
            fn will_close(&self, _note: &NSNotification) {
                // Only a still-open panel means a user-initiated close; a programmatic hide
                // has already dropped the mirror and recorded its own telemetry.
                if self.ivars().open.swap(false, Ordering::SeqCst) {
                    let app = &self.ivars().app;
                    record(app, "quick_look_hidden", serde_json::json!({}));
                    if let Err(error) = app.emit(QUICK_LOOK_CLOSED_EVENT, ()) {
                        eprintln!("failed to emit quick-look-closed event: {error}");
                    }
                }
            }
        }
    );

    impl QuickLookController {
        /// Create the controller and register it for the shared panel's close notification.
        fn new(
            mtm: MainThreadMarker,
            app: AppHandle,
            open: Arc<std::sync::atomic::AtomicBool>,
        ) -> Retained<Self> {
            let this = Self::alloc(mtm).set_ivars(Ivars {
                urls: RefCell::new(Vec::new()),
                app,
                open,
            });
            // SAFETY: standard `init` on a freshly allocated instance with its ivars set.
            let this: Retained<Self> = unsafe { msg_send![super(this), init] };

            // Observe close on the shared panel specifically (a process-wide singleton), so a
            // user Escape — which the panel handles itself — still reaches the frontend.
            let panel = shared_panel(mtm);
            let panel_object: &AnyObject = &panel;
            let center = NSNotificationCenter::defaultCenter();
            // SAFETY: standard notification registration; `this` outlives the observation
            // (owned for the process life), the selector exists on it, and the name/object
            // are valid borrows.
            unsafe {
                center.addObserver_selector_name_object(
                    &this,
                    sel!(beelineQuickLookWillClose:),
                    Some(NSWindowWillCloseNotification),
                    Some(panel_object),
                );
            }
            this
        }

        fn set_urls(&self, urls: Vec<Retained<NSURL>>) {
            *self.ivars().urls.borrow_mut() = urls;
        }
    }

    /// The process-wide shared `QLPreviewPanel` (standard front-panel behavior, no custom
    /// window — SPEC §9, the ticket's ownership rule).
    fn shared_panel(mtm: MainThreadMarker) -> Retained<QLPreviewPanel> {
        // SAFETY: `sharedPreviewPanel` returns the process-wide singleton, main-thread-gated
        // by the marker; it is documented never to be nil, so a missing panel is a hard bug.
        unsafe { QLPreviewPanel::sharedPreviewPanel(mtm) }
            .expect("shared QLPreviewPanel is unavailable")
    }

    /// Build `NSURL`s for `paths` on the main thread (their `Retained` handles are not
    /// `Send`, so they are constructed inside the hop).
    fn urls_from_paths(paths: &[String]) -> Vec<Retained<NSURL>> {
        paths
            .iter()
            .map(|path| NSURL::fileURLWithPath(&NSString::from_str(path)))
            .collect()
    }

    /// The controller, created on first use and reused thereafter.
    fn controller(
        mtm: MainThreadMarker,
        app: &AppHandle,
        open: &Arc<std::sync::atomic::AtomicBool>,
    ) -> Retained<QuickLookController> {
        CONTROLLER.with(|slot| {
            let mut slot = slot.borrow_mut();
            if slot.is_none() {
                *slot = Some(QuickLookController::new(mtm, app.clone(), open.clone()));
            }
            slot.as_ref().expect("controller just created").clone()
        })
    }

    fn clamp_index(index: usize, len: usize) -> isize {
        if len == 0 {
            0
        } else {
            index.min(len - 1) as isize
        }
    }

    /// Open (or re-point) the shared panel on `paths` at `index`.
    pub(super) fn present(
        mtm: MainThreadMarker,
        app: &AppHandle,
        paths: Vec<String>,
        index: usize,
    ) {
        let open = app.state::<QuickLook>().open.clone();
        let controller = controller(mtm, app, &open);
        let urls = urls_from_paths(&paths);
        let target = clamp_index(index, urls.len());
        controller.set_urls(urls);

        let panel = shared_panel(mtm);
        let data_source = ProtocolObject::from_ref(&*controller);
        // SAFETY: wiring the shared panel to our data source and showing it — the standard
        // Quick Look presentation. `reloadData` before `setCurrentPreviewItemIndex` so the
        // panel knows the new count before we position it.
        unsafe {
            panel.setDataSource(Some(data_source));
            panel.reloadData();
            panel.setCurrentPreviewItemIndex(target);
            panel.makeKeyAndOrderFront(None);
        }
    }

    /// Replace the open panel's list/position (Up/Down while open).
    pub(super) fn update(mtm: MainThreadMarker, paths: Vec<String>, index: usize) {
        let Some(controller) = CONTROLLER.with(|slot| slot.borrow().clone()) else {
            return;
        };
        let urls = urls_from_paths(&paths);
        let target = clamp_index(index, urls.len());
        controller.set_urls(urls);

        let panel = shared_panel(mtm);
        // SAFETY: refresh the already-wired panel in place.
        unsafe {
            panel.reloadData();
            panel.setCurrentPreviewItemIndex(target);
        }
    }

    /// Close the panel programmatically. The command already dropped the mirror and recorded
    /// telemetry, so the observer stays silent; this only orders the shared panel out.
    pub(super) fn hide(mtm: MainThreadMarker) {
        // Ordering the shared panel out is the documented programmatic close (a safe method
        // in objc2).
        shared_panel(mtm).orderOut(None);
    }
}

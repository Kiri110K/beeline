//! Energy-aware lazy refresh for dirty Junk subtrees (SPEC §6, §10).
//!
//! Filesystem bursts call [`JunkRefresh::after_fs_burst`]. External power allows the
//! background-QoS refresh; battery or an unknown source leaves the dirty set queued. The
//! IOKit power monitor calls [`JunkRefresh::power_source_changed`], so reconnecting power
//! drains that queue without a timer. A Search Query that targets Junk always refreshes,
//! regardless of power source.

use std::{
    path::PathBuf,
    sync::{
        atomic::{AtomicBool, Ordering},
        mpsc::{self, SyncSender},
        Arc, RwLock,
    },
    thread,
    time::{Duration, Instant},
};

use serde_json::json;
use tauri::{AppHandle, Manager};

use crate::{power::PowerSource, telemetry::Telemetry};

use super::{crawl, model::IndexData, SharedJunk};

/// A Junk-heavy build often emits several FSEvents bursts separated by a few hundred
/// milliseconds. Wait for a real quiet window before recrawling, without delaying the
/// immediate non-Junk event path in the watcher.
const FILESYSTEM_QUIET: Duration = Duration::from_secs(5);
/// Continuous Junk traffic (for example an active build plus `.codex` logs) must not keep
/// an external-power queue dirty forever. This remains a one-shot event deadline, not a
/// periodic timer: when no Junk event is pending, the worker blocks indefinitely.
const FILESYSTEM_MAX_DELAY: Duration = Duration::from_secs(30);

#[derive(Clone)]
pub struct JunkRefresh {
    data: Arc<RwLock<IndexData>>,
    root: PathBuf,
    junk: SharedJunk,
    app: Option<AppHandle>,
    deferred_recorded: Arc<AtomicBool>,
    scheduled_recorded: Arc<AtomicBool>,
    idle_tx: Option<SyncSender<()>>,
}

impl JunkRefresh {
    pub fn new(
        data: Arc<RwLock<IndexData>>,
        root: PathBuf,
        junk: SharedJunk,
        app: Option<AppHandle>,
    ) -> Self {
        let mut refresh = Self {
            data,
            root,
            junk,
            app,
            deferred_recorded: Arc::new(AtomicBool::new(false)),
            scheduled_recorded: Arc::new(AtomicBool::new(false)),
            idle_tx: None,
        };
        if refresh.app.is_some() {
            let (tx, rx) = mpsc::sync_channel(1);
            refresh.idle_tx = Some(tx);
            let worker = refresh.clone();
            thread::spawn(move || {
                crate::qos::set_background_qos();
                while rx.recv().is_ok() {
                    // Each new burst resets the one-shot quiet window. This is event-driven:
                    // once no burst is pending the worker blocks indefinitely on `recv`.
                    let deadline = Instant::now() + FILESYSTEM_MAX_DELAY;
                    loop {
                        let remaining = deadline.saturating_duration_since(Instant::now());
                        if remaining.is_zero() {
                            break;
                        }
                        match rx.recv_timeout(FILESYSTEM_QUIET.min(remaining)) {
                            Ok(()) => {}
                            Err(mpsc::RecvTimeoutError::Timeout) => break,
                            Err(mpsc::RecvTimeoutError::Disconnected) => return,
                        }
                    }
                    let reason = if Instant::now() >= deadline {
                        "filesystem_deadline"
                    } else {
                        "filesystem_quiet"
                    };
                    worker.after_fs_burst_for_reason(crate::power::current_source(), reason);
                }
            });
        }
        refresh
    }

    fn dirty_dirs(&self) -> usize {
        self.data
            .read()
            .expect("name index lock poisoned")
            .junk_dirty
            .len()
    }

    fn record(&self, event: &str, fields: serde_json::Value) {
        let Some(app) = &self.app else {
            return;
        };
        if let Err(error) = app.state::<Telemetry>().record(event, fields) {
            eprintln!("telemetry event {event} failed: {error}");
        }
    }

    fn drain(&self, reason: &str) {
        let junk = self.junk.read().expect("junk lock poisoned").clone();
        let outcome = crawl::drain_junk_dirty(&self.data, &self.root, &junk);
        self.scheduled_recorded.store(false, Ordering::Release);
        if outcome.dirty_dirs == 0 {
            return;
        }
        self.deferred_recorded.store(false, Ordering::Release);
        self.record(
            "junk_rescan_finished",
            json!({
                "reason": reason,
                "dirty_dirs": outcome.dirty_dirs,
                "duration_ms": outcome.duration_ms,
            }),
        );
    }

    pub fn after_fs_burst(&self) {
        let dirty_dirs = self.dirty_dirs();
        if dirty_dirs == 0 {
            // This exposes a missed-parent/index inconsistency without scheduling an empty
            // five-second wakeup. It is emitted only for a path classified as Junk by the
            // watcher, so ordinary filesystem traffic cannot create noise here.
            self.record("junk_event_unresolved", json!({}));
            return;
        }
        if !self.scheduled_recorded.swap(true, Ordering::AcqRel) {
            self.record("junk_rescan_scheduled", json!({ "dirty_dirs": dirty_dirs }));
        }
        if let Some(tx) = &self.idle_tx {
            // Capacity one deliberately coalesces bursts while the quiet-window worker is
            // already awake. A full queue means the pending refresh is already scheduled.
            let _ = tx.try_send(());
        } else {
            self.after_fs_burst_for(crate::power::current_source());
        }
    }

    pub(crate) fn after_fs_burst_for(&self, source: PowerSource) {
        self.after_fs_burst_for_reason(source, "filesystem_quiet");
    }

    fn after_fs_burst_for_reason(&self, source: PowerSource, reason: &str) {
        let dirty_dirs = self.dirty_dirs();
        if dirty_dirs == 0 {
            return;
        }
        if source == PowerSource::External {
            self.drain(reason);
            return;
        }
        if !self.deferred_recorded.swap(true, Ordering::AcqRel) {
            self.record(
                "junk_rescan_deferred",
                json!({ "source": source.as_str(), "dirty_dirs": dirty_dirs }),
            );
        }
    }

    pub fn power_source_changed(&self, source: PowerSource) {
        self.record(
            "power_source_changed",
            json!({ "source": source.as_str(), "junk_dirty": self.dirty_dirs() }),
        );
        if source == PowerSource::External {
            self.drain("external_power");
        }
    }

    pub fn targeting_query(&self) {
        self.drain("targeting_query");
    }
}

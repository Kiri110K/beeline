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
    time::Duration,
};

use serde_json::json;
use tauri::{AppHandle, Manager};

use crate::{power::PowerSource, telemetry::Telemetry};

use super::{crawl, model::IndexData, SharedJunk};

/// A Junk-heavy build often emits several FSEvents bursts separated by a few hundred
/// milliseconds. Wait for a real quiet window before recrawling, without delaying the
/// immediate non-Junk event path in the watcher.
const FILESYSTEM_QUIET: Duration = Duration::from_secs(5);

fn wait_until_quiet(rx: &mpsc::Receiver<()>, quiet: Duration) -> bool {
    loop {
        match rx.recv_timeout(quiet) {
            Ok(()) => {}
            Err(mpsc::RecvTimeoutError::Timeout) => return true,
            Err(mpsc::RecvTimeoutError::Disconnected) => return false,
        }
    }
}

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
                    // Each new burst restarts the quiet window. Continuous cache/build/log
                    // traffic leaves Junk dirty and searchable through its existing snapshot;
                    // it must not force repeated multi-minute recrawls in a hidden app.
                    if !wait_until_quiet(&rx, FILESYSTEM_QUIET) {
                        return;
                    }
                    worker.after_fs_burst_for_reason(
                        crate::power::current_source(),
                        "filesystem_quiet",
                    );
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

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Instant;

    #[test]
    fn each_junk_burst_restarts_the_quiet_window() {
        let (tx, rx) = mpsc::channel();
        let (release_tx, release_rx) = mpsc::channel();
        let sender = thread::spawn(move || {
            for _ in 0..4 {
                thread::sleep(Duration::from_millis(5));
                tx.send(()).unwrap();
            }
            release_rx.recv().unwrap();
        });
        let started = Instant::now();

        assert!(wait_until_quiet(&rx, Duration::from_millis(20)));
        release_tx.send(()).unwrap();
        sender.join().unwrap();
        assert!(started.elapsed() >= Duration::from_millis(35));
    }

    #[test]
    fn disconnected_worker_does_not_report_a_quiet_window() {
        let (tx, rx) = mpsc::channel();
        drop(tx);
        assert!(!wait_until_quiet(&rx, Duration::from_millis(1)));
    }
}

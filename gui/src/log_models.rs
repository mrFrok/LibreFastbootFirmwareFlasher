use slint::{Model, ModelRc, VecModel};
use std::rc::Rc;

use crate::{FlashHistoryItem, LogEntry, LogLevel, MainWindow, lvl, ts};

pub struct LogModels {
    pub device: Rc<VecModel<LogEntry>>,
    pub download: Rc<VecModel<LogEntry>>,
    pub flash: Rc<VecModel<LogEntry>>,
    pub partition: Rc<VecModel<LogEntry>>,
    pub history: Rc<VecModel<FlashHistoryItem>>,
}

impl LogModels {
    pub fn new() -> Self {
        Self {
            device: Rc::new(VecModel::<LogEntry>::default()),
            download: Rc::new(VecModel::<LogEntry>::default()),
            flash: Rc::new(VecModel::<LogEntry>::default()),
            partition: Rc::new(VecModel::<LogEntry>::default()),
            history: Rc::new(VecModel::<FlashHistoryItem>::default()),
        }
    }

    pub fn attach(&self, ui: &MainWindow) {
        ui.set_device_log(ModelRc::from(Rc::clone(&self.device)));
        ui.set_download_log(ModelRc::from(Rc::clone(&self.download)));
        ui.set_flash_log(ModelRc::from(Rc::clone(&self.flash)));
        ui.set_partition_log(ModelRc::from(Rc::clone(&self.partition)));
        ui.set_history_items(ModelRc::from(Rc::clone(&self.history)));
    }

    /// Reload flash history from disk into the history model (newest first).
    pub fn refresh_history(&self) {
        let history = lfff_lib::flash_history::load_history();
        let items: Vec<FlashHistoryItem> = history
            .iter()
            .rev()
            .map(|entry| {
                let result = if entry.aborted {
                    "Aborted"
                } else if entry.failed > 0 {
                    "Failed"
                } else {
                    "OK"
                };
                // Integer math — rounding minutes and seconds independently
                // produced labels like "2m 60s".
                let secs = entry.duration_s.round() as u64;
                let duration = if secs > 0 {
                    format!("{}m {:02}s", secs / 60, secs % 60)
                } else {
                    String::new()
                };
                FlashHistoryItem {
                    timestamp: entry.timestamp.clone().into(),
                    firmware: entry.firmware_name.clone().into(),
                    device: entry.device_product.clone().into(),
                    result: result.into(),
                    duration: duration.into(),
                }
            })
            .collect();
        self.history.set_vec(items);
    }

    pub fn by_tab(&self, tab: u8) -> &VecModel<LogEntry> {
        match tab {
            0 => &self.device,
            1 => &self.download,
            2 => &self.flash,
            3 => &self.partition,
            _ => &self.device,
        }
    }
}

thread_local! {
    /// This run's log on disk, opened with the first line; `Some(None)` once
    /// it could not be.
    static SESSION_LOG: std::cell::RefCell<Option<Option<lfff_lib::session_log::SessionLog>>> =
        const { std::cell::RefCell::new(None) };
}

/// Every log line of every tab, also kept on disk (one file per run of the
/// app, in [`lfff_lib::session_log::log_dir`]).
fn save_line(l: &LogLevel, m: &str) {
    SESSION_LOG.with(|c| {
        let mut c = c.borrow_mut();
        let log = c.get_or_insert_with(|| {
            lfff_lib::session_log::SessionLog::create("gui", env!("CARGO_PKG_VERSION"))
        });
        if let Some(log) = log {
            log.line(&format!("[{}] {:<7} {m}", ts(), lvl(l)));
        }
    });
}

pub fn add_log_m(model: &VecModel<LogEntry>, _ui: &MainWindow, l: &LogLevel, m: &str) {
    save_line(l, m);
    model.push(LogEntry {
        timestamp: ts().into(),
        level: lvl(l).into(),
        message: m.into(),
    });
    while model.row_count() > 500 {
        model.remove(0);
    }
}

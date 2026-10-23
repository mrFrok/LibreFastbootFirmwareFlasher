//! The install page's search for official firmware (lfff-ota), on threads of
//! its own: it only talks to the update servers, so the worker stays free.

use std::cell::RefCell;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc;

use lfff_ota::{Found, Region, Search};
use slint::{ComponentHandle, ModelRc, VecModel};

use crate::device_poll::DeviceSnapshot;
use crate::ui_callbacks::tr;
use crate::{Cmd, MainWindow, OtaBuild, WMsg};

#[derive(Default)]
struct State {
    found: Vec<Found>,
    /// Bumped by every search: answers of an older one are dropped.
    generation: u64,
    cancel: Option<Arc<AtomicBool>>,
    /// What the connected phone runs, to mark it in the list.
    phone_ota: String,
    phone_region: Option<Region>,
}

thread_local! {
    static STATE: RefCell<State> = RefCell::default();
}

pub fn register(ui: &MainWindow, mtx: &mpsc::Sender<WMsg>, ctx: &mpsc::Sender<Cmd>) {
    let tx = mtx.clone();
    let w = ui.as_weak();
    ui.on_find_ota(move |model| {
        let Some(ui) = w.upgrade() else { return };
        let model = model.trim().to_uppercase();
        if model.is_empty() {
            return;
        }
        let cancel = Arc::new(AtomicBool::new(false));
        let generation = STATE.with(|s| {
            let mut s = s.borrow_mut();
            if let Some(c) = s.cancel.replace(cancel.clone()) {
                c.store(true, Ordering::SeqCst);
            }
            s.found.clear();
            s.generation += 1;
            s.generation
        });
        ui.set_ota_model(model.clone().into());
        ui.set_ota_done(0);
        ui.set_ota_total(0);
        ui.set_ota_builds(ModelRc::default());
        ui.set_ota_state(1);
        let tx = tx.clone();
        std::thread::spawn(move || {
            let progress = |done, total| {
                tx.send(WMsg::OtaProgress {
                    generation,
                    done,
                    total,
                })
                .ok();
            };
            let result = lfff_ota::search(&Search::new(&model), &cancel, &progress)
                .map_err(|e| format!("{e:#}"));
            if !cancel.load(Ordering::SeqCst) {
                tx.send(WMsg::OtaFound { generation, result }).ok();
            }
        });
    });

    let w = ui.as_weak();
    ui.on_cancel_ota(move || {
        STATE.with(|s| {
            let mut s = s.borrow_mut();
            if let Some(c) = s.cancel.take() {
                c.store(true, Ordering::SeqCst);
            }
            s.generation += 1;
        });
        if let Some(ui) = w.upgrade() {
            ui.set_ota_state(0);
        }
    });

    let t = ctx.clone();
    let w = ui.as_weak();
    ui.on_download_ota(move |i| {
        let build = STATE.with(|s| {
            let s = s.borrow();
            usize::try_from(i)
                .ok()
                .and_then(|i| s.found.get(i))
                .map(|f| f.build.clone())
        });
        let (Some(build), Some(ui)) = (build, w.upgrade()) else {
            return;
        };
        if let Some(path) = build.size.and_then(downloaded_before) {
            crate::ui_callbacks::prepare(&ui, &t, &path.display().to_string());
        } else if let Some(link) = build.link() {
            ui.set_download_url(link.into());
            t.send(Cmd::Download { url: link.into() }).ok();
        }
    });

    let tx = mtx.clone();
    ui.on_load_ota_changes(move |i| {
        let (generation, url) = STATE.with(|s| {
            let s = s.borrow();
            let url = usize::try_from(i)
                .ok()
                .and_then(|i| s.found.get(i))
                .and_then(|f| f.build.changelog_url.clone());
            (s.generation, url)
        });
        let tx = tx.clone();
        std::thread::spawn(move || {
            let text = match url {
                Some(url) => lfff_ota::changelog::fetch(&url).unwrap_or_default(),
                None => String::new(),
            };
            tx.send(WMsg::OtaChanges {
                generation,
                index: i,
                text,
            })
            .ok();
        });
    });
}

/// A finished download of this exact size in the download folder: the same
/// package, downloaded before (the CDN names it the same, aria2 then adds
/// ".1"). A file aria2 is still filling has its ".aria2" beside it.
fn downloaded_before(size: u64) -> Option<std::path::PathBuf> {
    std::fs::read_dir(crate::config::get_output_dir())
        .ok()?
        .flatten()
        .map(|e| e.path())
        .find(|p| {
            p.extension().is_some_and(|x| x.eq_ignore_ascii_case("zip"))
                && std::fs::metadata(p).is_ok_and(|m| m.is_file() && m.len() == size)
                && !p.with_extension("zip.aria2").exists()
        })
}

/// A phone was found: remember what it runs, and offer its model.
pub fn on_device(ui: &MainWindow, d: &DeviceSnapshot) {
    if !d.ota_version.is_empty() {
        let region = Region::from_carrier(&d.nv_id);
        STATE.with(|s| {
            let mut s = s.borrow_mut();
            s.phone_ota = d.ota_version.clone();
            s.phone_region = region;
        });
        let text = region
            .map(|r| format!("{} — {}", r.code(), region_name(&ui.get_lang(), r)))
            .unwrap_or_default();
        ui.set_ota_phone_region(text.into());
    }
    if ui.get_ota_model().is_empty() && looks_like_model(&d.name) {
        ui.set_ota_model(d.name.as_str().into());
    }
}

/// OPPO / OnePlus / realme model codes: letters, digits, maybe a regional
/// suffix — CPH2747, CPH2747EEA, RMX3709, PJX110. Not codenames (OP611FL1).
fn looks_like_model(name: &str) -> bool {
    let letters = name.chars().take_while(|c| c.is_ascii_uppercase()).count();
    let digits = name[letters..]
        .chars()
        .take_while(|c| c.is_ascii_digit())
        .count();
    let suffix = &name[letters + digits..];
    (2..=4).contains(&letters) && digits >= 3 && suffix.chars().all(|c| c.is_ascii_uppercase())
}

pub fn on_progress(ui: &MainWindow, generation: u64, done: usize, total: usize) {
    if STATE.with(|s| s.borrow().generation) == generation {
        ui.set_ota_done(done as i32);
        ui.set_ota_total(total as i32);
    }
}

pub fn on_found(ui: &MainWindow, generation: u64, result: Result<Vec<Found>, String>) {
    let current = STATE.with(|s| s.borrow().generation == generation);
    if !current {
        return;
    }
    let found = match result {
        Ok(f) => f,
        Err(e) => {
            ui.set_ota_error(e.into());
            ui.set_ota_state(4);
            return;
        }
    };
    if found.is_empty() {
        ui.set_ota_state(3);
        return;
    }
    let lang = ui.get_lang();
    let mut found = found;
    let rows: Vec<OtaBuild> = STATE.with(|s| {
        let mut s = s.borrow_mut();
        s.cancel = None;
        let newest = found[0].build.version.clone();
        // The phone's own region first, newest first within each part.
        let mine = s.phone_region;
        let recommended = |f: &Found| mine.is_some_and(|r| f.regions.contains(&r));
        found.sort_by_key(|f| !recommended(f));
        let rows = found
            .iter()
            .map(|f| {
                let b = &f.build;
                let installed = !s.phone_ota.is_empty()
                    && b.ota_version == s.phone_ota
                    && s.phone_region.is_none_or(|r| f.regions.contains(&r));
                OtaBuild {
                    version: b.version.as_str().into(),
                    regions: f
                        .regions
                        .iter()
                        .map(|r| r.code())
                        .collect::<Vec<_>>()
                        .join(" · ")
                        .into(),
                    facts: facts(&lang, b).into(),
                    newest: b.version == newest,
                    installed,
                    recommended: recommended(f),
                    beta: f.beta,
                    changes: "".into(),
                }
            })
            .collect();
        s.found = found;
        rows
    });
    ui.set_ota_builds(ModelRc::new(VecModel::from(rows)));
    ui.set_ota_state(2);
}

pub fn on_changes(ui: &MainWindow, generation: u64, index: i32, text: String) {
    if STATE.with(|s| s.borrow().generation) != generation {
        return;
    }
    let lang = ui.get_lang();
    let text = if text.is_empty() {
        tr(
            &lang,
            "The server has no description for this build.",
            "У сервера нет описания этой сборки.",
            "服务器没有此版本的说明。",
            "伺服器沒有此版本的說明。",
        )
        .to_string()
    } else {
        text
    };
    let builds = ui.get_ota_builds();
    if let Some(mut row) = usize::try_from(index)
        .ok()
        .and_then(|i| slint::Model::row_data(&builds, i))
    {
        row.changes = text.into();
        slint::Model::set_row_data(&builds, index as usize, row);
    }
}

fn region_name(lang: &str, r: Region) -> String {
    let (ru, cn, tw) = match r.code() {
        "GL" => ("Глобальный", "全球", "全球"),
        "EU" => ("Европа", "欧洲", "歐洲"),
        "IN" => ("Индия", "印度", "印度"),
        "RU" => ("Россия", "俄罗斯", "俄羅斯"),
        "MEA" => ("Ближний Восток и Африка", "中东和非洲", "中東和非洲"),
        "TR" => ("Турция", "土耳其", "土耳其"),
        "SA" => ("Саудовская Аравия", "沙特阿拉伯", "沙烏地阿拉伯"),
        "SG" => ("Сингапур", "新加坡", "新加坡"),
        "TH" => ("Таиланд", "泰国", "泰國"),
        "MY" => ("Малайзия", "马来西亚", "馬來西亞"),
        "ID" => ("Индонезия", "印度尼西亚", "印尼"),
        "PH" => ("Филиппины", "菲律宾", "菲律賓"),
        "VN" => ("Вьетнам", "越南", "越南"),
        "TW" => ("Тайвань", "台湾", "台灣"),
        "OCA" => ("Океания", "大洋洲", "大洋洲"),
        "LATAM" => ("Латинская Америка", "拉丁美洲", "拉丁美洲"),
        "BR" => ("Бразилия", "巴西", "巴西"),
        "GB" => ("Великобритания", "英国", "英國"),
        "CN" => ("Китай", "中国", "中國"),
        _ => (r.name(), r.name(), r.name()),
    };
    tr(lang, r.name(), ru, cn, tw)
}

/// "2026-10-14 · 9.28 GB · patch 2026-09-01 · Android 16"
fn facts(lang: &str, b: &lfff_ota::Build) -> String {
    let mut out: Vec<String> = Vec::new();
    out.extend(b.date());
    if let Some(size) = b.size {
        let gb = format!("{:.2}", size as f64 / 1e9);
        out.push(match lang {
            "ru" => format!("{} ГБ", gb.replace('.', ",")),
            _ => format!("{gb} GB"),
        });
    }
    if let Some(p) = &b.security_patch {
        out.push(format!(
            "{} {p}",
            tr(lang, "patch", "патч", "补丁", "修補程式")
        ));
    }
    out.extend(b.android.clone());
    out.join(" · ")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn model_codes() {
        for m in ["CPH2747", "RMX3709", "PJX110", "NE2213", "CPH2747EEA"] {
            assert!(looks_like_model(m), "{m}");
        }
        for m in ["OnePlus 15", "OP611FL1", "taro", "", "CPH"] {
            assert!(!looks_like_model(m), "{m}");
        }
    }
}

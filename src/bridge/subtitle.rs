//! Subtitle panel: style (font/size/colors/border/position/bold), OpenSubtitles
//! search + download, and automatic translation — all off-thread work marshalled
//! back to the UI through the shared tokio runtime.

use std::sync::{Arc, Mutex, OnceLock};

use slint::{ComponentHandle, ModelRc, VecModel};

use crate::bridge::panels::refresh_panel;
use crate::error::LogErr;
use crate::mpv::player::MpvPlayer;
use crate::services;
use crate::services::opensubtitles::SubResult;
use crate::state::{AppState, MpvState};
use crate::translate_job;
use crate::util;
use crate::{MainWindow, SubSearchRow};

/// Shared tokio runtime for off-thread work (HTTP search/translate, ffmpeg).
/// Lives for the whole program; results marshal back via `invoke_from_event_loop`.
fn runtime() -> &'static tokio::runtime::Runtime {
    static RT: OnceLock<tokio::runtime::Runtime> = OnceLock::new();
    RT.get_or_init(|| tokio::runtime::Builder::new_multi_thread().enable_all().build().expect("tokio runtime"))
}

/// Raw OpenSubtitles results behind the search list, so a download can look up
/// the chosen row's link by index.
fn search_store() -> &'static Mutex<Vec<SubResult>> {
    static S: OnceLock<Mutex<Vec<SubResult>>> = OnceLock::new();
    S.get_or_init(|| Mutex::new(Vec::new()))
}

/// Apply the persisted subtitle style to mpv (called on each file load).
pub(crate) fn apply_sub_style(mpv: &MpvPlayer, app_state: &Arc<AppState>) {
    if let Ok(style) = app_state.with(|s, _| services::tracks::SubStyle::from(&s.subtitle_style)) {
        services::tracks::TracksService::set_sub_style(mpv, &style).log_err("apply subtitle style");
    }
}

/// Push the persisted subtitle style into the panel's style controls.
pub(crate) fn push_sub_style(ui: &MainWindow, app_state: &Arc<AppState>) {
    let Ok(st) = app_state.with(|s, _| s.subtitle_style.clone()) else { return };
    ui.set_sub_font(st.font.into());
    ui.set_sub_size(st.size as i32);
    ui.set_sub_bold(st.bold);
    ui.set_sub_border_size(st.border_size as i32);
    ui.set_sub_position(st.position as i32);
    let (r, g, b) = util::hex_to_rgb(&st.color);
    ui.set_sub_color(slint::Color::from_rgb_u8(r, g, b));
    ui.set_sub_color_hex(st.color.into());
    let (r, g, b) = util::hex_to_rgb(&st.border_color);
    ui.set_sub_border_color(slint::Color::from_rgb_u8(r, g, b));
    ui.set_sub_border_color_hex(st.border_color.into());
}

/// Read the panel's style controls, apply them to mpv, and persist them.
fn save_and_apply_sub_style(ui: &MainWindow, mpv_state: &Arc<MpvState>, app_state: &Arc<AppState>) {
    let c = ui.get_sub_color();
    let bc = ui.get_sub_border_color();
    let style = services::tracks::SubStyle {
        font: ui.get_sub_font().to_string(),
        size: ui.get_sub_size().max(0) as u32,
        color: util::rgb_to_hex(c.red(), c.green(), c.blue()),
        border_color: util::rgb_to_hex(bc.red(), bc.green(), bc.blue()),
        border_size: ui.get_sub_border_size().max(0) as u32,
        position: ui.get_sub_position().max(0) as u32,
        bold: ui.get_sub_bold(),
    };
    if let Ok(m) = mpv_state.get() {
        services::tracks::TracksService::set_sub_style(m, &style).log_err("apply subtitle style");
    }
    let _ = app_state.with(|s, _| {
        s.subtitle_style = (&style).into();
        s.save().log_err("save subtitle style");
    });
}

/// Translate the selected subtitle into the chosen language, off-thread, with
/// progress + completion marshalled back to the UI.
pub(crate) fn start_translation(ui: &MainWindow, mpv_state: &Arc<MpvState>, app_state: &Arc<AppState>) {
    if ui.get_translating() {
        return;
    }
    let lang = ui.get_translate_lang().to_string();
    if lang == "off" {
        return;
    }
    let Ok(mpv) = mpv_state.get().map(Arc::clone) else { return };
    ui.set_translating(true);
    ui.set_tr_progress(0);
    ui.set_tr_total(0);
    ui.set_tr_error(String::new().into());
    let (app, mpv_state2) = (app_state.clone(), mpv_state.clone());
    let (w_done, w_prog) = (ui.as_weak(), ui.as_weak());
    runtime().spawn(async move {
        let progress = move |cur: usize, total: usize, done: bool| {
            let w = w_prog.clone();
            let _ = slint::invoke_from_event_loop(move || {
                if let Some(ui) = w.upgrade() {
                    ui.set_tr_progress(cur as i32);
                    ui.set_tr_total(total as i32);
                    if done { ui.set_translating(false); }
                }
            });
        };
        let res = translate_job::run(mpv, app.clone(), lang, progress).await;
        let _ = slint::invoke_from_event_loop(move || {
            let Some(ui) = w_done.upgrade() else { return };
            ui.set_translating(false);
            match res {
                Ok(_) => { ui.set_tr_active(true); refresh_panel(&ui, &mpv_state2, &app, "sub"); }
                Err(e) => ui.set_tr_error(e.into()),
            }
        });
    });
}

pub(crate) fn wire(ui: &MainWindow, mpv_state: &Arc<MpvState>, app_state: &Arc<AppState>) {
    ui.on_apply_sub_style({
        let (ui_w, mpv, app) = (ui.as_weak(), mpv_state.clone(), app_state.clone());
        move || { if let Some(ui) = ui_w.upgrade() { save_and_apply_sub_style(&ui, &mpv, &app); } }
    });
    ui.on_reset_sub_style({
        let (ui_w, mpv, app) = (ui.as_weak(), mpv_state.clone(), app_state.clone());
        move || {
            let _ = app.with(|s, _| { s.subtitle_style = services::settings::SubtitleStyleSettings::default(); s.save().log_err("save subtitle style"); });
            if let Some(ui) = ui_w.upgrade() {
                push_sub_style(&ui, &app);
                if let Ok(m) = mpv.get() { apply_sub_style(m, &app); }
            }
        }
    });
    // Seed the in-app HSV picker from the current colour and show it.
    ui.on_pick_color({
        let ui_w = ui.as_weak();
        move |which| {
            let Some(ui) = ui_w.upgrade() else { return };
            let cur = if which == "border" { ui.get_sub_border_color() } else { ui.get_sub_color() };
            let (h, s, v) = util::rgb_to_hsv(cur.red(), cur.green(), cur.blue());
            ui.set_cp_hue(h);
            ui.set_cp_sat(s);
            ui.set_cp_val(v);
            ui.set_cp_target(which);
            ui.set_cp_show(true);
        }
    });
    // Apply the colour the picker returned: store it (+ hex), persist, push to mpv.
    ui.on_apply_picked_color({
        let (ui_w, mpv, app) = (ui.as_weak(), mpv_state.clone(), app_state.clone());
        move |which, c| {
            let Some(ui) = ui_w.upgrade() else { return };
            let (r, g, b) = (c.red(), c.green(), c.blue());
            let hex = util::rgb_to_hex(r, g, b);
            if which == "border" {
                ui.set_sub_border_color(slint::Color::from_rgb_u8(r, g, b));
                ui.set_sub_border_color_hex(hex.into());
            } else {
                ui.set_sub_color(slint::Color::from_rgb_u8(r, g, b));
                ui.set_sub_color_hex(hex.into());
            }
            save_and_apply_sub_style(&ui, &mpv, &app);
        }
    });
    ui.on_do_search({
        let (ui_w, app) = (ui.as_weak(), app_state.clone());
        move || {
            let Some(ui) = ui_w.upgrade() else { return };
            let (query, lang) = (ui.get_search_query().to_string(), ui.get_search_lang().to_string());
            ui.set_searching(true);
            ui.set_has_searched(true);
            ui.set_search_error(String::new().into());
            let path = app.with(|_, f| f.clone()).ok().flatten();
            let w = ui.as_weak();
            runtime().spawn(async move {
                let file_hash = match path {
                    Some(p) => tokio::task::spawn_blocking(move || services::opensubtitles::compute_hash(&p).ok()).await.ok().flatten(),
                    None => None,
                };
                let res = services::opensubtitles::search(file_hash, &query, &lang).await;
                if let Ok(list) = &res {
                    if let Ok(mut store) = search_store().lock() { *store = list.clone(); }
                }
                let _ = slint::invoke_from_event_loop(move || {
                    let Some(ui) = w.upgrade() else { return };
                    ui.set_searching(false);
                    match res {
                        Ok(list) => {
                            let rows: Vec<SubSearchRow> = list.iter().map(|r| SubSearchRow {
                                name: r.name.clone().into(),
                                lang: r.lang.clone().into(),
                                downloads: util::fmt_downloads(&r.downloads).into(),
                                matched: (if r.matched_by == "moviehash" { "hash" } else { r.matched_by.as_str() }).into(),
                            }).collect();
                            ui.set_search_results(ModelRc::new(VecModel::from(rows)));
                        }
                        Err(e) => ui.set_search_error(e.into()),
                    }
                });
            });
        }
    });
    ui.on_download_sub({
        let (ui_w, mpv, app) = (ui.as_weak(), mpv_state.clone(), app_state.clone());
        move |index| {
            let item = search_store().lock().ok().and_then(|v| v.get(index as usize).cloned());
            let (Some(item), Some(ui)) = (item, ui_w.upgrade()) else { return };
            ui.set_downloading_index(index);
            let (w, mpv, app) = (ui.as_weak(), mpv.clone(), app.clone());
            runtime().spawn(async move {
                let dir = dirs::cache_dir().or_else(dirs::data_local_dir).unwrap_or_else(std::env::temp_dir).join("Vayou").join("subtitles");
                let res = services::opensubtitles::download(&item.download_link, &dir, &item.name).await;
                let _ = slint::invoke_from_event_loop(move || {
                    let Some(ui) = w.upgrade() else { return };
                    ui.set_downloading_index(-1);
                    match res {
                        Ok(path) => {
                            if let Ok(m) = mpv.get() {
                                if let Err(e) = m.command(&["sub-add", &path.to_string_lossy(), "select"]) {
                                    tracing::warn!(error = %e, "add downloaded subtitle");
                                    ui.set_toast("Downloaded, but couldn't load the subtitle".into());
                                }
                            }
                            ui.set_sub_page("main".into());
                            refresh_panel(&ui, &mpv, &app, "sub");
                        }
                        Err(e) => ui.set_search_error(e.into()),
                    }
                });
            });
        }
    });
    ui.on_set_translate_lang({
        let (ui_w, mpv, app) = (ui.as_weak(), mpv_state.clone(), app_state.clone());
        move |code| {
            let Some(ui) = ui_w.upgrade() else { return };
            let _ = app.with(|s, _| { s.translate_lang = code.to_string(); s.save().log_err("save translate language"); });
            ui.set_translate_lang(code);
            start_translation(&ui, &mpv, &app);
        }
    });
    ui.on_translate_off({
        let (ui_w, mpv, app) = (ui.as_weak(), mpv_state.clone(), app_state.clone());
        move || {
            let _ = app.with(|s, _| { s.translate_lang = "off".to_string(); s.save().log_err("save translate language"); });
            if let Ok(m) = mpv.get() { translate_job::clear_translation(m); }
            if let Some(ui) = ui_w.upgrade() {
                ui.set_translate_lang("off".into());
                ui.set_tr_active(false);
                refresh_panel(&ui, &mpv, &app, "sub");
            }
        }
    });
    ui.on_set_sub_encoding({
        let (mpv, app) = (mpv_state.clone(), app_state.clone());
        move |code| {
            let _ = app.with(|s, _| { s.subtitle_encoding = code.to_string(); s.save().log_err("save subtitle encoding"); });
            if let Ok(m) = mpv.get() {
                m.set::<&str>("sub-codepage", if code.is_empty() { "auto" } else { code.as_str() }).log_err("set sub-codepage");
                m.command(&["sub-reload"]).log_err("sub-reload");
            }
        }
    });
    ui.on_toggle_embedded_styles({
        let (ui_w, mpv, app) = (ui.as_weak(), mpv_state.clone(), app_state.clone());
        move || {
            let Some(ui) = ui_w.upgrade() else { return };
            let on = ui.get_apply_embedded_styles();
            let _ = app.with(|s, _| { s.apply_embedded_styles = on; s.save().log_err("save embedded styles toggle"); });
            if let Ok(m) = mpv.get() { m.set::<&str>("sub-ass-override", if on { "no" } else { "force" }).log_err("set sub-ass-override"); }
        }
    });
}

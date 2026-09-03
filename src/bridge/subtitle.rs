//! Subtitle panel: style (font/size/colors/border/position/bold), OpenSubtitles
//! search + download, and automatic translation — all off-thread work marshalled
//! back to the UI through the shared tokio runtime.

use std::cell::{Cell, RefCell};
use std::sync::{Arc, Mutex, OnceLock};

use slint::{ComponentHandle, ModelRc, VecModel};

use crate::bridge::panels::refresh_panel;
use crate::bridge::persist;
use crate::bridge::runtime;
use crate::error::LogErr;
use crate::mpv::player::MpvPlayer;
use crate::services;
use crate::services::opensubtitles::SubResult;
use crate::state::{AppState, MpvState};
use crate::translate_job;
use crate::util;
use crate::{MainWindow, SubSearchRow};

/// Raw OpenSubtitles results behind the search list, so a download can look up
/// the chosen row's link by index.
fn search_store() -> &'static Mutex<Vec<SubResult>> {
    static S: OnceLock<Mutex<Vec<SubResult>>> = OnceLock::new();
    S.get_or_init(|| Mutex::new(Vec::new()))
}

thread_local! {
    /// Generation of the most recently started translation. A run whose
    /// generation is no longer the current one must not write to the panel —
    /// see `start_translation`. Lives on the UI thread, which is where every
    /// start and every marshalled callback runs.
    static TR_GENERATION: Cell<u64> = const { Cell::new(0) };

    /// The film the search page is currently set up for. Comparing against it
    /// is what separates a new file from a re-open of the panel: the first
    /// resets the page, the second leaves it alone so a query the user edited
    /// survives closing and reopening.
    static SEARCH_TITLE: RefCell<String> = const { RefCell::new(String::new()) };

    /// Generation of the most recently started search, on the same contract as
    /// `TR_GENERATION`. A search is superseded either by a newer query or by
    /// the film changing while it was in flight — OpenSubtitles takes seconds,
    /// which is time enough to skip to the next episode — and results that
    /// arrive after either must not land on the panel or in `search_store`,
    /// where a download would then fetch the wrong film's subtitle.
    static SEARCH_GENERATION: Cell<u64> = const { Cell::new(0) };
}

/// Invalidate any search in flight and return the generation for the next one.
fn next_search_generation() -> u64 {
    SEARCH_GENERATION.with(|g| {
        g.set(g.get() + 1);
        g.get()
    })
}

/// Whether `generation` is still the newest translation run.
fn is_current_translation(generation: u64) -> bool {
    TR_GENERATION.with(Cell::get) == generation
}

/// Point the OpenSubtitles search at the file now playing, unless it is already
/// pointing at it.
///
/// Without this the previous film's results sit on the page under the new
/// film's name until the next search, and a stale `has-searched` turns an empty
/// list into "No results" for a search that was never run. The query is re-seeded
/// from the new file's name at the same time — and only then, so editing it and
/// closing the panel does not throw the edit away.
pub(crate) fn reset_search_for_current_file(ui: &MainWindow) {
    let title = ui.get_media_title().to_string();
    if SEARCH_TITLE.with(|t| *t.borrow() == title) {
        return;
    }
    SEARCH_TITLE.with(|t| *t.borrow_mut() = title.clone());
    next_search_generation();

    ui.set_search_results(ModelRc::new(VecModel::from(Vec::<SubSearchRow>::new())));
    ui.set_search_error(String::new().into());
    ui.set_has_searched(false);
    // A search may be in flight for the film we just left. Bumping the
    // generation above means its reply will be dropped rather than clear the
    // spinner on its way past, so the spinner has to be cleared here instead.
    ui.set_searching(false);
    if let Ok(mut store) = search_store().lock() {
        store.clear();
    }
    let stem = title.rsplit_once('.').map_or(title.as_str(), |(s, _)| s);
    ui.set_search_query(stem.into());
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
    ui.set_sub_shadow(st.shadow as i32);
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
        shadow: ui.get_sub_shadow().max(0) as u32,
    };
    if let Ok(m) = mpv_state.get() {
        services::tracks::TracksService::set_sub_style(m, &style).log_err("apply subtitle style");
    }
    // mpv is updated on every move — that is the live preview, and it stays
    // immediate. Only the disk write waits for the slider to come to rest.
    let _ = app_state.with(|s, _| s.subtitle_style = (&style).into());
    persist::save_debounced(app_state);
}

/// Clear the translation panel for a newly loaded file, standing down a run
/// still going for the previous one.
///
/// `translate_job::run` hands its finished file to mpv before it reports being
/// done, so a run that outlives its film attaches the previous film's dialogue
/// to the new one and then reports the outcome on a panel that has moved on.
/// `start_translation` clears the way for its own run, but only when it is
/// reached — and a film that already carries the target language never gets
/// that far.
pub(crate) fn reset_translation_for_new_file(ui: &MainWindow) {
    if ui.get_translating() {
        translate_job::cancel();
        ui.set_translating(false);
    }
    TR_GENERATION.with(|g| g.set(g.get() + 1));
    ui.set_tr_active(false);
    ui.set_tr_error(String::new().into());
}

/// Translate the selected subtitle into the chosen language, off-thread, with
/// progress + completion marshalled back to the UI.
pub(crate) fn start_translation(ui: &MainWindow, mpv_state: &Arc<MpvState>, app_state: &Arc<AppState>) {
    let lang = ui.get_translate_lang().to_string();
    if lang == "off" {
        return;
    }
    let Ok(mpv) = mpv_state.get().map(Arc::clone) else { return };

    // Picking another language mid-run used to be dropped on the floor: a guard
    // here returned before the new choice could take effect, so a two-minute
    // extraction had to finish first — and what it applied at the end was the
    // language the user had already moved away from. Stand the in-flight run
    // down and start the new one instead.
    if ui.get_translating() {
        translate_job::cancel();
    }
    // Bumped on every start, so the callbacks of the run just superseded stop
    // writing to the panel. Without it that run's completion flips `translating`
    // off and reports its outcome while the new one is still working.
    let generation = TR_GENERATION.with(|g| {
        g.set(g.get() + 1);
        g.get()
    });

    ui.set_translating(true);
    ui.set_tr_progress(0);
    ui.set_tr_total(0);
    ui.set_tr_error(String::new().into());
    ui.set_tr_phase(translate_job::PHASE_EXTRACTING.into());
    let (app, mpv_state2) = (app_state.clone(), mpv_state.clone());
    let (w_done, w_prog) = (ui.as_weak(), ui.as_weak());
    runtime().spawn(async move {
        let progress = move |cur: usize, total: usize, done: bool, phase: &'static str| {
            let w = w_prog.clone();
            let _ = slint::invoke_from_event_loop(move || {
                if !is_current_translation(generation) {
                    return;
                }
                if let Some(ui) = w.upgrade() {
                    ui.set_tr_progress(cur as i32);
                    ui.set_tr_total(total as i32);
                    ui.set_tr_phase(phase.into());
                    if done { ui.set_translating(false); }
                }
            });
        };
        let res = translate_job::run(mpv, lang, progress).await;
        let _ = slint::invoke_from_event_loop(move || {
            let Some(ui) = w_done.upgrade() else { return };
            if !is_current_translation(generation) {
                return;
            }
            ui.set_translating(false);
            match res {
                Ok(outcome) => {
                    ui.set_tr_active(true);
                    // A partial result is still applied — most of the film is
                    // translated and withholding it helps nobody — but it is not
                    // silent, because the gaps only show up mid-sentence.
                    if outcome.partial {
                        ui.set_tr_error("partial".into());
                    }
                    refresh_panel(&ui, &mpv_state2, &app, "sub");
                }
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
    // Seed the in-app HSV picker from the current colour. The caller decides how
    // to show it: the subtitle panel opens an inline sub-page, the settings modal
    // sets `cp-show`.
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
            let generation = next_search_generation();
            runtime().spawn(async move {
                let file_hash = match path {
                    Some(p) => tokio::task::spawn_blocking(move || services::opensubtitles::compute_hash(&p).ok()).await.ok().flatten(),
                    None => None,
                };
                let res = services::opensubtitles::search(file_hash, &query, &lang).await;
                let _ = slint::invoke_from_event_loop(move || {
                    // Superseded: leave the panel — and the store the download
                    // indexes into — to whoever owns the current generation.
                    // Clearing `searching` here would kill their spinner too.
                    if SEARCH_GENERATION.with(Cell::get) != generation {
                        return;
                    }
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
                            if let Ok(mut store) = search_store().lock() { *store = list; }
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
            // Off during a run means stop it: the panel is reachable while
            // translating, and a 2-minute extraction is exactly when someone
            // reaches for it.
            translate_job::cancel();
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

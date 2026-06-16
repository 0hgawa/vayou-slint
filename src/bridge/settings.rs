//! Settings surface: persisted preferences, display-name lookups, the rebindable
//! shortcut table, and restore-to-defaults.

use std::collections::HashMap;
use std::sync::Arc;

use slint::{ComponentHandle, ModelRc, VecModel};

use crate::bridge::subtitle::{apply_sub_style, push_sub_style};
use crate::error::LogErr;
use crate::keybindings;
use crate::services;
use crate::services::audio_fx::AudioFxService;
use crate::services::video::VideoService;
use crate::state::{AppState, MpvState};
use crate::{MainWindow, ShortcutRow};

/// Native UI-language display name (matches the Settings list).
fn ui_lang_name(code: &str) -> &'static str {
    match code {
        "pt" => "Português", "es" => "Español", "fr" => "Français", "de" => "Deutsch",
        "it" => "Italiano", "ja" => "日本語", "ko" => "한국어", "zh" => "中文",
        "ru" => "Русский", "ar" => "العربية", "hi" => "हिन्दी", _ => "English",
    }
}

/// Preferred-track language display name ("" → Auto).
fn track_lang_name(code: &str) -> &'static str {
    match code {
        "eng" => "English", "por" => "Português", "spa" => "Español", "fre" => "Français",
        "ger" => "Deutsch", "ita" => "Italiano", "jpn" => "日本語", "kor" => "한국어",
        "chi" => "中文", "rus" => "Русский", "ara" => "العربية", "hin" => "हिन्दी", _ => "Auto",
    }
}

/// Subtitle-encoding display name ("" → Auto).
fn enc_name(code: &str) -> &'static str {
    match code {
        "utf-8" => "UTF-8", "cp1252" => "Western (Windows-1252)", "iso-8859-1" => "Western (ISO-8859-1)",
        "cp1251" => "Cyrillic (Windows-1251)", "iso-8859-2" => "Central European (ISO-8859-2)",
        "gbk" => "Chinese Simplified (GBK)", "big5" => "Chinese Traditional (Big5)",
        "shift-jis" => "Japanese (Shift-JIS)", "euc-kr" => "Korean (EUC-KR)", _ => "Auto",
    }
}

/// Build the shortcut rows. Labels are NOT resolved here — the action id and
/// category key are passed to the UI, which renders the (translatable) labels
/// via `@tr` so they re-localise live when the language changes.
fn build_shortcuts(custom: &HashMap<String, String>) -> ModelRc<ShortcutRow> {
    let mut last_cat = "";
    let rows: Vec<ShortcutRow> = keybindings::ACTIONS.iter().map(|a| {
        let first = a.category != last_cat;
        last_cat = a.category;
        ShortcutRow {
            action: a.id.into(),
            key: keybindings::display_key(custom, a.id).into(),
            category: a.category.into(),
            first_in_category: first,
        }
    }).collect();
    ModelRc::new(VecModel::from(rows))
}

/// Push persisted settings into the Settings surface.
pub(crate) fn push_settings(ui: &MainWindow, app_state: &Arc<AppState>) {
    let Ok(s) = app_state.with(|s, _| s.clone()) else { return };
    ui.set_ui_language(s.language.clone().into());
    ui.set_ui_language_name(ui_lang_name(&s.language).into());
    ui.set_def_volume(s.volume as f32);
    ui.set_def_speed(s.speed as f32);
    ui.set_pref_audio_lang(s.preferred_audio_lang.clone().into());
    ui.set_pref_audio_name(track_lang_name(&s.preferred_audio_lang).into());
    ui.set_pref_sub_lang(s.preferred_subtitle_lang.clone().into());
    ui.set_pref_sub_name(track_lang_name(&s.preferred_subtitle_lang).into());
    ui.set_remember_position(s.remember_position);
    ui.set_auto_play(s.auto_play);
    ui.set_remember_selections(s.remember_selections);
    ui.set_volume_boost(s.volume_boost);
    ui.set_eq_enabled(s.equalizer_enabled);
    ui.set_sub_encoding(s.subtitle_encoding.clone().into());
    ui.set_sub_encoding_name(enc_name(&s.subtitle_encoding).into());
    ui.set_apply_embedded_styles(s.apply_embedded_styles);
    push_sub_style(ui, app_state);
    ui.set_shortcuts(build_shortcuts(&s.keybindings));
}

pub(crate) fn wire(ui: &MainWindow, mpv_state: &Arc<MpvState>, app_state: &Arc<AppState>) {
    ui.on_set_ui_language({
        let app = app_state.clone();
        move |code| {
            let _ = app.with(|s, _| { s.language = code.to_string(); s.save().log_err("save settings"); });
            slint::select_bundled_translation(code.as_str()).log_err("apply UI language");
        }
    });
    ui.on_set_def_volume({
        let app = app_state.clone();
        move |v| { let _ = app.with(|s, _| { s.volume = f64::from(v); s.save().log_err("save settings"); }); }
    });
    ui.on_set_def_speed({
        let app = app_state.clone();
        move |v| { let _ = app.with(|s, _| { s.speed = f64::from(v); s.save().log_err("save settings"); }); }
    });
    ui.on_set_pref_audio({
        let (mpv, app) = (mpv_state.clone(), app_state.clone());
        move |code| {
            let _ = app.with(|s, _| { s.preferred_audio_lang = code.to_string(); s.save().log_err("save settings"); });
            if let Ok(m) = mpv.get() { m.set::<&str>("alang", code.as_str()).log_err("set alang"); }
        }
    });
    ui.on_set_pref_sub({
        let (mpv, app) = (mpv_state.clone(), app_state.clone());
        move |code| {
            let _ = app.with(|s, _| { s.preferred_subtitle_lang = code.to_string(); s.save().log_err("save settings"); });
            if let Ok(m) = mpv.get() { m.set::<&str>("slang", code.as_str()).log_err("set slang"); }
        }
    });
    ui.on_toggle_remember_position({
        let (ui_w, app) = (ui.as_weak(), app_state.clone());
        move || { if let Some(ui) = ui_w.upgrade() { let v = ui.get_remember_position(); let _ = app.with(|s, _| { s.remember_position = v; s.save().log_err("save settings"); }); } }
    });
    ui.on_toggle_auto_play({
        let (ui_w, app) = (ui.as_weak(), app_state.clone());
        move || { if let Some(ui) = ui_w.upgrade() { let v = ui.get_auto_play(); let _ = app.with(|s, _| { s.auto_play = v; s.save().log_err("save settings"); }); } }
    });
    ui.on_toggle_remember_selections({
        let (ui_w, app) = (ui.as_weak(), app_state.clone());
        move || { if let Some(ui) = ui_w.upgrade() { let v = ui.get_remember_selections(); let _ = app.with(|s, _| { s.remember_selections = v; s.save().log_err("save settings"); }); } }
    });
    ui.on_rebind_key({
        let (ui_w, app) = (ui.as_weak(), app_state.clone());
        move |key, ctrl, shift, alt| {
            let Some(ui) = ui_w.upgrade() else { return };
            let action = ui.get_rebinding_action().to_string();
            if action.is_empty() { return; }
            let combo = keybindings::build_combo(key.as_str(), ctrl, shift, alt);
            let _ = app.with(|s, _| { keybindings::set_key(&mut s.keybindings, &action, &combo); s.save().log_err("save settings"); });
            if let Ok(custom) = app.with(|s, _| s.keybindings.clone()) { ui.set_shortcuts(build_shortcuts(&custom)); }
        }
    });
    ui.on_reset_shortcuts({
        let (ui_w, app) = (ui.as_weak(), app_state.clone());
        move || {
            let _ = app.with(|s, _| { s.keybindings.clear(); s.save().log_err("save settings"); });
            if let Some(ui) = ui_w.upgrade() { ui.set_shortcuts(build_shortcuts(&HashMap::new())); }
        }
    });
    ui.on_restore_defaults({
        let (ui_w, mpv, app) = (ui.as_weak(), mpv_state.clone(), app_state.clone());
        move || {
            let _ = app.with(|s, _| { *s = services::settings::PlayerSettings::default(); s.save().log_err("save settings"); });
            slint::select_bundled_translation("en").log_err("apply UI language");
            if let Some(ui) = ui_w.upgrade() {
                push_settings(&ui, &app);
                ui.set_max_volume(100.0);
                ui.set_brightness(0); ui.set_contrast(0); ui.set_saturation(0); ui.set_vid_zoom(0.0);
                ui.set_eq_bands(ModelRc::new(VecModel::from(vec![0, 0, 0, 0, 0])));
                if let Ok(m) = mpv.get() {
                    VideoService::set_brightness(m, 0).log_err("reset brightness");
                    VideoService::set_contrast(m, 0).log_err("reset contrast");
                    VideoService::set_saturation(m, 0).log_err("reset saturation");
                    VideoService::reset_zoom_pan(m).log_err("reset zoom/pan");
                    AudioFxService::reset_equalizer(m).log_err("reset equalizer");
                    apply_sub_style(m, &app);
                }
            }
        }
    });

    // About tab: build version + signed self-update. update-status codes:
    //   0 idle · 1 checking · 2 latest · 3 available · 4 failed
    //   5 installing · 6 installed (restart pending)
    ui.set_app_version(env!("CARGO_PKG_VERSION").into());
    ui.on_check_updates({
        let ui_w = ui.as_weak();
        move || {
            let Some(ui) = ui_w.upgrade() else { return };
            ui.set_update_status(1); // checking
            let w = ui.as_weak();
            std::thread::spawn(move || {
                let res = run_async(crate::update::check());
                let _ = slint::invoke_from_event_loop(move || {
                    let Some(ui) = w.upgrade() else { return };
                    match res {
                        // Stash the verified UpdateInfo so "Install" can apply it.
                        Ok(Some(info)) => {
                            ui.set_update_detail(info.version.clone().into());
                            PENDING_UPDATE.with(|p| *p.borrow_mut() = Some(info));
                            ui.set_update_status(3);
                        }
                        Ok(None) => ui.set_update_status(2),
                        Err(e) => { ui.set_update_detail(e.into()); ui.set_update_status(4); }
                    }
                });
            });
        }
    });
    ui.on_install_update({
        let ui_w = ui.as_weak();
        move || {
            let Some(ui) = ui_w.upgrade() else { return };
            let Some(info) = PENDING_UPDATE.with(|p| p.borrow().clone()) else { return };
            ui.set_update_status(5); // installing
            let w = ui.as_weak();
            std::thread::spawn(move || {
                let res = run_async(crate::update::download_and_apply(&info));
                let _ = slint::invoke_from_event_loop(move || {
                    let Some(ui) = w.upgrade() else { return };
                    match res {
                        Ok(()) => ui.set_update_status(6), // installed — restart to apply
                        Err(e) => { ui.set_update_detail(e.into()); ui.set_update_status(4); }
                    }
                });
            });
        }
    });
    ui.on_relaunch_app(|| {
        crate::update::relaunch();
        let _ = slint::quit_event_loop();
    });
    ui.on_open_release_page(crate::update::open_release_page);
}

/// Run a future to completion on a throwaway current-thread runtime. Used by the
/// update check/install workers (off the UI thread).
fn run_async<F: std::future::Future>(fut: F) -> F::Output {
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("build update runtime")
        .block_on(fut)
}

thread_local! {
    // The verified update found by the last check, consumed by "Install". A
    // thread_local (touched only on the UI thread) keeps the worker closures
    // Send without wrapping it in an Arc/Mutex.
    static PENDING_UPDATE: std::cell::RefCell<Option<crate::update::UpdateInfo>> =
        const { std::cell::RefCell::new(None) };
}

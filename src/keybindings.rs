//! Rebindable keyboard shortcuts. Ported from the WebView build's
//! `keybindings.svelte.ts`: a fixed action table with defaults, plus a
//! per-user override map (persisted in settings) and a resolver that maps a
//! key event to an action id.

use std::collections::HashMap;

pub struct ActionDef {
    pub id: &'static str,
    pub category: &'static str,
    pub default: &'static str,
}

pub const ACTIONS: &[ActionDef] = &[
    ActionDef { id: "togglePause", category: "playback", default: "Space" },
    ActionDef { id: "seekForward", category: "playback", default: "ArrowRight" },
    ActionDef { id: "seekForwardLong", category: "playback", default: "Shift+ArrowRight" },
    ActionDef { id: "seekBack", category: "playback", default: "ArrowLeft" },
    ActionDef { id: "seekBackLong", category: "playback", default: "Shift+ArrowLeft" },
    ActionDef { id: "nextFile", category: "playback", default: "n" },
    ActionDef { id: "prevFile", category: "playback", default: "p" },
    ActionDef { id: "frameNext", category: "playback", default: "." },
    ActionDef { id: "framePrev", category: "playback", default: "," },
    ActionDef { id: "speedUp", category: "playback", default: "+" },
    ActionDef { id: "speedDown", category: "playback", default: "-" },
    ActionDef { id: "abLoop", category: "playback", default: "l" },
    ActionDef { id: "volumeUp", category: "volume", default: "ArrowUp" },
    ActionDef { id: "volumeDown", category: "volume", default: "ArrowDown" },
    ActionDef { id: "mute", category: "volume", default: "m" },
    ActionDef { id: "fullscreen", category: "video", default: "f" },
    ActionDef { id: "screenshot", category: "video", default: "s" },
    ActionDef { id: "aspectRatio", category: "video", default: "r" },
    ActionDef { id: "cycleSub", category: "tracks", default: "v" },
    ActionDef { id: "cycleAudio", category: "tracks", default: "a" },
    ActionDef { id: "openFile", category: "navigation", default: "Ctrl+o" },
    ActionDef { id: "openUrl", category: "navigation", default: "Ctrl+u" },
    ActionDef { id: "mediaInfo", category: "navigation", default: "i" },
];

/// The effective combo for an action: the user's override, or the default.
/// An override of `""` means the action is disabled.
fn effective<'a>(custom: &'a HashMap<String, String>, a: &'a ActionDef) -> &'a str {
    custom.get(a.id).map_or(a.default, String::as_str)
}

/// Whether the Shift modifier is meaningful for this key. For a single
/// punctuation/symbol char (e.g. "+", "-") Shift is already baked into the
/// produced character, so folding it into the combo would make defaults like
/// "+" unmatchable. Named keys (Arrow*, Space…) and letters keep Shift.
fn shift_matters(key: &str) -> bool {
    key.chars().count() != 1 || key.chars().all(|c| c.is_ascii_alphabetic())
}

/// Build a combo string for a key event, preserving the key's original casing
/// (e.g. "Shift+ArrowRight"). Used for both display/storage and, lowercased, for
/// matching — so the two can never disagree.
pub fn build_combo(key: &str, ctrl: bool, shift: bool, alt: bool) -> String {
    let mut parts = Vec::new();
    if ctrl { parts.push("Ctrl".to_string()); }
    if shift && shift_matters(key) { parts.push("Shift".to_string()); }
    if alt { parts.push("Alt".to_string()); }
    parts.push(key.to_string());
    parts.join("+")
}

/// The normalised (lowercased) combo string for a key event, used for matching.
pub fn combo(key: &str, ctrl: bool, shift: bool, alt: bool) -> String {
    build_combo(key, ctrl, shift, alt).to_lowercase()
}

/// Resolve a key event to an action id, honouring user overrides.
pub fn resolve(custom: &HashMap<String, String>, key: &str, ctrl: bool, shift: bool, alt: bool) -> Option<&'static str> {
    let c = combo(key, ctrl, shift, alt);
    ACTIONS.iter().find(|a| {
        let eff = effective(custom, a);
        !eff.is_empty() && eff.to_lowercase() == c
    }).map(|a| a.id)
}

/// The display key for an action (override or default), prettified.
pub fn display_key(custom: &HashMap<String, String>, id: &str) -> String {
    let key = ACTIONS.iter().find(|a| a.id == id)
        .map_or("", |a| custom.get(id).map_or(a.default, String::as_str));
    key_label(key)
}

/// Prettify a combo for display ("Shift+ArrowRight" → "Shift+→").
pub fn key_label(key: &str) -> String {
    if key.is_empty() {
        return "—".to_string();
    }
    key.replace("ArrowUp", "↑").replace("ArrowDown", "↓")
        .replace("ArrowLeft", "←").replace("ArrowRight", "→")
        .replace("Escape", "Esc")
}

/// Bind `combo` to `action`, clearing conflicts. Mirrors the store's `setKey`:
/// another action using the same key is removed; a default-owning action is
/// disabled (set to ""); binding back to the default removes the override.
pub fn set_key(custom: &mut HashMap<String, String>, action: &str, combo: &str) {
    let lc = combo.to_lowercase();
    // Remove other overrides using this key.
    let dup: Vec<String> = custom.iter()
        .filter(|(a, k)| k.to_lowercase() == lc && a.as_str() != action)
        .map(|(a, _)| a.clone())
        .collect();
    for a in dup { custom.remove(&a); }
    // Disable any default-owning action that uses this key.
    for a in ACTIONS {
        if a.id != action && a.default.to_lowercase() == lc {
            custom.insert(a.id.to_string(), String::new());
        }
    }
    let default = ACTIONS.iter().find(|a| a.id == action).map_or("", |a| a.default);
    if combo.to_lowercase() == default.to_lowercase() {
        custom.remove(action);
    } else {
        custom.insert(action.to_string(), combo.to_string());
    }
}

#[cfg(test)]
mod tests {
    use super::{build_combo, combo, key_label, resolve, set_key};
    use std::collections::HashMap;

    #[test]
    fn build_combo_preserves_casing_and_modifier_order() {
        assert_eq!(build_combo("ArrowRight", false, false, false), "ArrowRight");
        assert_eq!(build_combo("ArrowRight", false, true, false), "Shift+ArrowRight");
        assert_eq!(build_combo("o", true, false, false), "Ctrl+o");
        assert_eq!(build_combo("k", true, true, true), "Ctrl+Shift+Alt+k");
    }

    #[test]
    fn shift_is_dropped_for_symbol_keys() {
        // "+" already bakes in Shift — folding it in would make the default
        // unmatchable. Named keys and letters keep Shift.
        assert_eq!(build_combo("+", false, true, false), "+");
        assert_eq!(combo("ArrowLeft", false, true, false), "shift+arrowleft");
    }

    #[test]
    fn resolve_falls_back_to_defaults() {
        let custom = HashMap::new();
        assert_eq!(resolve(&custom, "Space", false, false, false), Some("togglePause"));
        assert_eq!(resolve(&custom, "s", false, false, false), Some("screenshot"));
        assert_eq!(resolve(&custom, "z", false, false, false), None);
    }

    #[test]
    fn resolve_honours_overrides() {
        let mut custom = HashMap::new();
        custom.insert("togglePause".to_string(), "k".to_string());
        assert_eq!(resolve(&custom, "k", false, false, false), Some("togglePause"));
        // Space no longer maps anywhere once togglePause was reassigned.
        assert_eq!(resolve(&custom, "Space", false, false, false), None);
    }

    #[test]
    fn set_key_binds_back_to_default_clears_the_override() {
        let mut custom = HashMap::new();
        set_key(&mut custom, "screenshot", "F5");
        assert_eq!(custom.get("screenshot").map(String::as_str), Some("F5"));
        // Rebinding to its own default removes the override entirely.
        set_key(&mut custom, "screenshot", "s");
        assert!(!custom.contains_key("screenshot"));
    }

    #[test]
    fn set_key_disables_a_default_owner_on_conflict() {
        let mut custom = HashMap::new();
        // "Space" is togglePause's default; giving it to screenshot disables it.
        set_key(&mut custom, "screenshot", "Space");
        assert_eq!(custom.get("screenshot").map(String::as_str), Some("Space"));
        assert_eq!(custom.get("togglePause").map(String::as_str), Some(""));
    }

    #[test]
    fn set_key_removes_a_conflicting_override() {
        let mut custom = HashMap::new();
        custom.insert("mute".to_string(), "g".to_string());
        set_key(&mut custom, "screenshot", "g");
        assert!(!custom.contains_key("mute"));
        assert_eq!(custom.get("screenshot").map(String::as_str), Some("g"));
    }

    #[test]
    fn key_label_prettifies_arrows_and_empty() {
        assert_eq!(key_label("Shift+ArrowRight"), "Shift+→");
        assert_eq!(key_label(""), "—");
    }
}

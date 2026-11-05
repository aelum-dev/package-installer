//! i18n: locale selection and the message catalogs.
//!
//! Mirrors the video-player's `i18n.rs` and the installer frontend's i18n
//! (`distro/installer/frontend/src/lib/i18n`), so every part of the desktop
//! speaks the same locale contract: flat dotted keys, `{placeholder}`
//! interpolation, and the same fallback chain — active locale → en-US → the
//! key itself. Catalogs are JSON files in `data/translations/`, embedded at
//! compile time, so the packaged binary carries them and needs no data
//! directory (offline-first: translation is a build-time fact, never a
//! network call).
//!
//! `SUPPORTED_LOCALES` is the locale contract, copied from the installer's
//! language list, and every one of them has a catalog. A supported-but-
//! untranslated locale would still resolve to English through the per-key
//! fallback, so shipping a translation is a data change: drop in
//! `data/translations/<code>.json` and add it to `CATALOGS` — the
//! completeness tests at the bottom of this file fail if a catalog is
//! missing a key, carries an unknown key, or loses a `{placeholder}`.
//!
//! This covers the app's own strings only. GTK's chrome is localized by GTK
//! itself from the process locale. The launcher is the one place that cannot
//! read these catalogs: it takes the app name from the desktop entry, so
//! that file carries a copy of `app.name` per locale, and the translated
//! `GenericName`/`Comment` the app itself never shows — tests below fail if
//! the two ever disagree or a locale is missing.

use std::collections::HashMap;
use std::sync::OnceLock;

/// The fallback every catalog leans on, and the locale an unusable
/// environment lands on.
const FALLBACK: &str = "en-US";

/// The locales the UI is expected to support — the installer's language list
/// (`distro/installer/frontend/src/translations`), BCP-47 codes keyed the
/// same way the installer keys its JSON files.
const SUPPORTED_LOCALES: &[&str] = &[
    "ar-SA", "bg-BG", "ca-ES", "de-DE", "el-GR", "en-US", "eo", "es-ES", "fa-IR", "fi-FI",
    "fr-FR", "he-IL", "hu-HU", "id-ID", "it-IT", "ja-JP", "ko-KR", "nb-NO", "nl-NL", "pl-PL",
    "pt-BR", "ru-RU", "sv-SE", "tr-TR", "uk-UA", "vi-VN", "zh-CN", "zh-TW",
];

/// The right-to-left locales in the supported list — the same set the
/// installer frontend carries as `RTL_LOCALES`. The window direction follows
/// this (see `is_rtl` and `ui::window::apply_rtl`).
const RTL_LOCALES: &[&str] = &["ar-SA", "fa-IR", "he-IL"];

/// The catalogs compiled into the binary. English must be here (it is the
/// fallback); the completeness test checks that every supported locale is too.
const CATALOGS: &[(&str, &str)] = &[
    ("ar-SA", include_str!("../data/translations/ar-SA.json")),
    ("bg-BG", include_str!("../data/translations/bg-BG.json")),
    ("ca-ES", include_str!("../data/translations/ca-ES.json")),
    ("de-DE", include_str!("../data/translations/de-DE.json")),
    ("el-GR", include_str!("../data/translations/el-GR.json")),
    ("en-US", include_str!("../data/translations/en-US.json")),
    ("eo", include_str!("../data/translations/eo.json")),
    ("es-ES", include_str!("../data/translations/es-ES.json")),
    ("fa-IR", include_str!("../data/translations/fa-IR.json")),
    ("fi-FI", include_str!("../data/translations/fi-FI.json")),
    ("fr-FR", include_str!("../data/translations/fr-FR.json")),
    ("he-IL", include_str!("../data/translations/he-IL.json")),
    ("hu-HU", include_str!("../data/translations/hu-HU.json")),
    ("id-ID", include_str!("../data/translations/id-ID.json")),
    ("it-IT", include_str!("../data/translations/it-IT.json")),
    ("ja-JP", include_str!("../data/translations/ja-JP.json")),
    ("ko-KR", include_str!("../data/translations/ko-KR.json")),
    ("nb-NO", include_str!("../data/translations/nb-NO.json")),
    ("nl-NL", include_str!("../data/translations/nl-NL.json")),
    ("pl-PL", include_str!("../data/translations/pl-PL.json")),
    ("pt-BR", include_str!("../data/translations/pt-BR.json")),
    ("ru-RU", include_str!("../data/translations/ru-RU.json")),
    ("sv-SE", include_str!("../data/translations/sv-SE.json")),
    ("tr-TR", include_str!("../data/translations/tr-TR.json")),
    ("uk-UA", include_str!("../data/translations/uk-UA.json")),
    ("vi-VN", include_str!("../data/translations/vi-VN.json")),
    ("zh-CN", include_str!("../data/translations/zh-CN.json")),
    ("zh-TW", include_str!("../data/translations/zh-TW.json")),
];

/// The process locale, picked up the way glibc would: LC_ALL, then
/// LC_MESSAGES, then LANG.
fn env_locale() -> Option<String> {
    ["LC_ALL", "LC_MESSAGES", "LANG"]
        .iter()
        .find_map(|var| std::env::var(var).ok())
        .filter(|value| !value.is_empty())
}

/// Normalize a locale string (`de_DE.UTF-8`, `de_DE@formal`, `de-DE`, `eo`)
/// to a BCP-47 code (`de-DE`, `eo`). `C`, `POSIX`, and unparseable values
/// return `None` — the caller treats that as "no preference".
fn normalize_locale(raw: &str) -> Option<String> {
    // Strip the codeset and the modifier (`language[_region][.codeset][@mod]`).
    let base = raw.split('@').next()?.split('.').next()?.trim();
    if base.is_empty() || base == "C" || base == "POSIX" {
        return None;
    }
    let mut parts = base.splitn(2, ['_', '-']);
    let language = parts.next()?.trim();
    if language.is_empty() || !language.chars().all(|c| c.is_ascii_alphabetic()) {
        return None;
    }
    let language = language.to_ascii_lowercase();
    // The region is the first segment after the separator (`de_DE_formal` →
    // `DE`); anything that is not two letters is a modifier, not a region.
    let region = parts
        .next()
        .and_then(|r| r.split('_').next())
        .filter(|r| r.len() == 2 && r.chars().all(|c| c.is_ascii_alphabetic()))
        .map(|r| r.to_ascii_uppercase());
    Some(match region {
        Some(region) => format!("{language}-{region}"),
        None => language,
    })
}

/// Pick the active locale for a raw environment value: an exact match on the
/// supported list, else the first supported variant of the same language
/// (`de-AT` → `de-DE`), else English.
fn select_locale(raw: Option<&str>) -> &'static str {
    if let Some(code) = raw.and_then(normalize_locale) {
        if let Some(supported) = SUPPORTED_LOCALES.iter().find(|l| **l == code) {
            return supported;
        }
        let language = code.split('-').next().unwrap_or("");
        if let Some(supported) = SUPPORTED_LOCALES
            .iter()
            .find(|l| l.split('-').next() == Some(language))
        {
            return supported;
        }
    }
    FALLBACK
}

/// Parse one embedded catalog; a catalog that does not parse is an error on
/// stderr and an empty map (the fallback chain keeps the UI in English).
fn catalog(code: &str) -> HashMap<String, String> {
    CATALOGS
        .iter()
        .find(|(c, _)| *c == code)
        .map(|(_, json)| match serde_json::from_str::<HashMap<String, String>>(json) {
            Ok(map) => map,
            Err(err) => {
                eprintln!("i18n: {code}.json does not parse: {err}");
                HashMap::new()
            }
        })
        .unwrap_or_default()
}

/// The active locale's messages with the English fallback behind them.
struct I18n {
    messages: HashMap<String, String>,
    fallback: HashMap<String, String>,
    rtl: bool,
}

impl I18n {
    fn load(locale: &str) -> I18n {
        let fallback = catalog(FALLBACK);
        let messages = if locale == FALLBACK {
            fallback.clone()
        } else {
            catalog(locale)
        };
        I18n {
            rtl: RTL_LOCALES.contains(&locale),
            messages,
            fallback,
        }
    }

    /// Active locale → en-US → the key itself, like the installer frontend.
    /// The result borrows from the catalogs or from `key` (both outlive it).
    fn t<'a>(&'a self, key: &'a str) -> &'a str {
        self.messages
            .get(key)
            .or_else(|| self.fallback.get(key))
            .map_or(key, String::as_str)
    }

    fn t_fmt(&self, key: &str, args: &[(&str, &str)]) -> String {
        interpolate(self.t(key), args)
    }
}

/// Replace `{name}` placeholders the way the installer frontend does: a
/// placeholder whose argument was not supplied interpolates as empty.
fn interpolate(message: &str, args: &[(&str, &str)]) -> String {
    let mut out = String::with_capacity(message.len());
    let mut rest = message;
    while let Some(open) = rest.find('{') {
        out.push_str(&rest[..open]);
        let after = &rest[open + 1..];
        match after.find('}') {
            Some(close) if is_name(&after[..close]) => {
                out.push_str(
                    args.iter()
                        .find(|(name, _)| *name == &after[..close])
                        .map_or("", |(_, value)| *value),
                );
                rest = &after[close + 1..];
            }
            // A `{` that does not open a placeholder stays literal.
            _ => {
                out.push('{');
                rest = after;
            }
        }
    }
    out.push_str(rest);
    out
}

/// Placeholder names are `\w+` — same as the frontend's regex.
fn is_name(name: &str) -> bool {
    !name.is_empty() && name.chars().all(|c| c.is_ascii_alphanumeric() || c == '_')
}

static I18N: OnceLock<I18n> = OnceLock::new();

/// Choose the locale from the environment and load the catalogs. Call once
/// at startup, before any string is built.
pub fn init() {
    let requested = env_locale();
    let locale = select_locale(requested.as_deref());
    eprintln!(
        "i18n: ui language {locale} (requested {})",
        requested.as_deref().unwrap_or("")
    );
    let _ = I18N.set(I18n::load(locale));
}

/// Translate a key, no arguments.
pub fn t(key: &str) -> String {
    I18N.get_or_init(|| I18n::load(select_locale(None)))
        .t(key)
        .to_owned()
}

/// Translate a key and interpolate its `{placeholders}`.
pub fn t_fmt(key: &str, args: &[(&str, &str)]) -> String {
    I18N.get_or_init(|| I18n::load(select_locale(None)))
        .t_fmt(key, args)
}

/// Whether the active locale reads right-to-left. The window layout follows
/// this (the installer frontend does the same through `getIsRTL()`).
pub fn is_rtl() -> bool {
    I18N.get_or_init(|| I18n::load(select_locale(None))).rtl
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn posix_locale_codes_normalize_to_bcp47() {
        assert_eq!(normalize_locale("de_DE.UTF-8").as_deref(), Some("de-DE"));
        assert_eq!(normalize_locale("pt_BR.utf8").as_deref(), Some("pt-BR"));
        assert_eq!(normalize_locale("zh_CN").as_deref(), Some("zh-CN"));
        assert_eq!(normalize_locale("eo.UTF-8").as_deref(), Some("eo"));
        assert_eq!(normalize_locale("en-US").as_deref(), Some("en-US"));
        assert_eq!(normalize_locale("de_DE@formal").as_deref(), Some("de-DE"));
    }

    #[test]
    fn neutral_and_unparseable_locales_are_none() {
        assert_eq!(normalize_locale("C"), None);
        assert_eq!(normalize_locale("POSIX"), None);
        assert_eq!(normalize_locale(""), None);
        assert_eq!(normalize_locale("123"), None);
    }

    #[test]
    fn selection_matches_the_supported_list() {
        assert_eq!(select_locale(Some("de_DE.UTF-8")), "de-DE");
        assert_eq!(select_locale(Some("pt_BR.UTF-8")), "pt-BR");
        assert_eq!(select_locale(Some("eo")), "eo");
    }

    #[test]
    fn unsupported_variants_resolve_to_the_same_language() {
        assert_eq!(select_locale(Some("de_AT.UTF-8")), "de-DE");
        assert_eq!(select_locale(Some("en_GB.UTF-8")), "en-US");
        assert_eq!(select_locale(Some("zh_Hans_CN.UTF-8")), "zh-CN");
    }

    #[test]
    fn unknown_locales_land_on_english() {
        assert_eq!(select_locale(Some("xx_XX.UTF-8")), "en-US");
        assert_eq!(select_locale(Some("C")), "en-US");
        assert_eq!(select_locale(None), "en-US");
    }

    #[test]
    fn a_locale_without_a_catalog_falls_back_to_english_per_key() {
        // No catalog is embedded for this code: every key resolves through
        // English and an unknown key stays itself. This is also what keeps a
        // partially translated future catalog safe, key by key.
        let i18n = I18n::load("hr-HR");
        assert_eq!(i18n.t("app.name"), "Packages");
        assert_eq!(i18n.t("no.such.key"), "no.such.key");
    }

    #[test]
    fn placeholders_interpolate_and_missing_args_are_empty() {
        let i18n = I18n::load("en-US");
        assert_eq!(
            i18n.t_fmt(
                "dialog.install.body",
                &[("name", "htop"), ("version", "3.1-1")]
            ),
            "Do you want to install htop 3.1-1?"
        );
        assert_eq!(
            i18n.t_fmt("dialog.download", &[("size", "541.7 KiB")]),
            "Download: 541.7 KiB."
        );
        assert_eq!(i18n.t_fmt("dialog.download", &[]), "Download: .");
    }

    #[test]
    fn a_brace_that_opens_no_placeholder_stays_literal() {
        // Unclosed, and an empty name: neither is a `{name}` placeholder.
        assert_eq!(interpolate("100% {ok", &[]), "100% {ok");
        assert_eq!(interpolate("{} x", &[]), "{} x");
    }

    #[test]
    fn the_english_catalog_defines_every_key_the_ui_asks_for() {
        // A missing key shows up on screen as the raw key — catch that here
        // instead. Keep this list in step with the t()/t_fmt() call sites.
        let json = CATALOGS
            .iter()
            .find(|(code, _)| *code == FALLBACK)
            .expect("the English catalog is embedded")
            .1;
        let english: HashMap<String, String> =
            serde_json::from_str(json).expect("en-US.json parses");
        for key in [
            "app.name",
            "tabs.installed",
            "tabs.browse",
            "tabs.aelum",
            "tabs.updates",
            "header.refresh",
            "action.install",
            "action.update",
            "action.updateAll",
            "action.uninstall",
            "action.remove",
            "action.cancel",
            "action.close",
            "status.reading",
            "status.refreshing",
            "rows.noMatches",
            "rows.truncated",
            "rows.truncatedShort",
            "tag.installed",
            "tag.installedDep",
            "tag.dependency",
            "installed.search",
            "installed.empty",
            "installed.selfCannotRemove",
            "browse.search",
            "browse.empty",
            "browse.fresh",
            "browse.noDbs",
            "aelum.dbError",
            "aelum.empty",
            "aelum.noRepo",
            "aelum.noRepoBody",
            "updates.countOne",
            "updates.countMany",
            "updates.empty",
            "updates.dbError",
            "dialog.remove.title",
            "dialog.remove.body",
            "dialog.remove.stillNeeded",
            "dialog.remove.orphans",
            "dialog.nameList",
            "dialog.install.title",
            "dialog.update.title",
            "dialog.install.body",
            "dialog.update.body",
            "dialog.download",
            "dialog.installedSize",
            "dialog.alsoInstallsOne",
            "dialog.alsoInstallsMany",
            "fileopen.title",
            "fileopen.body",
            "fileopen.noDesc",
            "fileopen.missingDeps",
            "fileopen.unsigned",
            "txnlog.title",
            "txnlog.running",
            "txnlog.done",
            "txnlog.failed",
            "txn.summaryRefresh",
            "txn.summaryInstall",
            "txn.summaryRemove",
            "txn.summaryUpgrade",
            "txn.summaryFile",
            "toast.busy",
            "toast.noHelper",
            "toast.dbErrors",
            "toast.modelLoad",
            "toast.refreshed",
            "toast.openExtra",
            "toast.openFallbackName",
            "toast.openNotLocal",
            "toast.startFailed",
        ] {
            assert!(english.contains_key(key), "en-US.json is missing {key}");
        }
    }

    #[test]
    fn every_supported_locale_has_an_embedded_catalog() {
        // The two lists are the locale contract; either half drifting is a bug
        // (a supported locale silently stays English, an embedded-but-
        // unsupported catalog is dead weight in the binary).
        for code in SUPPORTED_LOCALES {
            assert!(
                CATALOGS.iter().any(|(c, _)| c == code),
                "{code} is supported but has no catalog"
            );
        }
        for (code, _) in CATALOGS {
            assert!(
                SUPPORTED_LOCALES.contains(code),
                "{code} has a catalog but is not a supported locale"
            );
        }
    }

    #[test]
    fn the_name_the_launcher_shows_is_the_name_the_app_uses() {
        // The launcher cannot read these catalogs: it takes the name from the
        // desktop entry, resolving `Name[<locale>]` the way GLib does —
        // `<language>_<REGION>` first, then `<language>`, never the app's
        // BCP-47 code (measured against this box's glib). Two failures live
        // here: a supported locale with no key the launcher would match, which
        // shows up as an English launcher name over an app that speaks the
        // language; and a name that differs from `app.name`, which makes the
        // launcher and the window title disagree. en-US is the entry's
        // unlocalized `Name`.
        let desktop = include_str!("../data/aelum.packageinstaller.Packages.desktop");
        let localized = localized_values(desktop, "Name");
        assert!(!localized.is_empty(), "the desktop entry has no Name[] keys");
        let base = unlocalized_value(desktop, "Name");
        for code in SUPPORTED_LOCALES {
            let shown = resolve_localized(&localized, base, code);
            let i18n = I18n::load(code);
            let own = i18n.t("app.name");
            assert_eq!(
                shown, own,
                "{code}: the launcher would show {shown:?}, the app calls itself {own:?}"
            );
        }
        assert_no_stale_keys(&localized, "Name");
    }

    #[test]
    fn the_descriptions_the_launcher_shows_exist_for_every_supported_locale() {
        // `GenericName` and `Comment` are the entry's short descriptions. They
        // have no catalog counterpart — the app never shows them — so the
        // guard is coverage: every supported locale must resolve through GLib's
        // preference order to a translated form, never fall through to the
        // unlocalized English. Deleting any `GenericName[…]`/`Comment[…]` key
        // makes its locale fall through and fails this test.
        let desktop = include_str!("../data/aelum.packageinstaller.Packages.desktop");
        for key in ["GenericName", "Comment"] {
            let localized = localized_values(desktop, key);
            assert!(!localized.is_empty(), "the desktop entry has no {key}[] keys");
            let base = unlocalized_value(desktop, key);
            for code in SUPPORTED_LOCALES {
                // en-US is the entry's unlocalized value, like the Name.
                if code == &FALLBACK {
                    continue;
                }
                let shown = resolve_localized(&localized, base, code);
                assert_ne!(
                    shown, base,
                    "{code}: the launcher would show the unlocalized {key}"
                );
            }
            assert_no_stale_keys(&localized, key);
        }
    }

    /// Every `Key[<locale>]=` value in a desktop entry, as (locale-key, value).
    fn localized_values<'a>(desktop: &'a str, key: &str) -> Vec<(&'a str, &'a str)> {
        let prefix = format!("{key}[");
        desktop
            .lines()
            .filter_map(|line| {
                let rest = line.strip_prefix(&prefix)?;
                let (locale, value) = rest.split_once("]=")?;
                Some((locale, value))
            })
            .collect()
    }

    /// The entry's unlocalized value for a key — the value GLib falls back to.
    fn unlocalized_value<'a>(desktop: &'a str, key: &str) -> &'a str {
        desktop
            .lines()
            .find_map(|line| line.strip_prefix(&format!("{key}=")))
            .unwrap_or_else(|| panic!("the desktop entry has no unlocalized {key}"))
    }

    /// Resolve a localized key for one app locale the way GLib does:
    /// `<language>_<REGION>` first, then `<language>`, then the unlocalized base.
    fn resolve_localized<'a>(values: &[(&'a str, &'a str)], base: &'a str, code: &str) -> &'a str {
        let (language, region) = code.split_once('-').unwrap_or((code, ""));
        let qualified = format!("{language}_{}", region.to_ascii_uppercase());
        values
            .iter()
            .find(|(locale, _)| *locale == qualified)
            .or_else(|| values.iter().find(|(locale, _)| *locale == language))
            .map_or(base, |(_, value)| *value)
    }

    /// Every key in the entry belongs to a language the app supports — a key
    /// no catalog backs is a stale one.
    fn assert_no_stale_keys(values: &[(&str, &str)], key: &str) {
        for (locale, _) in values {
            let (language, region) = locale.split_once('_').unwrap_or((locale, ""));
            assert!(
                SUPPORTED_LOCALES.iter().any(|code| {
                    let (l, r) = code.split_once('-').unwrap_or((code, ""));
                    l == language && (region.is_empty() || r.eq_ignore_ascii_case(region))
                }),
                "{key}[{locale}] is not a supported locale"
            );
        }
    }

    #[test]
    fn every_catalog_defines_the_english_key_set_and_placeholders() {
        // A missing key quietly shows the English string, and a lost
        // `{placeholder}` drops its argument from the sentence — both are data
        // bugs that only surface on screen, so they get caught here instead.
        let english: HashMap<String, String> = serde_json::from_str(
            CATALOGS
                .iter()
                .find(|(code, _)| *code == FALLBACK)
                .expect("the English catalog is embedded")
                .1,
        )
        .expect("en-US.json parses");
        for (code, json) in CATALOGS {
            let parsed: HashMap<String, String> = serde_json::from_str(json)
                .unwrap_or_else(|err| panic!("{code}.json does not parse: {err}"));
            let mut got: Vec<&str> = parsed.keys().map(String::as_str).collect();
            let mut want: Vec<&str> = english.keys().map(String::as_str).collect();
            got.sort_unstable();
            want.sort_unstable();
            assert_eq!(got, want, "{code}.json does not define exactly the English keys");
            for (key, template) in &english {
                let value = &parsed[key];
                assert!(!value.trim().is_empty(), "{code}.json has an empty {key}");
                assert_eq!(
                    placeholders(value),
                    placeholders(template),
                    "{code}.json changed the placeholders in {key}"
                );
            }
        }
    }

    #[test]
    fn the_right_to_left_locales_are_flagged() {
        // Mirrors the installer frontend's RTL_LOCALES; the window direction
        // is set from this, so a missing entry means a mirrored UI in English
        // order.
        for code in RTL_LOCALES {
            assert!(I18n::load(code).rtl, "{code} should be right-to-left");
        }
        for code in ["de-DE", "zh-CN", "eo", FALLBACK] {
            assert!(!I18n::load(code).rtl, "{code} should stay left-to-right");
        }
    }

    /// The `{name}` names in a message, sorted and deduplicated — scanned the
    /// same way `interpolate` reads them.
    fn placeholders(message: &str) -> Vec<&str> {
        let mut found = Vec::new();
        let mut rest = message;
        while let Some(open) = rest.find('{') {
            let after = &rest[open + 1..];
            match after.find('}') {
                Some(close) if is_name(&after[..close]) => {
                    found.push(&after[..close]);
                    rest = &after[close + 1..];
                }
                _ => rest = after,
            }
        }
        found.sort_unstable();
        found.dedup();
        found
    }
}

//! Localization (i18n).
//!
//! All UI strings live in `locales.yaml` next to the app (editable at
//! runtime — no recompile needed). An embedded copy is compiled in as a
//! fallback so a missing/broken file can never leave the UI blank.
//!
//! Format:
//! ```yaml
//! en:
//!   toolbar.scan: "Scan"
//! ar:
//!   toolbar.scan: "فحص"
//! ```
//!
//! Keys are flat dotted names. `tr()` looks up the active language,
//! falls back to English, and finally to the key itself. `trf()` does
//! `{name}`-style placeholder substitution on top.
//!
//! Arabic needs two extras standard egui lacks:
//! * glyph coverage — a system font (Segoe UI/Tahoma/Arial) is appended
//!   as a fallback in `app::fonts`;
//! * shaping + RTL order — egui rasterizes glyph-by-glyph without a
//!   shaping engine, so `tr`/`trf` return display-ready text: letters
//!   are reshaped to their contextual forms (`arabic_reshaper`) and the
//!   string is reordered to visual order (`unicode_bidi`).

use std::borrow::Cow;
use std::collections::BTreeMap;
use std::fmt::Display;
use std::sync::{LazyLock, RwLock};

/// Compiled-in baseline — `locales.yaml` at the project root.
const EMBEDDED: &str = include_str!("../locales.yaml");

/// On-disk filename both apps can edit live.
const LOCALES_FILE: &str = "locales.yaml";

pub const LANGUAGE_ENGLISH: &str = "en-US";
pub const LANGUAGE_ARABIC: &str = "ar-SA";

struct Locales {
    /// "en" | "ar" — normalized from the `language` tag in config.yaml.
    lang: &'static str,
    active: BTreeMap<String, String>,
    english: BTreeMap<String, String>,
}

static LOCALES: LazyLock<RwLock<Locales>> = LazyLock::new(|| {
    let tables = load_tables();
    let english = tables.get("en").cloned().unwrap_or_default();
    RwLock::new(Locales {
        lang: "en",
        active: english.clone(),
        english,
    })
});

/// Parse `locales.yaml` — the on-disk file wins, embedded data is the
/// fallback. Both parse into `lang -> {key: text}`.
fn load_tables() -> BTreeMap<String, BTreeMap<String, String>> {
    for content in [std::fs::read_to_string(LOCALES_FILE).ok(), Some(EMBEDDED.to_string())]
        .into_iter()
        .flatten()
    {
        if let Ok(tables) =
            serde_yaml::from_str::<BTreeMap<String, BTreeMap<String, String>>>(&content)
        {
            return tables;
        }
    }
    BTreeMap::new()
}

/// BCP-47-ish tag -> our short code. Anything `ar*` is Arabic,
/// everything else is English.
fn normalize_lang(tag: &str) -> &'static str {
    if tag.trim_start().to_lowercase().starts_with("ar") {
        "ar"
    } else {
        "en"
    }
}

/// Load `locales.yaml` and select `language_tag` (`language` from the
/// shared config.yaml — e.g. `en-US`, `ar-SA`). Called once at startup.
pub fn init(language_tag: &str) {
    let tables = load_tables();
    let lang = normalize_lang(language_tag);
    if let Ok(mut locales) = LOCALES.write() {
        locales.english = tables.get("en").cloned().unwrap_or_default();
        locales.lang = lang;
        locales.active = tables
            .get(lang)
            .cloned()
            .unwrap_or_else(|| locales.english.clone());
    }
}

/// Switch language at runtime (after a settings save).
pub fn set_language(language_tag: &str) {
    init(language_tag);
}

/// Short code of the active language ("en" | "ar").
pub fn lang() -> &'static str {
    LOCALES.read().map(|l| l.lang).unwrap_or("en")
}

/// Whether the active language is right-to-left.
pub fn is_rtl() -> bool {
    lang() == "ar"
}

/// Raw lookup: active language -> English -> the key itself.
fn lookup(key: &str) -> String {
    let Ok(locales) = LOCALES.read() else {
        return key.to_string();
    };
    locales
        .active
        .get(key)
        .or_else(|| locales.english.get(key))
        .cloned()
        .unwrap_or_else(|| key.to_string())
}

/// Chars in the NOMINAL Arabic blocks — these are what `reshape`
/// converts into presentation forms. A string containing only
/// presentation forms (FB50–FDFF, FE70–FEFF) is already display-ready.
fn contains_nominal_arabic(text: &str) -> bool {
    text.chars().any(|c| {
        matches!(
            c,
            '\u{0600}'..='\u{06FF}' | '\u{0750}'..='\u{077F}' | '\u{08A0}'..='\u{08FF}'
        )
    })
}

/// Any Arabic at all — nominal or presentation forms.
#[cfg(test)]
fn contains_arabic(text: &str) -> bool {
    text.chars().any(|c| {
        matches!(
            c,
            '\u{0600}'..='\u{06FF}'
                | '\u{0750}'..='\u{077F}'
                | '\u{08A0}'..='\u{08FF}'
                | '\u{FB50}'..='\u{FDFF}'
                | '\u{FE70}'..='\u{FEFF}'
        )
    })
}

/// True for codepoints in the Arabic blocks — the characters the
/// embedded Amiri font serves (nominal + presentation forms).
fn is_arabic_char(c: char) -> bool {
    matches!(c as u32,
        0x0600..=0x06FF | 0x0750..=0x077F | 0x08A0..=0x08FF
        | 0xFB50..=0xFDFF | 0xFE70..=0xFEFF)
}

/// Shape text for display in egui.
///
/// epaint does NOT shape or reorder text itself — it splits a string
/// into runs **by font face** (Arabic codepoints resolve to Amiri;
/// spaces, digits and Latin resolve to the leading Latin fonts) and
/// hands each run to harfrust, which guesses the run's direction and
/// emits it in visual order. So each Arabic run arrives back
/// letter-reversed, but run *order* is always left-to-right as fed.
///
/// We therefore hand egui the reshaped string with its font-runs
/// already in visual order while keeping each run's letters in logical
/// order — harfrust performs the per-run letter reversal:
///
///   logical "بدء النسخ"  ->  runs [بدء][" "][النسخ]
///   -> reversed runs "النسخ بدء" (letters still logical)
///   -> hb reverses each Arabic run -> paints النسخ(left) بدء(right) ✓
///
/// `reorder_line` is NOT used — it flips letters too, which harfrust
/// then un-flips, producing mirrored words.
///
/// Triggered by CONTENT, not language state — any string containing
/// nominal Arabic is processed regardless of the active locale, so raw
/// Arabic can never reach a widget. Idempotent: output has no nominal
/// Arabic, so feeding it back through is a no-op.
fn shape(text: &str) -> Cow<'_, str> {
    use unicode_bidi::BidiClass;
    if !contains_nominal_arabic(text) {
        return Cow::Borrowed(text);
    }
    let reshaped = arabic_reshaper::arabic_reshape(text);

    // Paragraph direction from the first STRONG character: Arabic-led
    // text is an RTL paragraph (whole line mirrors); a Latin-led string
    // keeps run order so an embedded Arabic name still renders in place.
    let mut rtl = false;
    for c in reshaped.chars() {
        match unicode_bidi::bidi_class(c) {
            BidiClass::AL | BidiClass::R => {
                rtl = true;
                break;
            }
            BidiClass::L => break,
            _ => {}
        }
    }

    let reorder_runs = |line: &str| -> String {
        let mut runs: Vec<&str> = Vec::new();
        let mut start = 0usize;
        let mut prev: Option<bool> = None;
        for (i, c) in line.char_indices() {
            let a = is_arabic_char(c);
            if prev.is_some_and(|p| p != a) {
                runs.push(&line[start..i]);
                start = i;
            }
            prev = Some(a);
        }
        runs.push(&line[start..]);
        if rtl {
            runs.reverse();
        }
        runs.concat()
    };

    let out = reshaped
        .split('\n')
        .map(reorder_runs)
        .collect::<Vec<_>>()
        .join("\n");
    Cow::Owned(out)
}

/// Shape arbitrary user data (game names, paths) for display — same
/// shaping pipeline as `tr`, but for strings that aren't locale keys.
pub fn display(text: &str) -> String {
    shape(text).into_owned()
}

/// Translate `key` for the active language (display-ready).
pub fn tr(key: &str) -> String {
    shape(&lookup(key)).into_owned()
}

/// Translate + substitute `{name}` placeholders, e.g.
/// `trf("scan.done", &[("games", &games), ("found", &found)])`.
pub fn trf(key: &str, args: &[(&str, &dyn Display)]) -> String {
    let mut text = lookup(key);
    for (name, value) in args {
        text = text.replace(&format!("{{{name}}}"), &value.to_string());
    }
    shape(&text).into_owned()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;

    /// `LOCALES` is a process-wide global — serialize tests so language
    /// switches can't race.
    static LOCK: Mutex<()> = Mutex::new(());

    #[test]
    fn dump_shaping() {
        let input = "الإعدادات";
        println!(
            "input:    {}",
            input
                .chars()
                .map(|c| format!("U+{:04X}", c as u32))
                .collect::<Vec<_>>()
                .join(" ")
        );
        let out = shape(input);
        println!(
            "output:   {}",
            out.chars()
                .map(|c| format!("U+{:04X}", c as u32))
                .collect::<Vec<_>>()
                .join(" ")
        );
        println!("final: {out}");
    }

    #[test]
    fn font_coverage_probe() {
        use ab_glyph::Font;
        for path in [
            "C:/Windows/Fonts/segoeui.ttf",
            "C:/Windows/Fonts/tahoma.ttf",
            "C:/Windows/Fonts/arial.ttf",
            "C:/Windows/Fonts/tradbdo.ttf",
            "C:/Windows/Fonts/arabtype.ttf",
        ] {
            let Ok(bytes) = std::fs::read(path) else {
                println!("{path}: MISSING");
                continue;
            };
            let Some(font) = ab_glyph::FontVec::try_from_vec(bytes.clone()).ok() else {
                println!("{path}: unparseable");
                continue;
            };
            let nominal = ['\u{0627}', '\u{062A}']
                .iter()
                .all(|c| font.glyph_id(*c).0 != 0);
            let forms = ['\u{FE8D}', '\u{FE95}', '\u{FEF9}', '\u{FECB}']
                .iter()
                .all(|c| font.glyph_id(*c).0 != 0);
            // Distinct glyph IDs? If U+FE95 maps to the same glyph as
            // U+062A, the font collapses presentation forms onto
            // nominal glyphs -> renders disconnected anyway.
            let collapsed = ['\u{FE95}', '\u{FE8D}', '\u{FECB}', '\u{FEA9}']
                .iter()
                .zip(['\u{062A}', '\u{0627}', '\u{0639}', '\u{062F}'].iter())
                .filter(|(f, n)| font.glyph_id(**f) == font.glyph_id(**n))
                .count();
            println!("{path}: nominal={nominal} forms={forms} collapsed_to_nominal={collapsed}/4");
        }

        // The EMBEDDED font must render presentation forms correctly —
        // this is the fallback the whole Arabic UI depends on.
        // What matters: INITIAL/MEDIAL forms need distinct glyphs
        // (isolated forms legitimately share the nominal glyph).
        let bytes = std::fs::read(concat!(env!("CARGO_MANIFEST_DIR"), "/assets/Amiri-Regular.ttf"))
            .expect("assets/Amiri-Regular.ttf");
        let font = ab_glyph::FontVec::try_from_vec(bytes).expect("parse Amiri");
        // (joining form codepoint, expected-different nominal, label)
        let checks = [
            ('\u{FECB}', '\u{0639}', "AIN INITIAL"),
            ('\u{FECE}', '\u{0639}', "AIN MEDIAL"),
            ('\u{FE92}', '\u{0628}', "BEH INITIAL"),
            ('\u{FE94}', '\u{0628}', "BEH MEDIAL"),
            ('\u{FEB4}', '\u{0633}', "SEEN INITIAL"),
            ('\u{FEB6}', '\u{0633}', "SEEN MEDIAL"),
            ('\u{FE98}', '\u{062A}', "TEH INITIAL"),
            ('\u{FE9A}', '\u{062A}', "TEH MEDIAL"),
        ];
        for (form, nominal, label) in checks {
            let (fg, ng) = (font.glyph_id(form), font.glyph_id(nominal));
            println!("  {label}: form(U+{:04X})=gid{} nominal(U+{:04X})=gid{} {}",
                form as u32, fg.0, nominal as u32, ng.0,
                if fg.0 != 0 && fg != ng { "OK" } else { "BAD" });
            assert!(fg.0 != 0 && fg != ng, "{label} cannot render joined form");
        }
    }

    #[test]
    fn embedded_tables_parse() {
        let tables = load_tables();
        let en = tables.get("en").expect("en table");
        let ar = tables.get("ar").expect("ar table");
        assert!(!en.is_empty());
        assert!(!ar.is_empty());
        // Key sanity: a core key exists in both.
        assert!(en.contains_key("toolbar.scan"));
        assert!(ar.contains_key("toolbar.scan"));
    }

    #[test]
    fn lookup_falls_back_to_english_then_key() {
        let _g = LOCK.lock().unwrap();
        init("en-US");
        assert_eq!(tr("toolbar.scan"), "Scan");
        // Missing key -> the key itself, never blank.
        assert_eq!(tr("no.such.key.anywhere"), "no.such.key.anywhere");
    }

    #[test]
    fn arabic_active_missing_key_falls_back_to_english() {
        let _g = LOCK.lock().unwrap();
        init("ar-SA");
        assert_eq!(lang(), "ar");
        // A key only defined in English still renders something.
        assert_eq!(tr("definitely.only.in.test.missing"), "definitely.only.in.test.missing");
        init("en-US");
    }

    #[test]
    fn trf_substitutes_placeholders() {
        let _g = LOCK.lock().unwrap();
        init("en-US");
        let out = trf("scan.done", &[("games", &10usize), ("found", &4usize)]);
        assert_eq!(out, "Scan complete: 10 games scanned, 4 with saves or backups.");
    }

    #[test]
    fn arabic_output_is_shaped_and_reordered() {
        let _g = LOCK.lock().unwrap();
        init("ar-SA");
        let raw = lookup("toolbar.scan");
        assert!(contains_arabic(&raw));
        // Display form differs: contextual letterforms + visual order.
        let display = tr("toolbar.scan");
        assert!(!display.is_empty());
        assert!(contains_arabic(&display));
        init("en-US");
    }

    #[test]
    fn multi_word_arabic_reorders_to_rtl_visual() {
        let _g = LOCK.lock().unwrap();
        init("en-US"); // shaping is content-driven; locale irrelevant
        // epaint/harfrust reverses each font-run internally, so the
        // display string must carry WORDS in visual order while each
        // word's letters stay logical: "بدء النسخ" is handed to egui as
        // "النسخ بدء" (logical letters); hb then flips each word at
        // paint time and بدء lands on the right — correct RTL.
        let v2 = display("بدء النسخ");
        let expected2 = format!(
            "{} {}",
            arabic_reshaper::arabic_reshape("النسخ"),
            arabic_reshaper::arabic_reshape("بدء")
        );
        assert_eq!(v2, expected2);
        assert!(!contains_nominal_arabic(&v2));

        // The report's other example: "تحديث قاعدة البيانات" ->
        // handed to egui as "البيانات قاعدة تحديث" (logical letters).
        let v3 = display("تحديث قاعدة البيانات");
        let expected3 = format!(
            "{} {} {}",
            arabic_reshaper::arabic_reshape("البيانات"),
            arabic_reshaper::arabic_reshape("قاعدة"),
            arabic_reshaper::arabic_reshape("تحديث")
        );
        assert_eq!(v3, expected3);
        assert!(!contains_nominal_arabic(&v3));
    }

    #[test]
    fn shaping_is_content_driven_and_idempotent() {
        let _g = LOCK.lock().unwrap();
        // Even under English UI, a string containing Arabic MUST be
        // shaped — locale state can never leak raw Arabic to widgets.
        init("en-US");
        let shaped = display("الإعدادات");
        assert!(!contains_nominal_arabic(&shaped));
        assert!(contains_arabic(&shaped));
        // Feeding shaped text back through is a no-op (idempotent):
        // the visual order must not be reversed a second time.
        assert_eq!(display(&shaped), shaped);
        // Non-Arabic strings pass through untouched.
        assert_eq!(display("English text 123"), "English text 123");
    }
}

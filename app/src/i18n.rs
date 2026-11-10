//! Translations. The program is written in English; `tr("text")` returns the text in the selected
//! language. Languages:
//! - English (built in, the source texts);
//! - Polish (built in, `lang/pl.txt`, compiled into the program);
//! - any `lang/<code>.txt` next to DSO-510-Link.exe - a new language or a corrected Polish one.
//!
//! File format (UTF-8): one `English text = translation` per line, `#` starts a comment line,
//! `\n` in either side stands for a line break. The first line `@name = Polski` gives the name shown
//! in the language menu. Untranslated texts stay in English.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::RwLock;

const BUILTIN: &[(&str, &str)] = &[("pl", include_str!("../lang/pl.txt"))];

#[derive(Clone, Debug)]
pub struct Language {
    pub code: String,
    pub name: String,
    pub source: String,
}

static CURRENT: RwLock<Option<HashMap<&'static str, &'static str>>> = RwLock::new(None);

/// Translated text (the English text itself if there is no translation).
pub fn tr(s: &'static str) -> &'static str {
    if let Ok(g) = CURRENT.read() {
        if let Some(m) = g.as_ref() {
            if let Some(t) = m.get(s) {
                return t;
            }
        }
    }
    s
}

/// `tr` for a template with `{}` placeholders, filled in order.
pub fn trf(template: &'static str, args: &[&dyn std::fmt::Display]) -> String {
    let t = tr(template);
    let mut out = String::with_capacity(t.len() + 16);
    let mut it = args.iter();
    let mut rest = t;
    while let Some(i) = rest.find("{}") {
        out.push_str(&rest[..i]);
        if let Some(a) = it.next() {
            out.push_str(&a.to_string());
        }
        rest = &rest[i + 2..];
    }
    out.push_str(rest);
    out
}

fn unescape(s: &str) -> String {
    s.trim().replace("\\n", "\n")
}

fn parse(text: &str) -> (Option<String>, HashMap<String, String>) {
    let mut name = None;
    let mut map = HashMap::new();
    for line in text.lines() {
        let line = line.trim_start_matches('\u{feff}');
        if line.trim_start().starts_with('#') || line.trim().is_empty() {
            continue;
        }
        if let Some((k, v)) = line.split_once(" = ") {
            let (k, v) = (unescape(k), unescape(v));
            if k == "@name" {
                name = Some(v);
            } else if !v.is_empty() {
                map.insert(k, v);
            }
        }
    }
    (name, map)
}

pub fn lang_dir() -> PathBuf {
    std::env::current_exe().ok().and_then(|p| p.parent().map(|d| d.join("lang"))).unwrap_or_else(|| PathBuf::from("lang"))
}

/// English + built-in + files in lang_dir().
pub fn available() -> Vec<Language> {
    let mut v = vec![Language { code: "en".into(), name: "English".into(), source: "built in".into() }];
    for (code, text) in BUILTIN {
        let (name, _) = parse(text);
        v.push(Language { code: code.to_string(), name: name.unwrap_or_else(|| code.to_string()), source: "built in".into() });
    }
    if let Ok(rd) = std::fs::read_dir(lang_dir()) {
        for e in rd.flatten() {
            let p = e.path();
            if p.extension().and_then(|x| x.to_str()) != Some("txt") {
                continue;
            }
            let Some(code) = p.file_stem().and_then(|s| s.to_str()).map(|s| s.to_lowercase()) else { continue };
            let Ok(text) = std::fs::read_to_string(&p) else { continue };
            let (name, _) = parse(&text);
            let lang = Language { code: code.clone(), name: name.unwrap_or_else(|| code.clone()), source: p.display().to_string() };
            match v.iter_mut().find(|l| l.code == code) {
                Some(l) => *l = lang,
                None => v.push(lang),
            }
        }
    }
    v
}

/// Switch language; an external file overrides/extends the built-in table. Returns the number of
/// translated texts.
pub fn set_language(code: &str) -> usize {
    let mut map: HashMap<String, String> = HashMap::new();
    if code != "en" {
        if let Some((_, text)) = BUILTIN.iter().find(|(c, _)| *c == code) {
            map.extend(parse(text).1);
        }
        if let Ok(text) = std::fs::read_to_string(lang_dir().join(format!("{code}.txt"))) {
            map.extend(parse(&text).1);
        }
    }
    let n = map.len();
    // leaked on purpose: a handful of language switches per run at most
    let table: HashMap<&'static str, &'static str> =
        map.into_iter().map(|(k, v)| (&*Box::leak(k.into_boxed_str()), &*Box::leak(v.into_boxed_str()))).collect();
    if let Ok(mut g) = CURRENT.write() {
        *g = if code == "en" { None } else { Some(table) };
    }
    n
}

/// Writes a template with every English text of the program for a new translation.
pub fn write_template(path: &std::path::Path) -> std::io::Result<()> {
    let (_, pl) = parse(BUILTIN[0].1);
    let mut keys: Vec<&String> = pl.keys().collect();
    keys.sort();
    let mut s = String::from("# DSO-510 Link translation. Save as lang/<code>.txt next to DSO-510-Link.exe.\n# Format: English text = translation\n@name = My language\n");
    for k in keys {
        s.push_str(&format!("{} = \n", k.replace('\n', "\\n")));
    }
    std::fs::write(path, s)
}

// ---- settings (language) ------------------------------------------------------------------

fn settings_path() -> PathBuf {
    std::env::var_os("APPDATA").map(PathBuf::from).unwrap_or_else(|| PathBuf::from(".")).join("DSO-510-Link").join("settings.txt")
}

pub fn load_setting(key: &str) -> Option<String> {
    let text = std::fs::read_to_string(settings_path()).ok()?;
    text.lines().find_map(|l| l.split_once('=').filter(|(k, _)| k.trim() == key).map(|(_, v)| v.trim().to_string()))
}

pub fn save_setting(key: &str, value: &str) {
    let p = settings_path();
    let mut lines: Vec<String> = std::fs::read_to_string(&p).unwrap_or_default().lines().filter(|l| l.split_once('=').map(|(k, _)| k.trim() != key).unwrap_or(true)).map(String::from).collect();
    lines.push(format!("{key}={value}"));
    if let Some(d) = p.parent() {
        let _ = std::fs::create_dir_all(d);
    }
    let _ = std::fs::write(p, lines.join("\n") + "\n");
}

/// Saved language, else Polish.
pub fn initial_language() -> String {
    if let Some(l) = load_setting("language") {
        return l;
    }
    // Polish by default (the program's main audience); English if the system says otherwise and asks for it
    let lang = std::env::var("LANG").unwrap_or_default().to_lowercase();
    if lang.starts_with("en") && !system_is_polish() { "en".into() } else { "pl".into() }
}

#[cfg(windows)]
fn system_is_polish() -> bool {
    // LANGID of the user interface language: primary language Polish = 0x15
    unsafe extern "system" {
        fn GetUserDefaultUILanguage() -> u16;
    }
    (unsafe { GetUserDefaultUILanguage() } & 0x3FF) == 0x15
}

#[cfg(not(windows))]
fn system_is_polish() -> bool {
    false
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn polish_table_parses_and_formats() {
        let (name, map) = parse(BUILTIN[0].1);
        assert_eq!(name.as_deref(), Some("Polski"));
        assert!(map.len() > 100, "{}", map.len());
        for (k, v) in &map {
            assert_eq!(k.matches("{}").count(), v.matches("{}").count(), "placeholders differ: {k}");
        }
    }
}

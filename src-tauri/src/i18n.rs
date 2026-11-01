//! The few renderer strings Rust shows itself, picked from src/locales by build.rs.

use std::sync::OnceLock;

use serde_json::Value;

use crate::store::Store;

fn strings() -> &'static Value {
    static STRINGS: OnceLock<Value> = OnceLock::new();
    STRINGS.get_or_init(|| serde_json::from_str(include_str!(concat!(env!("OUT_DIR"), "/strings.json"))).unwrap_or(Value::Null))
}

/// i18next-style lookup in the user's language: `{{name}}` placeholders, and `key_one`/`key_other`
/// when a `count` var is given. Falls back to the base language, then English, then the key.
// shortcut: plural picks one/other by English rules; languages with more plural forms get `_other`.
pub fn t(store: &Store, ns: &str, key: &str, vars: &[(&str, &str)]) -> String {
    let language = store.get("", "language").ok().and_then(|v| v.as_str().map(str::to_string)).unwrap_or_default();
    tr(&language, ns, key, vars)
}

/// `t` for a language the caller already has.
pub fn tr(language: &str, ns: &str, key: &str, vars: &[(&str, &str)]) -> String {
    translate(strings(), language, ns, key, vars)
}

fn translate(strings: &Value, language: &str, ns: &str, key: &str, vars: &[(&str, &str)]) -> String {
    let plural = vars.iter().find(|(name, _)| *name == "count").map(|(_, c)| if *c == "1" { "_one" } else { "_other" });
    let base = language.split('-').next().unwrap_or_default();
    let template = [language, base, "en"]
        .iter()
        .flat_map(|lang| {
            let table = &strings[*lang][ns];
            [plural.map(|p| table[format!("{key}{p}")].clone()), Some(table[key].clone())]
        })
        .flatten()
        .find_map(|v| v.as_str().filter(|s| !s.is_empty()).map(str::to_string))
        .unwrap_or_else(|| key.to_string());
    vars.iter().fold(template, |text, (name, value)| text.replace(&format!("{{{{{name}}}}}"), value))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn plurals_placeholders_and_fallback() {
        let strings = json!({
            "en": { "n": { "count_one": "{{count}} repack", "count_other": "{{count}} repacks", "only_en": "Hi {{name}}" } },
            "pt": { "n": { "count_other": "{{count}} pacotes" } },
        });
        assert_eq!(translate(&strings, "en", "n", "count", &[("count", "1")]), "1 repack");
        assert_eq!(translate(&strings, "en", "n", "count", &[("count", "3")]), "3 repacks");
        assert_eq!(translate(&strings, "pt-BR", "n", "count", &[("count", "3")]), "3 pacotes");
        assert_eq!(translate(&strings, "pt-BR", "n", "only_en", &[("name", "Ana")]), "Hi Ana");
        assert_eq!(translate(&strings, "", "n", "missing", &[]), "missing");
    }

    #[test]
    fn real_strings_are_embedded() {
        assert_eq!(translate(strings(), "en", "notifications", "download_complete", &[]), "Download complete");
    }
}

//! Translation lookup. The CSV is baked in at compile time via `include_str!`
//! so we don't depend on Bevy's asset pipeline for static UI text.
//!
//! Format: `key,<lang1>,<lang2>,…` header line, then `key,<text1>,<text2>,…`
//! per row. To support a new language, add a column to `data/translations.csv`
//! and switch `LANGUAGE` (eventually a runtime resource).
//!
//! Values may not contain commas in this minimal parser. If a translation
//! needs commas, swap to a real CSV parser or change the separator here.

use std::collections::HashMap;
use std::sync::OnceLock;

/// Active language column. Hardcoded to English for now; lift to a resource
/// later if we need runtime switching.
pub const LANGUAGE: &str = "en";

const RAW_CSV: &str = include_str!("../data/translations.csv");

static TABLE: OnceLock<HashMap<&'static str, &'static str>> = OnceLock::new();

fn table() -> &'static HashMap<&'static str, &'static str> {
    TABLE.get_or_init(|| {
        let mut lines = RAW_CSV.lines();
        let header = lines.next().unwrap_or("");
        let cols: Vec<&str> = header.split(',').collect();
        let lang_idx = cols.iter().position(|c| c.trim() == LANGUAGE).unwrap_or(1);
        let mut map = HashMap::new();
        for line in lines {
            if line.trim().is_empty() { continue; }
            let parts: Vec<&str> = line.split(',').collect();
            if parts.len() > lang_idx {
                // Both key and value are leaked into 'static via `RAW_CSV`'s
                // own 'static lifetime — `lines()` returns &'static str slices
                // when the source is &'static str, so we can store them directly.
                map.insert(parts[0], parts[lang_idx]);
            }
        }
        map
    })
}

/// Return the localized string for `key`. Falls back to the key itself when
/// missing — that way a missing translation surfaces in the UI instead of
/// silently rendering empty.
pub fn tr(key: &str) -> &'static str {
    table().get(key).copied().unwrap_or_else(|| {
        // Leak the unknown key as 'static so we still return a stable ref.
        // This only happens when a key is missing; in practice none should.
        Box::leak(key.to_owned().into_boxed_str())
    })
}

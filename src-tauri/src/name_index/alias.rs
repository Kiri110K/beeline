//! The Alias Dictionary (SPEC §6, glossary): a user-editable mapping from typed words
//! to Locations. A query that matches an alias word *recommends* that Location — the
//! target surfaces as a top result — and never filters other results (the ranker adds
//! the target as one more candidate; it removes nothing).
//!
//! Storage: `aliases.json` in the app data dir, a flat `{ "word": "location" }` object
//! (e.g. `{ "загрузки": "~/Downloads" }`). It is user-editable via Settings in a later
//! chunk and loaded lazily — hot-reload is not required, so the file is read once on the
//! first search that needs it. A leading `~` in a target expands to the index root
//! (home); other targets are taken verbatim.

use std::{
    collections::HashMap,
    path::{Path, PathBuf},
};

/// Word → Location map. Keys are lowercased so lookup is case-insensitive.
pub struct AliasDictionary {
    map: HashMap<String, PathBuf>,
}

impl AliasDictionary {
    /// An empty dictionary (no aliases). Used before any file is loaded and in tests.
    pub fn empty() -> Self {
        Self {
            map: HashMap::new(),
        }
    }

    /// Build directly from word → Location pairs (the Settings seam and tests).
    pub fn from_pairs<I, K, V>(pairs: I, home: &Path) -> Self
    where
        I: IntoIterator<Item = (K, V)>,
        K: Into<String>,
        V: AsRef<str>,
    {
        let map = pairs
            .into_iter()
            .map(|(word, location)| {
                (
                    word.into().trim().to_lowercase(),
                    expand_home(location.as_ref(), home),
                )
            })
            .collect();
        Self { map }
    }

    /// Load `aliases.json`, returning an empty dictionary if the file is absent or
    /// malformed (a broken alias file must never take Search down).
    pub fn load(path: &Path, home: &Path) -> Self {
        let Ok(bytes) = std::fs::read(path) else {
            return Self::empty();
        };
        let Ok(raw) = serde_json::from_slice::<HashMap<String, String>>(&bytes) else {
            return Self::empty();
        };
        Self::from_pairs(raw, home)
    }

    /// Resolve a whole query to its recommended Location, if it names an alias word.
    /// The query is matched in full (trimmed, case-insensitive); a partial word is not
    /// an alias hit, so aliases stay predictable.
    pub fn resolve(&self, query_lower: &str) -> Option<&Path> {
        self.map.get(query_lower.trim()).map(PathBuf::as_path)
    }
}

/// Expand a leading `~` (optionally `~/…`) to the index root; leave anything else as is.
fn expand_home(location: &str, home: &Path) -> PathBuf {
    if location == "~" {
        return home.to_path_buf();
    }
    if let Some(rest) = location.strip_prefix("~/") {
        return home.join(rest);
    }
    PathBuf::from(location)
}

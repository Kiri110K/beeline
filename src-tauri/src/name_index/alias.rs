//! The Alias Dictionary (SPEC §6, glossary): a user-editable mapping from typed words
//! to Locations. A query that matches an alias word *recommends* that Location — the
//! target surfaces as a top result — and never filters other results (the ranker adds
//! the target as one more candidate; it removes nothing).
//!
//! Storage: the Alias Dictionary lives in the settings store (SPEC §12); the Name Index is
//! handed word → Location pairs at init and again on each Settings change, so the dictionary
//! is rebuilt in place rather than read from a file here. A leading `~` in a target expands
//! to the index root (home); other targets are taken verbatim. (The legacy standalone
//! `aliases.json` is absorbed into the settings store on first run — see `settings.rs`.)

use std::{
    collections::HashMap,
    path::{Path, PathBuf},
};

/// Word → Location map. Keys are lowercased so lookup is case-insensitive.
pub struct AliasDictionary {
    map: HashMap<String, PathBuf>,
}

impl AliasDictionary {
    /// An empty dictionary (no aliases). Used by the ranker tests.
    #[cfg(test)]
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

//! Junk classification (SPEC §6).
//!
//! Junk is decided purely by path *shape*: a path is Junk if any of its components
//! is a known dependency / build / cache / agent-session directory name. This is the
//! one place the pattern list lives; a later Settings chunk will replace
//! [`JunkPatterns::default`] with a user-editable source — the classification API
//! stays the same, so nothing else has to change (the seam).

use std::{collections::HashSet, path::Path};

use crate::name_index::model::Tier;

/// The set of path-component names that mark a subtree as Junk.
pub struct JunkPatterns {
    names: HashSet<String>,
    /// Lowercased copies, for the case-insensitive "does this query target Junk?" check.
    lowered: Vec<String>,
}

impl JunkPatterns {
    /// Build from an explicit list of component names (the Settings seam).
    pub fn from_names<I, S>(names: I) -> Self
    where
        I: IntoIterator<Item = S>,
        S: Into<String>,
    {
        let names: HashSet<String> = names.into_iter().map(Into::into).collect();
        let lowered = names.iter().map(|name| name.to_lowercase()).collect();
        Self { names, lowered }
    }

    /// Classify a path relative to the index root.
    ///
    /// Junk wins over Hidden: a `.git` or `.cache` directory contains a Junk-named
    /// component and is therefore Junk, not merely Hidden. A path is Hidden only when
    /// it has a dot-component and no Junk-component at all.
    pub fn classify(&self, relative: &Path) -> Tier {
        let mut hidden = false;
        for component in relative.components() {
            if let std::path::Component::Normal(name) = component {
                let name = name.to_string_lossy();
                if self.names.contains(name.as_ref()) {
                    return Tier::Junk;
                }
                if name.starts_with('.') {
                    hidden = true;
                }
            }
        }
        if hidden {
            Tier::Hidden
        } else {
            Tier::Normal
        }
    }

    /// Whether a query text plausibly targets Junk (contains a Junk name, case-insensitive).
    /// Used to trigger the lazy rescan of dirty Junk directories before searching.
    pub fn query_targets_junk(&self, query_lowercase: &str) -> bool {
        self.lowered
            .iter()
            .any(|name| query_lowercase.contains(name))
    }
}

/// The built-in v1 Junk names (SPEC §6). `Caches` covers `~/Library/Caches`. This is the
/// single source of the seed list: [`JunkPatterns::default`] uses it, and the settings store
/// seeds a fresh install's editable list from it (SPEC §12), so the two never drift.
pub(crate) fn default_names() -> Vec<String> {
    [
        "node_modules",
        ".git",
        "target",
        ".build",
        "dist",
        ".cache",
        "Caches",
        ".claude",
        ".codex",
    ]
    .iter()
    .map(|name| (*name).to_owned())
    .collect()
}

impl Default for JunkPatterns {
    fn default() -> Self {
        Self::from_names(default_names())
    }
}

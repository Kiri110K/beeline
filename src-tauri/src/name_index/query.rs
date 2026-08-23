//! Search over the Name Index.
//!
//! This is the *basic* order for chunk A, not the ranking contract: name-prefix match
//! first, then substring, case-insensitive (Unicode lowercase); ties broken by shorter
//! path, then lexicographic path. The full ranker (Visit Journal, Known Places, Alias
//! Dictionary, tier penalties, layout correction) lands in a later chunk and replaces
//! exactly one function — [`score_match`] — leaving the scan and ordering intact.

use serde::Serialize;

use crate::name_index::model::{IndexData, Tier};

/// Upper bound on candidates collected before ranking. Bounding what we *keep* (not
/// what we scan) caps the sort and path-reconstruction cost so search stays within the
/// §10 Instant budget (≤50 ms) even when a query matches a large fraction of the
/// index. A highly selective query still scans the whole index — that scan is a cheap
/// per-entry lowercase-and-compare, which is the ≤50 ms target on the reference
/// machine. When the cap is hit, results are drawn from crawl order; the real ranker
/// removes this bias next chunk.
const MAX_CANDIDATES: usize = 4096;

/// One search result, shaped for the IPC boundary (camelCase to match the TS schema).
#[derive(Serialize, Debug, Clone, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct SearchHit {
    pub name: String,
    pub path: String,
    pub is_directory: bool,
    /// `"normal" | "hidden" | "junk"`.
    pub tier: &'static str,
}

/// The swappable scorer. Returns `Some(rank)` for a match — `0` = name-prefix,
/// `1` = substring — or `None` for no match. Case-insensitive via Unicode lowercase;
/// `scratch` is reused across calls to avoid per-entry allocation.
fn score_match(query_lower: &str, name: &str, scratch: &mut String) -> Option<u8> {
    scratch.clear();
    for ch in name.chars() {
        for lower in ch.to_lowercase() {
            scratch.push(lower);
        }
    }
    if scratch.starts_with(query_lower) {
        Some(0)
    } else if scratch.contains(query_lower) {
        Some(1)
    } else {
        None
    }
}

struct Candidate {
    rank: u8,
    path: String,
    name: String,
    is_directory: bool,
    tier: Tier,
}

/// Run a search, returning up to `limit` hits in the basic deterministic order.
pub fn search(index: &IndexData, query: &str, limit: usize) -> Vec<SearchHit> {
    let query_lower = query.trim().to_lowercase();
    if query_lower.is_empty() || limit == 0 {
        return Vec::new();
    }

    let mut scratch = String::new();
    let mut candidates: Vec<Candidate> = Vec::new();
    for slot in &index.entries {
        let Some(entry) = slot else { continue };
        let Some(rank) = score_match(&query_lower, &entry.name, &mut scratch) else {
            continue;
        };
        candidates.push(Candidate {
            rank,
            path: index.entry_path(entry).to_string_lossy().into_owned(),
            name: entry.name.to_string(),
            is_directory: entry.is_directory,
            tier: entry.tier,
        });
        if candidates.len() >= MAX_CANDIDATES {
            break;
        }
    }

    // Order: prefix before substring, then shorter path, then lexicographic path.
    candidates.sort_by(|a, b| {
        a.rank
            .cmp(&b.rank)
            .then_with(|| a.path.len().cmp(&b.path.len()))
            .then_with(|| a.path.cmp(&b.path))
    });
    candidates.truncate(limit);

    candidates
        .into_iter()
        .map(|candidate| SearchHit {
            name: candidate.name,
            path: candidate.path,
            is_directory: candidate.is_directory,
            tier: candidate.tier.as_str(),
        })
        .collect()
}

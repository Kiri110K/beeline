//! Read-only q-gram candidate sidecar for Search v2 Typo Correction.
//!
//! The sidecar covers the immutable Name Index base. Overlay Items are scanned separately,
//! so file-system changes remain searchable without rewriting this file. A content hash ties
//! the sidecar to one exact base image; a mismatch starts a background rebuild.

use std::{
    fs::{self, File},
    io::{BufWriter, Write},
    path::Path,
    sync::Mutex,
};

use unicode_normalization::UnicodeNormalization;

use super::{
    model::{IndexData, MappedFile},
    query::{self, Cancel},
};

const MAGIC: &[u8; 8] = b"BLQGM001";
const HEADER_BYTES: usize = 40;
const BUCKETS: usize = 1 << 20;
const RETAINED_WORKSPACES: usize = 1;

#[derive(Debug)]
pub struct CandidateSet {
    pub slots: Vec<u32>,
    pub posting_visits: usize,
    pub aborted: bool,
}

pub struct QGramIndex {
    mapping: MappedFile,
    postings_offset: usize,
    postings: usize,
    source_entries: usize,
    workspaces: Mutex<Vec<DenseWorkspace>>,
}

struct DenseWorkspace {
    counts: Vec<u8>,
    touched: Vec<u32>,
}

impl DenseWorkspace {
    fn new(entries: usize) -> Self {
        Self {
            counts: vec![0; entries],
            touched: Vec::new(),
        }
    }

    fn clear(&mut self) {
        for slot in self.touched.drain(..) {
            self.counts[slot as usize] = 0;
        }
    }
}

impl QGramIndex {
    pub fn open(path: &Path, source_hash: u64, source_entries: usize) -> Option<Self> {
        let mapping = MappedFile::open(path).ok()?;
        let bytes = mapping.bytes();
        if bytes.get(..8) != Some(MAGIC.as_slice())
            || read_u64(bytes, 8)? as usize != BUCKETS
            || read_u64(bytes, 24)? != source_hash
            || read_u64(bytes, 32)? as usize != source_entries
        {
            return None;
        }
        let postings = read_u64(bytes, 16)? as usize;
        let postings_offset = HEADER_BYTES.checked_add((BUCKETS + 1).checked_mul(8)?)?;
        let expected = postings_offset.checked_add(postings.checked_mul(4)?)?;
        if bytes.len() != expected {
            return None;
        }
        Some(Self {
            mapping,
            postings_offset,
            postings,
            source_entries,
            workspaces: Mutex::new(Vec::new()),
        })
    }

    pub fn build(index: &IndexData, path: &Path) -> std::io::Result<()> {
        let source_hash = index
            .base_content_hash()
            .ok_or_else(|| std::io::Error::other("Name Index has no mapped base"))?;
        let source_entries = index.base_entry_count();
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)?;
        }

        let mut counts = vec![0u64; BUCKETS];
        for slot in 0..source_entries {
            let Some(entry) = index.entry(slot) else {
                continue;
            };
            for bucket in gram_buckets(entry.name) {
                counts[bucket as usize] += 1;
            }
        }

        let mut offsets = Vec::with_capacity(BUCKETS + 1);
        offsets.push(0u64);
        for count in counts {
            offsets.push(offsets.last().copied().expect("offset seed") + count);
        }
        let posting_count = *offsets.last().expect("final q-gram offset") as usize;
        let mut positions = offsets[..BUCKETS].to_vec();
        let mut postings = vec![0u32; posting_count];
        for slot in 0..source_entries {
            let Some(entry) = index.entry(slot) else {
                continue;
            };
            for bucket in gram_buckets(entry.name) {
                let position = &mut positions[bucket as usize];
                postings[*position as usize] = slot as u32;
                *position += 1;
            }
        }

        let temporary = path.with_extension("qgram.tmp");
        let file = File::create(&temporary)?;
        let mut writer = BufWriter::new(file);
        writer.write_all(MAGIC)?;
        writer.write_all(&(BUCKETS as u64).to_le_bytes())?;
        writer.write_all(&(posting_count as u64).to_le_bytes())?;
        writer.write_all(&source_hash.to_le_bytes())?;
        writer.write_all(&(source_entries as u64).to_le_bytes())?;
        for offset in offsets {
            writer.write_all(&offset.to_le_bytes())?;
        }
        for slot in postings {
            writer.write_all(&slot.to_le_bytes())?;
        }
        writer.flush()?;
        writer.get_ref().sync_all()?;
        fs::rename(&temporary, path)?;
        Ok(())
    }

    pub fn candidates(&self, query_text: &str, cancel: &Cancel<'_>) -> CandidateSet {
        let mut workspace = self.take_workspace();
        let mut candidates = Vec::new();
        let mut posting_visits = 0usize;
        for variant in query_variants(query_text) {
            for token in query_tokens(&variant) {
                if self.collect_token_candidates(
                    token,
                    cancel,
                    &mut candidates,
                    &mut posting_visits,
                    &mut workspace,
                ) {
                    self.return_workspace(workspace);
                    return aborted(posting_visits);
                }
            }
        }
        self.return_workspace(workspace);
        collected(candidates, posting_visits)
    }

    /// Literal candidate pool for the final token of an ordered multi-token interpretation.
    /// Every trigram must occur, so this is much narrower than the typo-safe global pool.
    /// Search v2 fully verifies these Items first, then runs the unchanged complete retrieval;
    /// this method changes arrival time, not final recall or ranking.
    pub fn ordered_tail_literal_candidates(
        &self,
        query_text: &str,
        cancel: &Cancel<'_>,
    ) -> Option<CandidateSet> {
        let mut candidates = Vec::new();
        let mut posting_visits = 0usize;
        let mut found_multi = false;
        for variant in query_variants(query_text) {
            let tokens = query_tokens(&variant).collect::<Vec<_>>();
            let Some(token) = (tokens.len() > 1).then(|| tokens[tokens.len() - 1]) else {
                continue;
            };
            found_multi = true;
            if self.collect_literal_token_candidates(
                token,
                cancel,
                &mut candidates,
                &mut posting_visits,
            ) {
                return Some(aborted(posting_visits));
            }
        }
        found_multi.then(|| collected(candidates, posting_visits))
    }

    pub fn postings(&self) -> usize {
        self.postings
    }

    fn take_workspace(&self) -> DenseWorkspace {
        self.workspaces
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .pop()
            .unwrap_or_else(|| DenseWorkspace::new(self.source_entries))
    }

    fn return_workspace(&self, mut workspace: DenseWorkspace) {
        workspace.clear();
        let mut retained = self
            .workspaces
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        if retained.len() < RETAINED_WORKSPACES {
            retained.push(workspace);
        }
    }

    fn posting_range(&self, bucket: usize) -> (usize, usize) {
        let bytes = self.mapping.bytes();
        let start =
            read_u64(bytes, HEADER_BYTES + bucket * 8).expect("validated q-gram offset") as usize;
        let end = read_u64(bytes, HEADER_BYTES + (bucket + 1) * 8).expect("validated q-gram offset")
            as usize;
        debug_assert!(end <= self.postings);
        (start, end)
    }

    /// Add one token's complete typo-safe q-gram candidates. Returns `true` when a newer
    /// query cancelled the posting walk.
    fn collect_token_candidates(
        &self,
        token: &str,
        cancel: &Cancel<'_>,
        candidates: &mut Vec<u32>,
        posting_visits: &mut usize,
        workspace: &mut DenseWorkspace,
    ) -> bool {
        let buckets = gram_buckets(token);
        if buckets.is_empty() {
            return false;
        }
        let minimum = buckets
            .len()
            .saturating_sub(4 * allowed_edits(token.chars().count()))
            .max(1);
        workspace.clear();
        for bucket in buckets {
            let (start, end) = self.posting_range(bucket as usize);
            for position in start..end {
                if posting_visits.is_multiple_of(4096) && cancel.superseded() {
                    return true;
                }
                *posting_visits += 1;
                let slot = read_u32(self.mapping.bytes(), self.postings_offset + position * 4)
                    .expect("validated q-gram posting");
                let Some(count) = workspace.counts.get_mut(slot as usize) else {
                    continue;
                };
                if *count == 0 {
                    workspace.touched.push(slot);
                }
                *count = count.saturating_add(1);
            }
        }
        candidates.extend(
            workspace
                .touched
                .iter()
                .copied()
                .filter(|slot| workspace.counts[*slot as usize] as usize >= minimum),
        );
        workspace.clear();
        false
    }

    /// Intersect the token's sorted posting lists. A literal substring contains every token
    /// trigram, so the intersection cannot drop a valid literal name match. Hash collisions
    /// may add false positives; the production ranker verifies them before emitting a row.
    fn collect_literal_token_candidates(
        &self,
        token: &str,
        cancel: &Cancel<'_>,
        candidates: &mut Vec<u32>,
        posting_visits: &mut usize,
    ) -> bool {
        let buckets = gram_buckets(token);
        if buckets.is_empty() {
            return false;
        }
        let mut ranges = buckets
            .into_iter()
            .map(|bucket| self.posting_range(bucket as usize))
            .collect::<Vec<_>>();
        ranges.sort_unstable_by_key(|(start, end)| end - start);
        let (start, end) = ranges[0];
        let bytes = self.mapping.bytes();
        for position in start..end {
            if posting_visits.is_multiple_of(4096) && cancel.superseded() {
                return true;
            }
            *posting_visits += 1;
            let slot = read_u32(bytes, self.postings_offset + position * 4)
                .expect("validated q-gram posting");
            let mut present = true;
            for &(other_start, other_end) in &ranges[1..] {
                let Some(found) = posting_contains(
                    bytes,
                    self.postings_offset,
                    other_start,
                    other_end,
                    slot,
                    posting_visits,
                    cancel,
                ) else {
                    return true;
                };
                if !found {
                    present = false;
                    break;
                }
            }
            if present {
                candidates.push(slot);
            }
        }
        false
    }
}

fn posting_contains(
    bytes: &[u8],
    postings_offset: usize,
    mut lo: usize,
    mut hi: usize,
    slot: u32,
    posting_visits: &mut usize,
    cancel: &Cancel<'_>,
) -> Option<bool> {
    while lo < hi {
        if posting_visits.is_multiple_of(4096) && cancel.superseded() {
            return None;
        }
        let mid = lo + (hi - lo) / 2;
        *posting_visits += 1;
        let candidate =
            read_u32(bytes, postings_offset + mid * 4).expect("validated q-gram posting");
        match candidate.cmp(&slot) {
            std::cmp::Ordering::Less => lo = mid + 1,
            std::cmp::Ordering::Greater => hi = mid,
            std::cmp::Ordering::Equal => return Some(true),
        }
    }
    Some(false)
}

fn query_variants(query_text: &str) -> Vec<String> {
    let normalized = normalize(query_text);
    let mut variants = vec![normalized.clone()];
    variants.extend(query::layout_variants(&normalized));
    variants
}

fn collected(mut candidates: Vec<u32>, posting_visits: usize) -> CandidateSet {
    candidates.sort_unstable();
    candidates.dedup();
    CandidateSet {
        slots: candidates,
        posting_visits,
        aborted: false,
    }
}

fn aborted(posting_visits: usize) -> CandidateSet {
    CandidateSet {
        slots: Vec::new(),
        posting_visits,
        aborted: true,
    }
}

pub fn significant_len(value: &str) -> usize {
    value
        .nfc()
        .filter(|character| character.is_alphanumeric())
        .count()
}

/// The sidecar can retrieve a complete prototype candidate pool only when every query
/// token contributes at least one trigram. Short-token mixtures fall back to the exact
/// scanner rather than silently losing base Items whose name matches only the short token.
pub fn supports_query(value: &str) -> bool {
    let normalized = normalize(value);
    let mut tokens = normalized
        .split(|character: char| !character.is_alphanumeric())
        .filter(|token| !token.is_empty())
        .peekable();
    tokens.peek().is_some() && tokens.all(|token| token.chars().count() >= 3)
}

fn allowed_edits(length: usize) -> usize {
    match length {
        0..=2 => 0,
        3..=6 => 1,
        7..=12 => 2,
        _ => 3,
    }
}

fn query_tokens(value: &str) -> impl Iterator<Item = &str> {
    value
        .split(|character: char| character.is_whitespace() || character == '/')
        .filter(|token| !token.is_empty())
}

fn normalize(value: &str) -> String {
    value.nfc().flat_map(char::to_lowercase).collect()
}

fn gram_buckets(value: &str) -> Vec<u32> {
    let normalized = normalize(value);
    let mut buckets = normalized
        .split(|character: char| !character.is_alphanumeric())
        .flat_map(|token| {
            let characters = token.chars().collect::<Vec<_>>();
            characters.windows(3).map(bucket).collect::<Vec<_>>()
        })
        .collect::<Vec<_>>();
    buckets.sort_unstable();
    buckets.dedup();
    buckets
}

fn bucket(gram: &[char]) -> u32 {
    let mut hash = 0xcbf29ce484222325u64;
    for character in gram {
        for byte in (*character as u32).to_le_bytes() {
            hash ^= u64::from(byte);
            hash = hash.wrapping_mul(0x100000001b3);
        }
    }
    (hash as usize & (BUCKETS - 1)) as u32
}

fn read_u64(bytes: &[u8], offset: usize) -> Option<u64> {
    Some(u64::from_le_bytes(
        bytes.get(offset..offset + 8)?.try_into().ok()?,
    ))
}

fn read_u32(bytes: &[u8], offset: usize) -> Option<u32> {
    Some(u32::from_le_bytes(
        bytes.get(offset..offset + 4)?.try_into().ok()?,
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::name_index::{
        model::{IndexData, Tier},
        persist,
    };
    use std::sync::{atomic::AtomicU64, Arc, RwLock};

    struct TempDir(std::path::PathBuf);

    impl TempDir {
        fn new() -> Self {
            static COUNTER: AtomicU64 = AtomicU64::new(0);
            let unique = COUNTER.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            let path = std::env::temp_dir().join(format!(
                "beeline_qgram_test_{}_{}",
                std::process::id(),
                unique
            ));
            std::fs::create_dir_all(&path).expect("create q-gram test dir");
            Self(path)
        }
    }

    impl Drop for TempDir {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    #[test]
    fn qgrams_share_normalization_with_queries() {
        assert_eq!(gram_buckets("Methodology"), gram_buckets("methodology"));
        assert!(gram_buckets("ab").is_empty());
        assert_eq!(gram_buckets("abcd").len(), 2);
        assert_eq!(significant_len("work/w"), 5);
    }

    #[test]
    fn retrieval_requires_a_trigram_from_every_token() {
        assert!(supports_query("work wip"));
        assert!(supports_query("метолология"));
        assert!(!supports_query("ab cd"));
        assert!(!supports_query("work x"));
    }

    #[test]
    fn ordered_literal_tail_is_a_safe_subset_of_whole_query_candidates() {
        let dir = TempDir::new();
        let root = dir.0.join("home");
        std::fs::create_dir_all(&root).expect("create index root");
        let shared = Arc::new(RwLock::new(IndexData::new(root.clone())));
        {
            let mut index = shared.write().expect("index lock");
            let vault = index.add_dir(0, "vault", Tier::Normal, 0);
            index.add_file(vault, "methodology.md", Tier::Normal);
            index.add_file(0, "vault-notes.md", Tier::Normal);
            index.add_file(0, "methodology-draft.md", Tier::Normal);
            index.add_file(0, "unrelated.txt", Tier::Normal);
        }
        let index_path = dir.0.join("index.idx");
        persist::save(&shared, &index_path).expect("persist test index");
        let index = persist::load(&index_path, &root).expect("load mapped test index");
        let qgram_path = dir.0.join("index.qgram");
        QGramIndex::build(&index, &qgram_path).expect("build test q-gram");
        let qgram = QGramIndex::open(
            &qgram_path,
            index.base_content_hash().expect("base hash"),
            index.base_entry_count(),
        )
        .expect("open test q-gram");
        let generation = AtomicU64::new(1);
        let cancel = Cancel::new(&generation, 1);

        let whole = qgram.candidates("vault methodology", &cancel);
        let tail = qgram
            .ordered_tail_literal_candidates("vault methodology", &cancel)
            .expect("multi-token tail");

        assert!(!whole.aborted && !tail.aborted);
        assert!(!tail.slots.is_empty());
        assert!(tail
            .slots
            .iter()
            .all(|slot| whole.slots.binary_search(slot).is_ok()));

        let cancelled_generation = AtomicU64::new(2);
        let cancelled = Cancel::new(&cancelled_generation, 1);
        assert!(qgram.candidates("vault methodology", &cancelled).aborted);
        assert!(
            qgram
                .ordered_tail_literal_candidates("vault methodology", &cancelled)
                .expect("multi-token tail")
                .aborted
        );

        // Cancellation must return a clean dense workspace to the pool. The next request
        // must produce the exact same complete candidate set as it did before the abort.
        assert_eq!(
            qgram.candidates("vault methodology", &cancel).slots,
            whole.slots
        );

        // Rapid typing can overlap a cancelled query with its replacement. The shared
        // mapped sidecar and its bounded workspace pool must stay deterministic.
        std::thread::scope(|scope| {
            let first = scope.spawn(|| qgram.candidates("vault methodology", &cancel));
            let second = scope.spawn(|| qgram.candidates("vault methodology", &cancel));
            assert_eq!(
                first.join().expect("first candidate thread").slots,
                whole.slots
            );
            assert_eq!(
                second.join().expect("second candidate thread").slots,
                whole.slots
            );
        });
    }
}

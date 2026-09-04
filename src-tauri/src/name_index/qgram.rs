//! Read-only q-gram candidate sidecar for Search v2 Typo Correction.
//!
//! The sidecar covers the immutable Name Index base. Overlay Items are scanned separately,
//! so file-system changes remain searchable without rewriting this file. A content hash ties
//! the sidecar to one exact base image; a mismatch starts a background rebuild.

use std::{
    fs::{self, File},
    io::{BufWriter, Seek, SeekFrom, Write},
    path::{Path, PathBuf},
    sync::Mutex,
};

use unicode_normalization::UnicodeNormalization;

use super::{
    model::{IndexData, MappedFile},
    query::{self, Cancel},
};

const MAGIC: &[u8; 8] = b"BLQGM003";
const HEADER_BYTES: usize = 64;
const BUCKETS: usize = 1 << 20;
const BUCKET_OFFSET_BYTES: usize = 4;
const RETAINED_WORKSPACES: usize = 1;
const SKIP_STRIDE: usize = 64;
const SKIP_RECORD_BYTES: usize = 8;
const BUILD_SHARDS: usize = 64;
const BUILD_SHARD_BUCKETS: usize = BUCKETS / BUILD_SHARDS;
const BUILD_RECORD_BYTES: usize = 6;
const ENCODED_POSTING_SLOTS: usize = 256 * 1024;

#[derive(Debug)]
pub struct CandidateSet {
    pub slots: Vec<u32>,
    pub posting_visits: usize,
    pub aborted: bool,
}

#[derive(Clone, Copy)]
struct PostingRange {
    start: usize,
    end: usize,
    skip_start: usize,
    skip_end: usize,
}

pub struct QGramIndex {
    mapping: MappedFile,
    postings_offset: usize,
    skip_offsets_offset: usize,
    skips_offset: usize,
    postings: usize,
    source_entries: usize,
    workspaces: Mutex<Vec<DenseWorkspace>>,
}

struct DenseWorkspace {
    counts: Vec<u8>,
    touched: Vec<u32>,
}

struct BuildParts {
    path: PathBuf,
}

impl BuildParts {
    fn create(sidecar: &Path) -> std::io::Result<Self> {
        let path = sidecar.with_extension("qgram.parts");
        match fs::remove_dir_all(&path) {
            Ok(()) => {}
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => return Err(error),
        }
        fs::create_dir(&path)?;
        Ok(Self { path })
    }

    fn shard(&self, shard: usize) -> PathBuf {
        self.path.join(format!("{shard:02}.part"))
    }

    fn skips(&self) -> PathBuf {
        self.path.join("skips.part")
    }
}

impl Drop for BuildParts {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.path);
    }
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
        let encoded_bytes = read_u64(bytes, 40)? as usize;
        let skip_count = read_u64(bytes, 48)? as usize;
        if read_u64(bytes, 56)? as usize != SKIP_STRIDE {
            return None;
        }
        let offsets_bytes = (BUCKETS + 1).checked_mul(BUCKET_OFFSET_BYTES)?;
        let skip_offsets_offset = HEADER_BYTES.checked_add(offsets_bytes)?;
        let postings_offset = skip_offsets_offset.checked_add(offsets_bytes)?;
        let skips_offset = postings_offset.checked_add(encoded_bytes)?;
        let expected = skips_offset.checked_add(skip_count.checked_mul(SKIP_RECORD_BYTES)?)?;
        if bytes.len() != expected {
            return None;
        }
        if read_u32(bytes, HEADER_BYTES + BUCKETS * BUCKET_OFFSET_BYTES)? as usize != encoded_bytes
            || read_u32(bytes, skip_offsets_offset + BUCKETS * BUCKET_OFFSET_BYTES)? as usize
                != skip_count
        {
            return None;
        }
        Some(Self {
            mapping,
            postings_offset,
            skip_offsets_offset,
            skips_offset,
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

        // The on-disk format keeps 64-bit offsets, but one build cannot create more than
        // u32::MAX postings without exceeding the current sidecar and addressable-slot
        // contracts. Keeping all three build tables at u32 saves 12 MiB of anonymous memory.
        let mut counts = vec![0u32; BUCKETS];
        for slot in 0..source_entries {
            let Some(entry) = index.entry(slot) else {
                continue;
            };
            for bucket in gram_buckets(entry.name) {
                let count = &mut counts[bucket as usize];
                *count = count
                    .checked_add(1)
                    .ok_or_else(|| std::io::Error::other("q-gram bucket exceeds u32 postings"))?;
            }
        }

        let mut offsets = Vec::with_capacity(BUCKETS + 1);
        offsets.push(0u32);
        for count in counts {
            let offset = offsets
                .last()
                .copied()
                .expect("offset seed")
                .checked_add(count)
                .ok_or_else(|| std::io::Error::other("q-gram sidecar exceeds u32 postings"))?;
            offsets.push(offset);
        }
        let posting_count = *offsets.last().expect("final q-gram offset") as usize;
        // Partition `(bucket, slot)` records by the bucket's high bits. Each part is written in
        // ascending slot order, then counting-sorted in memory and appended to the final sidecar.
        // This replaces the 520-MiB global postings vector with one roughly 8-MiB shard.
        let parts = BuildParts::create(path)?;
        let mut part_writers = (0..BUILD_SHARDS)
            .map(|shard| File::create(parts.shard(shard)).map(BufWriter::new))
            .collect::<std::io::Result<Vec<_>>>()?;
        for slot in 0..source_entries {
            let Some(entry) = index.entry(slot) else {
                continue;
            };
            let slot = u32::try_from(slot)
                .map_err(|_| std::io::Error::other("Name Index exceeds u32 slots"))?;
            for bucket in gram_buckets(entry.name) {
                let bucket = bucket as usize;
                let shard = bucket / BUILD_SHARD_BUCKETS;
                let local_bucket = u16::try_from(bucket % BUILD_SHARD_BUCKETS)
                    .expect("q-gram build shard bucket fits u16");
                part_writers[shard].write_all(&local_bucket.to_le_bytes())?;
                part_writers[shard].write_all(&slot.to_le_bytes())?;
            }
        }
        for writer in &mut part_writers {
            writer.flush()?;
        }
        drop(part_writers);

        let temporary = path.with_extension("qgram.tmp");
        let file = File::create(&temporary)?;
        let mut writer = BufWriter::new(file);
        writer.write_all(MAGIC)?;
        writer.write_all(&(BUCKETS as u64).to_le_bytes())?;
        writer.write_all(&(posting_count as u64).to_le_bytes())?;
        writer.write_all(&source_hash.to_le_bytes())?;
        writer.write_all(&(source_entries as u64).to_le_bytes())?;
        writer.write_all(&0u64.to_le_bytes())?; // encoded posting bytes, backfilled below
        writer.write_all(&0u64.to_le_bytes())?; // skip record count, backfilled below
        writer.write_all(&(SKIP_STRIDE as u64).to_le_bytes())?;
        write_zeroes(&mut writer, (BUCKETS + 1) * BUCKET_OFFSET_BYTES * 2)?;

        let mut encoded = Vec::with_capacity(ENCODED_POSTING_SLOTS * 4);
        let mut byte_offsets = Vec::with_capacity(BUCKETS + 1);
        let mut skip_offsets = Vec::with_capacity(BUCKETS + 1);
        let mut skip_writer = BufWriter::new(File::create(parts.skips())?);
        let mut encoded_bytes = 0u64;
        let mut skip_count = 0u64;
        let mut emitted_postings = 0usize;
        for shard in 0..BUILD_SHARDS {
            let bytes = fs::read(parts.shard(shard))?;
            if !bytes.len().is_multiple_of(BUILD_RECORD_BYTES) {
                return Err(std::io::Error::other("partial q-gram build record"));
            }
            let records = bytes.len() / BUILD_RECORD_BYTES;
            let first_bucket = shard * BUILD_SHARD_BUCKETS;
            let expected_records = offsets[first_bucket + BUILD_SHARD_BUCKETS]
                .checked_sub(offsets[first_bucket])
                .expect("global q-gram offsets are monotone")
                as usize;
            if records != expected_records {
                return Err(std::io::Error::other("q-gram shard count mismatch"));
            }
            emitted_postings = emitted_postings
                .checked_add(records)
                .ok_or_else(|| std::io::Error::other("q-gram emitted posting count overflow"))?;
            let mut local_counts = vec![0u32; BUILD_SHARD_BUCKETS];
            for record in bytes.chunks_exact(BUILD_RECORD_BYTES) {
                let bucket = u16::from_le_bytes([record[0], record[1]]) as usize;
                local_counts[bucket] = local_counts[bucket]
                    .checked_add(1)
                    .ok_or_else(|| std::io::Error::other("q-gram shard bucket exceeds u32"))?;
            }
            let mut local_offsets = Vec::with_capacity(BUILD_SHARD_BUCKETS + 1);
            local_offsets.push(0u32);
            for count in local_counts {
                local_offsets.push(
                    local_offsets
                        .last()
                        .copied()
                        .expect("local offset seed")
                        .checked_add(count)
                        .ok_or_else(|| std::io::Error::other("q-gram shard exceeds u32"))?,
                );
            }
            debug_assert_eq!(
                *local_offsets.last().expect("final local offset") as usize,
                records
            );
            let mut positions = local_offsets[..BUILD_SHARD_BUCKETS].to_vec();
            let mut postings = vec![0u32; records];
            for record in bytes.chunks_exact(BUILD_RECORD_BYTES) {
                let bucket = u16::from_le_bytes([record[0], record[1]]) as usize;
                let slot = u32::from_le_bytes([record[2], record[3], record[4], record[5]]);
                let position = &mut positions[bucket];
                postings[*position as usize] = slot;
                *position = position
                    .checked_add(1)
                    .ok_or_else(|| std::io::Error::other("q-gram shard position exceeds u32"))?;
            }
            for local_bucket in 0..BUILD_SHARD_BUCKETS {
                byte_offsets.push(u32::try_from(encoded_bytes).map_err(|_| {
                    std::io::Error::other("encoded q-gram exceeds u32 byte offsets")
                })?);
                skip_offsets.push(
                    u32::try_from(skip_count)
                        .map_err(|_| std::io::Error::other("q-gram skips exceed u32 offsets"))?,
                );
                let bucket_start = encoded_bytes;
                let start = local_offsets[local_bucket] as usize;
                let end = local_offsets[local_bucket + 1] as usize;
                let mut previous = 0u32;
                for (index, &slot) in postings[start..end].iter().enumerate() {
                    let delta = if index == 0 {
                        slot
                    } else {
                        slot.checked_sub(previous).ok_or_else(|| {
                            std::io::Error::other("q-gram postings are not sorted")
                        })?
                    };
                    let before = encoded.len();
                    encode_var_u32(delta, &mut encoded);
                    encoded_bytes = encoded_bytes
                        .checked_add((encoded.len() - before) as u64)
                        .ok_or_else(|| std::io::Error::other("encoded q-gram size overflow"))?;
                    if index.is_multiple_of(SKIP_STRIDE) {
                        let after = u32::try_from(encoded_bytes - bucket_start).map_err(|_| {
                            std::io::Error::other("q-gram bucket exceeds u32 encoded bytes")
                        })?;
                        skip_writer.write_all(&slot.to_le_bytes())?;
                        skip_writer.write_all(&after.to_le_bytes())?;
                        skip_count = skip_count
                            .checked_add(1)
                            .ok_or_else(|| std::io::Error::other("q-gram skip count overflow"))?;
                    }
                    previous = slot;
                    if encoded.len() >= ENCODED_POSTING_SLOTS * 4 {
                        writer.write_all(&encoded)?;
                        encoded.clear();
                    }
                }
            }
        }
        if emitted_postings != posting_count {
            return Err(std::io::Error::other("q-gram posting count mismatch"));
        }
        byte_offsets.push(
            u32::try_from(encoded_bytes)
                .map_err(|_| std::io::Error::other("encoded q-gram exceeds u32 byte offsets"))?,
        );
        skip_offsets.push(
            u32::try_from(skip_count)
                .map_err(|_| std::io::Error::other("q-gram skips exceed u32 offsets"))?,
        );
        if byte_offsets.len() != BUCKETS + 1 || skip_offsets.len() != BUCKETS + 1 {
            return Err(std::io::Error::other("q-gram bucket offset count mismatch"));
        }
        if !encoded.is_empty() {
            writer.write_all(&encoded)?;
        }
        skip_writer.flush()?;
        drop(skip_writer);
        let copied_skips = std::io::copy(&mut File::open(parts.skips())?, &mut writer)?;
        let expected_skip_bytes = skip_count
            .checked_mul(SKIP_RECORD_BYTES as u64)
            .ok_or_else(|| std::io::Error::other("q-gram skip byte size overflow"))?;
        if copied_skips != expected_skip_bytes {
            return Err(std::io::Error::other("q-gram skip byte count mismatch"));
        }

        writer.seek(SeekFrom::Start(40))?;
        writer.write_all(&encoded_bytes.to_le_bytes())?;
        writer.write_all(&skip_count.to_le_bytes())?;
        writer.seek(SeekFrom::Start(HEADER_BYTES as u64))?;
        for offset in byte_offsets {
            writer.write_all(&offset.to_le_bytes())?;
        }
        for offset in skip_offsets {
            writer.write_all(&offset.to_le_bytes())?;
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

    fn posting_byte_range(&self, bucket: usize) -> (usize, usize) {
        let bytes = self.mapping.bytes();
        let start = read_u32(bytes, HEADER_BYTES + bucket * BUCKET_OFFSET_BYTES)
            .expect("validated q-gram offset") as usize;
        let end = read_u32(bytes, HEADER_BYTES + (bucket + 1) * BUCKET_OFFSET_BYTES)
            .expect("validated q-gram offset") as usize;
        debug_assert!(start <= end);
        (self.postings_offset + start, self.postings_offset + end)
    }

    fn skip_range(&self, bucket: usize) -> (usize, usize) {
        let bytes = self.mapping.bytes();
        let start = read_u32(
            bytes,
            self.skip_offsets_offset + bucket * BUCKET_OFFSET_BYTES,
        )
        .expect("validated q-gram skip offset") as usize;
        let end = read_u32(
            bytes,
            self.skip_offsets_offset + (bucket + 1) * BUCKET_OFFSET_BYTES,
        )
        .expect("validated q-gram skip offset") as usize;
        debug_assert!(start <= end);
        (start, end)
    }

    fn posting_range(&self, bucket: usize) -> PostingRange {
        let (start, end) = self.posting_byte_range(bucket);
        let (skip_start, skip_end) = self.skip_range(bucket);
        PostingRange {
            start,
            end,
            skip_start,
            skip_end,
        }
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
            let (mut position, end) = self.posting_byte_range(bucket as usize);
            let mut previous = 0u32;
            while position < end {
                if posting_visits.is_multiple_of(4096) && cancel.superseded() {
                    return true;
                }
                *posting_visits += 1;
                let delta = read_var_u32(self.mapping.bytes(), &mut position, end)
                    .expect("validated q-gram posting");
                let slot = previous
                    .checked_add(delta)
                    .expect("validated q-gram posting delta");
                previous = slot;
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
        ranges.sort_unstable_by_key(|range| range.end - range.start);
        let mut position = ranges[0].start;
        let end = ranges[0].end;
        let bytes = self.mapping.bytes();
        let mut previous = 0u32;
        while position < end {
            if posting_visits.is_multiple_of(4096) && cancel.superseded() {
                return true;
            }
            *posting_visits += 1;
            let delta = read_var_u32(bytes, &mut position, end).expect("validated q-gram posting");
            let slot = previous
                .checked_add(delta)
                .expect("validated q-gram posting delta");
            previous = slot;
            let mut present = true;
            for range in &ranges[1..] {
                let Some(found) = posting_contains(
                    bytes,
                    self.skips_offset,
                    range,
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
    skips_offset: usize,
    range: &PostingRange,
    slot: u32,
    posting_visits: &mut usize,
    cancel: &Cancel<'_>,
) -> Option<bool> {
    let mut lo = range.skip_start;
    let mut hi = range.skip_end;
    while lo < hi {
        if posting_visits.is_multiple_of(4096) && cancel.superseded() {
            return None;
        }
        let mid = lo + (hi - lo) / 2;
        *posting_visits += 1;
        let candidate =
            read_u32(bytes, skips_offset + mid * SKIP_RECORD_BYTES).expect("validated q-gram skip");
        if candidate <= slot {
            lo = mid + 1;
        } else {
            hi = mid;
        }
    }
    if lo == range.skip_start {
        return Some(false);
    }
    let checkpoint = lo - 1;
    let record = skips_offset + checkpoint * SKIP_RECORD_BYTES;
    let mut previous = read_u32(bytes, record).expect("validated q-gram skip");
    if previous == slot {
        return Some(true);
    }
    let after = read_u32(bytes, record + 4).expect("validated q-gram skip") as usize;
    let mut position = range.start + after;
    let decode_end = if checkpoint + 1 < range.skip_end {
        let next_record = skips_offset + (checkpoint + 1) * SKIP_RECORD_BYTES;
        range.start + read_u32(bytes, next_record + 4).expect("validated q-gram skip") as usize
    } else {
        range.end
    };
    while position < decode_end {
        if posting_visits.is_multiple_of(4096) && cancel.superseded() {
            return None;
        }
        *posting_visits += 1;
        let delta =
            read_var_u32(bytes, &mut position, decode_end).expect("validated q-gram posting");
        previous = previous
            .checked_add(delta)
            .expect("validated q-gram posting delta");
        match previous.cmp(&slot) {
            std::cmp::Ordering::Less => {}
            std::cmp::Ordering::Equal => return Some(true),
            std::cmp::Ordering::Greater => return Some(false),
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

fn write_zeroes(writer: &mut impl Write, mut bytes: usize) -> std::io::Result<()> {
    const ZEROES: [u8; 8192] = [0; 8192];
    while bytes > 0 {
        let chunk = bytes.min(ZEROES.len());
        writer.write_all(&ZEROES[..chunk])?;
        bytes -= chunk;
    }
    Ok(())
}

fn encode_var_u32(mut value: u32, output: &mut Vec<u8>) {
    while value >= 0x80 {
        output.push((value as u8 & 0x7f) | 0x80);
        value >>= 7;
    }
    output.push(value as u8);
}

fn read_var_u32(bytes: &[u8], position: &mut usize, end: usize) -> Option<u32> {
    let mut value = 0u32;
    for shift in [0, 7, 14, 21, 28] {
        let byte = *bytes.get(*position..end)?.first()?;
        *position += 1;
        if shift == 28 && byte & 0xf0 != 0 {
            return None;
        }
        value |= u32::from(byte & 0x7f) << shift;
        if byte & 0x80 == 0 {
            return Some(value);
        }
    }
    None
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
    fn var_u32_round_trips_boundaries() {
        let values = [
            0,
            1,
            0x7f,
            0x80,
            0x3fff,
            0x4000,
            0x1f_ffff,
            0x20_0000,
            u32::MAX,
        ];
        let mut bytes = Vec::new();
        for value in values {
            encode_var_u32(value, &mut bytes);
        }
        let mut position = 0;
        for value in values {
            assert_eq!(
                read_var_u32(&bytes, &mut position, bytes.len()),
                Some(value)
            );
        }
        assert_eq!(position, bytes.len());
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
        assert!(!qgram_path.with_extension("qgram.parts").exists());
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

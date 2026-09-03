//! Read-only q-gram candidate sidecar for Search v2 Typo Correction.
//!
//! The sidecar covers the immutable Name Index base. Overlay Items are scanned separately,
//! so file-system changes remain searchable without rewriting this file. A content hash ties
//! the sidecar to one exact base image; a mismatch starts a background rebuild.

use std::{
    collections::{BTreeSet, HashMap},
    fs::{self, File},
    io::{BufWriter, Write},
    path::Path,
};

use unicode_normalization::UnicodeNormalization;

use super::{
    model::{IndexData, MappedFile},
    query::{self, Cancel},
};

const MAGIC: &[u8; 8] = b"BLQGM001";
const HEADER_BYTES: usize = 40;
const BUCKETS: usize = 1 << 20;

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
        let normalized = normalize(query_text);
        let mut variants = vec![normalized.clone()];
        variants.extend(query::layout_variants(&normalized));
        let mut candidates = BTreeSet::new();
        let mut posting_visits = 0usize;
        for variant in variants {
            for token in query_tokens(&variant) {
                let buckets = gram_buckets(token);
                if buckets.is_empty() {
                    continue;
                }
                let minimum = buckets
                    .len()
                    .saturating_sub(4 * allowed_edits(token.chars().count()))
                    .max(1);
                let mut overlaps = HashMap::<u32, u8>::new();
                for bucket in buckets {
                    let (start, end) = self.posting_range(bucket as usize);
                    for position in start..end {
                        if posting_visits.is_multiple_of(4096) && cancel.superseded() {
                            return CandidateSet {
                                slots: Vec::new(),
                                posting_visits,
                                aborted: true,
                            };
                        }
                        posting_visits += 1;
                        let slot =
                            read_u32(self.mapping.bytes(), self.postings_offset + position * 4)
                                .expect("validated q-gram posting");
                        let count = overlaps.entry(slot).or_default();
                        *count = count.saturating_add(1);
                    }
                }
                candidates.extend(
                    overlaps
                        .into_iter()
                        .filter_map(|(slot, count)| (count as usize >= minimum).then_some(slot)),
                );
            }
        }
        CandidateSet {
            slots: candidates.into_iter().collect(),
            posting_visits,
            aborted: false,
        }
    }

    pub fn source_entries(&self) -> usize {
        self.source_entries
    }

    pub fn postings(&self) -> usize {
        self.postings
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
}

use std::{
    collections::{BTreeSet, HashMap},
    fs::{self, File},
    io::{BufWriter, Write},
    path::Path,
    sync::atomic::{AtomicU64, Ordering},
    time::Instant,
};

use memmap2::Mmap;

use crate::{
    index::LiveIndex,
    matcher::{normalize, PreparedQuery},
};

const MAGIC: &[u8; 8] = b"BLQGM001";
const HEADER_BYTES: usize = 32;
const BUCKETS: usize = 1 << 20;

pub struct BuildReport {
    pub built: bool,
    pub build_ms: Option<f64>,
    pub bytes: u64,
    pub postings: u64,
    pub buckets: usize,
}

pub struct CandidateSet {
    pub slots: Vec<u32>,
    pub posting_visits: usize,
    pub aborted: bool,
}

pub struct QGramIndex {
    mmap: Mmap,
    postings_offset: usize,
    postings: usize,
}

impl QGramIndex {
    pub fn open_or_build(path: &Path, index: &LiveIndex) -> (Self, BuildReport) {
        let (built, build_ms) = if path.exists() {
            (false, None)
        } else {
            let started = Instant::now();
            build(path, index);
            (true, Some(started.elapsed().as_secs_f64() * 1000.0))
        };
        let file = File::open(path).expect("open q-gram sidecar");
        let bytes = file.metadata().expect("stat q-gram sidecar").len();
        let mmap = unsafe { Mmap::map(&file).expect("map q-gram sidecar") };
        assert_eq!(mmap.get(..8), Some(MAGIC.as_slice()), "q-gram magic");
        let buckets = read_u64(&mmap, 8) as usize;
        assert_eq!(buckets, BUCKETS, "q-gram bucket count");
        let postings = read_u64(&mmap, 16) as usize;
        let postings_offset = HEADER_BYTES + (buckets + 1) * 8;
        assert_eq!(
            mmap.len(),
            postings_offset + postings * 4,
            "q-gram file length"
        );
        (
            Self {
                mmap,
                postings_offset,
                postings,
            },
            BuildReport {
                built,
                build_ms,
                bytes,
                postings: postings as u64,
                buckets,
            },
        )
    }

    pub fn candidates(
        &self,
        prepared: &PreparedQuery,
        generation: &AtomicU64,
        mine: u64,
    ) -> CandidateSet {
        let mut candidates = BTreeSet::new();
        let mut posting_visits = 0;
        for variant in &prepared.variants {
            for token in &variant.tokens {
                let buckets = gram_buckets(&token.text);
                if buckets.is_empty() {
                    continue;
                }
                let minimum = buckets
                    .len()
                    .saturating_sub(4 * token.global_edit_limit())
                    .max(1);
                let mut overlaps = HashMap::<u32, u8>::new();
                for bucket in buckets {
                    let (start, end) = self.posting_range(bucket as usize);
                    for position in start..end {
                        if posting_visits % 4096 == 0 && generation.load(Ordering::Relaxed) != mine
                        {
                            return CandidateSet {
                                slots: Vec::new(),
                                posting_visits,
                                aborted: true,
                            };
                        }
                        posting_visits += 1;
                        let slot = read_u32(&self.mmap, self.postings_offset + position * 4);
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

    fn posting_range(&self, bucket: usize) -> (usize, usize) {
        debug_assert!(bucket < BUCKETS);
        let offsets = HEADER_BYTES;
        let start = read_u64(&self.mmap, offsets + bucket * 8) as usize;
        let end = read_u64(&self.mmap, offsets + (bucket + 1) * 8) as usize;
        debug_assert!(end <= self.postings);
        (start, end)
    }
}

fn build(path: &Path, index: &LiveIndex) {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).expect("create q-gram sidecar directory");
    }
    let mut counts = vec![0u64; BUCKETS];
    for slot in 0..index.slot_count() {
        let Some(entry) = index.entry(slot as u32) else {
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
    eprintln!(
        "q-gram count pass: {posting_count} postings ({:.1} MiB)",
        posting_count as f64 * 4.0 / 1_048_576.0
    );
    let mut positions = offsets[..BUCKETS].to_vec();
    let mut postings = vec![0u32; posting_count];
    for slot in 0..index.slot_count() {
        let Some(entry) = index.entry(slot as u32) else {
            continue;
        };
        for bucket in gram_buckets(entry.name) {
            let position = &mut positions[bucket as usize];
            postings[*position as usize] = slot as u32;
            *position += 1;
        }
    }

    let file = File::create(path).expect("create q-gram sidecar");
    let mut writer = BufWriter::new(file);
    writer.write_all(MAGIC).expect("write q-gram magic");
    writer
        .write_all(&(BUCKETS as u64).to_le_bytes())
        .expect("write q-gram bucket count");
    writer
        .write_all(&(posting_count as u64).to_le_bytes())
        .expect("write q-gram posting count");
    writer
        .write_all(&0u64.to_le_bytes())
        .expect("write reserved");
    for offset in offsets {
        writer
            .write_all(&offset.to_le_bytes())
            .expect("write q-gram offset");
    }
    for slot in postings {
        writer
            .write_all(&slot.to_le_bytes())
            .expect("write q-gram posting");
    }
    writer.flush().expect("flush q-gram sidecar");
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
            hash ^= byte as u64;
            hash = hash.wrapping_mul(0x100000001b3);
        }
    }
    (hash as usize & (BUCKETS - 1)) as u32
}

fn read_u64(bytes: &[u8], offset: usize) -> u64 {
    u64::from_le_bytes(bytes[offset..offset + 8].try_into().expect("u64 bytes"))
}

fn read_u32(bytes: &[u8], offset: usize) -> u32 {
    u32::from_le_bytes(bytes[offset..offset + 4].try_into().expect("u32 bytes"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn grams_are_stable_and_case_insensitive() {
        assert_eq!(gram_buckets("Methodology"), gram_buckets("methodology"));
        assert_eq!(gram_buckets("ab").len(), 0);
        assert_eq!(gram_buckets("abcd").len(), 2);
    }
}

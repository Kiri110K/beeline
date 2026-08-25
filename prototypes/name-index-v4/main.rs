use std::{
    env,
    ffi::c_void,
    fs::{self, File},
    io::{BufWriter, Write},
    os::fd::AsRawFd,
    path::Path,
    ptr, thread,
    time::Duration,
    time::Instant,
};

const V3_MAGIC: &[u8; 4] = b"BLNI";
const V4_MAGIC: &[u8; 4] = b"BNV4";
const V4_VERSION: u32 = 1;
const HEADER_LEN: usize = 64;
const NODE_RECORD_LEN: usize = 20;
const ENTRY_RECORD_LEN: usize = 24;
const NO_DIR: u32 = u32::MAX;
const FNV_OFFSET: u64 = 0xcbf29ce484222325;
const FNV_PRIME: u64 = 0x100000001b3;
const CACHED_MASK: u64 = 1 << 63;

#[derive(Clone, Copy)]
struct SourceNode {
    parent: u32,
    tier: u8,
    mtime_ms: i64,
    name_start: u32,
    name_len: u16,
}

#[derive(Clone, Copy)]
struct SourceEntry {
    parent: u32,
    is_directory: bool,
    tier: u8,
    dir_id: u32,
    name_start: u32,
    name_len: u16,
    filter: u64,
}

struct SourceIndex {
    bytes: Vec<u8>,
    root_start: usize,
    root_len: usize,
    nodes: Vec<SourceNode>,
    entries: Vec<SourceEntry>,
}

impl SourceIndex {
    fn root(&self) -> &[u8] {
        &self.bytes[self.root_start..self.root_start + self.root_len]
    }

    fn node_name(&self, node: SourceNode) -> &[u8] {
        let start = node.name_start as usize;
        &self.bytes[start..start + node.name_len as usize]
    }

    fn entry_name(&self, entry: SourceEntry) -> &[u8] {
        let start = entry.name_start as usize;
        &self.bytes[start..start + entry.name_len as usize]
    }
}

struct Cursor<'a> {
    bytes: &'a [u8],
    offset: usize,
}

impl<'a> Cursor<'a> {
    fn new(bytes: &'a [u8]) -> Self {
        Self { bytes, offset: 0 }
    }

    fn take(&mut self, count: usize) -> Result<&'a [u8], String> {
        let end = self
            .offset
            .checked_add(count)
            .ok_or_else(|| "offset overflow".to_owned())?;
        let out = self
            .bytes
            .get(self.offset..end)
            .ok_or_else(|| format!("truncated input at byte {}", self.offset))?;
        self.offset = end;
        Ok(out)
    }

    fn u8(&mut self) -> Result<u8, String> {
        Ok(self.take(1)?[0])
    }

    fn u32(&mut self) -> Result<u32, String> {
        let bytes: [u8; 4] = self.take(4)?.try_into().map_err(|_| "u32".to_owned())?;
        Ok(u32::from_le_bytes(bytes))
    }

    fn u64(&mut self) -> Result<u64, String> {
        let bytes: [u8; 8] = self.take(8)?.try_into().map_err(|_| "u64".to_owned())?;
        Ok(u64::from_le_bytes(bytes))
    }

    fn i64(&mut self) -> Result<i64, String> {
        let bytes: [u8; 8] = self.take(8)?.try_into().map_err(|_| "i64".to_owned())?;
        Ok(i64::from_le_bytes(bytes))
    }

    fn string_range(&mut self) -> Result<(u32, u16), String> {
        let len = self.u32()? as usize;
        let start = self.offset;
        let bytes = self.take(len)?;
        std::str::from_utf8(bytes).map_err(|error| format!("invalid UTF-8: {error}"))?;
        let start = u32::try_from(start).map_err(|_| "source file exceeds 4 GiB".to_owned())?;
        let len = u16::try_from(len).map_err(|_| format!("name is {len} bytes"))?;
        Ok((start, len))
    }
}

fn parse_v3(path: &Path) -> Result<SourceIndex, String> {
    let bytes = fs::read(path).map_err(|error| format!("read {}: {error}", path.display()))?;
    let mut cursor = Cursor::new(&bytes);
    if cursor.take(4)? != V3_MAGIC {
        return Err("not a Beeline Name Index".to_owned());
    }
    let version = cursor.u32()?;
    if !(2..=3).contains(&version) {
        return Err(format!("unsupported v3 source version {version}"));
    }
    let root_len = cursor.u32()? as usize;
    let root_start = cursor.offset;
    std::str::from_utf8(cursor.take(root_len)?)
        .map_err(|error| format!("invalid root UTF-8: {error}"))?;

    let node_count = cursor.u32()? as usize;
    let mut nodes = Vec::with_capacity(node_count);
    for _ in 0..node_count {
        let parent = cursor.u32()?;
        let tier = cursor.u8()?;
        let mtime_ms = cursor.i64()?;
        let (name_start, name_len) = cursor.string_range()?;
        nodes.push(SourceNode {
            parent,
            tier,
            mtime_ms,
            name_start,
            name_len,
        });
    }

    let entry_count = cursor.u32()? as usize;
    let mut entries = Vec::with_capacity(entry_count);
    for _ in 0..entry_count {
        let parent = cursor.u32()?;
        let is_directory = cursor.u8()? != 0;
        let tier = cursor.u8()?;
        let dir_id = cursor.u32()?;
        let (name_start, name_len) = cursor.string_range()?;
        let filter = if version >= 3 {
            cursor.u64()?
        } else {
            let start = name_start as usize;
            name_filter(&bytes[start..start + name_len as usize])
        };
        entries.push(SourceEntry {
            parent,
            is_directory,
            tier,
            dir_id,
            name_start,
            name_len,
            filter,
        });
    }
    if cursor.offset != bytes.len() {
        return Err(format!(
            "{} trailing bytes after v{version} payload",
            bytes.len() - cursor.offset
        ));
    }
    if nodes.is_empty() {
        return Err("index has no root node".to_owned());
    }
    Ok(SourceIndex {
        bytes,
        root_start,
        root_len,
        nodes,
        entries,
    })
}

fn reachable_nodes(source: &SourceIndex) -> Result<(Vec<bool>, Vec<u32>), String> {
    let mut referenced = vec![false; source.nodes.len()];
    referenced[0] = true;
    for entry in &source.entries {
        if entry.parent as usize >= source.nodes.len() {
            return Err(format!("entry parent {} is out of range", entry.parent));
        }
        if entry.is_directory {
            if entry.dir_id as usize >= source.nodes.len() {
                return Err(format!("directory id {} is out of range", entry.dir_id));
            }
            referenced[entry.dir_id as usize] = true;
        }
    }

    let mut reachable = vec![false; source.nodes.len()];
    let mut mapping = vec![NO_DIR; source.nodes.len()];
    reachable[0] = true;
    mapping[0] = 0;
    let mut next = 1u32;
    for old_id in 1..source.nodes.len() {
        let parent = source.nodes[old_id].parent as usize;
        if parent >= old_id {
            if referenced[old_id] {
                return Err(format!(
                    "node {old_id} has non-ancestral parent {parent}; prototype expects parent-first ids"
                ));
            }
            continue;
        }
        if referenced[old_id] && reachable[parent] {
            reachable[old_id] = true;
            mapping[old_id] = next;
            next += 1;
        }
    }
    Ok((reachable, mapping))
}

fn retained_entry(entry: SourceEntry, reachable: &[bool]) -> bool {
    reachable[entry.parent as usize] && (!entry.is_directory || reachable[entry.dir_id as usize])
}

fn build_v4(source_path: &Path, output_path: &Path) -> Result<(), String> {
    let started = Instant::now();
    let source = parse_v3(source_path)?;
    let parsed_ms = started.elapsed().as_secs_f64() * 1000.0;
    let (reachable, mapping) = reachable_nodes(&source)?;
    let node_count = reachable.iter().filter(|&&keep| keep).count();
    let entry_count = source
        .entries
        .iter()
        .filter(|&&entry| retained_entry(entry, &reachable))
        .count();
    let dropped_nodes = source.nodes.len() - node_count;
    let dropped_entries = source.entries.len() - entry_count;

    let arena_len: usize = source
        .nodes
        .iter()
        .zip(&reachable)
        .filter(|(_, keep)| **keep)
        .map(|(node, _)| node.name_len as usize)
        .chain(
            source
                .entries
                .iter()
                .filter(|&&entry| retained_entry(entry, &reachable))
                .map(|entry| entry.name_len as usize),
        )
        .sum();
    if arena_len > u32::MAX as usize {
        return Err(format!(
            "name arena is {arena_len} bytes, exceeds u32 offsets"
        ));
    }

    let mut hash = FNV_OFFSET;
    hash_bytes(&mut hash, source.root());
    let mut arena_offset = 0u32;
    for (old_id, node) in source.nodes.iter().copied().enumerate() {
        if !reachable[old_id] {
            continue;
        }
        let parent = if old_id == 0 {
            0
        } else {
            mapping[node.parent as usize]
        };
        let record = node_record(node, parent, arena_offset);
        hash_bytes(&mut hash, &record);
        hash_bytes(&mut hash, source.node_name(node));
        arena_offset += node.name_len as u32;
    }
    for entry in source.entries.iter().copied() {
        if !retained_entry(entry, &reachable) {
            continue;
        }
        let record = entry_record(entry, &mapping, arena_offset);
        hash_bytes(&mut hash, &record);
        hash_bytes(&mut hash, source.entry_name(entry));
        arena_offset += entry.name_len as u32;
    }
    debug_assert_eq!(arena_offset as usize, arena_len);

    let root_len = u32::try_from(source.root_len).map_err(|_| "root path too long".to_owned())?;
    let node_count_u32 = u32::try_from(node_count).map_err(|_| "too many nodes".to_owned())?;
    let entry_count_u32 = u32::try_from(entry_count).map_err(|_| "too many entries".to_owned())?;
    let mut header = [0u8; HEADER_LEN];
    header[0..4].copy_from_slice(V4_MAGIC);
    header[4..8].copy_from_slice(&V4_VERSION.to_le_bytes());
    header[8..12].copy_from_slice(&(HEADER_LEN as u32).to_le_bytes());
    header[12..16].copy_from_slice(&root_len.to_le_bytes());
    header[16..20].copy_from_slice(&node_count_u32.to_le_bytes());
    header[20..24].copy_from_slice(&entry_count_u32.to_le_bytes());
    header[24..32].copy_from_slice(&(arena_len as u64).to_le_bytes());
    header[32..40].copy_from_slice(&hash.to_le_bytes());
    header[40..44].copy_from_slice(&(dropped_nodes as u32).to_le_bytes());
    header[44..48].copy_from_slice(&(dropped_entries as u32).to_le_bytes());

    let output = File::create(output_path)
        .map_err(|error| format!("create {}: {error}", output_path.display()))?;
    let mut writer = BufWriter::with_capacity(1024 * 1024, output);
    writer.write_all(&header).map_err(io_error)?;
    writer.write_all(source.root()).map_err(io_error)?;

    arena_offset = 0;
    for (old_id, node) in source.nodes.iter().copied().enumerate() {
        if !reachable[old_id] {
            continue;
        }
        let parent = if old_id == 0 {
            0
        } else {
            mapping[node.parent as usize]
        };
        writer
            .write_all(&node_record(node, parent, arena_offset))
            .map_err(io_error)?;
        arena_offset += node.name_len as u32;
    }
    for entry in source.entries.iter().copied() {
        if !retained_entry(entry, &reachable) {
            continue;
        }
        writer
            .write_all(&entry_record(entry, &mapping, arena_offset))
            .map_err(io_error)?;
        arena_offset += entry.name_len as u32;
    }
    for (node, keep) in source.nodes.iter().copied().zip(&reachable) {
        if *keep {
            writer.write_all(source.node_name(node)).map_err(io_error)?;
        }
    }
    for entry in source.entries.iter().copied() {
        if retained_entry(entry, &reachable) {
            writer
                .write_all(source.entry_name(entry))
                .map_err(io_error)?;
        }
    }
    writer.flush().map_err(io_error)?;
    let output_size = fs::metadata(output_path).map_err(io_error)?.len();
    let expected_size = HEADER_LEN as u64
        + source.root_len as u64
        + node_count as u64 * NODE_RECORD_LEN as u64
        + entry_count as u64 * ENTRY_RECORD_LEN as u64
        + arena_len as u64;
    if output_size != expected_size {
        return Err(format!(
            "wrote {output_size} bytes, expected {expected_size}"
        ));
    }
    println!(
        "source: {} bytes, {} nodes, {} entries, parsed in {parsed_ms:.2} ms",
        source.bytes.len(),
        source.nodes.len(),
        source.entries.len()
    );
    println!(
        "v4: {output_size} bytes ({:.1} MiB), {node_count} nodes, {entry_count} entries, arena {:.1} MiB",
        output_size as f64 / 1024.0 / 1024.0,
        arena_len as f64 / 1024.0 / 1024.0
    );
    println!(
        "compacted: {dropped_nodes} orphan nodes, {dropped_entries} entries; content hash {hash:016x}; total {:.2} ms",
        started.elapsed().as_secs_f64() * 1000.0
    );
    Ok(())
}

fn node_record(node: SourceNode, parent: u32, name_offset: u32) -> [u8; NODE_RECORD_LEN] {
    let mut out = [0u8; NODE_RECORD_LEN];
    out[0..8].copy_from_slice(&node.mtime_ms.to_le_bytes());
    out[8..12].copy_from_slice(&parent.to_le_bytes());
    out[12..16].copy_from_slice(&name_offset.to_le_bytes());
    out[16..18].copy_from_slice(&node.name_len.to_le_bytes());
    out[18] = node.tier;
    out
}

fn entry_record(entry: SourceEntry, mapping: &[u32], name_offset: u32) -> [u8; ENTRY_RECORD_LEN] {
    let mut out = [0u8; ENTRY_RECORD_LEN];
    out[0..8].copy_from_slice(&entry.filter.to_le_bytes());
    out[8..12].copy_from_slice(&mapping[entry.parent as usize].to_le_bytes());
    let dir_id = if entry.is_directory {
        mapping[entry.dir_id as usize]
    } else {
        NO_DIR
    };
    out[12..16].copy_from_slice(&dir_id.to_le_bytes());
    out[16..20].copy_from_slice(&name_offset.to_le_bytes());
    out[20..22].copy_from_slice(&entry.name_len.to_le_bytes());
    out[22] = u8::from(entry.is_directory) | (entry.tier << 1);
    out
}

fn io_error(error: std::io::Error) -> String {
    error.to_string()
}

struct View<'a> {
    bytes: &'a [u8],
    root_start: usize,
    root_len: usize,
    node_start: usize,
    node_count: usize,
    entry_start: usize,
    entry_count: usize,
    arena_start: usize,
    arena_len: usize,
    expected_hash: u64,
    dropped_nodes: u32,
    dropped_entries: u32,
}

impl<'a> View<'a> {
    fn parse(bytes: &'a [u8]) -> Result<Self, String> {
        if bytes.len() < HEADER_LEN || &bytes[0..4] != V4_MAGIC {
            return Err("not a v4 prototype image".to_owned());
        }
        if read_u32(bytes, 4)? != V4_VERSION {
            return Err("unsupported prototype version".to_owned());
        }
        if read_u32(bytes, 8)? as usize != HEADER_LEN {
            return Err("unexpected header length".to_owned());
        }
        let root_len = read_u32(bytes, 12)? as usize;
        let node_count = read_u32(bytes, 16)? as usize;
        let entry_count = read_u32(bytes, 20)? as usize;
        let arena_len = usize::try_from(read_u64(bytes, 24)?)
            .map_err(|_| "arena length overflow".to_owned())?;
        let root_start = HEADER_LEN;
        let node_start = root_start
            .checked_add(root_len)
            .ok_or_else(|| "root offset overflow".to_owned())?;
        let entry_start = node_start
            .checked_add(
                node_count
                    .checked_mul(NODE_RECORD_LEN)
                    .ok_or("node overflow")?,
            )
            .ok_or_else(|| "entry offset overflow".to_owned())?;
        let arena_start = entry_start
            .checked_add(
                entry_count
                    .checked_mul(ENTRY_RECORD_LEN)
                    .ok_or("entry overflow")?,
            )
            .ok_or_else(|| "arena offset overflow".to_owned())?;
        let expected_len = arena_start
            .checked_add(arena_len)
            .ok_or_else(|| "file length overflow".to_owned())?;
        if expected_len != bytes.len() {
            return Err(format!(
                "file is {} bytes, header describes {expected_len}",
                bytes.len()
            ));
        }
        std::str::from_utf8(&bytes[root_start..node_start])
            .map_err(|error| format!("invalid root UTF-8: {error}"))?;
        Ok(Self {
            bytes,
            root_start,
            root_len,
            node_start,
            node_count,
            entry_start,
            entry_count,
            arena_start,
            arena_len,
            expected_hash: read_u64(bytes, 32)?,
            dropped_nodes: read_u32(bytes, 40)?,
            dropped_entries: read_u32(bytes, 44)?,
        })
    }

    fn root(&self) -> &[u8] {
        &self.bytes[self.root_start..self.root_start + self.root_len]
    }

    fn node_record(&self, id: usize) -> &[u8] {
        let start = self.node_start + id * NODE_RECORD_LEN;
        &self.bytes[start..start + NODE_RECORD_LEN]
    }

    fn node_parent(&self, id: usize) -> u32 {
        read_u32_unchecked(self.node_record(id), 8)
    }

    fn node_name(&self, id: usize) -> &[u8] {
        let record = self.node_record(id);
        let offset = read_u32_unchecked(record, 12) as usize;
        let len = read_u16_unchecked(record, 16) as usize;
        &self.bytes[self.arena_start + offset..self.arena_start + offset + len]
    }

    fn entry_record(&self, slot: usize) -> &[u8] {
        let start = self.entry_start + slot * ENTRY_RECORD_LEN;
        &self.bytes[start..start + ENTRY_RECORD_LEN]
    }

    fn entry_filter(&self, slot: usize) -> u64 {
        read_u64_unchecked(self.entry_record(slot), 0)
    }

    fn entry_parent(&self, slot: usize) -> u32 {
        read_u32_unchecked(self.entry_record(slot), 8)
    }

    fn entry_name(&self, slot: usize) -> &[u8] {
        let record = self.entry_record(slot);
        let offset = read_u32_unchecked(record, 16) as usize;
        let len = read_u16_unchecked(record, 20) as usize;
        &self.bytes[self.arena_start + offset..self.arena_start + offset + len]
    }

    fn validate(&self) -> Result<u64, String> {
        let mut hash = FNV_OFFSET;
        hash_bytes(&mut hash, self.root());
        for id in 0..self.node_count {
            let record = self.node_record(id);
            let parent = self.node_parent(id) as usize;
            if (id == 0 && parent != 0) || (id > 0 && parent >= id) {
                return Err(format!("node {id} has invalid parent {parent}"));
            }
            let name = self.node_name(id);
            std::str::from_utf8(name)
                .map_err(|error| format!("node {id} name is invalid UTF-8: {error}"))?;
            hash_bytes(&mut hash, record);
            hash_bytes(&mut hash, name);
        }
        for slot in 0..self.entry_count {
            let record = self.entry_record(slot);
            let parent = self.entry_parent(slot) as usize;
            if parent >= self.node_count {
                return Err(format!("entry {slot} has invalid parent {parent}"));
            }
            let name = self.entry_name(slot);
            std::str::from_utf8(name)
                .map_err(|error| format!("entry {slot} name is invalid UTF-8: {error}"))?;
            hash_bytes(&mut hash, record);
            hash_bytes(&mut hash, name);
        }
        if hash != self.expected_hash {
            return Err(format!(
                "content hash {hash:016x}, expected {:016x}",
                self.expected_hash
            ));
        }
        Ok(hash)
    }
}

fn bench_view(view: &View<'_>, storage: &str, open_ms: f64, hold: bool) -> Result<(), String> {
    let validate_started = Instant::now();
    let hash = view.validate()?;
    let validate_ms = validate_started.elapsed().as_secs_f64() * 1000.0;
    println!(
        "{storage}: {:.1} MiB, {} nodes, {} entries, arena {:.1} MiB, open {open_ms:.2} ms, validate {validate_ms:.2} ms",
        view.bytes.len() as f64 / 1024.0 / 1024.0,
        view.node_count,
        view.entry_count,
        view.arena_len as f64 / 1024.0 / 1024.0
    );
    println!(
        "content hash {hash:016x}; compacted {} nodes / {} entries",
        view.dropped_nodes, view.dropped_entries
    );

    let queries = [
        "g",
        "gh",
        "zzz_no_such_entry_zz",
        "процедура приемки",
        "ghjwtlehf ghbtvrb",
        "kirill macbook",
    ];
    for query in queries {
        let prepared = Prepared::new(view, query);
        let _ = scan(view, &prepared);
        let mut samples = Vec::new();
        let mut last = ScanResult::default();
        for _ in 0..5 {
            let started = Instant::now();
            last = scan(view, &prepared);
            samples.push(started.elapsed().as_secs_f64() * 1000.0);
        }
        samples.sort_by(f64::total_cmp);
        println!(
            "q={query:?}: median {:.2} ms, min {:.2}, max {:.2}; {} candidates, hash {:016x}",
            samples[2], samples[0], samples[4], last.matches, last.hash
        );
    }
    if hold {
        println!(
            "holding pid {} for 30 seconds after all pages were exercised",
            std::process::id()
        );
        std::io::stdout().flush().map_err(io_error)?;
        thread::sleep(Duration::from_secs(30));
    }
    Ok(())
}

#[derive(Default)]
struct ScanResult {
    matches: usize,
    hash: u64,
}

struct OverlayEntry {
    name: Vec<u8>,
    parent: u32,
    filter: u64,
}

struct Overlay {
    tombstones: Vec<u64>,
    additions: Vec<OverlayEntry>,
    removed: usize,
}

impl Overlay {
    fn new(base_entries: usize) -> Self {
        Self {
            tombstones: vec![0; base_entries.div_ceil(64)],
            additions: Vec::new(),
            removed: 0,
        }
    }

    fn remove(&mut self, slot: usize) {
        let word = slot / 64;
        let bit = 1u64 << (slot % 64);
        if self.tombstones[word] & bit == 0 {
            self.tombstones[word] |= bit;
            self.removed += 1;
        }
    }

    fn is_removed(&self, slot: usize) -> bool {
        self.tombstones[slot / 64] & (1u64 << (slot % 64)) != 0
    }

    fn add(&mut self, parent: u32, name: Vec<u8>) {
        let filter = name_filter(&name);
        self.additions.push(OverlayEntry {
            name,
            parent,
            filter,
        });
    }

    fn rename(&mut self, view: &View<'_>, slot: usize, name: Vec<u8>) {
        self.remove(slot);
        self.add(view.entry_parent(slot), name);
    }

    fn allocated_bytes(&self) -> usize {
        self.tombstones.capacity() * std::mem::size_of::<u64>()
            + self.additions.capacity() * std::mem::size_of::<OverlayEntry>()
            + self
                .additions
                .iter()
                .map(|entry| entry.name.capacity())
                .sum::<usize>()
    }
}

struct Prepared {
    sets: Vec<Vec<Vec<u8>>>,
    filters: Vec<Vec<u64>>,
    path_masks: Vec<Vec<u64>>,
}

impl Prepared {
    fn new(view: &View<'_>, query: &str) -> Self {
        let lower = query.to_lowercase();
        let mut variants = vec![lower.clone()];
        for variant in layout_variants(&lower) {
            if !variants.contains(&variant) {
                variants.push(variant);
            }
        }
        let sets: Vec<Vec<Vec<u8>>> = variants
            .iter()
            .map(|variant| {
                variant
                    .split_whitespace()
                    .map(|token| token.as_bytes().to_vec())
                    .collect()
            })
            .collect();
        let filters = sets
            .iter()
            .map(|tokens| tokens.iter().map(|token| name_filter(token)).collect())
            .collect();
        let path_masks = sets
            .iter()
            .map(|tokens| build_path_masks(view, tokens))
            .collect();
        Self {
            sets,
            filters,
            path_masks,
        }
    }
}

fn build_path_masks(view: &View<'_>, tokens: &[Vec<u8>]) -> Vec<u64> {
    if tokens.len() <= 1 || tokens.len() >= 63 {
        return Vec::new();
    }
    let mut masks = vec![CACHED_MASK | component_bits(view.root(), tokens); view.node_count];
    for id in 1..view.node_count {
        let parent = view.node_parent(id) as usize;
        let bits = component_bits(view.node_name(id), tokens);
        masks[id] = CACHED_MASK | (masks[parent] & !CACHED_MASK) | bits;
    }
    masks
}

fn component_bits(name: &[u8], tokens: &[Vec<u8>]) -> u64 {
    let mut bits = 0u64;
    for (index, token) in tokens.iter().enumerate() {
        if contains_ci(name, token) {
            bits |= 1 << index;
        }
    }
    bits
}

fn scan(view: &View<'_>, prepared: &Prepared) -> ScanResult {
    scan_with_overlay(view, prepared, None)
}

fn scan_with_overlay(
    view: &View<'_>,
    prepared: &Prepared,
    overlay: Option<&Overlay>,
) -> ScanResult {
    let mut out = ScanResult {
        matches: 0,
        hash: FNV_OFFSET,
    };
    for slot in 0..view.entry_count {
        if overlay.is_some_and(|overlay| overlay.is_removed(slot)) {
            continue;
        }
        let name = view.entry_name(slot);
        let item_filter = view.entry_filter(slot);
        let parent = view.entry_parent(slot) as usize;
        if item_matches(name, item_filter, parent, prepared) {
            out.matches += 1;
            hash_bytes(&mut out.hash, &(slot as u64).to_le_bytes());
            hash_bytes(&mut out.hash, name);
        }
    }
    if let Some(overlay) = overlay {
        for (index, entry) in overlay.additions.iter().enumerate() {
            if item_matches(&entry.name, entry.filter, entry.parent as usize, prepared) {
                out.matches += 1;
                let logical_slot = view.entry_count as u64 + index as u64;
                hash_bytes(&mut out.hash, &logical_slot.to_le_bytes());
                hash_bytes(&mut out.hash, &entry.name);
            }
        }
    }
    out
}

fn item_matches(name: &[u8], item_filter: u64, parent: usize, prepared: &Prepared) -> bool {
    prepared.sets.iter().enumerate().any(|(set_index, tokens)| {
        if tokens.len() == 1 {
            return filter_contains(item_filter, prepared.filters[set_index][0])
                && contains_ci(name, &tokens[0]);
        }
        tokens.iter().enumerate().all(|(token_index, token)| {
            (filter_contains(item_filter, prepared.filters[set_index][token_index])
                && contains_ci(name, token))
                || prepared.path_masks[set_index][parent] & (1 << token_index) != 0
        })
    })
}

fn bench_overlay(view: &View<'_>) -> Result<(), String> {
    const REMOVALS: usize = 10_000;
    const RENAMES: usize = 5_000;
    const ADDS: usize = 5_000;
    if view.entry_count < REMOVALS {
        return Err("overlay benchmark needs at least 10,000 base entries".to_owned());
    }
    let started = Instant::now();
    let mut overlay = Overlay::new(view.entry_count);
    for index in 0..REMOVALS {
        let slot = index * view.entry_count / REMOVALS;
        if index < RENAMES {
            overlay.rename(
                view,
                slot,
                format!("__beeline_v4_overlay_probe__{index:05}.txt").into_bytes(),
            );
        } else {
            overlay.remove(slot);
        }
    }
    for index in 0..ADDS {
        overlay.add(
            0,
            format!("__beeline_v4_overlay_added__{index:05}.txt").into_bytes(),
        );
    }
    let mutation_ms = started.elapsed().as_secs_f64() * 1000.0;
    if overlay.removed != REMOVALS || overlay.additions.len() != RENAMES + ADDS {
        return Err("overlay mutation counts do not match".to_owned());
    }

    let probes = [
        ("__beeline_v4_overlay_probe__", RENAMES),
        ("__beeline_v4_overlay_added__", ADDS),
        ("zzz_no_such_entry_zz", 0),
    ];
    println!(
        "overlay: {REMOVALS} tombstones, {RENAMES} renames, {ADDS} adds in {mutation_ms:.2} ms; allocated {:.2} MiB",
        overlay.allocated_bytes() as f64 / 1024.0 / 1024.0
    );
    for (query, expected) in probes {
        let prepared = Prepared::new(view, query);
        let mut samples = Vec::new();
        let mut last = ScanResult::default();
        for _ in 0..5 {
            let started = Instant::now();
            last = scan_with_overlay(view, &prepared, Some(&overlay));
            samples.push(started.elapsed().as_secs_f64() * 1000.0);
        }
        samples.sort_by(f64::total_cmp);
        if last.matches != expected {
            return Err(format!(
                "overlay query {query:?} returned {}, expected {expected}",
                last.matches
            ));
        }
        println!(
            "overlay q={query:?}: median {:.2} ms; {} candidates, hash {:016x}",
            samples[2], last.matches, last.hash
        );
    }
    Ok(())
}

fn name_filter(name: &[u8]) -> u64 {
    let mut filter = 0u64;
    if name.is_ascii() {
        for &byte in name {
            filter |= filter_bit(byte);
        }
        return filter;
    }
    let name = String::from_utf8_lossy(name);
    for ch in name.chars() {
        for lower in ch.to_lowercase() {
            let mut encoded = [0u8; 4];
            for &byte in lower.encode_utf8(&mut encoded).as_bytes() {
                filter |= filter_bit(byte);
            }
        }
    }
    filter
}

fn filter_bit(byte: u8) -> u64 {
    let lower = byte.to_ascii_lowercase();
    let index = match lower {
        b'a'..=b'z' => lower - b'a',
        b'0'..=b'9' => 26 + lower - b'0',
        _ => 36 + lower % 28,
    };
    1u64 << index
}

fn filter_contains(item_filter: u64, query_filter: u64) -> bool {
    item_filter & query_filter == query_filter
}

fn contains_ci(haystack: &[u8], needle_lower: &[u8]) -> bool {
    if needle_lower.is_empty() {
        return true;
    }
    if haystack.len() < needle_lower.len() {
        return false;
    }
    if haystack.is_ascii() && needle_lower.is_ascii() {
        return haystack
            .windows(needle_lower.len())
            .any(|window| window.eq_ignore_ascii_case(needle_lower));
    }
    let haystack = String::from_utf8_lossy(haystack).to_lowercase();
    let needle = String::from_utf8_lossy(needle_lower);
    haystack.contains(needle.as_ref())
}

const LAYOUT_PAIRS: &[(char, char)] = &[
    ('`', 'ё'),
    ('q', 'й'),
    ('w', 'ц'),
    ('e', 'у'),
    ('r', 'к'),
    ('t', 'е'),
    ('y', 'н'),
    ('u', 'г'),
    ('i', 'ш'),
    ('o', 'щ'),
    ('p', 'з'),
    ('[', 'х'),
    (']', 'ъ'),
    ('a', 'ф'),
    ('s', 'ы'),
    ('d', 'в'),
    ('f', 'а'),
    ('g', 'п'),
    ('h', 'р'),
    ('j', 'о'),
    ('k', 'л'),
    ('l', 'д'),
    (';', 'ж'),
    ('\'', 'э'),
    ('z', 'я'),
    ('x', 'ч'),
    ('c', 'с'),
    ('v', 'м'),
    ('b', 'и'),
    ('n', 'т'),
    ('m', 'ь'),
    (',', 'б'),
    ('.', 'ю'),
    ('/', '.'),
];

fn layout_variants(query: &str) -> Vec<String> {
    let mut variants = Vec::new();
    let to_ru = map_layout(query, true);
    if to_ru != query {
        variants.push(to_ru);
    }
    let to_en = map_layout(query, false);
    if to_en != query {
        variants.push(to_en);
    }
    variants
}

fn map_layout(input: &str, to_ru: bool) -> String {
    input
        .chars()
        .map(|ch| {
            let lower = ch.to_lowercase().next().unwrap_or(ch);
            LAYOUT_PAIRS
                .iter()
                .find_map(|&(en, ru)| {
                    if (to_ru && lower == en) || (!to_ru && lower == ru) {
                        Some(if to_ru { ru } else { en })
                    } else {
                        None
                    }
                })
                .unwrap_or(ch)
        })
        .collect()
}

fn hash_bytes(hash: &mut u64, bytes: &[u8]) {
    for &byte in bytes {
        *hash ^= byte as u64;
        *hash = hash.wrapping_mul(FNV_PRIME);
    }
}

fn read_u16_unchecked(bytes: &[u8], offset: usize) -> u16 {
    u16::from_le_bytes([bytes[offset], bytes[offset + 1]])
}

fn read_u32_unchecked(bytes: &[u8], offset: usize) -> u32 {
    u32::from_le_bytes(bytes[offset..offset + 4].try_into().unwrap())
}

fn read_u64_unchecked(bytes: &[u8], offset: usize) -> u64 {
    u64::from_le_bytes(bytes[offset..offset + 8].try_into().unwrap())
}

fn read_u32(bytes: &[u8], offset: usize) -> Result<u32, String> {
    let value = bytes
        .get(offset..offset + 4)
        .ok_or_else(|| format!("missing u32 at {offset}"))?;
    Ok(u32::from_le_bytes(value.try_into().unwrap()))
}

fn read_u64(bytes: &[u8], offset: usize) -> Result<u64, String> {
    let value = bytes
        .get(offset..offset + 8)
        .ok_or_else(|| format!("missing u64 at {offset}"))?;
    Ok(u64::from_le_bytes(value.try_into().unwrap()))
}

struct ReadOnlyMmap {
    ptr: *mut u8,
    len: usize,
}

impl ReadOnlyMmap {
    fn map(file: &File) -> Result<Self, String> {
        let len = file.metadata().map_err(io_error)?.len() as usize;
        if len == 0 {
            return Err("cannot map an empty file".to_owned());
        }
        let mapped = unsafe {
            mmap(
                ptr::null_mut(),
                len,
                PROT_READ,
                MAP_PRIVATE,
                file.as_raw_fd(),
                0,
            )
        };
        if mapped as isize == -1 {
            return Err(std::io::Error::last_os_error().to_string());
        }
        Ok(Self {
            ptr: mapped.cast(),
            len,
        })
    }

    fn as_slice(&self) -> &[u8] {
        unsafe { std::slice::from_raw_parts(self.ptr, self.len) }
    }
}

impl Drop for ReadOnlyMmap {
    fn drop(&mut self) {
        unsafe {
            munmap(self.ptr.cast(), self.len);
        }
    }
}

const PROT_READ: i32 = 0x1;
const MAP_PRIVATE: i32 = 0x0002;

extern "C" {
    fn mmap(
        address: *mut c_void,
        length: usize,
        protection: i32,
        flags: i32,
        fd: i32,
        offset: i64,
    ) -> *mut c_void;
    fn munmap(address: *mut c_void, length: usize) -> i32;
}

fn run() -> Result<(), String> {
    let args: Vec<String> = env::args().collect();
    match args.as_slice() {
        [_, command, source, output] if command == "build" => {
            build_v4(Path::new(source), Path::new(output))
        }
        [_, command, image] if command == "bench-heap" || command == "hold-heap" => {
            let started = Instant::now();
            let bytes = fs::read(image).map_err(io_error)?;
            let open_ms = started.elapsed().as_secs_f64() * 1000.0;
            let view = View::parse(&bytes)?;
            bench_view(&view, "heap", open_ms, command == "hold-heap")
        }
        [_, command, image] if command == "bench-mmap" || command == "hold-mmap" => {
            let started = Instant::now();
            let file = File::open(image).map_err(io_error)?;
            let mapped = ReadOnlyMmap::map(&file)?;
            let open_ms = started.elapsed().as_secs_f64() * 1000.0;
            let view = View::parse(mapped.as_slice())?;
            bench_view(&view, "mmap", open_ms, command == "hold-mmap")
        }
        [_, command, image] if command == "bench-overlay" => {
            let file = File::open(image).map_err(io_error)?;
            let mapped = ReadOnlyMmap::map(&file)?;
            let view = View::parse(mapped.as_slice())?;
            view.validate()?;
            bench_overlay(&view)
        }
        _ => Err(format!(
            "usage:\n  {} build <v3.idx> <v4-prototype.idx>\n  {} bench-heap|hold-heap <v4-prototype.idx>\n  {} bench-mmap|hold-mmap|bench-overlay <v4-prototype.idx>",
            args.first().map_or("prototype", String::as_str),
            args.first().map_or("prototype", String::as_str),
            args.first().map_or("prototype", String::as_str)
        )),
    }
}

fn main() {
    if let Err(error) = run() {
        eprintln!("error: {error}");
        std::process::exit(1);
    }
}

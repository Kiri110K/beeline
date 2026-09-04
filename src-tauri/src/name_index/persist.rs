//! Atomic persistence for the mapped Name Index v4.
//!
//! Version 3 and older are deliberately not migrated. A non-v4 file loads as absent and
//! the normal initial crawl replaces it. Saves stream records and the UTF-8 arena directly
//! to the temporary file; no whole-index encode or decode buffer exists.

use std::{
    fs::{self, File},
    io::{BufWriter, Seek, SeekFrom, Write},
    os::fd::AsRawFd,
    os::unix::fs::MetadataExt,
    path::{Path, PathBuf},
    sync::{Arc, RwLock},
};

use crate::name_index::model::{
    hash_bytes, IndexData, MappedBase, ENTRY_RECORD_LEN, FNV_OFFSET, HEADER_LEN, MAGIC,
    NODE_RECORD_LEN, NO_DIR, VERSION,
};

struct Layout {
    nodes: Vec<u32>,
    node_ranges: Vec<(u32, u32)>,
    entries: Vec<u32>,
    dir_mapping: Vec<u32>,
    arena_len: usize,
    node_name_bytes: usize,
}

struct PersistedShape {
    root_len: usize,
    node_count: usize,
    entry_count: usize,
    arena_len: usize,
}

const F_NOCACHE: i32 = 48;
const TRUST_MAGIC: &[u8; 4] = b"BLNT";
const TRUST_VERSION: u32 = 1;
const TRUST_LEN: usize = 64;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct FileIdentity {
    device: u64,
    inode: u64,
    size: u64,
    modified_seconds: i64,
    modified_nanoseconds: i64,
    changed_seconds: i64,
    changed_nanoseconds: i64,
}

impl FileIdentity {
    fn read(path: &Path) -> std::io::Result<Self> {
        let metadata = fs::metadata(path)?;
        Ok(Self {
            device: metadata.dev(),
            inode: metadata.ino(),
            size: metadata.size(),
            modified_seconds: metadata.mtime(),
            modified_nanoseconds: metadata.mtime_nsec(),
            changed_seconds: metadata.ctime(),
            changed_nanoseconds: metadata.ctime_nsec(),
        })
    }

    fn encode(self) -> [u8; TRUST_LEN] {
        let mut bytes = [0u8; TRUST_LEN];
        bytes[0..4].copy_from_slice(TRUST_MAGIC);
        bytes[4..8].copy_from_slice(&TRUST_VERSION.to_le_bytes());
        bytes[8..16].copy_from_slice(&self.device.to_le_bytes());
        bytes[16..24].copy_from_slice(&self.inode.to_le_bytes());
        bytes[24..32].copy_from_slice(&self.size.to_le_bytes());
        bytes[32..40].copy_from_slice(&self.modified_seconds.to_le_bytes());
        bytes[40..48].copy_from_slice(&self.modified_nanoseconds.to_le_bytes());
        bytes[48..56].copy_from_slice(&self.changed_seconds.to_le_bytes());
        bytes[56..64].copy_from_slice(&self.changed_nanoseconds.to_le_bytes());
        bytes
    }

    fn decode(bytes: &[u8]) -> Option<Self> {
        if bytes.len() != TRUST_LEN
            || bytes.get(0..4) != Some(TRUST_MAGIC)
            || u32::from_le_bytes(bytes.get(4..8)?.try_into().ok()?) != TRUST_VERSION
        {
            return None;
        }
        let u64_at = |start| {
            bytes
                .get(start..start + 8)?
                .try_into()
                .ok()
                .map(u64::from_le_bytes)
        };
        let i64_at = |start| {
            bytes
                .get(start..start + 8)?
                .try_into()
                .ok()
                .map(i64::from_le_bytes)
        };
        Some(Self {
            device: u64_at(8)?,
            inode: u64_at(16)?,
            size: u64_at(24)?,
            modified_seconds: i64_at(32)?,
            modified_nanoseconds: i64_at(40)?,
            changed_seconds: i64_at(48)?,
            changed_nanoseconds: i64_at(56)?,
        })
    }
}

fn trust_path(path: &Path) -> PathBuf {
    path.with_extension("idx.trust")
}

fn read_trust(path: &Path) -> Option<FileIdentity> {
    FileIdentity::decode(&fs::read(trust_path(path)).ok()?)
}

fn write_trust(path: &Path, identity: FileIdentity) -> std::io::Result<()> {
    let trust = trust_path(path);
    let temporary = path.with_extension("idx.trust.tmp");
    fs::write(&temporary, identity.encode())?;
    fs::rename(temporary, trust)
}

unsafe extern "C" {
    fn fcntl(fd: i32, command: i32, ...) -> i32;
}

fn disable_file_cache(file: &File) -> std::io::Result<()> {
    let result = unsafe { fcntl(file.as_raw_fd(), F_NOCACHE, 1) };
    if result == 0 {
        Ok(())
    } else {
        Err(std::io::Error::last_os_error())
    }
}

fn build_layout(index: &IndexData) -> std::io::Result<Layout> {
    let mut nodes = vec![0u32];
    let mut node_ranges = Vec::new();
    let mut entries = Vec::with_capacity(index.len());
    let mut dir_mapping = vec![NO_DIR; index.node_len()];
    let mut entry_name_bytes = 0usize;
    dir_mapping[0] = 0;
    let mut cursor = 0usize;
    while cursor < nodes.len() {
        let dir = nodes[cursor];
        // Query ranking has its own total-order tie break, so the on-disk Item order is not
        // observable. Preserve the existing/overlay order instead of sorting millions of
        // names every time startup changes a few filesystem entries.
        let direct = index.direct_entry_slots(dir);
        let first = u32::try_from(entries.len())
            .map_err(|_| std::io::Error::other("Name Index exceeds u32 entries"))?;
        for &slot in &direct {
            let entry = index.entry(slot as usize).expect("live direct entry");
            entry_name_bytes = entry_name_bytes
                .checked_add(entry.name.len())
                .ok_or_else(|| std::io::Error::other("Name Index arena overflow"))?;
            if entry.is_directory && dir_mapping[entry.dir_id as usize] == NO_DIR {
                let next = u32::try_from(nodes.len())
                    .map_err(|_| std::io::Error::other("Name Index exceeds u32 nodes"))?;
                dir_mapping[entry.dir_id as usize] = next;
                nodes.push(entry.dir_id);
            }
        }
        let count = u32::try_from(direct.len())
            .map_err(|_| std::io::Error::other("directory exceeds u32 children"))?;
        node_ranges.push((first, count));
        entries.extend(direct);
        cursor += 1;
    }

    let node_name_bytes = nodes.iter().try_fold(0usize, |total, id| {
        let len = index
            .node(*id)
            .ok_or_else(|| std::io::Error::other("reachable directory node is missing"))?
            .name
            .len();
        total
            .checked_add(len)
            .ok_or_else(|| std::io::Error::other("Name Index arena overflow"))
    })?;
    let arena_len = node_name_bytes
        .checked_add(entry_name_bytes)
        .ok_or_else(|| std::io::Error::other("Name Index arena overflow"))?;
    if arena_len > u32::MAX as usize {
        return Err(std::io::Error::other(
            "Name Index name arena exceeds u32 offsets",
        ));
    }
    Ok(Layout {
        nodes,
        node_ranges,
        entries,
        dir_mapping,
        arena_len,
        node_name_bytes,
    })
}

fn node_record(
    index: &IndexData,
    layout: &Layout,
    position: usize,
    name_offset: u32,
) -> std::io::Result<[u8; NODE_RECORD_LEN]> {
    let old_id = layout.nodes[position];
    let node = index
        .node(old_id)
        .ok_or_else(|| std::io::Error::other("directory disappeared during save"))?;
    let parent = if position == 0 {
        0
    } else {
        layout.dir_mapping[node.parent as usize]
    };
    if parent == NO_DIR {
        return Err(std::io::Error::other("directory parent is unreachable"));
    }
    let name_len = u16::try_from(node.name.len())
        .map_err(|_| std::io::Error::other("directory name exceeds u16 bytes"))?;
    let (first, count) = layout.node_ranges[position];
    let mut record = [0u8; NODE_RECORD_LEN];
    record[0..8].copy_from_slice(&node.mtime_ms.to_le_bytes());
    record[8..12].copy_from_slice(&parent.to_le_bytes());
    record[12..16].copy_from_slice(&first.to_le_bytes());
    record[16..20].copy_from_slice(&count.to_le_bytes());
    record[20..24].copy_from_slice(&name_offset.to_le_bytes());
    record[24..26].copy_from_slice(&name_len.to_le_bytes());
    record[26] = node.tier as u8;
    Ok(record)
}

fn entry_record(
    index: &IndexData,
    layout: &Layout,
    slot: u32,
    name_offset: u32,
) -> std::io::Result<[u8; ENTRY_RECORD_LEN]> {
    let entry = index
        .entry(slot as usize)
        .ok_or_else(|| std::io::Error::other("Item disappeared during save"))?;
    let parent = layout.dir_mapping[entry.parent as usize];
    if parent == NO_DIR {
        return Err(std::io::Error::other("Item parent is unreachable"));
    }
    let dir_id = if entry.is_directory {
        let mapped = layout.dir_mapping[entry.dir_id as usize];
        if mapped == NO_DIR {
            return Err(std::io::Error::other("directory Item is unreachable"));
        }
        mapped
    } else {
        NO_DIR
    };
    let name_len = u16::try_from(entry.name.len())
        .map_err(|_| std::io::Error::other("Item name exceeds u16 bytes"))?;
    let mut record = [0u8; ENTRY_RECORD_LEN];
    record[0..8].copy_from_slice(&entry.filter.to_le_bytes());
    record[8..12].copy_from_slice(&parent.to_le_bytes());
    record[12..16].copy_from_slice(&dir_id.to_le_bytes());
    record[16..20].copy_from_slice(&name_offset.to_le_bytes());
    record[20..22].copy_from_slice(&name_len.to_le_bytes());
    record[22] = u8::from(entry.is_directory) | ((entry.tier as u8) << 1);
    Ok(record)
}

fn visit_payload(
    index: &IndexData,
    layout: &Layout,
    mut visit: impl FnMut(&[u8]) -> std::io::Result<()>,
) -> std::io::Result<()> {
    visit(index.root.to_string_lossy().as_bytes())?;
    let mut name_offset = 0u32;
    for position in 0..layout.nodes.len() {
        let record = node_record(index, layout, position, name_offset)?;
        visit(&record)?;
        name_offset = name_offset
            .checked_add(
                u32::try_from(index.node(layout.nodes[position]).unwrap().name.len()).unwrap(),
            )
            .ok_or_else(|| std::io::Error::other("Name Index arena overflow"))?;
    }
    debug_assert_eq!(name_offset as usize, layout.node_name_bytes);
    for &slot in &layout.entries {
        let record = entry_record(index, layout, slot, name_offset)?;
        visit(&record)?;
        name_offset = name_offset
            .checked_add(u32::try_from(index.entry(slot as usize).unwrap().name.len()).unwrap())
            .ok_or_else(|| std::io::Error::other("Name Index arena overflow"))?;
    }
    for &id in &layout.nodes {
        visit(index.node(id).unwrap().name.as_bytes())?;
    }
    for &slot in &layout.entries {
        visit(index.entry(slot as usize).unwrap().name.as_bytes())?;
    }
    Ok(())
}

fn write_index(index: &IndexData, path: &Path, layout: &Layout) -> std::io::Result<PersistedShape> {
    let root = index.root.to_string_lossy();
    let root_len = u32::try_from(root.len())
        .map_err(|_| std::io::Error::other("Name Index root exceeds u32 bytes"))?;
    let node_count = u32::try_from(layout.nodes.len())
        .map_err(|_| std::io::Error::other("Name Index exceeds u32 nodes"))?;
    let entry_count = u32::try_from(layout.entries.len())
        .map_err(|_| std::io::Error::other("Name Index exceeds u32 entries"))?;

    let file = File::create(path)?;
    // The temp index is written once, fsynced, then memory-mapped. Keeping its dirty pages
    // in this process's unified file cache can add the full 300+ MiB output size to the
    // resident old mapping, so bypass that cache for this sequential atomic write.
    disable_file_cache(&file)?;
    let mut writer = BufWriter::with_capacity(1024 * 1024, file);
    // The checksum covers only the payload. Reserve the header, hash while writing the
    // payload once, then seek back to fill it; the old path walked all 5M entries twice.
    writer.write_all(&[0u8; HEADER_LEN])?;
    let mut hash = FNV_OFFSET;
    visit_payload(index, layout, |bytes| {
        hash_bytes(&mut hash, bytes);
        writer.write_all(bytes)
    })?;
    writer.flush()?;

    let mut header = [0u8; HEADER_LEN];
    header[0..4].copy_from_slice(MAGIC);
    header[4..8].copy_from_slice(&VERSION.to_le_bytes());
    header[8..12].copy_from_slice(&(HEADER_LEN as u32).to_le_bytes());
    header[12..16].copy_from_slice(&root_len.to_le_bytes());
    header[16..20].copy_from_slice(&node_count.to_le_bytes());
    header[20..24].copy_from_slice(&entry_count.to_le_bytes());
    header[24..32].copy_from_slice(&(layout.arena_len as u64).to_le_bytes());
    header[32..40].copy_from_slice(&hash.to_le_bytes());
    writer.seek(SeekFrom::Start(0))?;
    writer.write_all(&header)?;
    writer.flush()?;
    writer.get_ref().sync_all()?;
    Ok(PersistedShape {
        root_len: root.len(),
        node_count: layout.nodes.len(),
        entry_count: layout.entries.len(),
        arena_len: layout.arena_len,
    })
}

pub fn index_path(app_data_dir: &Path, volume_key: &str) -> PathBuf {
    app_data_dir
        .join("name_index")
        .join(format!("{volume_key}.idx"))
}

pub fn load(path: &Path, expected_root: &Path) -> Option<IndexData> {
    let identity = FileIdentity::read(path).ok()?;
    let prevalidated = read_trust(path).is_some_and(|trusted| trusted == identity);
    let base = if prevalidated {
        MappedBase::open_prevalidated(path, expected_root)
    } else {
        MappedBase::open(path, expected_root)
    }
    .ok()
    .flatten()?;
    if !prevalidated {
        // A missing stamp is only a performance miss. The fully validated index remains
        // usable even if this best-effort cache write fails.
        let _ = write_trust(path, identity);
    }
    Some(IndexData::from_base(expected_root.to_path_buf(), base))
}

pub fn save(shared: &Arc<RwLock<IndexData>>, path: &Path) -> std::io::Result<()> {
    if path.exists() && !shared.read().expect("name index lock poisoned").is_dirty() {
        return Ok(());
    }
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    let temp_path = path.with_extension("idx.tmp");
    let (revision, layout) = {
        let index = shared.read().expect("name index lock poisoned");
        let revision = index.revision;
        let layout = build_layout(&index)?;
        (revision, layout)
    };
    let mut index = shared.write().expect("name index lock poisoned");
    if index.revision != revision {
        return Err(std::io::Error::other(
            "Name Index changed while preparing persistence",
        ));
    }
    // `build_layout` walked the entire old mapping. Replace it with an untouched mapping
    // before the payload pass so their resident page sets cannot accumulate. Keep the write
    // lock through the pass: a concurrent full-index query would otherwise warm it again.
    index.remap_base_cold(path)?;
    let shape = write_index(&index, &temp_path, &layout)?;
    let base = MappedBase::open_trusted(
        &temp_path,
        shape.root_len,
        shape.node_count,
        shape.entry_count,
        shape.arena_len,
    )?;
    fs::rename(&temp_path, path)?;
    if let Some(parent) = path.parent() {
        // The journal and FSEvents cursor may advance immediately after this returns. Make
        // the directory entry durable first so a power loss cannot resurrect the old base
        // while preserving the newer cursor.
        File::open(parent)?.sync_all()?;
    }
    // The file was checksummed while writing, synced, and structurally produced from a
    // live IndexData. Bind that validation to the final inode for the next launch.
    if let Ok(identity) = FileIdentity::read(path) {
        let _ = write_trust(path, identity);
    }
    index.replace_base(base);
    Ok(())
}

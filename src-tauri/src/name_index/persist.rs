//! Atomic persistence for the mapped Name Index v4.
//!
//! Version 3 and older are deliberately not migrated. A non-v4 file loads as absent and
//! the normal initial crawl replaces it. Saves stream records and the UTF-8 arena directly
//! to the temporary file; no whole-index encode or decode buffer exists.

use std::{
    fs::{self, File},
    io::{BufWriter, Write},
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

fn build_layout(index: &IndexData) -> std::io::Result<Layout> {
    let mut nodes = vec![0u32];
    let mut node_ranges = Vec::new();
    let mut entries = Vec::with_capacity(index.len());
    let mut dir_mapping = vec![NO_DIR; index.node_len()];
    dir_mapping[0] = 0;
    let mut cursor = 0usize;
    while cursor < nodes.len() {
        let dir = nodes[cursor];
        let mut direct = index.direct_entry_slots(dir);
        direct.sort_unstable_by(|left, right| {
            let left = index.entry(*left as usize).expect("live direct entry");
            let right = index.entry(*right as usize).expect("live direct entry");
            left.name.as_bytes().cmp(right.name.as_bytes())
        });
        let first = u32::try_from(entries.len())
            .map_err(|_| std::io::Error::other("Name Index exceeds u32 entries"))?;
        for &slot in &direct {
            let entry = index.entry(slot as usize).expect("live direct entry");
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
    let arena_len = entries.iter().try_fold(node_name_bytes, |total, slot| {
        let len = index
            .entry(*slot as usize)
            .ok_or_else(|| std::io::Error::other("reachable Item is missing"))?
            .name
            .len();
        total
            .checked_add(len)
            .ok_or_else(|| std::io::Error::other("Name Index arena overflow"))
    })?;
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

fn write_index(index: &IndexData, path: &Path) -> std::io::Result<()> {
    let layout = build_layout(index)?;
    let root = index.root.to_string_lossy();
    let root_len = u32::try_from(root.len())
        .map_err(|_| std::io::Error::other("Name Index root exceeds u32 bytes"))?;
    let node_count = u32::try_from(layout.nodes.len())
        .map_err(|_| std::io::Error::other("Name Index exceeds u32 nodes"))?;
    let entry_count = u32::try_from(layout.entries.len())
        .map_err(|_| std::io::Error::other("Name Index exceeds u32 entries"))?;

    let mut hash = FNV_OFFSET;
    visit_payload(index, &layout, |bytes| {
        hash_bytes(&mut hash, bytes);
        Ok(())
    })?;

    let mut header = [0u8; HEADER_LEN];
    header[0..4].copy_from_slice(MAGIC);
    header[4..8].copy_from_slice(&VERSION.to_le_bytes());
    header[8..12].copy_from_slice(&(HEADER_LEN as u32).to_le_bytes());
    header[12..16].copy_from_slice(&root_len.to_le_bytes());
    header[16..20].copy_from_slice(&node_count.to_le_bytes());
    header[20..24].copy_from_slice(&entry_count.to_le_bytes());
    header[24..32].copy_from_slice(&(layout.arena_len as u64).to_le_bytes());
    header[32..40].copy_from_slice(&hash.to_le_bytes());

    let file = File::create(path)?;
    let mut writer = BufWriter::with_capacity(1024 * 1024, file);
    writer.write_all(&header)?;
    visit_payload(index, &layout, |bytes| writer.write_all(bytes))?;
    writer.flush()?;
    writer.get_ref().sync_all()
}

pub fn index_path(app_data_dir: &Path, volume_key: &str) -> PathBuf {
    app_data_dir
        .join("name_index")
        .join(format!("{volume_key}.idx"))
}

pub fn load(path: &Path, expected_root: &Path) -> Option<IndexData> {
    MappedBase::open(path, expected_root)
        .ok()
        .flatten()
        .map(|base| IndexData::from_base(expected_root.to_path_buf(), base))
}

pub fn save(shared: &Arc<RwLock<IndexData>>, path: &Path) -> std::io::Result<()> {
    if path.exists() && !shared.read().expect("name index lock poisoned").is_dirty() {
        return Ok(());
    }
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    let temp_path = path.with_extension("idx.tmp");
    let revision = {
        let index = shared.read().expect("name index lock poisoned");
        let revision = index.revision;
        write_index(&index, &temp_path)?;
        revision
    };
    fs::rename(&temp_path, path)?;
    let Some(base) = MappedBase::open(path, &shared.read().unwrap().root)? else {
        return Err(std::io::Error::other("new Name Index v4 failed validation"));
    };
    let mut index = shared.write().expect("name index lock poisoned");
    if index.revision == revision {
        index.replace_base(base);
    }
    Ok(())
}

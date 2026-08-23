//! On-disk persistence for the Name Index (SPEC §11).
//!
//! Format choice: a hand-rolled, length-prefixed binary layout rather than
//! `serde_json`. At the reference machine's scale (3.2 M entries) a JSON document is
//! hundreds of MB and parses in well over a second; the binary format below is a flat
//! sequence of fixed-width integers and length-prefixed UTF-8, so a load is a single
//! `read` plus a linear scan with no tokenizing — tens of ms, and a much smaller file.
//!
//! Compaction: only live entries are written, so tombstones from churn disappear on
//! every save. Directory *node* ids are preserved (entries reference them), so a few
//! orphaned nodes from deep removals can accumulate across save/load cycles — a slow,
//! bounded leak, noted as an open question rather than solved here.
//!
//! The file is written atomically (temp + rename): a crash mid-write can never leave a
//! half-written index.

use std::{
    fs,
    path::{Path, PathBuf},
    sync::{Arc, RwLock},
};

use crate::name_index::model::{DirNode, Entry, IndexData, Tier, NO_DIR};

const MAGIC: &[u8; 4] = b"BLNI";
const VERSION: u32 = 1;

fn put_u32(buffer: &mut Vec<u8>, value: u32) {
    buffer.extend_from_slice(&value.to_le_bytes());
}

fn put_i64(buffer: &mut Vec<u8>, value: i64) {
    buffer.extend_from_slice(&value.to_le_bytes());
}

fn put_str(buffer: &mut Vec<u8>, value: &str) {
    put_u32(buffer, value.len() as u32);
    buffer.extend_from_slice(value.as_bytes());
}

/// Cursor over the loaded bytes; every read is bounds-checked and yields `None` on a
/// short or malformed buffer, so a corrupt file degrades to "no index" not a panic.
struct Reader<'a> {
    bytes: &'a [u8],
    offset: usize,
}

impl<'a> Reader<'a> {
    fn new(bytes: &'a [u8]) -> Self {
        Self { bytes, offset: 0 }
    }

    fn take(&mut self, count: usize) -> Option<&'a [u8]> {
        let end = self.offset.checked_add(count)?;
        let slice = self.bytes.get(self.offset..end)?;
        self.offset = end;
        Some(slice)
    }

    fn u32(&mut self) -> Option<u32> {
        let bytes = self.take(4)?;
        Some(u32::from_le_bytes(bytes.try_into().ok()?))
    }

    fn i64(&mut self) -> Option<i64> {
        let bytes = self.take(8)?;
        Some(i64::from_le_bytes(bytes.try_into().ok()?))
    }

    fn u8(&mut self) -> Option<u8> {
        Some(self.take(1)?[0])
    }

    fn string(&mut self) -> Option<String> {
        let length = self.u32()? as usize;
        let bytes = self.take(length)?;
        String::from_utf8(bytes.to_vec()).ok()
    }
}

/// Serialize the index into the binary layout.
fn encode(index: &IndexData) -> Vec<u8> {
    let mut buffer = Vec::new();
    buffer.extend_from_slice(MAGIC);
    put_u32(&mut buffer, VERSION);
    put_str(&mut buffer, &index.root.to_string_lossy());

    put_u32(&mut buffer, index.nodes.len() as u32);
    for node in &index.nodes {
        put_u32(&mut buffer, node.parent);
        buffer.push(node.tier as u8);
        put_i64(&mut buffer, node.mtime_ms);
        put_str(&mut buffer, &node.name);
    }

    let live = index.entries.iter().flatten().count();
    put_u32(&mut buffer, live as u32);
    for entry in index.entries.iter().flatten() {
        put_u32(&mut buffer, entry.parent);
        buffer.push(u8::from(entry.is_directory));
        buffer.push(entry.tier as u8);
        put_u32(&mut buffer, entry.dir_id);
        put_str(&mut buffer, &entry.name);
    }
    buffer
}

/// Parse the binary layout back into an [`IndexData`], rebuilding the derived
/// `child_dirs` and per-directory `entries` lists from the flat records. Returns
/// `None` on any corruption or a root that no longer matches `expected_root`.
fn decode(bytes: &[u8], expected_root: &Path) -> Option<IndexData> {
    let mut reader = Reader::new(bytes);
    if reader.take(4)? != MAGIC {
        return None;
    }
    if reader.u32()? != VERSION {
        return None;
    }
    let root = PathBuf::from(reader.string()?);
    if root != expected_root {
        return None;
    }

    let node_count = reader.u32()? as usize;
    let mut nodes: Vec<DirNode> = Vec::with_capacity(node_count);
    for _ in 0..node_count {
        let parent = reader.u32()?;
        let tier = Tier::from_u8(reader.u8()?);
        let mtime_ms = reader.i64()?;
        let name = reader.string()?;
        nodes.push(DirNode::from_parts(
            name.into_boxed_str(),
            parent,
            tier,
            mtime_ms,
        ));
    }
    if nodes.is_empty() {
        return None; // Must contain at least the root node.
    }

    let entry_count = reader.u32()? as usize;
    let mut entries: Vec<Option<Entry>> = Vec::with_capacity(entry_count);
    for _ in 0..entry_count {
        let parent = reader.u32()?;
        let is_directory = reader.u8()? != 0;
        let tier = Tier::from_u8(reader.u8()?);
        let dir_id = reader.u32()?;
        let name = reader.string()?.into_boxed_str();

        if parent as usize >= nodes.len() {
            return None;
        }
        let index = entries.len() as u32;
        nodes[parent as usize].entries.push(index);
        if is_directory {
            if dir_id as usize >= nodes.len() {
                return None;
            }
            nodes[parent as usize]
                .child_dirs
                .insert(name.clone(), dir_id);
        }
        entries.push(Some(Entry {
            name,
            parent,
            is_directory,
            dir_id: if is_directory { dir_id } else { NO_DIR },
            tier,
        }));
    }

    let live = entries.len();
    Some(IndexData {
        root,
        nodes,
        entries,
        live,
        junk_dirty: std::collections::HashSet::new(),
        // A freshly loaded index starts a new generation; the diff-rescan that follows
        // bumps it as it applies changes.
        revision: 0,
    })
}

/// Path of the index file for a volume key, e.g. `name_index/home.idx`.
pub fn index_path(app_data_dir: &Path, volume_key: &str) -> PathBuf {
    app_data_dir
        .join("name_index")
        .join(format!("{volume_key}.idx"))
}

/// Load a persisted index, or `None` if absent / corrupt / root-mismatched.
pub fn load(path: &Path, expected_root: &Path) -> Option<IndexData> {
    let bytes = fs::read(path).ok()?;
    decode(&bytes, expected_root)
}

/// Atomically write the index to `path` (temp file + rename). No filesystem IO happens
/// while the read lock is held: the bytes are built under the lock, then flushed after.
pub fn save(shared: &Arc<RwLock<IndexData>>, path: &Path) -> std::io::Result<()> {
    let bytes = {
        let index = shared.read().expect("name index lock poisoned");
        encode(&index)
    };
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    let temp_path = path.with_extension("idx.tmp");
    fs::write(&temp_path, &bytes)?;
    fs::rename(&temp_path, path)
}

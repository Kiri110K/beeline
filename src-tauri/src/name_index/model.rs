//! Name Index v4: an immutable mapped base plus a small mutable overlay.
//!
//! The base stores fixed-width directory and Item records and one UTF-8 name arena.
//! Direct children are contiguous per directory. Filesystem changes never modify the
//! mapping: removals set a tombstone bit and additions live in the overlay until the next
//! atomic rebuild.

use std::{
    collections::{HashMap, HashSet},
    ffi::c_void,
    fs::File,
    os::fd::AsRawFd,
    path::{Component, Path, PathBuf},
    ptr,
};

pub type DirId = u32;
pub const NO_DIR: u32 = u32::MAX;

pub(crate) const MAGIC: &[u8; 4] = b"BLNI";
pub(crate) const VERSION: u32 = 4;
pub(crate) const HEADER_LEN: usize = 64;
pub(crate) const NODE_RECORD_LEN: usize = 28;
pub(crate) const ENTRY_RECORD_LEN: usize = 24;
pub(crate) const FNV_OFFSET: u64 = 0xcbf29ce484222325;
pub(crate) const FNV_PRIME: u64 = 0x100000001b3;

pub(crate) fn hash_bytes(hash: &mut u64, bytes: &[u8]) {
    for &byte in bytes {
        *hash ^= u64::from(byte);
        *hash = hash.wrapping_mul(FNV_PRIME);
    }
}

pub(crate) fn name_filter(name: &str) -> u64 {
    fn bit(byte: u8) -> u64 {
        let lower = byte.to_ascii_lowercase();
        let index = match lower {
            b'a'..=b'z' => lower - b'a',
            b'0'..=b'9' => 26 + (lower - b'0'),
            _ => 36 + (lower % 28),
        };
        1u64 << index
    }

    let mut filter = 0u64;
    if name.is_ascii() {
        for byte in name.bytes() {
            filter |= bit(byte);
        }
        return filter;
    }
    let mut encoded = [0u8; 4];
    for ch in name.chars() {
        for lower in ch.to_lowercase() {
            for byte in lower.encode_utf8(&mut encoded).bytes() {
                filter |= bit(byte);
            }
        }
    }
    filter
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
#[repr(u8)]
pub enum Tier {
    Normal = 0,
    Hidden = 1,
    Junk = 2,
}

impl Tier {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Normal => "normal",
            Self::Hidden => "hidden",
            Self::Junk => "junk",
        }
    }

    pub fn from_u8(value: u8) -> Self {
        match value {
            1 => Self::Hidden,
            2 => Self::Junk,
            _ => Self::Normal,
        }
    }
}

#[derive(Clone, Copy)]
pub struct EntryRef<'a> {
    pub name: &'a str,
    pub parent: DirId,
    pub is_directory: bool,
    pub dir_id: DirId,
    pub tier: Tier,
    pub filter: u64,
}

#[derive(Clone, Copy)]
pub struct NodeRef<'a> {
    pub name: &'a str,
    pub parent: DirId,
    pub tier: Tier,
    pub mtime_ms: i64,
}

struct OwnedEntry {
    name: Box<str>,
    parent: DirId,
    is_directory: bool,
    dir_id: DirId,
    tier: Tier,
    filter: u64,
}

struct OwnedNode {
    name: Box<str>,
    parent: DirId,
    tier: Tier,
    mtime_ms: i64,
    removed: bool,
}

pub struct MappedFile {
    pointer: *mut u8,
    len: usize,
}

unsafe impl Send for MappedFile {}
unsafe impl Sync for MappedFile {}

impl MappedFile {
    pub(crate) fn open(path: &Path) -> std::io::Result<Self> {
        let file = File::open(path)?;
        let len = usize::try_from(file.metadata()?.len())
            .map_err(|_| std::io::Error::other("Name Index file is too large"))?;
        if len == 0 {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                "empty Name Index",
            ));
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
            return Err(std::io::Error::last_os_error());
        }
        Ok(Self {
            pointer: mapped.cast(),
            len,
        })
    }

    fn bytes(&self) -> &[u8] {
        unsafe { std::slice::from_raw_parts(self.pointer, self.len) }
    }
}

impl Drop for MappedFile {
    fn drop(&mut self) {
        unsafe {
            munmap(self.pointer.cast(), self.len);
        }
    }
}

const PROT_READ: i32 = 0x1;
const MAP_PRIVATE: i32 = 0x0002;

unsafe extern "C" {
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

pub struct MappedBase {
    mapping: MappedFile,
    root_start: usize,
    root_len: usize,
    node_start: usize,
    node_count: usize,
    entry_start: usize,
    entry_count: usize,
    arena_start: usize,
    arena_len: usize,
}

impl MappedBase {
    pub(crate) fn open(path: &Path, expected_root: &Path) -> std::io::Result<Option<Self>> {
        let mapping = match MappedFile::open(path) {
            Ok(mapping) => mapping,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(error) => return Err(error),
        };
        match Self::parse(mapping, expected_root) {
            Ok(base) => Ok(Some(base)),
            Err(()) => Ok(None),
        }
    }

    /// Open an index whose exact file identity was validated on an earlier launch.
    /// Header, layout, size, and root are still checked on every open; the expensive
    /// payload checksum and full Item walk are skipped only when persistence has proved
    /// that this is the same unchanged file.
    pub(crate) fn open_prevalidated(
        path: &Path,
        expected_root: &Path,
    ) -> std::io::Result<Option<Self>> {
        let mapping = match MappedFile::open(path) {
            Ok(mapping) => mapping,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(error) => return Err(error),
        };
        match Self::parse_layout(mapping, expected_root) {
            Ok(base) => Ok(Some(base)),
            Err(()) => Ok(None),
        }
    }

    fn parse(mapping: MappedFile, expected_root: &Path) -> Result<Self, ()> {
        let base = Self::parse_layout(mapping, expected_root)?;
        let expected_hash = read_u64(base.mapping.bytes(), 32)?;
        let mut hash = FNV_OFFSET;
        hash_bytes(&mut hash, &base.mapping.bytes()[HEADER_LEN..]);
        if hash != expected_hash {
            return Err(());
        }

        base.validate()?;
        Ok(base)
    }

    fn parse_layout(mapping: MappedFile, expected_root: &Path) -> Result<Self, ()> {
        let bytes = mapping.bytes();
        if bytes.len() < HEADER_LEN
            || bytes.get(0..4) != Some(MAGIC)
            || read_u32(bytes, 4)? != VERSION
            || read_u32(bytes, 8)? as usize != HEADER_LEN
        {
            return Err(());
        }
        let root_len = read_u32(bytes, 12)? as usize;
        let node_count = read_u32(bytes, 16)? as usize;
        let entry_count = read_u32(bytes, 20)? as usize;
        let arena_len = usize::try_from(read_u64(bytes, 24)?).map_err(|_| ())?;
        let root_start = HEADER_LEN;
        let node_start = root_start.checked_add(root_len).ok_or(())?;
        let entry_start = node_start
            .checked_add(node_count.checked_mul(NODE_RECORD_LEN).ok_or(())?)
            .ok_or(())?;
        let arena_start = entry_start
            .checked_add(entry_count.checked_mul(ENTRY_RECORD_LEN).ok_or(())?)
            .ok_or(())?;
        let expected_len = arena_start.checked_add(arena_len).ok_or(())?;
        if expected_len != bytes.len() || node_count == 0 {
            return Err(());
        }
        let root_bytes = bytes.get(root_start..node_start).ok_or(())?;
        let root = std::str::from_utf8(root_bytes).map_err(|_| ())?;
        if Path::new(root) != expected_root {
            return Err(());
        }
        Ok(Self {
            mapping,
            root_start,
            root_len,
            node_start,
            node_count,
            entry_start,
            entry_count,
            arena_start,
            arena_len,
        })
    }

    /// Map a file just written and synced by the v4 persister without reading its entire
    /// payload again. Normal startup still performs checksum and structural validation;
    /// this trusted path prevents atomic replacement from making both old and new 300+ MiB
    /// mappings resident in the same process.
    pub(crate) fn open_trusted(
        path: &Path,
        root_len: usize,
        node_count: usize,
        entry_count: usize,
        arena_len: usize,
    ) -> std::io::Result<Self> {
        let mapping = MappedFile::open(path)?;
        let root_start = HEADER_LEN;
        let node_start = root_start
            .checked_add(root_len)
            .ok_or_else(|| std::io::Error::other("Name Index layout overflow"))?;
        let entry_start = node_start
            .checked_add(
                node_count
                    .checked_mul(NODE_RECORD_LEN)
                    .ok_or_else(|| std::io::Error::other("Name Index layout overflow"))?,
            )
            .ok_or_else(|| std::io::Error::other("Name Index layout overflow"))?;
        let arena_start = entry_start
            .checked_add(
                entry_count
                    .checked_mul(ENTRY_RECORD_LEN)
                    .ok_or_else(|| std::io::Error::other("Name Index layout overflow"))?,
            )
            .ok_or_else(|| std::io::Error::other("Name Index layout overflow"))?;
        let expected_len = arena_start
            .checked_add(arena_len)
            .ok_or_else(|| std::io::Error::other("Name Index layout overflow"))?;
        if node_count == 0 || mapping.len != expected_len {
            return Err(std::io::Error::other(
                "new Name Index v4 has an unexpected size",
            ));
        }
        Ok(Self {
            mapping,
            root_start,
            root_len,
            node_start,
            node_count,
            entry_start,
            entry_count,
            arena_start,
            arena_len,
        })
    }

    fn cold_remap(&self, path: &Path) -> std::io::Result<Self> {
        let mapping = MappedFile::open(path)?;
        if mapping.len != self.mapping.len {
            return Err(std::io::Error::other(
                "Name Index changed while preparing persistence",
            ));
        }
        Ok(Self {
            mapping,
            root_start: self.root_start,
            root_len: self.root_len,
            node_start: self.node_start,
            node_count: self.node_count,
            entry_start: self.entry_start,
            entry_count: self.entry_count,
            arena_start: self.arena_start,
            arena_len: self.arena_len,
        })
    }

    fn validate(&self) -> Result<(), ()> {
        let mut expected_first = 0usize;
        let mut referenced_dirs = vec![false; self.node_count];
        referenced_dirs[0] = true;
        for id in 0..self.node_count {
            let record = self.node_record(id).ok_or(())?;
            let parent = read_u32(record, 8)? as usize;
            let first = read_u32(record, 12)? as usize;
            let count = read_u32(record, 16)? as usize;
            if (id == 0 && parent != 0)
                || (id > 0 && parent >= id)
                || first != expected_first
                || first.checked_add(count).ok_or(())? > self.entry_count
                || self.node_name(id).is_none()
            {
                return Err(());
            }
            for slot in first..first + count {
                let entry = self.entry(slot).ok_or(())?;
                if entry.parent as usize != id {
                    return Err(());
                }
                if entry.is_directory {
                    let child = self.node(entry.dir_id).ok_or(())?;
                    let child_id = entry.dir_id as usize;
                    if child.parent as usize != id
                        || child.name != entry.name
                        || referenced_dirs[child_id]
                    {
                        return Err(());
                    }
                    referenced_dirs[child_id] = true;
                }
            }
            expected_first += count;
        }
        if expected_first != self.entry_count || referenced_dirs.iter().any(|seen| !seen) {
            return Err(());
        }
        for slot in 0..self.entry_count {
            let entry = self.entry(slot).ok_or(())?;
            if entry.parent as usize >= self.node_count
                || (entry.is_directory && entry.dir_id as usize >= self.node_count)
            {
                return Err(());
            }
        }
        Ok(())
    }

    fn root(&self) -> &str {
        std::str::from_utf8(&self.mapping.bytes()[self.root_start..self.root_start + self.root_len])
            .expect("validated v4 root")
    }

    fn node_record(&self, id: usize) -> Option<&[u8]> {
        if id >= self.node_count {
            return None;
        }
        let start = self
            .node_start
            .checked_add(id.checked_mul(NODE_RECORD_LEN)?)?;
        self.mapping
            .bytes()
            .get(start..start.checked_add(NODE_RECORD_LEN)?)
    }

    fn entry_record(&self, slot: usize) -> Option<&[u8]> {
        if slot >= self.entry_count {
            return None;
        }
        let start = self
            .entry_start
            .checked_add(slot.checked_mul(ENTRY_RECORD_LEN)?)?;
        self.mapping
            .bytes()
            .get(start..start.checked_add(ENTRY_RECORD_LEN)?)
    }

    fn arena_name(&self, offset: usize, len: usize) -> Option<&str> {
        let end = offset.checked_add(len)?;
        if end > self.arena_len {
            return None;
        }
        std::str::from_utf8(
            self.mapping
                .bytes()
                .get(self.arena_start + offset..self.arena_start + end)?,
        )
        .ok()
    }

    fn node_name(&self, id: usize) -> Option<&str> {
        let record = self.node_record(id)?;
        self.arena_name(
            read_u32(record, 20).ok()? as usize,
            read_u16(record, 24).ok()? as usize,
        )
    }

    fn node(&self, id: DirId) -> Option<NodeRef<'_>> {
        let record = self.node_record(id as usize)?;
        Some(NodeRef {
            name: self.node_name(id as usize)?,
            parent: read_u32(record, 8).ok()?,
            tier: Tier::from_u8(*record.get(26)?),
            mtime_ms: read_i64(record, 0).ok()?,
        })
    }

    fn entry(&self, slot: usize) -> Option<EntryRef<'_>> {
        let record = self.entry_record(slot)?;
        let meta = *record.get(22)?;
        Some(EntryRef {
            name: self.arena_name(
                read_u32(record, 16).ok()? as usize,
                read_u16(record, 20).ok()? as usize,
            )?,
            parent: read_u32(record, 8).ok()?,
            is_directory: meta & 1 != 0,
            dir_id: read_u32(record, 12).ok()?,
            tier: Tier::from_u8(meta >> 1),
            filter: read_u64(record, 0).ok()?,
        })
    }

    fn direct_slots(&self, dir: DirId) -> Option<std::ops::Range<usize>> {
        let record = self.node_record(dir as usize)?;
        let first = read_u32(record, 12).ok()? as usize;
        let count = read_u32(record, 16).ok()? as usize;
        Some(first..first + count)
    }
}

pub struct IndexData {
    pub root: PathBuf,
    base: Option<MappedBase>,
    base_tombstones: Vec<u64>,
    base_node_tombstones: Vec<u64>,
    base_first_child: Vec<DirId>,
    base_next_sibling: Vec<DirId>,
    overlay_nodes: Vec<OwnedNode>,
    overlay_entries: Vec<Option<OwnedEntry>>,
    added_by_parent: HashMap<DirId, Vec<u32>>,
    mtime_overrides: HashMap<DirId, i64>,
    live: usize,
    dirty: bool,
    pub junk_dirty: HashSet<DirId>,
    pub revision: u64,
}

impl IndexData {
    pub fn new(root: PathBuf) -> Self {
        Self {
            root,
            base: None,
            base_tombstones: Vec::new(),
            base_node_tombstones: Vec::new(),
            base_first_child: Vec::new(),
            base_next_sibling: Vec::new(),
            overlay_nodes: vec![OwnedNode {
                name: Box::from(""),
                parent: 0,
                tier: Tier::Normal,
                mtime_ms: 0,
                removed: false,
            }],
            overlay_entries: Vec::new(),
            added_by_parent: HashMap::new(),
            mtime_overrides: HashMap::new(),
            live: 0,
            dirty: true,
            junk_dirty: HashSet::new(),
            revision: 0,
        }
    }

    pub(crate) fn from_base(root: PathBuf, base: MappedBase) -> Self {
        debug_assert_eq!(Path::new(base.root()), root);
        let entry_count = base.entry_count;
        let node_count = base.node_count;
        let mut base_first_child = vec![NO_DIR; node_count];
        let mut base_next_sibling = vec![NO_DIR; node_count];
        // Persisted v4 nodes contain their parent directly. Link them once at load so the
        // startup traversal visits only directory nodes instead of rescanning all 5M Item
        // records to rediscover the 418k directory Items.
        for child in (1..node_count).rev() {
            let parent = base.node(child as DirId).expect("validated v4 node").parent as usize;
            base_next_sibling[child] = base_first_child[parent];
            base_first_child[parent] = child as DirId;
        }
        Self {
            root,
            base: Some(base),
            base_tombstones: vec![0; entry_count.div_ceil(64)],
            base_node_tombstones: vec![0; node_count.div_ceil(64)],
            base_first_child,
            base_next_sibling,
            overlay_nodes: Vec::new(),
            overlay_entries: Vec::new(),
            added_by_parent: HashMap::new(),
            mtime_overrides: HashMap::new(),
            live: entry_count,
            dirty: false,
            junk_dirty: HashSet::new(),
            revision: 0,
        }
    }

    pub(crate) fn replace_base(&mut self, base: MappedBase) {
        let revision = self.revision.wrapping_add(1);
        let root = self.root.clone();
        *self = Self::from_base(root, base);
        self.revision = revision;
    }

    fn base_node_count(&self) -> usize {
        self.base.as_ref().map_or(0, |base| base.node_count)
    }

    fn base_entry_count(&self) -> usize {
        self.base.as_ref().map_or(0, |base| base.entry_count)
    }

    pub fn len(&self) -> usize {
        self.live
    }

    pub fn is_dirty(&self) -> bool {
        self.dirty
    }

    pub fn slot_len(&self) -> usize {
        self.base_entry_count() + self.overlay_entries.len()
    }

    pub fn node_len(&self) -> usize {
        self.base_node_count() + self.overlay_nodes.len()
    }

    pub(crate) fn remap_base_cold(&mut self, path: &Path) -> std::io::Result<()> {
        let Some(base) = &self.base else {
            return Ok(());
        };
        let cold = base.cold_remap(path)?;
        self.base = Some(cold);
        Ok(())
    }

    fn base_removed(&self, slot: usize) -> bool {
        self.base_tombstones
            .get(slot / 64)
            .is_some_and(|word| word & (1u64 << (slot % 64)) != 0)
    }

    fn remove_base_slot(&mut self, slot: usize) -> bool {
        let word = &mut self.base_tombstones[slot / 64];
        let bit = 1u64 << (slot % 64);
        if *word & bit != 0 {
            return false;
        }
        *word |= bit;
        true
    }

    fn base_node_removed(&self, id: DirId) -> bool {
        let id = id as usize;
        self.base_node_tombstones
            .get(id / 64)
            .is_some_and(|word| word & (1u64 << (id % 64)) != 0)
    }

    fn remove_base_node(&mut self, id: DirId) {
        let id = id as usize;
        if let Some(word) = self.base_node_tombstones.get_mut(id / 64) {
            *word |= 1u64 << (id % 64);
        }
    }

    pub fn entry(&self, slot: usize) -> Option<EntryRef<'_>> {
        let base_count = self.base_entry_count();
        if slot < base_count {
            if self.base_removed(slot) {
                return None;
            }
            return self.base.as_ref()?.entry(slot);
        }
        let entry = self.overlay_entries.get(slot - base_count)?.as_ref()?;
        Some(EntryRef {
            name: &entry.name,
            parent: entry.parent,
            is_directory: entry.is_directory,
            dir_id: entry.dir_id,
            tier: entry.tier,
            filter: entry.filter,
        })
    }

    pub fn node(&self, id: DirId) -> Option<NodeRef<'_>> {
        let base_count = self.base_node_count();
        if (id as usize) < base_count {
            if self.base_node_removed(id) {
                return None;
            }
            let mut node = self.base.as_ref()?.node(id)?;
            if let Some(mtime) = self.mtime_overrides.get(&id) {
                node.mtime_ms = *mtime;
            }
            return Some(node);
        }
        let node = self.overlay_nodes.get(id as usize - base_count)?;
        (!node.removed).then_some(NodeRef {
            name: &node.name,
            parent: node.parent,
            tier: node.tier,
            mtime_ms: node.mtime_ms,
        })
    }

    pub fn set_node_mtime(&mut self, id: DirId, mtime_ms: i64) {
        if self.node(id).is_some_and(|node| node.mtime_ms == mtime_ms) {
            return;
        }
        let base_count = self.base_node_count();
        if (id as usize) < base_count {
            self.mtime_overrides.insert(id, mtime_ms);
        } else if let Some(node) = self.overlay_nodes.get_mut(id as usize - base_count) {
            node.mtime_ms = mtime_ms;
        }
        self.dirty = true;
        self.revision = self.revision.wrapping_add(1);
    }

    pub fn direct_entry_slots(&self, parent: DirId) -> Vec<u32> {
        let mut slots = Vec::new();
        if let Some(base) = &self.base {
            if let Some(range) = base.direct_slots(parent) {
                slots.extend(
                    range
                        // Keep persistence total even if this base range was partially
                        // tombstoned while its directory subtree was removed.
                        .filter(|slot| self.entry(*slot).is_some())
                        .map(|slot| slot as u32),
                );
            }
        }
        if let Some(added) = self.added_by_parent.get(&parent) {
            slots.extend(
                added
                    .iter()
                    .copied()
                    .filter(|slot| self.entry(*slot as usize).is_some()),
            );
        }
        slots
    }

    pub fn child_slot(&self, parent: DirId, name: &str) -> Option<u32> {
        if let Some(range) = self
            .base
            .as_ref()
            .and_then(|base| base.direct_slots(parent))
        {
            for slot in range {
                if self.entry(slot).is_some_and(|entry| entry.name == name) {
                    return Some(slot as u32);
                }
            }
        }
        if let Some(added) = self.added_by_parent.get(&parent) {
            for &slot in added {
                if self
                    .entry(slot as usize)
                    .is_some_and(|entry| entry.name == name)
                {
                    return Some(slot);
                }
            }
        }
        None
    }

    pub fn child_dir(&self, parent: DirId, name: &str) -> Option<DirId> {
        let slot = self.child_slot(parent, name)?;
        let entry = self.entry(slot as usize)?;
        entry.is_directory.then_some(entry.dir_id)
    }

    pub fn direct_children(&self, parent: DirId) -> Vec<(String, bool)> {
        self.direct_entry_slots(parent)
            .into_iter()
            .filter_map(|slot| {
                self.entry(slot as usize)
                    .map(|entry| (entry.name.to_owned(), entry.is_directory))
            })
            .collect()
    }

    pub fn child_dirs(&self, parent: DirId) -> Vec<(String, DirId)> {
        let mut dirs = Vec::new();
        if let Some(base) = &self.base {
            let mut child = self
                .base_first_child
                .get(parent as usize)
                .copied()
                .unwrap_or(NO_DIR);
            while child != NO_DIR {
                if !self.base_node_removed(child) {
                    let node = base.node(child).expect("validated v4 child node");
                    dirs.push((node.name.to_owned(), child));
                }
                child = self
                    .base_next_sibling
                    .get(child as usize)
                    .copied()
                    .unwrap_or(NO_DIR);
            }
        }
        if let Some(added) = self.added_by_parent.get(&parent) {
            for &slot in added {
                let Some(entry) = self.entry(slot as usize) else {
                    continue;
                };
                if entry.is_directory {
                    dirs.push((entry.name.to_owned(), entry.dir_id));
                }
            }
        }
        dirs
    }

    #[cfg(test)]
    pub fn has_child(&self, parent: DirId, name: &str) -> bool {
        self.child_slot(parent, name).is_some()
    }

    pub fn full_path(&self, dir: DirId) -> PathBuf {
        if dir == 0 {
            return self.root.clone();
        }
        let mut components = Vec::new();
        let mut current = dir;
        while current != 0 {
            let Some(node) = self.node(current) else {
                break;
            };
            components.push(node.name);
            current = node.parent;
        }
        let mut path = self.root.clone();
        for component in components.iter().rev() {
            path.push(component);
        }
        path
    }

    pub fn entry_path(&self, entry: EntryRef<'_>) -> PathBuf {
        self.full_path(entry.parent).join(entry.name)
    }

    pub fn resolve_dir(&self, path: &Path) -> Option<DirId> {
        let relative = path.strip_prefix(&self.root).ok()?;
        let mut current = 0;
        for component in relative.components() {
            if let Component::Normal(name) = component {
                current = self.child_dir(current, &name.to_string_lossy())?;
            }
        }
        Some(current)
    }

    fn push_entry(&mut self, entry: OwnedEntry) -> u32 {
        let slot = self.base_entry_count() + self.overlay_entries.len();
        let slot = u32::try_from(slot).expect("Name Index exceeds u32 slots");
        self.added_by_parent
            .entry(entry.parent)
            .or_default()
            .push(slot);
        self.overlay_entries.push(Some(entry));
        self.live += 1;
        self.dirty = true;
        self.revision = self.revision.wrapping_add(1);
        slot
    }

    pub fn add_file(&mut self, parent: DirId, name: &str, tier: Tier) {
        self.push_entry(OwnedEntry {
            name: Box::from(name),
            parent,
            is_directory: false,
            dir_id: NO_DIR,
            tier,
            filter: name_filter(name),
        });
    }

    #[cfg(test)]
    pub fn add_dir(&mut self, parent: DirId, name: &str, tier: Tier, mtime_ms: i64) -> DirId {
        if let Some(existing) = self.child_dir(parent, name) {
            self.set_node_mtime(existing, mtime_ms);
            return existing;
        }
        self.add_dir_known_absent(parent, name, tier, mtime_ms)
    }

    /// Insert a directory after the caller has proved that `parent` has no child named
    /// `name`. Fresh filesystem crawls use this path: probing the siblings for every new
    /// directory turns a wide subtree into quadratic work and used 14.8 GB on startup.
    pub(crate) fn add_dir_known_absent(
        &mut self,
        parent: DirId,
        name: &str,
        tier: Tier,
        mtime_ms: i64,
    ) -> DirId {
        let id = self.base_node_count() + self.overlay_nodes.len();
        let id = u32::try_from(id).expect("Name Index exceeds u32 directory nodes");
        self.overlay_nodes.push(OwnedNode {
            name: Box::from(name),
            parent,
            tier,
            mtime_ms,
            removed: false,
        });
        self.push_entry(OwnedEntry {
            name: Box::from(name),
            parent,
            is_directory: true,
            dir_id: id,
            tier,
            filter: name_filter(name),
        });
        id
    }

    pub fn remove_child(&mut self, parent: DirId, name: &str) -> bool {
        self.remove_child_count(parent, name) > 0
    }

    pub(crate) fn remove_child_count(&mut self, parent: DirId, name: &str) -> usize {
        let Some(slot) = self.child_slot(parent, name) else {
            return 0;
        };
        self.remove_slot_count(slot as usize)
    }

    fn remove_slot_count(&mut self, slot: usize) -> usize {
        let Some(entry) = self.entry(slot) else {
            return 0;
        };
        if !entry.is_directory {
            return usize::from(self.remove_slot_shallow(slot));
        }
        let dir_id = entry.dir_id;
        let has_base_children = self
            .base
            .as_ref()
            .and_then(|base| base.direct_slots(dir_id))
            .is_some_and(|mut range| range.any(|child| !self.base_removed(child)));
        let has_overlay_children = self.added_by_parent.get(&dir_id).is_some_and(|added| {
            added
                .iter()
                .any(|child| self.entry(*child as usize).is_some())
        });
        if !has_base_children && !has_overlay_children {
            return usize::from(self.remove_slot_shallow(slot));
        }
        self.remove_slots(vec![slot as u32])
    }

    pub fn clear_children(&mut self, dir_id: DirId) {
        let slots = self.direct_entry_slots(dir_id);
        self.remove_slots(slots);
    }

    /// Remove whole directory subtrees without using the call stack. A deleted tree in a
    /// real v4 index previously recursed through `remove_slot`/`clear_children` 10,102 times,
    /// retained every sibling buffer on that stack, and then aborted on the stack guard.
    fn remove_slots(&mut self, roots: Vec<u32>) -> usize {
        if roots.is_empty() {
            return 0;
        }
        // Every live Item has one parent, and validated v4 directory links form a tree.
        // Roots from `clear_children` are siblings, so their subtrees cannot overlap. A
        // global `seen` bitset used to zero one bit per slot in the entire 5M-Item index for
        // every small directory removed by an FSEvents batch. Walking the pending subtree
        // directly keeps both CPU and scratch memory proportional to the removal itself.
        let mut pending = roots;
        let mut removed = 0usize;

        while let Some(slot) = pending.pop() {
            let Some(entry) = self.entry(slot as usize) else {
                continue;
            };
            let dir_id = entry.is_directory.then_some(entry.dir_id);
            if let Some(dir_id) = dir_id {
                if let Some(range) = self
                    .base
                    .as_ref()
                    .and_then(|base| base.direct_slots(dir_id))
                {
                    for child in range.filter(|child| !self.base_removed(*child)) {
                        pending.push(child as u32);
                    }
                }
                if let Some(added) = self.added_by_parent.remove(&dir_id) {
                    for child in added
                        .into_iter()
                        .filter(|child| self.entry(*child as usize).is_some())
                    {
                        pending.push(child);
                    }
                }
            }
            removed += usize::from(self.remove_slot_shallow(slot as usize));
        }
        removed
    }

    /// Tombstone one live Item without walking its descendants. The caller either proved
    /// that a directory is empty or already queued every child.
    fn remove_slot_shallow(&mut self, slot: usize) -> bool {
        let Some(entry) = self.entry(slot) else {
            return false;
        };
        let dir_id = entry.is_directory.then_some(entry.dir_id);
        if let Some(dir_id) = dir_id {
            self.added_by_parent.remove(&dir_id);
            let base_node_count = self.base_node_count();
            if (dir_id as usize) < base_node_count {
                self.remove_base_node(dir_id);
            } else if let Some(node) = self
                .overlay_nodes
                .get_mut(dir_id as usize - base_node_count)
            {
                node.removed = true;
            }
            self.junk_dirty.remove(&dir_id);
        }

        let base_entry_count = self.base_entry_count();
        if slot < base_entry_count {
            if !self.remove_base_slot(slot) {
                return false;
            }
        } else {
            let overlay_slot = slot - base_entry_count;
            let Some(entry) = self.overlay_entries.get_mut(overlay_slot) else {
                return false;
            };
            if entry.take().is_none() {
                return false;
            }
        }
        self.live -= 1;
        self.dirty = true;
        self.revision = self.revision.wrapping_add(1);
        true
    }
}

fn read_u16(bytes: &[u8], offset: usize) -> Result<u16, ()> {
    Ok(u16::from_le_bytes(
        bytes
            .get(offset..offset + 2)
            .ok_or(())?
            .try_into()
            .map_err(|_| ())?,
    ))
}

fn read_u32(bytes: &[u8], offset: usize) -> Result<u32, ()> {
    Ok(u32::from_le_bytes(
        bytes
            .get(offset..offset + 4)
            .ok_or(())?
            .try_into()
            .map_err(|_| ())?,
    ))
}

fn read_u64(bytes: &[u8], offset: usize) -> Result<u64, ()> {
    Ok(u64::from_le_bytes(
        bytes
            .get(offset..offset + 8)
            .ok_or(())?
            .try_into()
            .map_err(|_| ())?,
    ))
}

fn read_i64(bytes: &[u8], offset: usize) -> Result<i64, ()> {
    Ok(i64::from_le_bytes(
        bytes
            .get(offset..offset + 8)
            .ok_or(())?
            .try_into()
            .map_err(|_| ())?,
    ))
}

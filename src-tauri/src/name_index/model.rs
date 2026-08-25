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

    fn parse(mapping: MappedFile, expected_root: &Path) -> Result<Self, ()> {
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
        let expected_hash = read_u64(bytes, 32)?;
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
        let mut hash = FNV_OFFSET;
        hash_bytes(&mut hash, &bytes[HEADER_LEN..]);
        if hash != expected_hash {
            return Err(());
        }

        let base = Self {
            mapping,
            root_start,
            root_len,
            node_start,
            node_count,
            entry_start,
            entry_count,
            arena_start,
            arena_len,
        };
        base.validate()?;
        Ok(base)
    }

    fn validate(&self) -> Result<(), ()> {
        let mut expected_first = 0usize;
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
                if self.entry(slot).ok_or(())?.parent as usize != id {
                    return Err(());
                }
            }
            expected_first += count;
        }
        if expected_first != self.entry_count {
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
        let start = self
            .node_start
            .checked_add(id.checked_mul(NODE_RECORD_LEN)?)?;
        self.mapping.bytes().get(start..start + NODE_RECORD_LEN)
    }

    fn entry_record(&self, slot: usize) -> Option<&[u8]> {
        let start = self
            .entry_start
            .checked_add(slot.checked_mul(ENTRY_RECORD_LEN)?)?;
        self.mapping.bytes().get(start..start + ENTRY_RECORD_LEN)
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
        Self {
            root,
            base: Some(base),
            base_tombstones: vec![0; entry_count.div_ceil(64)],
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
                        .filter(|slot| !self.base_removed(*slot))
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
        self.direct_entry_slots(parent).into_iter().find(|slot| {
            self.entry(*slot as usize)
                .is_some_and(|entry| entry.name == name)
        })
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
        self.direct_entry_slots(parent)
            .into_iter()
            .filter_map(|slot| {
                let entry = self.entry(slot as usize)?;
                entry
                    .is_directory
                    .then(|| (entry.name.to_owned(), entry.dir_id))
            })
            .collect()
    }

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

    pub fn add_dir(&mut self, parent: DirId, name: &str, tier: Tier, mtime_ms: i64) -> DirId {
        if let Some(existing) = self.child_dir(parent, name) {
            self.set_node_mtime(existing, mtime_ms);
            return existing;
        }
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
        let Some(slot) = self.child_slot(parent, name) else {
            return false;
        };
        self.remove_slot(slot as usize)
    }

    fn remove_slot(&mut self, slot: usize) -> bool {
        let Some(entry) = self.entry(slot) else {
            return false;
        };
        let dir_id = entry.is_directory.then_some(entry.dir_id);
        if let Some(dir_id) = dir_id {
            self.clear_children(dir_id);
            let base_count = self.base_node_count();
            if dir_id as usize >= base_count {
                if let Some(node) = self.overlay_nodes.get_mut(dir_id as usize - base_count) {
                    node.removed = true;
                }
            }
            self.junk_dirty.remove(&dir_id);
        }
        let base_entry_count = self.base_entry_count();
        if slot < base_entry_count {
            self.remove_base_slot(slot);
        } else {
            self.overlay_entries[slot - base_entry_count] = None;
        }
        self.live -= 1;
        self.dirty = true;
        self.revision = self.revision.wrapping_add(1);
        true
    }

    pub fn clear_children(&mut self, dir_id: DirId) {
        let slots = self.direct_entry_slots(dir_id);
        for slot in slots {
            self.remove_slot(slot as usize);
        }
        self.revision = self.revision.wrapping_add(1);
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

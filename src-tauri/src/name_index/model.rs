//! In-memory data model for the Name Index.
//!
//! Memory layout is chosen for the reference machine's 3.2 M files. The two ideas
//! that keep it small:
//!
//! 1. Directory paths are interned into a parent-index tree ([`DirNode`]). A full
//!    path is never stored as a `String`; it is reconstructed on demand by walking
//!    `parent` pointers up to the root. Files — the overwhelming majority — carry
//!    only their own name plus a `u32` parent id.
//! 2. Every [`Entry`] hangs off its parent directory's `entries` list, so there is
//!    no separate `HashMap<path, _>`; a lookup navigates the tree by component.
//!
//! Nothing here touches the filesystem; the fs-driven operations (crawl, reconcile,
//! incremental apply) live in `crawl.rs` and call these structural methods.

use std::{
    collections::{HashMap, HashSet},
    path::{Component, Path, PathBuf},
};

/// Index of a directory node in [`IndexData::nodes`]. Node `0` is always the root.
pub type DirId = u32;

/// Sentinel `dir_id` on an [`Entry`] that is a file, not a directory.
pub const NO_DIR: u32 = u32::MAX;

/// Compact necessary-condition filter for case-insensitive substring matching. Each ASCII
/// letter and digit owns a stable bit; punctuation and lowercased Unicode bytes share the
/// remaining bits. If every bit in a query is not present in an Item name's filter, that name
/// cannot contain the query. Collisions only cause extra string checks, never missed matches.
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

/// Ranking / refresh tier of an item, per SPEC §6.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
#[repr(u8)]
pub enum Tier {
    /// Ordinary visible content.
    Normal = 0,
    /// A path with a dot-component that is not itself Junk.
    Hidden = 1,
    /// Dependency / build / cache / agent-session content (heavy penalty, lazy refresh).
    Junk = 2,
}

impl Tier {
    /// Lowercase wire name, matching the TypeScript-side discriminated union.
    pub fn as_str(self) -> &'static str {
        match self {
            Tier::Normal => "normal",
            Tier::Hidden => "hidden",
            Tier::Junk => "junk",
        }
    }

    /// Decode from the persisted byte; unknown values fall back to `Normal`.
    pub fn from_u8(value: u8) -> Tier {
        match value {
            1 => Tier::Hidden,
            2 => Tier::Junk,
            _ => Tier::Normal,
        }
    }
}

/// One interned directory. Its own name is a single path component; the full path
/// is `root` joined with the reversed chain of `name`s up to node `0`.
pub struct DirNode {
    pub name: Box<str>,
    pub parent: DirId,
    pub tier: Tier,
    /// Directory mtime in Unix ms, used by the diff-rescan to skip unchanged dirs.
    pub mtime_ms: i64,
    /// Child directories by component name, for O(depth) path navigation.
    pub child_dirs: HashMap<Box<str>, DirId>,
    /// Indices into [`IndexData::entries`] of every item directly inside this dir
    /// (files and child directories alike).
    pub entries: Vec<u32>,
}

impl DirNode {
    /// Construct a node with empty derived tables (the loader rebuilds `child_dirs`
    /// and `entries` from the flat entry records).
    pub fn from_parts(name: Box<str>, parent: DirId, tier: Tier, mtime_ms: i64) -> Self {
        Self::new(name, parent, tier, mtime_ms)
    }

    fn new(name: Box<str>, parent: DirId, tier: Tier, mtime_ms: i64) -> Self {
        Self {
            name,
            parent,
            tier,
            mtime_ms,
            child_dirs: HashMap::new(),
            entries: Vec::new(),
        }
    }
}

/// One searchable item: a file or a directory. Directories additionally own a
/// [`DirNode`] referenced through `dir_id`.
pub struct Entry {
    pub name: Box<str>,
    pub parent: DirId,
    pub is_directory: bool,
    /// The [`DirId`] of this entry when it is a directory, else [`NO_DIR`].
    pub dir_id: u32,
    pub tier: Tier,
}

/// The whole index. Held behind an `Arc<RwLock<..>>`; every method here is a short,
/// non-blocking structural mutation — no filesystem IO happens under the lock.
pub struct IndexData {
    pub root: PathBuf,
    /// Node `0` is the root directory (its `name` is empty and `parent` is itself).
    pub nodes: Vec<DirNode>,
    /// Slot vector; a removed item leaves a `None` tombstone so indices stay stable.
    pub entries: Vec<Option<Entry>>,
    /// Case-insensitive necessary-condition filters aligned one-for-one with `entries`.
    /// Tombstones carry zero. Keeping this as a dense side array lets a miss reject a name
    /// without following the `Entry::name` heap pointer.
    pub name_filters: Vec<u64>,
    /// Count of live (non-tombstone) entries.
    pub live: usize,
    /// Junk directories whose contents may be stale and need a lazy rescan.
    pub junk_dirty: HashSet<DirId>,
    /// Index generation counter, bumped on every structural mutation. The search
    /// response returns it so the UI can later reconcile progressive results (SPEC §6):
    /// a rising revision between two responses means the index changed underneath them.
    pub revision: u64,
}

impl IndexData {
    /// A fresh index rooted at `root`, containing only the (empty) root node.
    pub fn new(root: PathBuf) -> Self {
        let root_node = DirNode::new(Box::from(""), 0, Tier::Normal, 0);
        Self {
            root,
            nodes: vec![root_node],
            entries: Vec::new(),
            name_filters: Vec::new(),
            live: 0,
            junk_dirty: HashSet::new(),
            revision: 0,
        }
    }

    /// Number of live entries (files + directories); the root itself is not counted.
    pub fn len(&self) -> usize {
        self.live
    }

    /// Reconstruct the absolute path of a directory node.
    pub fn full_path(&self, dir: DirId) -> PathBuf {
        if dir == 0 {
            return self.root.clone();
        }
        let mut components: Vec<&str> = Vec::new();
        let mut current = dir;
        while current != 0 {
            let node = &self.nodes[current as usize];
            components.push(&node.name);
            current = node.parent;
        }
        let mut path = self.root.clone();
        for component in components.iter().rev() {
            path.push(component);
        }
        path
    }

    /// Absolute path of an entry.
    pub fn entry_path(&self, entry: &Entry) -> PathBuf {
        self.full_path(entry.parent).join(&*entry.name)
    }

    /// Navigate to the [`DirId`] for an absolute path, or `None` if not indexed.
    pub fn resolve_dir(&self, path: &Path) -> Option<DirId> {
        let relative = path.strip_prefix(&self.root).ok()?;
        let mut current: DirId = 0;
        for component in relative.components() {
            if let Component::Normal(name) = component {
                let name = name.to_string_lossy();
                current = *self.nodes[current as usize].child_dirs.get(name.as_ref())?;
            }
        }
        Some(current)
    }

    /// Whether `parent` already holds a live child with this name.
    pub fn has_child(&self, parent: DirId, name: &str) -> bool {
        self.nodes[parent as usize].entries.iter().any(|&index| {
            self.entries[index as usize]
                .as_ref()
                .is_some_and(|entry| &*entry.name == name)
        })
    }

    fn push_entry(&mut self, entry: Entry) -> u32 {
        let index = self.entries.len() as u32;
        self.name_filters.push(name_filter(&entry.name));
        self.entries.push(Some(entry));
        self.live += 1;
        self.revision += 1;
        index
    }

    /// Add a file entry under `parent`. The caller guarantees it is new.
    pub fn add_file(&mut self, parent: DirId, name: &str, tier: Tier) {
        let index = self.push_entry(Entry {
            name: Box::from(name),
            parent,
            is_directory: false,
            dir_id: NO_DIR,
            tier,
        });
        self.nodes[parent as usize].entries.push(index);
    }

    /// Intern a directory under `parent`, returning its id. If it already exists the
    /// stored mtime is refreshed and the existing id returned (idempotent).
    pub fn add_dir(&mut self, parent: DirId, name: &str, tier: Tier, mtime_ms: i64) -> DirId {
        if let Some(&existing) = self.nodes[parent as usize].child_dirs.get(name) {
            self.nodes[existing as usize].mtime_ms = mtime_ms;
            return existing;
        }
        let id = self.nodes.len() as DirId;
        self.nodes
            .push(DirNode::new(Box::from(name), parent, tier, mtime_ms));
        let entry_index = self.push_entry(Entry {
            name: Box::from(name),
            parent,
            is_directory: true,
            dir_id: id,
            tier,
        });
        self.nodes[parent as usize]
            .child_dirs
            .insert(Box::from(name), id);
        self.nodes[parent as usize].entries.push(entry_index);
        id
    }

    /// Remove a directly-contained child by name, tombstoning its whole subtree if it
    /// is a directory. Returns whether anything was removed.
    pub fn remove_child(&mut self, parent: DirId, name: &str) -> bool {
        let position = self.nodes[parent as usize]
            .entries
            .iter()
            .position(|&index| {
                self.entries[index as usize]
                    .as_ref()
                    .is_some_and(|entry| &*entry.name == name)
            });
        let Some(position) = position else {
            return false;
        };
        let entry_index = self.nodes[parent as usize].entries.swap_remove(position);
        let (is_directory, dir_id) = {
            let entry = self.entries[entry_index as usize].as_ref().unwrap();
            (entry.is_directory, entry.dir_id)
        };
        if is_directory {
            self.remove_dir_subtree(dir_id);
            self.nodes[parent as usize].child_dirs.remove(name);
        }
        self.entries[entry_index as usize] = None;
        self.name_filters[entry_index as usize] = 0;
        self.live -= 1;
        self.revision += 1;
        true
    }

    /// Tombstone everything inside `dir_id` but keep the directory node itself, so it
    /// can be re-crawled fresh (used by the Junk lazy drain).
    pub fn clear_children(&mut self, dir_id: DirId) {
        self.revision += 1;
        let entries = std::mem::take(&mut self.nodes[dir_id as usize].entries);
        let children = std::mem::take(&mut self.nodes[dir_id as usize].child_dirs);
        for index in entries {
            if self.entries[index as usize].take().is_some() {
                self.name_filters[index as usize] = 0;
                self.live -= 1;
            }
        }
        for (_, child) in children {
            self.remove_dir_subtree(child);
        }
    }

    /// Tombstone every entry inside `dir_id` and, recursively, inside its child dirs.
    /// The node slots themselves are left orphaned (never reused) — churn leaks a few
    /// `DirNode`s, which the persisted round-trip compacts away on next load.
    fn remove_dir_subtree(&mut self, dir_id: DirId) {
        let entries = std::mem::take(&mut self.nodes[dir_id as usize].entries);
        let children = std::mem::take(&mut self.nodes[dir_id as usize].child_dirs);
        for index in entries {
            if self.entries[index as usize].take().is_some() {
                self.name_filters[index as usize] = 0;
                self.live -= 1;
            }
        }
        for (_, child) in children {
            self.remove_dir_subtree(child);
        }
        self.junk_dirty.remove(&dir_id);
    }
}

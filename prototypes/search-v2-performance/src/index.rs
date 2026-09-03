#![allow(dead_code)]

#[path = "../../../src-tauri/src/name_index/alias.rs"]
mod alias;
#[path = "../../../src-tauri/src/name_index/model.rs"]
pub mod model;
#[path = "../../../src-tauri/src/name_index/query.rs"]
mod query;
#[path = "../../../src-tauri/src/name_index/visit_journal.rs"]
mod visit_journal;

use std::{
    collections::BTreeSet,
    path::{Component, Path, PathBuf},
    sync::atomic::AtomicU64,
};

use alias::AliasDictionary;
use model::{EntryRef, IndexData, MappedBase};
use query::RankContext;
use visit_journal::Aggregate;

pub struct ExactOutcome {
    pub paths: Vec<String>,
    pub scanned: usize,
    pub aborted: bool,
}

pub struct LiveIndex {
    pub data: IndexData,
    aggregate: Aggregate,
    aliases: AliasDictionary,
}

impl LiveIndex {
    pub fn load(path: &Path, root: &Path) -> Self {
        let base = MappedBase::open(path, root)
            .expect("read production Name Index v4")
            .expect("validate production Name Index v4");
        Self {
            data: IndexData::from_base(root.to_path_buf(), base),
            aggregate: Aggregate::default(),
            aliases: AliasDictionary::empty(),
        }
    }

    pub fn item_count(&self) -> usize {
        self.data.len()
    }

    pub fn directory_count(&self) -> usize {
        self.data.node_len()
    }

    pub fn slot_count(&self) -> usize {
        self.data.slot_len()
    }

    pub fn entry(&self, slot: u32) -> Option<EntryRef<'_>> {
        self.data.entry(slot as usize)
    }

    pub fn path(&self, entry: EntryRef<'_>) -> PathBuf {
        self.data.entry_path(entry)
    }

    pub fn ancestor_names(&self, mut parent: u32) -> Vec<String> {
        let mut components = Vec::new();
        while parent != 0 {
            let Some(node) = self.data.node(parent) else {
                break;
            };
            components.push(node.name.to_owned());
            parent = node.parent;
        }
        components.reverse();
        components
    }

    pub fn direct_slots(&self, location: &Path) -> Vec<u32> {
        self.data
            .resolve_dir(location)
            .map(|id| self.data.direct_entry_slots(id))
            .unwrap_or_default()
    }

    pub fn slots_for_paths<'a>(&self, paths: impl IntoIterator<Item = &'a Path>) -> Vec<u32> {
        paths
            .into_iter()
            .filter_map(|path| self.resolve_item_slot(path))
            .collect::<BTreeSet<_>>()
            .into_iter()
            .collect()
    }

    pub fn resolve_item_slot(&self, path: &Path) -> Option<u32> {
        let relative = path.strip_prefix(&self.data.root).ok()?;
        let components = relative
            .components()
            .filter_map(|component| match component {
                Component::Normal(name) => Some(name.to_string_lossy().into_owned()),
                _ => None,
            })
            .collect::<Vec<_>>();
        let (name, parents) = components.split_last()?;
        let mut parent = 0;
        for component in parents {
            parent = self.data.child_dir(parent, component)?;
        }
        self.data.child_slot(parent, name)
    }

    pub fn exact_search(
        &self,
        query_text: &str,
        generation: &AtomicU64,
        mine: u64,
    ) -> ExactOutcome {
        let context = RankContext {
            journal: &self.aggregate,
            aliases: &self.aliases,
        };
        let cancel = query::Cancel::new(generation, mine);
        let outcome = query::run_impl(
            &self.data,
            &context,
            query_text,
            50,
            None,
            Some(&cancel),
            query::resolve_shards(self.data.slot_len()),
        );
        ExactOutcome {
            paths: outcome.hits.into_iter().map(|hit| hit.path).collect(),
            scanned: outcome.scanned,
            aborted: outcome.aborted,
        }
    }
}

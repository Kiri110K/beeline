#![allow(dead_code)]

#[path = "../../../../src-tauri/src/name_index/alias.rs"]
mod alias;
#[path = "../../../../src-tauri/src/name_index/junk.rs"]
mod junk;
#[path = "../../../../src-tauri/src/name_index/model.rs"]
mod model;
#[path = "../../../../src-tauri/src/name_index/query.rs"]
mod query;
#[path = "../../../../src-tauri/src/name_index/visit_journal.rs"]
mod visit_journal;

use std::{
    collections::BTreeSet,
    path::{Component, Path},
    time::Instant,
};

use alias::AliasDictionary;
use model::{IndexData, MappedBase};
use query::{RankContext, SearchOutcome};
use serde::Serialize;
use visit_journal::Aggregate;

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SearchMeasurement {
    pub median_ms: f64,
    pub min_ms: f64,
    pub max_ms: f64,
    pub hits: usize,
    pub scanned: usize,
    pub exhaustive: bool,
    pub target_rank: Option<usize>,
    pub top_paths: Vec<String>,
}

pub struct LiveIndex {
    data: IndexData,
    aggregate: Aggregate,
    aliases: AliasDictionary,
}

impl LiveIndex {
    pub fn load(index_path: &Path, root: &Path, aliases: &[(String, String)]) -> Self {
        let base = MappedBase::open(index_path, root)
            .expect("read production Name Index v4")
            .expect("validate production Name Index v4");
        let data = IndexData::from_base(root.to_path_buf(), base);
        Self {
            data,
            aggregate: Aggregate::default(),
            aliases: AliasDictionary::from_pairs(aliases.iter().cloned(), root),
        }
    }

    pub fn item_count(&self) -> usize {
        self.data.len()
    }

    pub fn directory_count(&self) -> usize {
        self.data.node_len()
    }

    pub fn direct_slots(&self, location: &Path) -> Vec<u32> {
        self.data
            .resolve_dir(location)
            .map(|id| self.data.direct_entry_slots(id))
            .unwrap_or_default()
    }

    pub fn slots_for_paths<'a>(
        &self,
        paths: impl IntoIterator<Item = &'a Path>,
    ) -> (Vec<u32>, usize) {
        let mut slots = BTreeSet::new();
        let mut unresolved = 0;
        for path in paths {
            match self.resolve_item_slot(path) {
                Some(slot) => {
                    slots.insert(slot);
                }
                None => unresolved += 1,
            }
        }
        (slots.into_iter().collect(), unresolved)
    }

    pub fn merge_slots(groups: impl IntoIterator<Item = Vec<u32>>) -> Vec<u32> {
        groups
            .into_iter()
            .flatten()
            .collect::<BTreeSet<_>>()
            .into_iter()
            .collect()
    }

    pub fn contains_path(&self, slots: &[u32], path: &Path) -> bool {
        self.resolve_item_slot(path)
            .is_some_and(|slot| slots.binary_search(&slot).is_ok())
    }

    pub fn measure(
        &self,
        query_text: &str,
        slots: Option<&[u32]>,
        target: Option<&Path>,
        samples: usize,
    ) -> SearchMeasurement {
        let context = RankContext {
            journal: &self.aggregate,
            aliases: &self.aliases,
        };
        let shards = if slots.is_some() {
            1
        } else {
            query::resolve_shards(self.data.slot_len())
        };

        let run = || query::run_impl(&self.data, &context, query_text, 50, slots, None, shards);
        let _ = run();
        let mut timings = Vec::with_capacity(samples);
        let mut last = None;
        for _ in 0..samples {
            let started = Instant::now();
            let outcome = run();
            timings.push(started.elapsed().as_secs_f64() * 1000.0);
            last = Some(outcome);
        }
        timings.sort_by(f64::total_cmp);
        let outcome = last.expect("at least one measurement sample");
        measurement(timings, outcome, target)
    }

    fn resolve_item_slot(&self, path: &Path) -> Option<u32> {
        let relative = path.strip_prefix(&self.data.root).ok()?;
        let components: Vec<_> = relative
            .components()
            .filter_map(|component| match component {
                Component::Normal(name) => Some(name.to_string_lossy().into_owned()),
                _ => None,
            })
            .collect();
        let (name, parents) = components.split_last()?;
        let mut parent = 0;
        for component in parents {
            parent = self.data.child_dir(parent, component)?;
        }
        self.data.child_slot(parent, name)
    }
}

fn measurement(
    timings: Vec<f64>,
    outcome: SearchOutcome,
    target: Option<&Path>,
) -> SearchMeasurement {
    let target = target.map(|path| path.to_string_lossy());
    let target_rank = target.as_deref().and_then(|target| {
        outcome
            .hits
            .iter()
            .position(|hit| hit.path == target)
            .map(|rank| rank + 1)
    });
    SearchMeasurement {
        median_ms: timings[timings.len() / 2],
        min_ms: timings[0],
        max_ms: timings[timings.len() - 1],
        hits: outcome.hits.len(),
        scanned: outcome.scanned,
        exhaustive: outcome.exhaustive,
        target_rank,
        top_paths: outcome
            .hits
            .into_iter()
            .take(3)
            .map(|hit| hit.path)
            .collect(),
    }
}

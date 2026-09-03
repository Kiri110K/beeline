use std::borrow::Cow;

use crate::index::{
    model::{name_filter, EntryRef},
    LiveIndex,
};
use serde::Serialize;
use unicode_normalization::UnicodeNormalization;

#[derive(Clone)]
pub struct PreparedToken {
    pub text: String,
    chars: Vec<char>,
    filter: u64,
    missing_bits_per_edit: u32,
    ascii_masks: Option<Box<[u64; 256]>>,
}

impl PreparedToken {
    fn new(text: String) -> Self {
        let chars = text.chars().collect::<Vec<_>>();
        let missing_bits_per_edit = chars
            .iter()
            .map(|character| {
                let lowered = character.to_lowercase().collect::<String>();
                name_filter(&lowered).count_ones()
            })
            .max()
            .unwrap_or(1);
        let ascii_masks = (text.is_ascii() && !text.is_empty() && text.len() <= 64).then(|| {
            let mut masks = Box::new([0u64; 256]);
            for (position, byte) in text.bytes().enumerate() {
                masks[byte.to_ascii_lowercase() as usize] |= 1u64 << position;
            }
            masks
        });
        Self {
            filter: name_filter(&text),
            text,
            chars,
            missing_bits_per_edit,
            ascii_masks,
        }
    }

    pub fn global_edit_limit(&self) -> usize {
        allowed_edits(self.chars.len(), false)
    }
}

#[derive(Clone)]
pub struct PreparedVariant {
    pub kind: VariantKind,
    pub tokens: Vec<PreparedToken>,
}

#[derive(Clone, Copy, Debug, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum VariantKind {
    Direct,
    Layout,
}

#[derive(Clone)]
pub struct PreparedQuery {
    pub query: String,
    pub significant_characters: usize,
    pub variants: Vec<PreparedVariant>,
}

impl PreparedQuery {
    pub fn new(query: &str) -> Self {
        let normalized = normalize(query);
        let mut variants = vec![prepare_variant(VariantKind::Direct, &normalized)];
        if let Some(layout) = map_layout(&normalized) {
            if layout != normalized {
                variants.push(prepare_variant(VariantKind::Layout, &layout));
            }
        }
        Self {
            query: query.to_owned(),
            significant_characters: significant_len(&normalized),
            variants,
        }
    }
}

fn prepare_variant(kind: VariantKind, query: &str) -> PreparedVariant {
    let tokens = query
        .split(|character: char| character.is_whitespace() || character == '/')
        .filter(|token| !token.is_empty())
        .map(|token| PreparedToken::new(token.to_owned()))
        .collect();
    PreparedVariant { kind, tokens }
}

#[derive(Clone, Copy, Debug, Serialize, PartialEq, Eq, PartialOrd, Ord)]
#[serde(rename_all = "snake_case")]
pub enum Relation {
    Typo,
    Substring,
    Prefix,
    TokenExact,
    StemExact,
    FullExact,
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TokenEvidence {
    pub query: String,
    pub target: String,
    pub relation: Relation,
    pub distance: usize,
    pub quality: i64,
}

#[derive(Clone, Copy, Debug, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum Interpretation {
    Ordinary,
    Path,
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TextEvidence {
    pub variant: VariantKind,
    pub interpretation: Interpretation,
    pub tokens: Vec<TokenEvidence>,
    pub raw_quality: i64,
    pub intent_multiplier: i64,
    pub contribution: i64,
}

#[derive(Default)]
pub struct AncestorCache {
    parent: Option<u32>,
    names: Vec<String>,
}

impl AncestorCache {
    fn names<'a>(&'a mut self, index: &LiveIndex, parent: u32) -> &'a [String] {
        if self.parent != Some(parent) {
            self.names = index
                .ancestor_names(parent)
                .into_iter()
                .map(|component| normalize(&component))
                .collect();
            self.parent = Some(parent);
        }
        &self.names
    }
}

pub fn could_match_name(entry: EntryRef<'_>, prepared: &PreparedQuery, short_fuzzy: bool) -> bool {
    prepared.variants.iter().any(|variant| {
        variant.tokens.iter().any(|token| {
            let edits = allowed_edits(token.chars.len(), short_fuzzy);
            let allowed_missing = token.missing_bits_per_edit * edits as u32;
            (token.filter & !entry.filter).count_ones() <= allowed_missing
        })
    })
}

pub fn best_text_evidence(
    index: &LiveIndex,
    entry: EntryRef<'_>,
    prepared: &PreparedQuery,
    short_fuzzy: bool,
    ancestors: &mut AncestorCache,
) -> Option<TextEvidence> {
    let normalized = normalize_item(entry.name);
    let stem = stem(&normalized);
    let target = NameTarget {
        full: &normalized,
        stem,
    };
    let mut best = None;
    for variant in &prepared.variants {
        if variant.tokens.is_empty() {
            continue;
        }
        choose_stronger(
            &mut best,
            ordinary_explanation(
                index,
                entry,
                target,
                variant,
                prepared.significant_characters,
                short_fuzzy,
                ancestors,
            ),
        );
        if variant.tokens.len() >= 2 {
            choose_stronger(
                &mut best,
                path_explanation(
                    index,
                    entry,
                    target,
                    variant,
                    prepared.significant_characters,
                    short_fuzzy,
                    ancestors,
                ),
            );
        }
    }
    best
}

#[derive(Clone, Copy)]
struct NameTarget<'a> {
    full: &'a str,
    stem: &'a str,
}

fn ordinary_explanation(
    index: &LiveIndex,
    entry: EntryRef<'_>,
    target: NameTarget<'_>,
    variant: &PreparedVariant,
    significant: usize,
    short_fuzzy: bool,
    ancestors: &mut AncestorCache,
) -> Option<TextEvidence> {
    let mut hits = variant
        .tokens
        .iter()
        .map(|token| best_name_hit(token, target, short_fuzzy))
        .collect::<Vec<_>>();
    if hits.iter().all(Option::is_none) {
        return None;
    }
    if hits.iter().any(Option::is_none) {
        let ancestor_names = ancestors.names(index, entry.parent);
        for (position, hit) in hits.iter_mut().enumerate() {
            if hit.is_none() {
                *hit = best_component_hit(&variant.tokens[position], ancestor_names, short_fuzzy);
            }
        }
    }
    complete_explanation(hits, variant.kind, Interpretation::Ordinary, significant)
}

fn path_explanation(
    index: &LiveIndex,
    entry: EntryRef<'_>,
    target: NameTarget<'_>,
    variant: &PreparedVariant,
    significant: usize,
    short_fuzzy: bool,
    ancestors: &mut AncestorCache,
) -> Option<TextEvidence> {
    let (last, parents) = variant.tokens.split_last()?;
    let final_hit = best_name_hit(last, target, short_fuzzy)?;
    let ancestor_names = ancestors.names(index, entry.parent);
    let mut cursor = ancestor_names.len();
    let mut parent_hits = Vec::with_capacity(parents.len());
    for token in parents.iter().rev() {
        let mut found = None;
        while cursor > 0 {
            cursor -= 1;
            let component = normalize_item(&ancestor_names[cursor]);
            if let Some(hit) = best_target_hit(
                token,
                NameTarget {
                    full: &component,
                    stem: &component,
                },
                short_fuzzy,
            ) {
                found = Some(hit);
                break;
            }
        }
        parent_hits.push(found?);
    }
    parent_hits.reverse();
    parent_hits.push(final_hit);
    complete_explanation(
        parent_hits.into_iter().map(Some).collect(),
        variant.kind,
        Interpretation::Path,
        significant,
    )
}

fn complete_explanation(
    hits: Vec<Option<TokenEvidence>>,
    variant: VariantKind,
    interpretation: Interpretation,
    significant: usize,
) -> Option<TextEvidence> {
    let hits = hits.into_iter().collect::<Option<Vec<_>>>()?;
    let weakest = hits.iter().map(|hit| hit.quality).min()?;
    let average = hits.iter().map(|hit| hit.quality).sum::<i64>() / hits.len() as i64;
    let raw_quality = (weakest * 60 + average * 40) / 100;
    let layout_multiplier = if variant == VariantKind::Layout {
        900
    } else {
        1000
    };
    let path_multiplier = if interpretation == Interpretation::Path {
        950
    } else {
        1000
    };
    let intent_multiplier = (400 + 120 * significant.min(5) as i64).min(1000);
    let contribution =
        raw_quality * layout_multiplier / 1000 * path_multiplier / 1000 * intent_multiplier / 1000;
    Some(TextEvidence {
        variant,
        interpretation,
        tokens: hits,
        raw_quality,
        intent_multiplier,
        contribution,
    })
}

fn choose_stronger(current: &mut Option<TextEvidence>, candidate: Option<TextEvidence>) {
    let Some(candidate) = candidate else {
        return;
    };
    let replace = current.as_ref().is_none_or(|existing| {
        candidate.contribution > existing.contribution
            || (candidate.contribution == existing.contribution
                && explanation_order(&candidate) > explanation_order(existing))
    });
    if replace {
        *current = Some(candidate);
    }
}

fn explanation_order(evidence: &TextEvidence) -> (u8, u8) {
    let variant = match evidence.variant {
        VariantKind::Direct => 1,
        VariantKind::Layout => 0,
    };
    let interpretation = match evidence.interpretation {
        Interpretation::Ordinary => 1,
        Interpretation::Path => 0,
    };
    (variant, interpretation)
}

fn best_name_hit(
    query: &PreparedToken,
    target: NameTarget<'_>,
    short_fuzzy: bool,
) -> Option<TokenEvidence> {
    best_target_hit(query, target, short_fuzzy)
}

fn best_component_hit(
    query: &PreparedToken,
    components: &[String],
    short_fuzzy: bool,
) -> Option<TokenEvidence> {
    components
        .iter()
        .filter_map(|component| {
            let normalized = normalize_item(component);
            best_target_hit(
                query,
                NameTarget {
                    full: &normalized,
                    stem: &normalized,
                },
                short_fuzzy,
            )
        })
        .max_by_key(|hit| (hit.quality, hit.relation))
}

fn best_target_hit(
    query: &PreparedToken,
    target: NameTarget<'_>,
    short_fuzzy: bool,
) -> Option<TokenEvidence> {
    let query_text = query.text.as_str();
    let direct = if query_text == target.full {
        Some((target.full, Relation::FullExact, 0, 1000))
    } else if query_text == target.stem {
        Some((target.stem, Relation::StemExact, 0, 970))
    } else if let Some(token) = item_tokens(target.full).find(|token| *token == query_text) {
        Some((token, Relation::TokenExact, 0, 940))
    } else if target.full.starts_with(query_text) || target.stem.starts_with(query_text) {
        Some((target.stem, Relation::Prefix, 0, 850))
    } else if target.full.contains(query_text) || target.stem.contains(query_text) {
        Some((target.stem, Relation::Substring, 0, 700))
    } else {
        None
    };
    if let Some((matched, relation, distance, quality)) = direct {
        return Some(evidence(query_text, matched, relation, distance, quality));
    }

    let edits = allowed_edits(query.chars.len(), short_fuzzy);
    if edits == 0 {
        return None;
    }
    item_tokens(target.full)
        .filter_map(|candidate| {
            fuzzy_distance(query, candidate, edits).map(|distance| {
                evidence(
                    query_text,
                    candidate,
                    Relation::Typo,
                    distance,
                    650 - distance as i64 * 100,
                )
            })
        })
        .max_by_key(|hit| hit.quality)
}

fn evidence(
    query: &str,
    target: &str,
    relation: Relation,
    distance: usize,
    quality: i64,
) -> TokenEvidence {
    TokenEvidence {
        query: query.to_owned(),
        target: target.to_owned(),
        relation,
        distance,
        quality,
    }
}

fn fuzzy_distance(query: &PreparedToken, target: &str, limit: usize) -> Option<usize> {
    let target_len = target.chars().count();
    if query.chars.len().abs_diff(target_len) > limit {
        return None;
    }
    if let Some(masks) = &query.ascii_masks {
        if target.is_ascii()
            && myers_distance(masks, query.text.len(), target.as_bytes()) > limit * 2
        {
            return None;
        }
        if target.is_ascii() {
            return bounded_osa(query.text.as_bytes(), target.as_bytes(), limit);
        }
    }
    let target_chars = target.chars().collect::<Vec<_>>();
    bounded_osa(&query.chars, &target_chars, limit)
}

fn myers_distance(masks: &[u64; 256], pattern_len: usize, target: &[u8]) -> usize {
    let mut positive = !0u64;
    let mut negative = 0u64;
    let mut score = pattern_len;
    let high_bit = 1u64 << (pattern_len - 1);
    for byte in target {
        let equal = masks[byte.to_ascii_lowercase() as usize];
        let vertical = equal | negative;
        let horizontal = (((equal & positive).wrapping_add(positive)) ^ positive) | equal;
        let positive_horizontal = negative | !(horizontal | positive);
        let negative_horizontal = positive & horizontal;
        if positive_horizontal & high_bit != 0 {
            score += 1;
        }
        if negative_horizontal & high_bit != 0 {
            score -= 1;
        }
        let positive_horizontal = (positive_horizontal << 1) | 1;
        let negative_horizontal = negative_horizontal << 1;
        positive = negative_horizontal | !(vertical | positive_horizontal);
        negative = positive_horizontal & vertical;
    }
    score
}

fn bounded_osa<T: Eq>(left: &[T], right: &[T], limit: usize) -> Option<usize> {
    if left.len().abs_diff(right.len()) > limit {
        return None;
    }
    let mut previous_two = (0..=right.len()).collect::<Vec<_>>();
    let mut previous = previous_two.clone();
    let mut current = vec![0; right.len() + 1];
    for (left_index, left_value) in left.iter().enumerate() {
        current[0] = left_index + 1;
        let mut row_min = current[0];
        for (right_index, right_value) in right.iter().enumerate() {
            let substitution = usize::from(left_value != right_value);
            let mut distance = (current[right_index] + 1)
                .min(previous[right_index + 1] + 1)
                .min(previous[right_index] + substitution);
            if left_index > 0
                && right_index > 0
                && left[left_index] == right[right_index - 1]
                && left[left_index - 1] == right[right_index]
            {
                distance = distance.min(previous_two[right_index - 1] + 1);
            }
            current[right_index + 1] = distance;
            row_min = row_min.min(distance);
        }
        if row_min > limit {
            return None;
        }
        std::mem::swap(&mut previous_two, &mut previous);
        std::mem::swap(&mut previous, &mut current);
    }
    (previous[right.len()] <= limit).then_some(previous[right.len()])
}

fn allowed_edits(length: usize, short_fuzzy: bool) -> usize {
    match length {
        0 => 0,
        1 | 2 if short_fuzzy => 1,
        1 | 2 => 0,
        3..=6 => 1,
        7..=12 => 2,
        _ => 3,
    }
}

fn normalize_item(value: &str) -> Cow<'_, str> {
    if value.is_ascii() && !value.bytes().any(|byte| byte.is_ascii_uppercase()) {
        Cow::Borrowed(value)
    } else {
        Cow::Owned(normalize(value))
    }
}

fn item_tokens(value: &str) -> impl Iterator<Item = &str> {
    value
        .split(|character: char| !character.is_alphanumeric())
        .filter(|token| !token.is_empty())
}

fn stem(value: &str) -> &str {
    let Some((stem, extension)) = value.rsplit_once('.') else {
        return value;
    };
    if stem.is_empty() || extension.is_empty() {
        value
    } else {
        stem
    }
}

pub fn normalize(value: &str) -> String {
    value
        .nfc()
        .flat_map(char::to_lowercase)
        .collect::<String>()
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
}

pub fn significant_len(value: &str) -> usize {
    value
        .nfc()
        .filter(|character| character.is_alphanumeric())
        .count()
}

fn map_layout(value: &str) -> Option<String> {
    let mut changed = false;
    let mapped = value
        .chars()
        .map(|character| {
            let replacement = layout_pair(character).unwrap_or(character);
            changed |= replacement != character;
            replacement
        })
        .collect::<String>();
    changed.then_some(mapped)
}

fn layout_pair(character: char) -> Option<char> {
    const PAIRS: &[(char, char)] = &[
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
    PAIRS.iter().find_map(|(latin, cyrillic)| {
        if character == *latin {
            Some(*cyrillic)
        } else if character == *cyrillic {
            Some(*latin)
        } else {
            None
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bounded_osa_covers_supported_edits() {
        assert_eq!(
            bounded_osa(&chars("метолология"), &chars("методология"), 2),
            Some(1)
        );
        assert_eq!(
            bounded_osa(&chars("methodolgy"), &chars("methodology"), 2),
            Some(1)
        );
        assert_eq!(
            bounded_osa(&chars("methodoology"), &chars("methodology"), 2),
            Some(1)
        );
        assert_eq!(
            bounded_osa(&chars("methdoology"), &chars("methodology"), 2),
            Some(1)
        );
    }

    #[test]
    fn layout_examples_map_by_physical_key() {
        assert_eq!(map_layout("ьуерщвщдщпн").as_deref(), Some("methodology"));
        assert_eq!(map_layout("ьуерщвщдпн").as_deref(), Some("methodolgy"));
    }

    #[test]
    fn relaxed_filter_and_myers_do_not_reject_one_edit() {
        let alphabet = ['a', 'b', 'я'];
        for source in words(&alphabet, 3) {
            let token = PreparedToken::new(source.clone());
            for target in one_edit_targets(&source, &alphabet) {
                let missing = (token.filter & !name_filter(&target)).count_ones();
                assert!(
                    missing <= token.missing_bits_per_edit,
                    "filter rejected {source:?} -> {target:?}"
                );
                if let Some(masks) = &token.ascii_masks {
                    if target.is_ascii() {
                        assert!(
                            myers_distance(masks, source.len(), target.as_bytes()) <= 2,
                            "Myers rejected OSA-one {source:?} -> {target:?}"
                        );
                    }
                }
            }
        }
    }

    fn chars(value: &str) -> Vec<char> {
        value.chars().collect()
    }

    fn words(alphabet: &[char], max_len: usize) -> Vec<String> {
        let mut level = vec![String::new()];
        let mut result = Vec::new();
        for _ in 0..max_len {
            let mut next = Vec::new();
            for prefix in level {
                for character in alphabet {
                    let mut word = prefix.clone();
                    word.push(*character);
                    result.push(word.clone());
                    next.push(word);
                }
            }
            level = next;
        }
        result
    }

    fn one_edit_targets(source: &str, alphabet: &[char]) -> Vec<String> {
        let source = chars(source);
        let mut targets = Vec::new();
        for position in 0..=source.len() {
            for character in alphabet {
                let mut inserted = source.clone();
                inserted.insert(position, *character);
                targets.push(inserted.iter().collect());
            }
        }
        for position in 0..source.len() {
            let mut deleted = source.clone();
            deleted.remove(position);
            targets.push(deleted.iter().collect());
            for character in alphabet {
                let mut substituted = source.clone();
                substituted[position] = *character;
                targets.push(substituted.iter().collect());
            }
            if position + 1 < source.len() {
                let mut transposed = source.clone();
                transposed.swap(position, position + 1);
                targets.push(transposed.iter().collect());
            }
        }
        targets
    }
}

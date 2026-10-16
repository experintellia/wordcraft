//! What changed between two documents, as a patch another member can apply to their copy: in each
//! story the run of changed blocks (blocks are `Arc`s, so unchanged ones compare by pointer), and
//! each other field of the document that differs, whole. Every change carries a hash of what it
//! replaces; [`apply`] refuses the whole patch when any of those no longer matches (someone changed
//! the same paragraph, table or field since). Pictures travel once, base64-encoded.

use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;

use serde::{Deserialize, Serialize};
use serde_json::Value as Json;
use wordcraft_doc::{Block, Blocks, Document, StoryRef};
use wordcraft_engine::cmd::insert::{base64_decode, base64_encode};

/// Caps on what one patch may make a member do.
const MAX_SPLICES: usize = 4096;
const MAX_BLOCKS: usize = 1 << 20;

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Patch {
    /// Changed block runs.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub b: Vec<Splice>,
    /// Other fields replaced whole.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub f: Vec<Field>,
    /// Pictures added (key, base64) and removed.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub m: BTreeMap<String, String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub mr: Vec<String>,
}

/// In story `s` (`None`: the body), the `n` blocks at `at`, which hash to `h`, become `v`.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Splice {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub s: Option<u32>,
    pub at: usize,
    pub n: usize,
    pub h: u64,
    pub v: Vec<Block>,
}

/// Field `k` (a document field, or `part:<id>` for a part's settings), which hashes to `h`, becomes
/// `v` (`null`: a part that goes).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Field {
    pub k: String,
    pub h: u64,
    pub v: Json,
}

impl Patch {
    pub fn is_empty(&self) -> bool {
        self.b.is_empty() && self.f.is_empty() && self.m.is_empty() && self.mr.is_empty()
    }
}

pub fn story(s: Option<u32>) -> StoryRef {
    s.map_or(StoryRef::Body, StoryRef::Part)
}

fn story_key(s: StoryRef) -> Option<u32> {
    match s {
        StoryRef::Body => None,
        StoryRef::Part(id) => Some(id),
    }
}

/// FNV-1a over the JSON of `v`: the same on every member (struct fields serialize in order and
/// the document's maps are B-trees).
pub fn hash<T: Serialize + ?Sized>(v: &T) -> u64 {
    let json = serde_json::to_vec(v).unwrap_or_default();
    json.iter().fold(0xcbf2_9ce4_8422_2325, |h, b| (h ^ u64::from(*b)).wrapping_mul(0x0100_0000_01b3))
}

fn same(x: &Arc<Block>, y: &Arc<Block>) -> bool {
    Arc::ptr_eq(x, y) || x == y
}

/// The run of blocks that differs between `a` and `b`, as a splice.
fn splice(s: StoryRef, a: &Blocks, b: &Blocks) -> Option<Splice> {
    let prefix = a.iter().zip(b).take_while(|(x, y)| same(x, y)).count();
    if prefix == a.len() && prefix == b.len() {
        return None;
    }
    let max = a.len().min(b.len()) - prefix;
    let suffix = a.iter().rev().zip(b.iter().rev()).take(max).take_while(|(x, y)| same(x, y)).count();
    let removed = a.get(prefix..a.len() - suffix)?;
    Some(Splice {
        s: story_key(s),
        at: prefix,
        n: removed.len(),
        h: hash(removed),
        v: b.get(prefix..b.len() - suffix)?.iter().map(|x| Block::clone(x)).collect(),
    })
}

macro_rules! fields {
    ($m:ident) => {
        $m!(last_section, "lastSection");
        $m!(styles, "styles");
        $m!(numbering, "numbering");
        $m!(comments, "comments");
        $m!(revisions, "revisions");
        $m!(settings, "settings");
        $m!(core, "core");
        $m!(sources, "sources");
        $m!(custom_props, "customProps");
        $m!(footnote_separators, "footnoteSeparators");
        $m!(endnote_separators, "endnoteSeparators");
        $m!(passthrough, "passthrough");
    };
}

/// The patch that makes `a` into `b`.
pub fn diff(a: &Document, b: &Document) -> Patch {
    let mut p = Patch::default();
    p.b.extend(splice(StoryRef::Body, &a.body, &b.body));
    for id in a.parts.keys().chain(b.parts.keys()).collect::<BTreeSet<_>>() {
        let (x, y) = (a.parts.get(id), b.parts.get(id));
        if let (Some(x), Some(y)) = (x, y)
            && x.kind == y.kind
            && x.body == y.body
        {
            p.b.extend(splice(StoryRef::Part(*id), &x.blocks, &y.blocks));
            continue;
        }
        // A part that comes, goes or changes kind travels whole.
        p.f.push(Field { k: format!("part:{id}"), h: hash(&x), v: y.map_or(Json::Null, |y| serde_json::to_value(y).unwrap_or(Json::Null)) });
    }
    macro_rules! field {
        ($f:ident, $k:literal) => {
            if a.$f != b.$f {
                p.f.push(Field { k: $k.into(), h: hash(&a.$f), v: serde_json::to_value(&b.$f).unwrap_or(Json::Null) });
            }
        };
    }
    fields!(field);
    for (k, v) in &b.media {
        if a.media.get(k).is_none_or(|old| !Arc::ptr_eq(old, v) && old != v) {
            p.m.insert(k.clone(), base64_encode(v));
        }
    }
    p.mr = a.media.keys().filter(|k| !b.media.contains_key(*k)).cloned().collect();
    p
}

/// `doc` with `patch` applied, if everything it replaces is still as it was; `at` maps a splice's
/// story and block index into `doc` (positions move when others edited concurrently).
pub fn apply(doc: &Document, patch: &Patch, at: &dyn Fn(StoryRef, usize) -> (StoryRef, usize)) -> Option<Document> {
    if patch.b.len() > MAX_SPLICES || patch.b.iter().map(|s| s.v.len()).sum::<usize>() > MAX_BLOCKS {
        return None;
    }
    let mut out = doc.clone();
    // Parts first: a splice may be into a part the patch brings.
    for f in &patch.f {
        let Some(id) = f.k.strip_prefix("part:") else { continue };
        let id: u32 = id.parse().ok()?;
        if hash(&doc.parts.get(&id)) != f.h {
            return None;
        }
        match &f.v {
            Json::Null => drop(out.parts.remove(&id)),
            v => drop(out.parts.insert(id, serde_json::from_value(v.clone()).ok()?)),
        }
    }
    // Splices from the last, so earlier indices stay put.
    let mut splices: Vec<(StoryRef, usize, &Splice)> = patch
        .b
        .iter()
        .map(|s| {
            let (st, i) = at(story(s.s), s.at);
            (st, i, s)
        })
        .collect();
    splices.sort_by_key(|x| std::cmp::Reverse((story_key(x.0), x.1)));
    for (st, i, s) in splices {
        let blocks = out.story_mut(st).ok()?;
        let end = i.checked_add(s.n)?;
        if hash(blocks.get(i..end)?) != s.h {
            return None;
        }
        blocks.splice(i..end, s.v.iter().map(|b| Arc::new(b.clone())));
    }
    macro_rules! field {
        ($f:ident, $k:literal) => {
            if let Some(f) = patch.f.iter().find(|f| f.k == $k) {
                if hash(&doc.$f) != f.h {
                    return None;
                }
                out.$f = serde_json::from_value(f.v.clone()).ok()?;
            }
        };
    }
    fields!(field);
    for k in &patch.mr {
        out.media.remove(k);
    }
    for (k, v) in &patch.m {
        out.media.insert(k.clone(), Arc::new(base64_decode(v)?));
    }
    out.ensure_nonempty();
    Some(out)
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]
    use super::*;

    fn same_place(s: StoryRef, i: usize) -> (StoryRef, usize) {
        (s, i)
    }

    #[test]
    fn a_patch_makes_one_document_into_the_other() {
        let a = Document::from_text("one\ntwo\nthree");
        let mut b = a.clone();
        b.body.insert(1, b.body[0].clone());
        b.core.title = "Shared".into();
        b.media.insert("p.png".into(), Arc::new(vec![1, 2, 3]));
        let p = diff(&a, &b);
        assert_eq!(p.b.len(), 1);
        let json = serde_json::to_string(&p).unwrap();
        let p: Patch = serde_json::from_str(&json).unwrap();
        assert_eq!(apply(&a, &p, &same_place).unwrap(), b);
        // Undo: the other way.
        assert_eq!(apply(&b, &diff(&b, &a), &same_place).unwrap(), a);
    }

    #[test]
    fn a_patch_is_refused_once_the_same_thing_changed() {
        let a = Document::from_text("one\ntwo");
        let b = Document::from_text("one\nTWO");
        let p = diff(&a, &b);
        let other = Document::from_text("one\ntwo!");
        assert!(apply(&other, &p, &same_place).is_none());
        // Elsewhere is fine.
        let elsewhere = Document::from_text("ONE\ntwo");
        assert_eq!(apply(&elsewhere, &p, &same_place).unwrap().plain_text(StoryRef::Body), "ONE\nTWO");
    }

    #[test]
    fn hostile_patches_are_refused() {
        let a = Document::from_text("x");
        let p = Patch { b: vec![Splice { s: Some(7), at: usize::MAX, n: 3, h: 0, v: vec![] }], ..Default::default() };
        assert!(apply(&a, &p, &same_place).is_none());
        let p = Patch { f: vec![Field { k: "part:x".into(), h: 0, v: Json::Null }], ..Default::default() };
        assert!(apply(&a, &p, &same_place).is_none());
        let p = Patch { m: [("k".to_string(), "!!".to_string())].into(), ..Default::default() };
        assert!(apply(&a, &p, &same_place).is_none());
    }
}

//! Every character's identity, so a position travels as "just after this character" and lands
//! where its author meant, whatever others did meanwhile (what a Yjs text gives, kept beside the
//! document instead of holding its text).
//!
//! A character's id is the op that made it and which of that op's characters it was; each
//! paragraph mark, table and table cell start has one too. Ids are worked out from each op's
//! documents before and after, the same way on every member: typing, Backspace and Delete at the
//! op's selection are matched exactly, anything else by aligning the changed text (Myers' diff).
//! Unchanged blocks share their ids with the state before.

use std::collections::{BTreeMap, HashMap};
use std::sync::Arc;

use wordcraft_doc::{Block, Document, Path, Pos, StoryRef};
use wordcraft_engine::Selection;

use super::rebase;

/// The op that made a character (client, n) and which of its characters it was.
pub type Id = (u64, u32, u32);

/// Deepest table nesting followed.
const MAX_DEPTH: usize = 8;

#[derive(Debug, PartialEq)]
enum BlockIds {
    /// One per character, then the paragraph mark's.
    Para(Vec<Id>),
    /// The table's own, and per cell its start's and its blocks'.
    Table(Id, Vec<Vec<(Id, Vec<Arc<BlockIds>>)>>),
}

/// The ids of a document's characters, per story.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Ids(BTreeMap<Option<u32>, Arc<Vec<Arc<BlockIds>>>>);

/// Gives out the ids of the characters an op made.
struct Gen((u64, u32), u32);

impl Gen {
    fn next(&mut self) -> Id {
        self.1 = self.1.wrapping_add(1);
        (self.0.0, self.0.1, self.1)
    }
}

fn key(s: StoryRef) -> Option<u32> {
    match s {
        StoryRef::Body => None,
        StoryRef::Part(id) => Some(id),
    }
}

fn stories(doc: &Document) -> impl Iterator<Item = (StoryRef, &Vec<Arc<Block>>)> {
    std::iter::once((StoryRef::Body, &doc.body)).chain(doc.parts.iter().map(|(id, p)| (StoryRef::Part(*id), &p.blocks)))
}

fn same(x: &Arc<Block>, y: &Arc<Block>) -> bool {
    Arc::ptr_eq(x, y) || x == y
}

impl Ids {
    /// New ids for every character of `doc`, made by `op`.
    pub fn fresh(doc: &Document, op: (u64, u32)) -> Ids {
        let mut g = Gen(op, 0);
        Ids(stories(doc).map(|(s, b)| (key(s), Arc::new(fresh(b, &mut g, 0)))).collect())
    }

    /// The ids of `b`, which `op` made from `a` (whose ids these are); `sel` is where in `a` it
    /// ran, when it ran as a command.
    pub fn next(&self, a: &Document, b: &Document, sel: Option<&Selection>, op: (u64, u32)) -> Ids {
        let mut g = Gen(op, 0);
        let out = stories(b).map(|(s, bb)| {
            let k = key(s);
            let ids = match (self.0.get(&k), a.story(s)) {
                (Some(old), Some(ab)) if ab.len() == bb.len() && ab.iter().zip(bb).all(|(x, y)| same(x, y)) && old.len() == ab.len() => old.clone(),
                (Some(old), Some(ab)) => {
                    let hint: Vec<(&[u32], usize)> =
                        sel.iter().flat_map(|s| [&s.anchor, &s.focus]).filter(|p| p.story == s).map(|p| (p.path.0.as_slice(), p.off)).collect();
                    Arc::new(carry(old, ab, bb, &hint, &mut g, 0))
                }
                _ => Arc::new(fresh(bb, &mut g, 0)),
            };
            (k, ids)
        });
        Ids(out.collect())
    }

    /// The character just before `p` (`Some(None)`: the start of its story); `None` when these
    /// ids don't cover it.
    pub fn anchor(&self, doc: &Document, p: &Pos) -> Option<Option<Id>> {
        anchor(self.0.get(&key(p.story))?, doc.story(p.story)?, &p.path.0, p.off, 0)
    }

    /// Where just after `after` is in `story` of `doc` (these its ids); `None` when it's gone.
    pub fn find(&self, doc: &Document, story: StoryRef, after: Option<Id>) -> Option<Pos> {
        let (ids, blocks) = (self.0.get(&key(story))?, doc.story(story)?);
        let (path, off) = match after {
            None => start_of(blocks, 0, 0)?,
            Some(id) => find(ids, blocks, id, false, 0)?,
        };
        Some(Pos { story, path: Path(path), off })
    }

    /// The character at `p` (just after it; a paragraph's mark at its end).
    pub fn at(&self, doc: &Document, p: &Pos) -> Option<Id> {
        at(self.0.get(&key(p.story))?, doc.story(p.story)?, &p.path.0, p.off, 0)
    }

    /// Where just before character `id` is in `story` of `doc`; `None` when it's gone.
    pub fn find_before(&self, doc: &Document, story: StoryRef, id: Id) -> Option<Pos> {
        let (path, off) = find(self.0.get(&key(story))?, doc.story(story)?, id, true, 0)?;
        Some(Pos { story, path: Path(path), off })
    }
}

fn fresh(blocks: &[Arc<Block>], g: &mut Gen, depth: usize) -> Vec<Arc<BlockIds>> {
    blocks.iter().map(|b| Arc::new(fresh_block(b, g, depth))).collect()
}

fn fresh_block(b: &Block, g: &mut Gen, depth: usize) -> BlockIds {
    match b {
        Block::Para(p) => BlockIds::Para((0..=p.text.chars().count()).map(|_| g.next()).collect()),
        Block::Table(t) => {
            let mark = g.next();
            let cells = if depth < MAX_DEPTH {
                t.rows.iter().map(|r| r.cells.iter().map(|c| (g.next(), fresh(&c.blocks, g, depth + 1))).collect()).collect()
            } else {
                vec![]
            };
            BlockIds::Table(mark, cells)
        }
    }
}

/// Whether table ids fit table `t` cell for cell.
fn fits(cells: &[Vec<(Id, Vec<Arc<BlockIds>>)>], t: &wordcraft_doc::Table) -> bool {
    cells.len() == t.rows.len() && cells.iter().zip(&t.rows).all(|(c, r)| c.len() == r.cells.len())
}

fn same_shape(x: &wordcraft_doc::Table, y: &wordcraft_doc::Table) -> bool {
    x.rows.len() == y.rows.len() && x.rows.iter().zip(&y.rows).all(|(a, b)| a.cells.len() == b.cells.len())
}

/// The ids of blocks `b`, made from `a` (whose ids are `old`); `hint` holds the op's selection at
/// this level (path, byte offset).
fn carry(old: &[Arc<BlockIds>], a: &[Arc<Block>], b: &[Arc<Block>], hint: &[(&[u32], usize)], g: &mut Gen, depth: usize) -> Vec<Arc<BlockIds>> {
    if old.len() != a.len() || depth > MAX_DEPTH {
        return fresh(b, g, depth);
    }
    let prefix = a.iter().zip(b).take_while(|(x, y)| same(x, y)).count();
    let max_suffix = a.len().min(b.len()) - prefix;
    let suffix = a.iter().rev().zip(b.iter().rev()).take(max_suffix).take_while(|(x, y)| same(x, y)).count();
    let (am, bm, om) = (&a[prefix..a.len() - suffix], &b[prefix..b.len() - suffix], &old[prefix..a.len() - suffix]);
    let mid = match (am, bm, om) {
        // One table changed inside, keeping its shape: cell by cell.
        ([x], [y], [o])
            if let (Block::Table(x), Block::Table(y), BlockIds::Table(mark, cells)) = (&**x, &**y, &**o)
                && same_shape(x, y)
                && fits(cells, x) =>
        {
            let cells = cells
                .iter()
                .zip(x.rows.iter().zip(&y.rows))
                .enumerate()
                .map(|(r, (cells, (xr, yr)))| {
                    cells
                        .iter()
                        .zip(xr.cells.iter().zip(&yr.cells))
                        .enumerate()
                        .map(|(c, ((cid, cold), (xc, yc)))| {
                            let inner: Vec<(&[u32], usize)> = hint
                                .iter()
                                .filter_map(|(p, off)| match p {
                                    [k, pr, pc, rest @ ..] if *k as usize == prefix && *pr as usize == r && *pc as usize == c => Some((rest, *off)),
                                    _ => None,
                                })
                                .collect();
                            (*cid, carry(cold, &xc.blocks, &yc.blocks, &inner, g, depth + 1))
                        })
                        .collect()
                })
                .collect();
            vec![Arc::new(BlockIds::Table(*mark, cells))]
        }
        _ => {
            let hint: Vec<usize> = hint.iter().filter_map(|(p, off)| flat_index(am, prefix, p, *off)).collect();
            flat_carry(om, am, bm, &hint, g, depth)
        }
    };
    old[..prefix].iter().cloned().chain(mid).chain(old[a.len() - suffix..].iter().cloned()).collect()
}

/// A block run as one sequence: each paragraph's characters then its mark, each table one element.
fn flat(blocks: &[Arc<Block>]) -> Vec<char> {
    let mut out = vec![];
    for b in blocks {
        match &**b {
            Block::Para(p) => {
                out.extend(p.text.chars());
                out.push('\n');
            }
            // A private-use character: U+FFFC stands for pictures in paragraph text.
            Block::Table(_) => out.push('\u{F8FF}'),
        }
    }
    out
}

/// Where a position in paragraph `path` (of the run starting at block `first`) is in [`flat`].
fn flat_index(run: &[Arc<Block>], first: usize, path: &[u32], off: usize) -> Option<usize> {
    let [i] = path else { return None };
    let k = (*i as usize).checked_sub(first)?;
    let before: usize = run.get(..k)?.iter().map(|b| b.as_para().map_or(1, |p| p.text.chars().count() + 1)).sum();
    let p = run.get(k)?.as_para()?;
    Some(before + p.text.char_indices().take_while(|(o, _)| *o < off).count())
}

/// The ids of the changed run `bm`, made from `am` (ids `om`): characters both keep keep their ids.
fn flat_carry(om: &[Arc<BlockIds>], am: &[Arc<Block>], bm: &[Arc<Block>], hint: &[usize], g: &mut Gen, depth: usize) -> Vec<Arc<BlockIds>> {
    let mut oi: Vec<Id> = vec![];
    // The old tables, by their id: a table both keep keeps its cells' ids.
    let mut tables: HashMap<Id, (&wordcraft_doc::Table, &Arc<BlockIds>)> = HashMap::new();
    for (o, a) in om.iter().zip(am) {
        match (&**o, &**a) {
            (BlockIds::Para(v), Block::Para(p)) if v.len() == p.text.chars().count() + 1 => oi.extend(v),
            (BlockIds::Table(mark, _), Block::Table(t)) => {
                oi.push(*mark);
                tables.insert(*mark, (t, o));
            }
            // Ids that don't fit: nothing is kept.
            _ => return fresh(bm, g, depth),
        }
    }
    let (oc, nc) = (flat(am), flat(bm));
    let pairs = splice(&oc, &nc, hint).or_else(|| rebase::lcs(&oc, &nc)).unwrap_or_default();
    let mut ni: Vec<Option<Id>> = vec![None; nc.len()];
    for (i, j) in pairs {
        if let (Some(slot), Some(id)) = (ni.get_mut(j), oi.get(i)) {
            *slot = Some(*id);
        }
    }
    let ni: Vec<Id> = ni.into_iter().map(|id| id.unwrap_or_else(|| g.next())).collect();
    let mut ids = ni.into_iter();
    bm.iter()
        .map(|b| {
            Arc::new(match &**b {
                Block::Para(p) => BlockIds::Para(ids.by_ref().take(p.text.chars().count() + 1).collect()),
                Block::Table(t) => {
                    let mark = ids.next().unwrap_or_else(|| g.next());
                    match tables.get(&mark) {
                        Some((old, o)) if **old == *t => return Arc::clone(o),
                        Some((old, o)) if same_shape(old, t) && depth < MAX_DEPTH => match &***o {
                            BlockIds::Table(_, cells) if fits(cells, old) => BlockIds::Table(
                                mark,
                                cells
                                    .iter()
                                    .zip(old.rows.iter().zip(&t.rows))
                                    .map(|(cells, (xr, yr))| {
                                        cells
                                            .iter()
                                            .zip(xr.cells.iter().zip(&yr.cells))
                                            .map(|((cid, cold), (xc, yc))| (*cid, carry(cold, &xc.blocks, &yc.blocks, &[], g, depth + 1)))
                                            .collect()
                                    })
                                    .collect(),
                            ),
                            _ => fresh_table(mark, t, g, depth),
                        },
                        _ => fresh_table(mark, t, g, depth),
                    }
                }
            })
        })
        .collect()
}

fn fresh_table(mark: Id, t: &wordcraft_doc::Table, g: &mut Gen, depth: usize) -> BlockIds {
    match fresh_block(&Block::Table(t.clone()), g, depth) {
        BlockIds::Table(_, cells) => BlockIds::Table(mark, cells),
        other => other,
    }
}

/// The edit as one splice at the op's selection (typing, typing over a selection, Backspace,
/// Delete), when that's what it was: the characters kept, as index pairs.
fn splice(oc: &[char], nc: &[char], hint: &[usize]) -> Option<Vec<(usize, usize)>> {
    let (lo, hi) = (*hint.iter().min()?, *hint.iter().max()?);
    let d = nc.len() as isize - oc.len() as isize;
    let gone = d.unsigned_abs();
    let mut tries = vec![(lo, hi)];
    if lo == hi && d < 0 {
        tries.extend(lo.checked_sub(gone).map(|s| (s, lo)));
        tries.push((lo, lo + gone));
    }
    tries.into_iter().find_map(|(s, e)| {
        let ne = usize::try_from(e as isize + d).ok().filter(|ne| *ne >= s)?;
        if oc.get(..s)? != nc.get(..s)? || oc.get(e..)? != nc.get(ne..)? {
            return None;
        }
        // Inside the splice, what both keep at its ends (formatting keeps every character).
        let (om, nm) = (oc.get(s..e)?, nc.get(s..ne)?);
        let p = om.iter().zip(nm).take_while(|(x, y)| x == y).count();
        let q = om.iter().skip(p).rev().zip(nm.iter().skip(p).rev()).take_while(|(x, y)| x == y).count();
        let kept = (0..s + p).map(|i| (i, i)).chain((1..=q).map(|i| (e - i, ne - i)));
        Some(kept.chain((e..oc.len()).map(|i| (i, i + ne - e))).collect())
    })
}

/// The position at the start of block `i`: the first paragraph from there (a table someone put
/// there meanwhile doesn't take it in), into a table's first cell only when no paragraph follows.
fn start_of(blocks: &[Arc<Block>], i: usize, depth: usize) -> Option<(Vec<u32>, usize)> {
    if let Some(j) = blocks.get(i..).and_then(|r| r.iter().position(|b| b.as_para().is_some())) {
        return Some((vec![(i + j) as u32], 0));
    }
    match &**blocks.get(i)? {
        Block::Para(_) => Some((vec![i as u32], 0)),
        Block::Table(t) if depth < MAX_DEPTH => {
            let (p, off) = start_of(&t.rows.first()?.cells.first()?.blocks, 0, depth + 1)?;
            Some(([i as u32, 0, 0].into_iter().chain(p).collect(), off))
        }
        Block::Table(_) => None,
    }
}

/// Just after `id` in `blocks` (`before`: just before it; only for characters and marks).
fn find(ids: &[Arc<BlockIds>], blocks: &[Arc<Block>], id: Id, before: bool, depth: usize) -> Option<(Vec<u32>, usize)> {
    if depth > MAX_DEPTH {
        return None;
    }
    for (i, (bi, b)) in ids.iter().zip(blocks).enumerate() {
        match (&**bi, &**b) {
            (BlockIds::Para(v), Block::Para(p)) => {
                let Some(c) = v.iter().position(|x| *x == id) else { continue };
                if before {
                    return Some((vec![i as u32], p.text.char_indices().nth(c).map_or(p.text.len(), |(o, _)| o)));
                }
                if c + 1 < v.len() {
                    let off = p.text.char_indices().nth(c + 1).map_or(p.text.len(), |(o, _)| o);
                    return Some((vec![i as u32], off));
                }
                // After a paragraph mark: the start of what follows (the end of the story's last).
                return start_of(blocks, i + 1, depth).or(Some((vec![i as u32], p.text.len())));
            }
            (BlockIds::Table(mark, cells), Block::Table(t)) => {
                if *mark == id {
                    return if before { None } else { start_of(blocks, i + 1, depth) };
                }
                for (r, (cells, row)) in cells.iter().zip(&t.rows).enumerate() {
                    for (c, ((cid, cids), cell)) in cells.iter().zip(&row.cells).enumerate() {
                        let found = match *cid == id {
                            true if before => return None,
                            true => start_of(&cell.blocks, 0, depth + 1),
                            false => find(cids, &cell.blocks, id, before, depth + 1),
                        };
                        if let Some((p, off)) = found {
                            return Some(([i as u32, r as u32, c as u32].into_iter().chain(p).collect(), off));
                        }
                    }
                }
            }
            _ => {}
        }
    }
    None
}

/// The id at `path`/`off` in `blocks`: the character just after it, or the paragraph's mark.
fn at(ids: &[Arc<BlockIds>], blocks: &[Arc<Block>], path: &[u32], off: usize, depth: usize) -> Option<Id> {
    let (&i, rest) = path.split_first()?;
    let i = i as usize;
    match (&**ids.get(i)?, &**blocks.get(i)?) {
        (BlockIds::Para(v), Block::Para(p)) if rest.is_empty() => v.get(p.text.char_indices().take_while(|(o, _)| *o < off).count()).copied(),
        (BlockIds::Table(_, cells), Block::Table(t)) if depth < MAX_DEPTH => {
            let [r, c, inner @ ..] = rest else { return None };
            let (_, cids) = cells.get(*r as usize)?.get(*c as usize)?;
            at(cids, &t.rows.get(*r as usize)?.cells.get(*c as usize)?.blocks, inner, off, depth + 1)
        }
        _ => None,
    }
}

/// The id just before `path`/`off` in `blocks` (`Some(None)`: their start).
fn anchor(ids: &[Arc<BlockIds>], blocks: &[Arc<Block>], path: &[u32], off: usize, depth: usize) -> Option<Option<Id>> {
    let (&i, rest) = path.split_first()?;
    let i = i as usize;
    match (&**ids.get(i)?, &**blocks.get(i)?) {
        (BlockIds::Para(v), Block::Para(p)) if rest.is_empty() => {
            let c = p.text.char_indices().take_while(|(o, _)| *o < off).count();
            match c.checked_sub(1) {
                Some(c) => Some(Some(*v.get(c)?)),
                None if i == 0 => Some(None),
                None => match &**ids.get(i - 1)? {
                    BlockIds::Para(v) => Some(Some(*v.last()?)),
                    BlockIds::Table(mark, _) => Some(Some(*mark)),
                },
            }
        }
        (BlockIds::Table(_, cells), Block::Table(t)) if depth < MAX_DEPTH => {
            let [r, c, inner @ ..] = rest else { return None };
            let (cid, cids) = cells.get(*r as usize)?.get(*c as usize)?;
            let cell = t.rows.get(*r as usize)?.cells.get(*c as usize)?;
            Some(anchor(cids, &cell.blocks, inner, off, depth + 1)?.or(Some(*cid)))
        }
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]
    use super::*;

    fn doc(text: &str) -> Document {
        Document::from_text(text)
    }

    fn caret(block: usize, off: usize) -> Selection {
        Selection::caret(Pos::body(block, off))
    }

    fn at(ids: &Ids, d: &Document, after: Option<Id>) -> (Vec<u32>, usize) {
        let p = ids.find(d, StoryRef::Body, after).unwrap();
        (p.path.0, p.off)
    }

    #[test]
    fn typed_characters_keep_their_place_through_others_typing() {
        let a = doc("aa");
        let ids = Ids::fresh(&a, (0, 0));
        // After the first "a".
        let first = ids.anchor(&a, &Pos::body(0, 1)).unwrap();
        // An "a" typed at the very start: exact, though the text alone can't tell.
        let b = doc("aaa");
        let next = ids.next(&a, &b, Some(&caret(0, 0)), (7, 0));
        assert_eq!(at(&next, &b, first), (vec![0], 2));
        // Typed after it instead.
        let next = ids.next(&a, &b, Some(&caret(0, 1)), (7, 0));
        assert_eq!(at(&next, &b, first), (vec![0], 1));
        assert_eq!(next.anchor(&b, &Pos::body(0, 2)).unwrap(), Some((7, 0, 1)));
    }

    #[test]
    fn backspace_delete_splits_and_merges_keep_ids() {
        let a = doc("abc\ndef");
        let ids = Ids::fresh(&a, (0, 0));
        let after_c = ids.anchor(&a, &Pos::body(0, 3)).unwrap();
        let start_def = ids.anchor(&a, &Pos::body(1, 0)).unwrap();
        // Backspace at the end of "abc".
        let b = doc("ab\ndef");
        let n = ids.next(&a, &b, Some(&caret(0, 3)), (1, 0));
        assert_eq!(n.find(&b, StoryRef::Body, after_c), None, "a deleted character is gone");
        assert_eq!(at(&n, &b, start_def), (vec![1], 0));
        // Enter in "def": "de" keeps the old mark, a new one ends "d".
        let c = doc("ab\nd\nef");
        let n2 = n.next(&b, &c, Some(&caret(1, 1)), (2, 0));
        assert_eq!(at(&n2, &c, start_def), (vec![1], 0));
        let after_d = n.anchor(&b, &Pos::body(1, 1)).unwrap();
        assert_eq!(at(&n2, &c, after_d), (vec![1], 1));
        // The two merged again (no hint: by the text).
        let n3 = n2.next(&c, &b, None, (3, 0));
        assert_eq!(at(&n3, &b, after_d), (vec![1], 1));
        assert_eq!(at(&n3, &b, None), (vec![0], 0));
    }

    #[test]
    fn formatting_keeps_ids_and_a_new_table_doesnt_take_a_paragraph_start() {
        let a = doc("hello\ndef");
        let ids = Ids::fresh(&a, (0, 0));
        // Bold on "hello" (the text the same): every character keeps its id.
        let pairs = splice(&flat(&a.body), &flat(&a.body), &[0, 5]).unwrap();
        assert_eq!(pairs, (0..10).map(|i| (i, i)).collect::<Vec<_>>());
        // Typing over "ll": "he" and "o" keep theirs.
        let pairs = splice(&"hello".chars().collect::<Vec<_>>(), &"heXo".chars().collect::<Vec<_>>(), &[2, 4]).unwrap();
        assert_eq!(pairs, vec![(0, 0), (1, 1), (4, 3)]);
        // A table put before "def": the start of "def" stays the start of "def".
        let start_def = ids.anchor(&a, &Pos::body(1, 0)).unwrap();
        let mut t = a.clone();
        let table = Document::from_text("x").body[0].clone();
        let table = Arc::new(Block::Table(wordcraft_doc::Table {
            rows: vec![wordcraft_doc::Row { cells: vec![wordcraft_doc::Cell { blocks: vec![table], ..Default::default() }], ..Default::default() }],
            ..Default::default()
        }));
        t.body.insert(1, table);
        let n = ids.next(&a, &t, None, (2, 0));
        assert_eq!(at(&n, &t, start_def), (vec![2], 0));
        assert_eq!(at(&n, &t, None), (vec![0], 0));
    }

    #[test]
    fn a_character_is_found_before_and_after() {
        let a = doc("abc\nd");
        let ids = Ids::fresh(&a, (0, 0));
        let b = ids.at(&a, &Pos::body(0, 1)).unwrap();
        assert_eq!(ids.find_before(&a, StoryRef::Body, b).map(|p| (p.path.0, p.off)), Some((vec![0], 1)));
        assert_eq!(ids.find(&a, StoryRef::Body, Some(b)).map(|p| (p.path.0, p.off)), Some((vec![0], 2)));
        // At the end of a paragraph: its mark.
        let mark = ids.at(&a, &Pos::body(0, 3)).unwrap();
        assert_eq!(ids.find_before(&a, StoryRef::Body, mark).map(|p| (p.path.0, p.off)), Some((vec![0], 3)));
        assert_eq!(ids.at(&a, &Pos::body(5, 0)), None);
    }

    #[test]
    fn hostile_anchors_and_paths_never_panic() {
        let a = doc("x\ny");
        let ids = Ids::fresh(&a, (0, 0));
        assert_eq!(ids.find(&a, StoryRef::Body, Some((9, 9, 9))), None);
        assert_eq!(ids.find(&a, StoryRef::Part(4), None), None);
        for path in [vec![], vec![99], vec![0, 5, 5, 5], vec![u32::MAX]] {
            let p = Pos { story: StoryRef::Body, path: Path(path), off: usize::MAX };
            assert_eq!(ids.anchor(&a, &p), None);
        }
        // Ids of another document: nothing is kept, nothing breaks.
        let other = Ids::fresh(&doc("one\ntwo\nthree"), (1, 1));
        let b = doc("x!\ny");
        let _ = other.next(&a, &b, Some(&caret(5, 99)), (2, 2));
    }
}

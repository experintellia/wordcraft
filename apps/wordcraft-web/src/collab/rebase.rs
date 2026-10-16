//! Where a position lands after a change someone else made: the change is read off the two
//! documents themselves, so it works for every command without per-command code.
//!
//! In each story, blocks equal in both documents (the same `Arc`, or equal content) at the start
//! and the end are unchanged; between them, the changed blocks' text (paragraphs joined by
//! newlines, a table as one placeholder) is compared as one string, whose common prefix and suffix
//! give the replaced span. A position before the span stays, one after it moves by the span's
//! change in length, and one inside it goes to the end of the replacement. Splitting, merging,
//! inserting and deleting paragraphs are all spans of that string. A position inside a table that
//! is still there, with the same shape, maps cell by cell. Everything is deterministic, so every
//! member maps the same position the same way.

use std::sync::Arc;

use wordcraft_doc::{Block, Document, Path, Pos, StoryRef};

/// Stands for a table in the joined text of changed blocks.
const TABLE: char = '\u{FFFC}';
/// Deepest table nesting a position is followed into.
const MAX_DEPTH: usize = 8;

/// Where `p`, a position in `a`, is in `b` (`a` after one change). `part` maps a part id of `a`
/// to its id in `b` (parts created concurrently get other ids in the final order). `None` when
/// the change deleted the story `p` is in (a header, a note, a text box).
pub fn map_pos(a: &Document, b: &Document, p: &Pos, part: &dyn Fn(u32) -> u32) -> Option<Pos> {
    let story = match p.story {
        StoryRef::Body => StoryRef::Body,
        StoryRef::Part(id) => StoryRef::Part(part(id)),
    };
    match (a.story(p.story), b.story(story)) {
        (Some(x), Some(y)) => {
            let mapped = map_in(x, y, &p.path.0, p.off, 0).map(|(path, off)| Pos { story, path: Path(path), off });
            Some(b.clamp(&mapped.unwrap_or_else(|| Pos { story, ..p.clone() })))
        }
        // A story this change didn't touch (made by a later op, or by none): as it was.
        (None, None) => Some(Pos { story, ..p.clone() }),
        (Some(_), None) => None,
        (None, Some(_)) => Some(b.clamp(&Pos { story, ..p.clone() })),
    }
}

/// Where block `i` of `story` in `a` is in `b`: before the changed run it stays, after it moves
/// by the run's change in length, inside it goes to the run's start. `None` when the change
/// deleted the story.
pub fn map_block(a: &Document, b: &Document, story: StoryRef, i: usize) -> Option<usize> {
    let (x, y) = match (a.story(story), b.story(story)) {
        (Some(x), Some(y)) => (x, y),
        (None, None) => return Some(i),
        (Some(_), None) => return None,
        (None, Some(_)) => return Some(i),
    };
    let prefix = x.iter().zip(y).take_while(|(p, q)| same(p, q)).count();
    let max_suffix = x.len().min(y.len()) - prefix;
    let suffix = x.iter().rev().zip(y.iter().rev()).take(max_suffix).take_while(|(p, q)| same(p, q)).count();
    Some(if i < prefix {
        i
    } else if i >= x.len().saturating_sub(suffix) {
        (i + y.len()).saturating_sub(x.len())
    } else {
        prefix
    })
}

fn same(x: &Arc<Block>, y: &Arc<Block>) -> bool {
    Arc::ptr_eq(x, y) || x == y
}

/// Maps `path`/`off` from blocks `a` to blocks `b`.
fn map_in(a: &[Arc<Block>], b: &[Arc<Block>], path: &[u32], off: usize, depth: usize) -> Option<(Vec<u32>, usize)> {
    let (&i, rest) = path.split_first()?;
    let i = i as usize;
    let prefix = a.iter().zip(b).take_while(|(x, y)| same(x, y)).count();
    let max_suffix = a.len().min(b.len()) - prefix;
    let suffix = a.iter().rev().zip(b.iter().rev()).take(max_suffix).take_while(|(x, y)| same(x, y)).count();
    let with = |j: usize| -> Vec<u32> { std::iter::once(j as u32).chain(rest.iter().copied()).collect() };
    if i < prefix {
        return Some((with(i), off));
    }
    if i >= a.len().saturating_sub(suffix) {
        return Some((with((i + b.len()).checked_sub(a.len())?), off));
    }
    let (am, bm) = (a.get(prefix..a.len() - suffix)?, b.get(prefix..b.len() - suffix)?);
    let k = i - prefix;
    // Inside a table: follow it while it keeps its place and shape.
    if let ([r, c, inner @ ..], Some(Block::Table(x)), Some(Block::Table(y))) = (rest, am.get(k).map(|b| &**b), bm.get(k).map(|b| &**b))
        && am.len() == bm.len()
        && depth < MAX_DEPTH
        && let (Some(cx), Some(cy)) =
            (x.rows.get(*r as usize).and_then(|row| row.cells.get(*c as usize)), y.rows.get(*r as usize).and_then(|row| row.cells.get(*c as usize)))
    {
        let (p, o) = map_in(&cx.blocks, &cy.blocks, inner, off, depth + 1)?;
        return Some(([i as u32, *r, *c].into_iter().chain(p).collect(), o));
    }
    let (ta, _) = flat(am);
    let fb = flat(bm);
    let tb = &fb.0;
    // In a table that changed shape or went: where the table started.
    let g = offset_of(am, k, if rest.is_empty() { off } else { 0 });
    // The replaced span: common prefix and suffix of the joined texts, on char boundaries.
    let mut cp = ta.bytes().zip(tb.bytes()).take_while(|(x, y)| x == y).count();
    while !ta.is_char_boundary(cp) || !tb.is_char_boundary(cp) {
        cp -= 1;
    }
    let mut cs = ta.bytes().rev().zip(tb.bytes().rev()).take(ta.len().min(tb.len()) - cp).take_while(|(x, y)| x == y).count();
    while !ta.is_char_boundary(ta.len() - cs) || !tb.is_char_boundary(tb.len() - cs) {
        cs -= 1;
    }
    let to = if g <= cp {
        g
    } else if g >= ta.len() - cs {
        g + tb.len() - ta.len()
    } else {
        cp + within(ta.get(cp..ta.len() - cs)?, tb.get(cp..tb.len() - cs)?, g - cp)
    };
    Some(locate(bm, prefix, to, &fb))
}

/// Most edits [`lcs`] looks for between the replaced and the replacing text; past that a
/// position in the replaced text goes to the end of the replacement.
const MAX_EDITS: isize = 300;

/// Byte offset `g` of `a` (a char boundary) in `b`, `a` edited: just after the last character
/// before it that both keep (Myers' diff), so a position stays before text inserted there, and one
/// inside deleted text goes to where that text was.
fn within(a: &str, b: &str, g: usize) -> usize {
    let (ac, bc): (Vec<(usize, char)>, Vec<(usize, char)>) = (a.char_indices().collect(), b.char_indices().collect());
    let chars = |v: &[(usize, char)]| v.iter().map(|c| c.1).collect::<Vec<_>>();
    let Some(pairs) = lcs(&chars(&ac), &chars(&bc)) else { return b.len() };
    let gi = ac.iter().position(|(o, _)| *o >= g).unwrap_or(ac.len());
    match pairs.iter().rev().find(|(i, _)| *i < gi).and_then(|(_, j)| bc.get(*j)) {
        Some((o, c)) => o + c.len_utf8(),
        None => 0,
    }
}

/// The characters `a` and `b` have in common, as index pairs in order (Myers' O(ND) diff), or
/// `None` past [`MAX_EDITS`] edits.
fn lcs(a: &[char], b: &[char]) -> Option<Vec<(usize, usize)>> {
    let (n, m) = (a.len() as isize, b.len() as isize);
    let max = MAX_EDITS.min(n + m);
    let off = max + 1;
    let at = |v: &[isize], k: isize| v.get((k + off) as usize).copied().unwrap_or(0);
    let mut v = vec![0isize; (2 * max + 3) as usize];
    let mut trace: Vec<Vec<isize>> = vec![];
    for d in 0..=max {
        trace.push(v.clone());
        for k in (-d..=d).step_by(2) {
            let mut x = if k == -d || (k != d && at(&v, k - 1) < at(&v, k + 1)) { at(&v, k + 1) } else { at(&v, k - 1) + 1 };
            let mut y = x - k;
            while x < n && y < m && a.get(x as usize) == b.get(y as usize) {
                x += 1;
                y += 1;
            }
            *v.get_mut((k + off) as usize)? = x;
            if x >= n && y >= m {
                return Some(backtrack(&trace, n, m, off));
            }
        }
    }
    None
}

fn backtrack(trace: &[Vec<isize>], n: isize, m: isize, off: isize) -> Vec<(usize, usize)> {
    let at = |v: &[isize], k: isize| v.get((k + off) as usize).copied().unwrap_or(0);
    let (mut x, mut y) = (n, m);
    let mut pairs = vec![];
    for (d, v) in trace.iter().enumerate().rev() {
        let (d, k) = (d as isize, x - y);
        let prev_k = if k == -d || (k != d && at(v, k - 1) < at(v, k + 1)) { k + 1 } else { k - 1 };
        let prev_x = at(v, prev_k);
        let prev_y = prev_x - prev_k;
        while x > prev_x && y > prev_y && x > 0 && y > 0 {
            x -= 1;
            y -= 1;
            pairs.push((x as usize, y as usize));
        }
        (x, y) = (prev_x, prev_y);
    }
    pairs.reverse();
    pairs
}

/// The changed blocks' joined text (paragraphs' text then `\n`; a table as [`TABLE`] then `\n`)
/// and where each block starts in it.
fn flat(blocks: &[Arc<Block>]) -> (String, Vec<usize>) {
    let mut s = String::new();
    let mut starts = Vec::with_capacity(blocks.len());
    for b in blocks {
        starts.push(s.len());
        match &**b {
            Block::Para(p) => s.push_str(&p.text),
            Block::Table(_) => s.push(TABLE),
        }
        s.push('\n');
    }
    (s, starts)
}

/// Byte offset of `off` in block `k` of the joined text.
fn offset_of(blocks: &[Arc<Block>], k: usize, off: usize) -> usize {
    let before: usize = blocks.iter().take(k).map(|b| block_len(b) + 1).sum();
    before + off.min(blocks.get(k).map_or(0, |b| block_len(b)))
}

fn block_len(b: &Block) -> usize {
    match b {
        Block::Para(p) => p.text.len(),
        Block::Table(_) => TABLE.len_utf8(),
    }
}

/// The position at byte `g` of the joined text of `blocks` (which start at index `first`). A
/// table's place goes to its first cell's first paragraph.
fn locate(blocks: &[Arc<Block>], first: usize, g: usize, (text, starts): &(String, Vec<usize>)) -> (Vec<u32>, usize) {
    let k = starts.iter().rposition(|s| *s <= g.min(text.len())).unwrap_or(0);
    let off = g.saturating_sub(starts.get(k).copied().unwrap_or(0));
    match blocks.get(k).map(|b| &**b) {
        Some(Block::Para(p)) => (vec![(first + k) as u32], off.min(p.text.len())),
        Some(Block::Table(_)) => (vec![(first + k) as u32, 0, 0, 0], 0),
        // Nothing replaced the blocks: the block that now follows them (`clamp` fixes the rest).
        None => (vec![first as u32], 0),
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]
    use super::*;

    fn doc(text: &str) -> Document {
        Document::from_text(text)
    }

    fn at(block: usize, off: usize) -> Pos {
        Pos::body(block, off)
    }

    fn map(a: &Document, b: &Document, p: Pos) -> (Vec<u32>, usize) {
        let q = map_pos(a, b, &p, &|id| id).unwrap();
        (q.path.0, q.off)
    }

    #[test]
    fn typing_before_a_position_moves_it() {
        let a = doc("hello world");
        let b = doc("hello big world");
        assert_eq!(map(&a, &b, at(0, 6)), (vec![0], 6), "at the insertion point it stays before");
        assert_eq!(map(&a, &b, at(0, 8)), (vec![0], 12));
        assert_eq!(map(&a, &b, at(0, 2)), (vec![0], 2));
    }

    #[test]
    fn same_spot_stays_before_the_insertion() {
        let a = doc("abc");
        let b = doc("abXc");
        assert_eq!(map(&a, &b, at(0, 2)), (vec![0], 2));
    }

    #[test]
    fn splits_merges_and_new_paragraphs_shift_paths() {
        let a = doc("one\ntwo\nthree");
        // Split "two" into "t" / "wo".
        let b = doc("one\nt\nwo\nthree");
        assert_eq!(map(&a, &b, at(2, 3)), (vec![3], 3), "a later paragraph moves down");
        assert_eq!(map(&a, &b, at(1, 2)), (vec![2], 1), "inside the split paragraph");
        assert_eq!(map(&a, &b, at(0, 1)), (vec![0], 1));
        // Merge two paragraphs.
        let c = doc("onetwo\nthree");
        assert_eq!(map(&a, &c, at(1, 1)), (vec![0], 4));
        assert_eq!(map(&a, &c, at(2, 2)), (vec![1], 2));
        // A paragraph deleted under the position.
        let d = doc("one\nthree");
        assert_eq!(map(&a, &d, at(1, 2)), (vec![1], 0));
    }

    #[test]
    fn deleted_text_collapses_and_parts_follow_their_new_ids() {
        let a = doc("abcdef");
        let b = doc("af");
        assert_eq!(map(&a, &b, at(0, 3)), (vec![0], 1));
        assert_eq!(map(&a, &b, at(0, 6)), (vec![0], 2));
        // A story neither document has is left alone (a later op makes it).
        let p = Pos { story: StoryRef::Part(9), path: Path::top(0), off: 0 };
        assert_eq!(map_pos(&a, &b, &p, &|id| id + 1).unwrap().story, StoryRef::Part(10));
        // A story the change deleted: nowhere.
        let mut c = a.clone();
        c.parts.insert(4, wordcraft_doc::Part { kind: wordcraft_doc::PartKind::TextBox, blocks: a.body.clone(), body: Default::default() });
        let in_part = Pos { story: StoryRef::Part(4), path: Path::top(0), off: 0 };
        assert!(map_pos(&c, &a, &in_part, &|id| id).is_none());
        // Block indices map without clamping: past the end stays past the end.
        assert_eq!(map_block(&a, &b, StoryRef::Body, 1), Some(1));
        let longer = doc("abcdef\nx\ny");
        assert_eq!(map_block(&a, &longer, StoryRef::Body, 1), Some(3), "the end of the story moves with it");
    }

    #[test]
    fn a_position_between_two_changes_keeps_its_place() {
        // Changes at both ends of a paragraph (one patch): the middle keeps its place.
        let a = doc("hello world");
        let b = doc(">hello world!");
        assert_eq!(map(&a, &b, at(0, 5)), (vec![0], 6));
        let c = doc("help wild");
        assert_eq!(map(&a, &c, at(0, 6)), (vec![0], 5), "after the space both keep");
        assert_eq!(super::lcs(&['a', 'b'], &['b']), Some(vec![(1, 0)]));
        assert_eq!(super::lcs(&[], &[]), Some(vec![]));
    }

    #[test]
    fn hostile_paths_never_panic() {
        let a = doc("x\ny");
        let b = doc("y");
        for path in [vec![], vec![99], vec![0, 5, 5, 5], vec![u32::MAX, 0, 0, 0, 0, 0, 0], vec![1, 0, 0, 0]] {
            let p = Pos { story: StoryRef::Body, path: Path(path), off: usize::MAX };
            let q = map_pos(&a, &b, &p, &|id| id).unwrap();
            assert!(b.para_at(&q).is_some());
        }
    }
}

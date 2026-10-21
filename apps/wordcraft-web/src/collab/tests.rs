#![allow(clippy::unwrap_used)]

use serde_json::json;
use wordcraft_doc::{Document, Pos, StoryRef};
use wordcraft_engine::{Selection, Session};

use super::*;

type Peer = (Session, Collab);

fn peer(client: u64, name: &str) -> Peer {
    let mut s = Session::new(base());
    s.author = name.into();
    let c = Collab::new(&mut s, client);
    (s, c)
}

/// A clock far enough on that held typing is shared.
const LATER: f64 = 1e12;

fn at(s: &mut Session, block: usize, off: usize) {
    s.sel = Selection::caret(Pos::body(block, off));
}

fn run(s: &mut Session, id: &str, params: serde_json::Value) {
    s.run(id, &params).unwrap_or_else(|e| panic!("{id}: {e}"));
}

fn text(s: &Session) -> String {
    s.doc.plain_text(StoryRef::Body)
}

/// A peer's edits, typing held back included, as updates.
fn share(s: &mut Session, c: &mut Collab) -> Vec<Vec<u8>> {
    [c.local(s, LATER), c.local(s, 2.0 * LATER)].into_iter().flatten().collect()
}

/// Each peer's pending edits, as updates.
fn pending(peers: &mut [&mut Peer]) -> Vec<Vec<u8>> {
    peers.iter_mut().flat_map(|(s, c)| share(s, c)).collect()
}

/// Delivers every peer's pending edits to everyone, itself included (as webxdc does).
fn sync(peers: &mut [&mut Peer]) {
    let updates = pending(peers);
    for (s, c) in peers.iter_mut() {
        let held = c.remote(s, updates.clone(), LATER);
        assert!(held.is_empty(), "held typing after local(): {} updates", held.len());
    }
}

fn same_doc(a: &Session, b: &Session) {
    assert!(patch::diff(&a.doc, &b.doc).is_empty(), "\n{:?}\n!=\n{:?}", text(a), text(b));
}

/// Starts a shared document with `body` on peer `a` and gives it to the others.
fn start(body: &str, peers: &mut [&mut Peer]) {
    let (s, _) = &mut *peers[0];
    at(s, 0, 0);
    for (i, line) in body.split('\n').enumerate() {
        if i > 0 {
            run(s, "text.newParagraph", json!({}));
        }
        run(s, "text.insert", json!({"text": line}));
    }
    sync(peers);
}

#[test]
fn typing_in_different_paragraphs_merges() {
    let (mut a, mut b) = (peer(1, "Ann"), peer(2, "Bo"));
    start("One", &mut [&mut a, &mut b]);
    at(&mut a.0, 0, 3);
    run(&mut a.0, "text.newParagraph", json!({}));
    run(&mut a.0, "text.insert", json!({"text": "Two"}));
    sync(&mut [&mut a, &mut b]);
    at(&mut a.0, 0, 0);
    run(&mut a.0, "text.insert", json!({"text": "A:"}));
    at(&mut b.0, 1, 3);
    run(&mut b.0, "text.insert", json!({"text": " (Bo)"}));
    sync(&mut [&mut a, &mut b]);
    same_doc(&a.0, &b.0);
    assert_eq!(text(&a.0), "A:One\nTwo (Bo)");
}

#[test]
fn typing_in_the_same_paragraph_lands_where_each_meant() {
    let (mut a, mut b) = (peer(1, "Ann"), peer(2, "Bo"));
    start("hello world", &mut [&mut a, &mut b]);
    // Neither has seen the other's typing.
    at(&mut a.0, 0, 5);
    run(&mut a.0, "text.insert", json!({"text": " big"}));
    at(&mut b.0, 0, 11);
    run(&mut b.0, "text.insert", json!({"text": "!"}));
    at(&mut b.0, 0, 0);
    run(&mut b.0, "text.insert", json!({"text": ">"}));
    sync(&mut [&mut a, &mut b]);
    same_doc(&a.0, &b.0);
    assert_eq!(text(&a.0), ">hello big world!");
    // At the same spot both land, in log order.
    at(&mut a.0, 0, 1);
    run(&mut a.0, "text.insert", json!({"text": "A"}));
    at(&mut b.0, 0, 1);
    run(&mut b.0, "text.insert", json!({"text": "B"}));
    sync(&mut [&mut a, &mut b]);
    same_doc(&a.0, &b.0);
    let t = text(&a.0);
    assert!(t == ">ABhello big world!" || t == ">BAhello big world!", "{t}");
}

#[test]
fn a_split_and_typing_after_it_merge() {
    let (mut a, mut b) = (peer(1, "Ann"), peer(2, "Bo"));
    start("First second", &mut [&mut a, &mut b]);
    // Ann splits the paragraph; Bo types at its end, unaware.
    at(&mut a.0, 0, 5);
    run(&mut a.0, "text.newParagraph", json!({}));
    at(&mut b.0, 0, 12);
    run(&mut b.0, "text.insert", json!({"text": " third"}));
    sync(&mut [&mut a, &mut b]);
    same_doc(&a.0, &b.0);
    assert_eq!(text(&a.0), "First\n second third");
}

#[test]
fn the_selection_follows_others_edits() {
    let (mut a, mut b) = (peer(1, "Ann"), peer(2, "Bo"));
    start("abc", &mut [&mut a, &mut b]);
    at(&mut b.0, 0, 2);
    at(&mut a.0, 0, 0);
    run(&mut a.0, "text.insert", json!({"text": "xyz"}));
    sync(&mut [&mut a, &mut b]);
    assert_eq!(b.0.sel, Selection::caret(Pos::body(0, 5)));
}

#[test]
fn author_time_and_autocorrect_travel_with_the_op() {
    let (mut a, mut b) = (peer(1, "Ann"), peer(2, "Bo"));
    start("x", &mut [&mut a, &mut b]);
    a.0.prefs.autocorrect.replace_text = false;
    at(&mut a.0, 0, 1);
    run(&mut a.0, "text.insert", json!({"text": " (c) "}));
    run(&mut a.0, "review.newComment", json!({"text": "note"}));
    sync(&mut [&mut a, &mut b]);
    same_doc(&a.0, &b.0);
    let c = b.0.doc.comments.values().next().unwrap();
    assert_eq!(c.author, "Ann");
    assert!(text(&b.0).contains("(c)"), "AutoCorrect stayed off: {}", text(&b.0));
}

#[test]
fn concurrent_comments_and_replies_keep_their_threads() {
    let (mut a, mut b) = (peer(1, "Ann"), peer(2, "Bo"));
    start("words here", &mut [&mut a, &mut b]);
    at(&mut a.0, 0, 0);
    run(&mut a.0, "review.newComment", json!({"text": "Ann's"}));
    at(&mut b.0, 0, 6);
    let id = b.0.run("review.newComment", &json!({"text": "Bo's"})).unwrap()["id"].as_u64().unwrap();
    run(&mut b.0, "review.reply", json!({"id": id, "text": "Bo replies to himself"}));
    sync(&mut [&mut a, &mut b]);
    same_doc(&a.0, &b.0);
    let doc = &a.0.doc;
    let text_of = |id: u32| doc.plain_text(StoryRef::Part(doc.comments[&id].part));
    let reply = doc.comments.iter().find(|(_, c)| c.parent.is_some()).unwrap();
    assert_eq!(text_of(reply.1.parent.unwrap()), "Bo's", "the reply stays under Bo's comment");
}

#[test]
fn undo_takes_back_only_your_own_changes() {
    let (mut a, mut b) = (peer(1, "Ann"), peer(2, "Bo"));
    start("One\nTwo", &mut [&mut a, &mut b]);
    at(&mut a.0, 0, 3);
    run(&mut a.0, "text.insert", json!({"text": " Ann"}));
    sync(&mut [&mut a, &mut b]);
    at(&mut b.0, 1, 3);
    run(&mut b.0, "text.insert", json!({"text": " Bo"}));
    sync(&mut [&mut a, &mut b]);
    assert!(a.0.can_undo());
    run(&mut a.0, "edit.undo", json!({}));
    sync(&mut [&mut a, &mut b]);
    same_doc(&a.0, &b.0);
    assert_eq!(text(&a.0), "One\nTwo Bo");
    run(&mut a.0, "edit.redo", json!({}));
    sync(&mut [&mut a, &mut b]);
    same_doc(&a.0, &b.0);
    assert_eq!(text(&a.0), "One Ann\nTwo Bo");
}

#[test]
fn undo_is_refused_once_someone_changed_the_same_paragraph() {
    let (mut a, mut b) = (peer(1, "Ann"), peer(2, "Bo"));
    start("One", &mut [&mut a, &mut b]);
    at(&mut a.0, 0, 3);
    run(&mut a.0, "text.insert", json!({"text": " Ann"}));
    sync(&mut [&mut a, &mut b]);
    at(&mut b.0, 0, 0);
    run(&mut b.0, "text.insert", json!({"text": "Bo: "}));
    sync(&mut [&mut a, &mut b]);
    run(&mut a.0, "edit.undo", json!({}));
    sync(&mut [&mut a, &mut b]);
    assert!(a.1.refused.is_some());
    assert_eq!(text(&a.0), "Bo: One Ann");
    same_doc(&a.0, &b.0);
}

#[test]
fn typing_is_shared_as_one_op_once_it_pauses() {
    let (mut a, _) = (peer(1, "Ann"), ());
    for (i, ch) in "hello".chars().enumerate() {
        at(&mut a.0, 0, i);
        run(&mut a.0, "text.insert", json!({"text": ch.to_string()}));
        assert!(a.1.local(&mut a.0, i as f64 * 100.0).is_empty(), "typing waits");
    }
    assert!(!a.1.local(&mut a.0, 10_000.0).is_empty());
    // The start op (the blank document) and one typing op.
    assert!(a.1.applied.len() <= 2, "{:?}", a.1.applied);
    assert!(a.1.applied.last().unwrap().contains("hello"));
}

#[test]
fn an_op_that_cant_run_again_carries_its_change() {
    let (mut a, mut b) = (peer(1, "Ann"), peer(2, "Bo"));
    start("plain words", &mut [&mut a, &mut b]);
    // A paste carries Ann's clipboard (Bo has no such clipboard) and runs again.
    a.0.sel = Selection { anchor: Pos::body(0, 0), focus: Pos::body(0, 5) };
    run(&mut a.0, "format.bold", json!({}));
    run(&mut a.0, "edit.copy", json!({}));
    at(&mut a.0, 0, 11);
    run(&mut a.0, "edit.paste", json!({}));
    sync(&mut [&mut a, &mut b]);
    same_doc(&a.0, &b.0);
    assert_eq!(text(&b.0), "plain wordsplain");
    let last = a.1.applied.last().unwrap();
    assert!(last.contains("\"cb\"") && !last.contains("\"d\""), "the paste runs again: {last}");
    // A change made outside any command travels as a patch.
    let mut doc = a.0.doc.clone();
    doc.core.title = "Shared".into();
    a.0.doc = doc;
    a.0.touch();
    sync(&mut [&mut a, &mut b]);
    same_doc(&a.0, &b.0);
    assert_eq!(b.0.doc.core.title, "Shared");
    assert!(a.1.applied.last().unwrap().contains("\"d\""), "it travels as a patch");
}

/// Ann pastes and paints formatting in a paragraph Bo is typing in at the same time: both land.
/// (As patches, Ann's edits were refused whenever Bo's typing reached the same paragraph first.)
#[test]
fn a_paste_and_paste_formatting_merge_with_typing_in_the_same_paragraph() {
    for _ in 0..8 {
        let (mut a, mut b) = (peer(1, "Ann"), peer(2, "Bo"));
        start("one two three", &mut [&mut a, &mut b]);
        a.0.sel = Selection { anchor: Pos::body(0, 0), focus: Pos::body(0, 3) };
        run(&mut a.0, "format.bold", json!({}));
        run(&mut a.0, "edit.copy", json!({}));
        run(&mut a.0, "edit.copyFormat", json!({}));
        sync(&mut [&mut a, &mut b]);
        a.0.sel = Selection { anchor: Pos::body(0, 4), focus: Pos::body(0, 7) };
        run(&mut a.0, "edit.pasteFormat", json!({}));
        at(&mut a.0, 0, 13);
        run(&mut a.0, "edit.paste", json!({}));
        at(&mut b.0, 0, 0);
        run(&mut b.0, "text.insert", json!({"text": "Bo: "}));
        sync(&mut [&mut a, &mut b]);
        same_doc(&a.0, &b.0);
        assert_eq!(text(&a.0), "Bo: one two threeone");
        // "two" got the copied formatting.
        let p = a.0.doc.para_at(&Pos::body(0, 0)).unwrap();
        assert_eq!(p.props_at(9).bold, Some(true), "{:?}", p.props_at(9));
    }
}

#[test]
fn hostile_ops_are_refused() {
    let (mut a, _) = (peer(1, "Ann"), ());
    start("safe", &mut [&mut a]);
    let before = a.0.doc.clone();
    let deep = Pos { story: StoryRef::Body, path: wordcraft_doc::Path(vec![0; 1000]), off: usize::MAX };
    let hostile = [
        json!({"i": [9, 0], "c": "file.save", "p": {"path": "/tmp/x.docx"}}),
        json!({"i": [9, 1], "c": "file.open", "p": {"path": "/etc/passwd"}}),
        json!({"i": [9, 2], "c": "no.such.command"}),
        json!({"i": [9, 3], "c": "tools.macros", "p": {"run": "x"}}),
        json!({"i": [9, 4], "c": "text.insert", "p": {"text": "x"}, "sel": {"anchor": deep, "focus": deep}}),
        json!({"i": [9, 5], "c": "Edit", "d": {"b": [{"s": 77, "at": 999999, "n": 5, "h": 0, "v": []}]}}),
        json!({"i": [9, 6], "c": "Edit", "d": {"f": [{"k": "styles", "h": 1, "v": "junk"}]}}),
        json!("not an op"),
    ];
    let mut other = peer(9, "Mallory");
    for h in hostile {
        let json = serde_json::to_string(&h).unwrap();
        let u = other.1.append(&json);
        a.1.remote(&mut a.0, vec![u], LATER);
    }
    assert!(patch::diff(&before, &a.0.doc).is_empty(), "{}", text(&a.0));
}

#[test]
fn layout_dependent_results_are_the_same_everywhere() {
    let (mut a, mut b) = (peer(1, "Ann"), peer(2, "Bo"));
    start("Intro", &mut [&mut a, &mut b]);
    a.0.sel = Selection { anchor: Pos::body(0, 0), focus: Pos::body(0, 5) };
    run(&mut a.0, "para.style", json!({"style": "Heading1"}));
    at(&mut a.0, 0, 5);
    run(&mut a.0, "text.newParagraph", json!({}));
    run(&mut a.0, "text.pageBreak", json!({}));
    run(&mut a.0, "text.insert", json!({"text": "Body"}));
    at(&mut a.0, 0, 0);
    run(&mut a.0, "references.toc", json!({}));
    sync(&mut [&mut a, &mut b]);
    same_doc(&a.0, &b.0);
    // A third member replaying the whole log from blank gets the same too.
    let mut c = peer(3, "Cy");
    let all = {
        let txn = a.1.ydoc.transact();
        txn.encode_state_as_update_v1(&yrs::StateVector::default())
    };
    c.1.remote(&mut c.0, vec![all], LATER);
    same_doc(&a.0, &c.0);
}

/// N members make random edits; their updates reach each member in a different order. Everyone
/// ends with the same document.
#[test]
fn random_concurrent_edits_converge() {
    let seeds: u64 = std::env::var("WORDCRAFT_COLLAB_SEEDS").ok().and_then(|s| s.parse().ok()).unwrap_or(6);
    for seed in 0..seeds {
        let mut rng = Rng(seed.wrapping_mul(0x9e37_79b9_7f4a_7c15) | 1);
        let mut peers: Vec<Peer> = (0..3).map(|i| peer(i + 1, ["Ann", "Bo", "Cy"][i as usize])).collect();
        {
            let mut refs: Vec<&mut Peer> = peers.iter_mut().collect();
            start("alpha beta gamma\ndelta epsilon\nzeta", &mut refs);
        }
        for round in 0..12 {
            for (s, _) in peers.iter_mut() {
                for _ in 0..1 + rng.next() % 3 {
                    random_edit(s, &mut rng);
                }
            }
            let mut updates: Vec<Vec<u8>> = peers.iter_mut().flat_map(|(s, c)| share(s, c)).collect();
            for (s, c) in peers.iter_mut() {
                rng.shuffle(&mut updates);
                c.remote(s, updates.clone(), LATER);
            }
            let (first, rest) = peers.split_first().unwrap();
            for p in rest {
                assert!(patch::diff(&first.0.doc, &p.0.doc).is_empty(), "seed {seed} round {round}:\n{:?}\n{:?}", text(&first.0), text(&p.0));
            }
        }
        // Someone opening the chat later replays the whole log from blank to the same document.
        let all = {
            let txn = peers[0].1.ydoc.transact();
            txn.encode_state_as_update_v1(&yrs::StateVector::default())
        };
        let mut late = peer(9, "Late");
        late.1.remote(&mut late.0, vec![all], LATER);
        assert!(patch::diff(&peers[0].0.doc, &late.0.doc).is_empty(), "seed {seed}: the late joiner differs");
    }
}

fn random_edit(s: &mut Session, rng: &mut Rng) {
    let paths = s.doc.para_paths(StoryRef::Body);
    let path = paths[rng.next() as usize % paths.len()].clone();
    let len = s.doc.para(StoryRef::Body, &path).map_or(0, |p| p.text.len());
    let off = s.doc.para(StoryRef::Body, &path).map_or(0, |p| p.clamp(rng.next() as usize % (len + 1)));
    s.sel = Selection::caret(Pos { story: StoryRef::Body, path: path.clone(), off });
    let r = match rng.next() % 12 {
        9 => s.run("edit.undo", &json!({})),
        10 => s.run("edit.redo", &json!({})),
        11 => s.run("insert.picture", &json!({"data": PNG})),
        0..=2 => {
            let t = ["x", "yz", " w ", "é"][rng.next() as usize % 4];
            s.run("text.insert", &json!({"text": t}))
        }
        3 => s.run("text.newParagraph", &json!({})),
        4 => s.run("text.backspace", &json!({})),
        5 => s.run("text.delete", &json!({})),
        6 => {
            let end = s.doc.para(StoryRef::Body, &path).map_or(off, |p| p.clamp(off + 3));
            s.sel = Selection { anchor: Pos { off, ..s.sel.anchor.clone() }, focus: Pos { story: StoryRef::Body, path, off: end } };
            s.run(["format.bold", "format.italic"][rng.next() as usize % 2], &json!({}))
        }
        7 => s.run("review.newComment", &json!({"text": "c"})),
        _ => s.run("insert.table", &json!({"rows": 2, "cols": 2})),
    };
    let _ = r;
}

/// A 1×1 PNG, base64.
const PNG: &str = "iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAYAAAAfFcSJAAAADUlEQVR42mP8z8DwHwAFBQIAX8jx0gAAAABJRU5ErkJggg==";

/// A small deterministic random number generator (xorshift).
struct Rng(u64);

impl Rng {
    fn next(&mut self) -> u64 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        self.0
    }
    fn shuffle<T>(&mut self, v: &mut [T]) {
        for i in (1..v.len()).rev() {
            v.swap(i, self.next() as usize % (i + 1));
        }
    }
}

#[test]
fn every_member_starts_from_the_same_blank_document() {
    let mut a = peer(1, "Ann");
    // Ann's session started on her region's paper.
    a.0.doc = wordcraft_engine::blank_document(&wordcraft_engine::Region::from_tag("de-DE"));
    at(&mut a.0, 0, 0);
    run(&mut a.0, "text.insert", json!({"text": "Hallo"}));
    let mut b = peer(2, "Bo");
    assert!(b.1.is_empty(), "nobody has started it for Bo yet");
    sync(&mut [&mut a, &mut b]);
    same_doc(&a.0, &b.0);
    assert!(!b.1.is_empty());
    assert_eq!(b.0.doc.last_section.page_w, a.0.doc.last_section.page_w);
    let _ = Document::new();
}

#[test]
fn an_edit_too_big_to_share_is_taken_back() {
    let (mut a, mut b) = (peer(1, "Ann"), peer(2, "Bo"));
    start("Small", &mut [&mut a, &mut b]);
    a.1.max_op = 2_000;
    at(&mut a.0, 0, 5);
    run(&mut a.0, "text.insert", json!({"text": "x".repeat(5_000)}));
    sync(&mut [&mut a, &mut b]);
    assert!(a.1.refused.as_deref().is_some_and(|r| r.contains("too big")));
    assert_eq!(text(&a.0), "Small");
    same_doc(&a.0, &b.0);
}

#[test]
fn an_opened_file_travels_as_its_bytes_and_names_the_document() {
    let (mut a, mut b) = (peer(1, "Ann"), peer(2, "Bo"));
    let file = wordcraft_engine::io::save_bytes_with("x.docx", &Document::from_text("From a file"), None, &Default::default()).unwrap();
    let data = wordcraft_engine::cmd::insert::base64_encode(&file);
    run(&mut a.0, "file.open", json!({"path": "Report.docx", "data": data}));
    sync(&mut [&mut a, &mut b]);
    same_doc(&a.0, &b.0);
    assert_eq!(text(&b.0), "From a file");
    assert_eq!(b.1.file_name().as_deref(), Some("Report.docx"));
    assert!(!a.1.applied.last().unwrap().contains("\"d\""), "not a patch");
}

#[test]
fn only_editing_preferences_travel() {
    let mut a = peer(1, "Ann");
    a.0.prefs.dictionaries_folder = Some("/home/ann/private".into());
    a.0.prefs.autocorrect.replace_text = false;
    a.0.prefs.autocorrect.entries.push(("annsecret".into(), "Ann's private replacement".into()));
    start("Text", &mut [&mut a]);
    let all = a.1.applied.join("\n");
    assert!(!all.contains("/home/ann"), "{all}");
    assert!(!all.contains("annsecret"), "AutoCorrect's own entries stay home: {all}");
    assert!(all.contains("replaceText"), "{all}");
}

#[test]
fn a_patch_lands_in_the_right_block_while_others_edit() {
    let (mut a, mut b) = (peer(1, "Ann"), peer(2, "Bo"));
    start("Top", &mut [&mut a, &mut b]);
    at(&mut a.0, 0, 3);
    run(&mut a.0, "text.newParagraph", json!({}));
    run(&mut a.0, "insert.table", json!({"rows": 1, "cols": 2}));
    sync(&mut [&mut a, &mut b]);
    // Ann pastes her own clipboard into the table's first cell (a patch); Bo types in the first
    // paragraph meanwhile.
    a.0.sel = Selection { anchor: Pos::body(0, 0), focus: Pos::body(0, 3) };
    run(&mut a.0, "edit.copy", json!({}));
    let cell = a.0.doc.para_paths(StoryRef::Body).into_iter().find(|p| p.0.len() == 4).unwrap();
    a.0.sel = Selection::caret(Pos { story: StoryRef::Body, path: cell, off: 0 });
    run(&mut a.0, "edit.paste", json!({}));
    at(&mut b.0, 0, 0);
    run(&mut b.0, "text.insert", json!({"text": "Bo "}));
    sync(&mut [&mut a, &mut b]);
    same_doc(&a.0, &b.0);
    let t = text(&a.0);
    assert!(t.starts_with("Bo Top"), "{t}");
    assert_eq!(t.matches("Top").count(), 2, "the paste is in the table: {t}");
}

#[test]
fn an_edit_in_a_story_someone_deleted_does_nothing() {
    let mut a = Document::from_text("Body");
    a.parts
        .insert(4, wordcraft_doc::Part { kind: wordcraft_doc::PartKind::TextBox, blocks: Document::from_text("Box").body, body: Default::default() });
    let b = Document::from_text("Body");
    let op: Op = serde_json::from_value(json!({
        "i": [3, 0], "k": {}, "c": "text.insert", "p": {"text": "typed in the box"},
        "sel": {"anchor": {"story": {"part": 4}, "path": [0], "off": 3}, "focus": {"story": {"part": 4}, "path": [0], "off": 3}},
    }))
    .unwrap();
    let mut r = Session::new(base());
    let out = replay(&mut r, &op, &[(a, b.clone())], None, b.clone()).0;
    assert!(patch::diff(&b, &out).is_empty(), "{}", out.plain_text(StoryRef::Body));
}

#[test]
fn an_op_that_crashed_before_is_left_out() {
    let (mut a, mut b) = (peer(1, "Ann"), peer(2, "Bo"));
    start("Kept", &mut [&mut a, &mut b]);
    at(&mut a.0, 0, 4);
    run(&mut a.0, "text.insert", json!({"text": " and more"}));
    let u = share(&mut a.0, &mut a.1);
    let bad = a.1.applied.last().map(|j| op_hash(j)).unwrap();
    b.1.skip.insert(bad);
    b.1.remote(&mut b.0, u, LATER);
    assert_eq!(text(&b.0), "Kept");
    assert!(replaying().is_none(), "the marker is cleared after each op");
}

#[test]
fn an_edit_nobody_stored_can_be_stored_by_whoever_has_it() {
    let (mut a, mut b) = (peer(1, "Ann"), peer(2, "Bo"));
    start("Shared", &mut [&mut a, &mut b]);
    // What the messenger stored so far.
    let stored = a.1.encode_since(&yrs::StateVector::default());
    let stored_sv = a.1.state_vector();
    // Ann's next edit reaches Bo only over the realtime channel; Ann leaves before storing it.
    at(&mut a.0, 0, 6);
    run(&mut a.0, "text.insert", json!({"text": " by Ann"}));
    let realtime = share(&mut a.0, &mut a.1);
    b.1.remote(&mut b.0, realtime, LATER);
    assert_ne!(a.1.yjs_client(), b.1.yjs_client());
    // Bo stores the items nobody stored; a member opening later gets the edit.
    let orphans = b.1.encode_since(&stored_sv);
    let mut late = peer(3, "Cy");
    late.1.remote(&mut late.0, vec![stored, orphans], LATER);
    same_doc(&a.0, &late.0);
    assert_eq!(text(&late.0), "Shared by Ann");
}

/// Ann's and Bo's typing at one spot, Bo going on typing before Ann's arrives: whichever order the
/// log gives, each one's words stay together.
#[test]
fn typing_on_at_the_same_spot_keeps_each_ones_words_together() {
    for typed in ["the", "a"] {
        let (mut a, mut b) = (peer(1, "Ann"), peer(2, "Bo"));
        start("[]", &mut [&mut a, &mut b]);
        at(&mut a.0, 0, 1);
        run(&mut a.0, "text.insert", json!({"text": typed}));
        let ann = share(&mut a.0, &mut a.1);
        at(&mut b.0, 0, 1);
        run(&mut b.0, "text.insert", json!({"text": typed}));
        let bo1 = share(&mut b.0, &mut b.1);
        run(&mut b.0, "text.insert", json!({"text": " end"}));
        let bo2 = share(&mut b.0, &mut b.1);
        for (s, c) in [&mut a, &mut b] {
            c.remote(s, [ann.clone(), bo1.clone(), bo2.clone()].concat(), LATER);
        }
        same_doc(&a.0, &b.0);
        let t = text(&a.0);
        let ok = [format!("[{typed}{typed} end]"), format!("[{typed} end{typed}]")];
        assert!(ok.contains(&t), "{typed}: {t}");
        // Bo's caret is still after his own words.
        run(&mut b.0, "text.insert", json!({"text": "!"}));
        assert!(text(&b.0).contains(&format!("{typed} end!")), "{}", text(&b.0));
    }
}

/// Ann and Bo each type two words at one spot, each seeing the other's first word before their
/// second: the log interleaves them, and still each one's words stay in order and together.
#[test]
fn interleaved_typing_at_one_spot_stays_in_each_ones_order() {
    let (mut a, mut b) = (peer(1, "Ann"), peer(2, "Bo"));
    start("[]", &mut [&mut a, &mut b]);
    at(&mut a.0, 0, 1);
    run(&mut a.0, "text.insert", json!({"text": "Hello"}));
    let a1 = share(&mut a.0, &mut a.1);
    at(&mut b.0, 0, 1);
    run(&mut b.0, "text.insert", json!({"text": "Foo"}));
    let b1 = share(&mut b.0, &mut b.1);
    // Each sees the other's first word, then types the second before anything else arrives.
    a.1.remote(&mut a.0, b1.clone(), LATER);
    b.1.remote(&mut b.0, a1.clone(), LATER);
    run(&mut a.0, "text.insert", json!({"text": " world"}));
    let a2 = share(&mut a.0, &mut a.1);
    run(&mut b.0, "text.insert", json!({"text": " bar"}));
    let b2 = share(&mut b.0, &mut b.1);
    a.1.remote(&mut a.0, b2, LATER);
    b.1.remote(&mut b.0, a2, LATER);
    same_doc(&a.0, &b.0);
    let t = text(&a.0);
    assert!(t.contains("Hello world") && t.contains("Foo bar"), "{t}");
}

#[test]
#[ignore = "timing"]
fn timing_a_late_joiner_replays_a_long_log() {
    let mut a = peer(1, "Ann");
    let body: Vec<String> = (0..200).map(|i| format!("Paragraph {i} with some words in it to make it longer than a line.")).collect();
    start(&body.join("\n"), &mut [&mut a]);
    let mut all = vec![];
    for i in 0..2000 {
        at(&mut a.0, (i * 7) % 200, 5);
        run(&mut a.0, "text.insert", json!({"text": "xy"}));
        all.extend(share(&mut a.0, &mut a.1));
    }
    let t = std::time::Instant::now();
    let mut late = peer(3, "Cy");
    late.1.remote(&mut late.0, vec![a.1.encode_since(&yrs::StateVector::default())], LATER);
    eprintln!("replayed {} ops in {:?}", a.1.applied.len(), t.elapsed());
    same_doc(&a.0, &late.0);
}

/// Ann puts a table at the start of a paragraph while Bo types there: whichever order the log
/// gives, Bo's text stays in the paragraph, not in the table.
#[test]
fn a_table_put_where_someone_types_doesnt_take_the_typing() {
    for _ in 0..8 {
        let (mut a, mut b) = (peer(1, "Ann"), peer(2, "Bo"));
        start("abc\ndef", &mut [&mut a, &mut b]);
        at(&mut a.0, 1, 0);
        run(&mut a.0, "insert.table", json!({"rows": 1, "cols": 1}));
        at(&mut b.0, 1, 0);
        run(&mut b.0, "text.insert", json!({"text": "X"}));
        sync(&mut [&mut a, &mut b]);
        same_doc(&a.0, &b.0);
        assert!(text(&a.0).ends_with("Xdef"), "{:?}", text(&a.0));
    }
}

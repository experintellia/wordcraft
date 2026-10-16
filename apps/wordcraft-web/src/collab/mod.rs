//! Shared editing for the webxdc build: everyone in a chat edits one document.
//!
//! Each change to the document becomes an op appended to a Yjs array (`yrs`). Members exchange the
//! array's updates through the messenger; Yjs gives every member the same order of ops, and every
//! member's document is that order replayed from a blank document on a replica session (so the
//! member's own selection, view and clipboard never leak into it). When ops land before ops
//! already replayed here, the replica rolls back to a cached state and replays from there.
//!
//! An op is a command with what it ran with: the selection, the formatting waiting at the caret,
//! the author, the clock and the preferences that differ from the defaults (AutoCorrect…). Its
//! author checks that replaying it gives the same document; when it doesn't (it read the
//! clipboard, the view, the format painter…) the op carries the change instead, as a
//! [`patch::Patch`] (the changed paragraphs or tables and fields). Opening a file is a command
//! with the file's bytes.
//!
//! **Concurrent edits merge.** An op records which ops its author had seen (per member, how many).
//! Replaying it, its positions are carried through the ops that come before it but its author
//! hadn't seen ([`rebase`]), so two members typing in the same paragraph both land where they
//! meant; at the same spot the log order decides. Parts (headers, notes, text boxes) and comments
//! made concurrently get other ids in the final order, and ids an op names are moved past them.
//! All of it is deterministic, so every member ends with the same document.
//!
//! **Undo takes back only your own changes**: each of this member's steps keeps the document before
//! and after it; Undo finds what the step left in the document as it is now and puts back what was
//! there before, refused when someone changed one of the same paragraphs, tables or fields since.

pub mod patch;
pub mod rebase;

use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;

use serde::{Deserialize, Serialize};
use serde_json::Value as Json;
use wordcraft_doc::props::CharProps;
use wordcraft_doc::{Document, Path, Pos, StoryRef};
use wordcraft_engine::{Edit, Prefs, Selection, Session};
use yrs::updates::decoder::Decode as _;
use yrs::{Any, Array as _, ArrayRef, Doc, Out, ReadTxn as _, Transact as _, Update};

use patch::Patch;

/// Ops further back than this are taken as seen by every later op.
// ponytail: an op made after this many unseen ops (a long offline session) lands unrebased;
// raise it, or rebase through a checkpoint, if long offline edits get common.
const WINDOW: usize = 512;
/// Cached states: after each of the last `RECENT` ops, and every `CHECKPOINT_EVERY` ops (the last
/// `CHECKPOINTS`); anything else is replayed from the nearest one before it.
const RECENT: usize = 64;
const CHECKPOINT_EVERY: usize = 32;
const CHECKPOINTS: usize = 64;
/// Typing is held back this long (ms) and shared as one op.
const TYPING_HOLD_MS: f64 = 700.0;
/// Deepest position path a shared op may carry.
const MAX_PATH: usize = 64;
/// Most steps Undo Actions takes back at once.
const MAX_UNDO_STEPS: usize = 100;

/// Who made an op: (this member and device's client id, how many ops it made before).
type OpId = (u64, u32);

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Op {
    i: OpId,
    /// The ops its author had seen: per client, how many.
    #[serde(default)]
    k: BTreeMap<u64, u32>,
    /// The command (or, with `d`, a label).
    c: String,
    #[serde(default, skip_serializing_if = "Json::is_null")]
    p: Json,
    #[serde(default)]
    sel: Selection,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pp: Option<CharProps>,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    a: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    ai: String,
    #[serde(default)]
    t: u64,
    /// Preferences that differ from the defaults.
    #[serde(default, skip_serializing_if = "Json::is_null")]
    o: Json,
    /// The change, for an op that can't run again elsewhere.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    d: Option<Patch>,
}

/// One of this member's changes, for their undo and redo.
struct Step {
    label: String,
    before: Document,
    after: Document,
}

/// Typing held back to share as one op.
struct Held {
    first: Edit,
    text: String,
    after: Document,
    since: f64,
}

pub struct Collab {
    ydoc: Doc,
    ops: ArrayRef,
    /// The ops replayed so far, in order, and their ids.
    applied: Vec<Arc<str>>,
    ids: Vec<OpId>,
    /// Per client, how many of its ops are replayed.
    counts: BTreeMap<u64, u32>,
    /// The document after n ops.
    states: BTreeMap<usize, Document>,
    /// Runs the ops: default view, empty clipboard, no history.
    replica: Session,
    /// This member and device.
    client: u64,
    /// The shared document as this member's session should hold it.
    expected: Document,
    held: Option<Held>,
    undo: Vec<Step>,
    redo: Vec<Step>,
    /// Why the last Undo or Redo, or an edit too big to share, was refused, for the status bar.
    pub refused: Option<String>,
    /// Largest op (JSON bytes) the messenger can carry; a bigger edit is taken back.
    pub max_op: usize,
    /// An edit was refused this frame: the session goes back to the shared document.
    revert: bool,
}

/// The document every member starts from.
pub fn base() -> Document {
    Document::new()
}

impl Collab {
    /// Records `s`'s edits from now on. `client` identifies this member and device across runs.
    pub fn new(s: &mut Session, client: u64) -> Collab {
        let ydoc = Doc::new();
        let ops = ydoc.get_or_insert_array("ops");
        s.journal = Some(vec![]);
        s.host_history = Some((vec![], vec![]));
        let mut replica = Session::new(base());
        replica.author = String::new();
        Collab {
            ydoc,
            ops,
            applied: vec![],
            ids: vec![],
            counts: BTreeMap::new(),
            states: [(0, base())].into(),
            replica,
            client,
            expected: base(),
            held: None,
            undo: vec![],
            redo: vec![],
            refused: None,
            max_op: usize::MAX,
            revert: false,
        }
    }

    /// The name of the file last opened into the shared document, if any.
    pub fn file_name(&self) -> Option<String> {
        self.applied.iter().rev().filter(|j| j.contains("\"c\":\"file.open\"")).find_map(|j| {
            let op: Op = serde_json::from_str(j).ok()?;
            let path = op.p.get("path")?.as_str()?;
            std::path::Path::new(path).file_name().map(|n| n.to_string_lossy().to_string())
        })
    }

    /// Whether nobody has started the shared document: no ops yet.
    pub fn is_empty(&self) -> bool {
        self.applied.is_empty()
    }

    /// The edits made in `s` since the last call (and typing held long enough), as an update for
    /// the others. `now` is a clock in milliseconds.
    pub fn local(&mut self, s: &mut Session, now: f64) -> Option<Vec<u8>> {
        let edits = s.journal.as_mut().map(std::mem::take).unwrap_or_default();
        let mut ops: Vec<Op> = vec![];
        for e in edits {
            if matches!(e.id.as_str(), "edit.undo" | "edit.redo" | "edit.undoTo") {
                self.flush(s, &mut ops);
                self.undo_redo(s, &e, &mut ops);
                continue;
            }
            // An edit made on a document that has changed since (an Undo earlier in the same
            // frame): stale. What the session holds is shared below as it is.
            // (The chat's first edit brings its own start: the member's blank page.)
            let current = self.held.as_ref().map_or(&self.expected, |h| &h.after);
            let first = self.applied.is_empty() && self.held.is_none() && ops.is_empty();
            if !first && !patch::diff(current, &e.before).is_empty() {
                log::info!("dropped a stale `{}`", e.id);
                continue;
            }
            if e.id == "text.insert"
                && let Some(text) = e.params.get("text").and_then(Json::as_str)
                && e.params.as_object().is_some_and(|p| p.len() == 1)
            {
                // Only typing that goes on where it stopped.
                if let Some(h) = &mut self.held
                    && e.sel == h.first.sel_after
                    && patch::diff(&h.after, &e.before).is_empty()
                {
                    h.text.push_str(text);
                    h.after = e.after;
                    h.first.sel_after = e.sel_after;
                    continue;
                }
                self.flush(s, &mut ops);
                let text = text.to_string();
                let after = e.after.clone();
                self.held = Some(Held { first: e, text, after, since: now });
                continue;
            }
            self.flush(s, &mut ops);
            self.record(s, e, &mut ops);
        }
        if self.held.as_ref().is_some_and(|h| now - h.since >= TYPING_HOLD_MS) {
            self.flush(s, &mut ops);
        }
        if std::mem::take(&mut self.revert) {
            self.held = None;
            s.doc = self.expected.clone();
            s.touch();
            s.clamp_selection();
        }
        // A change that came some other way than a command: share it as it is.
        let current = self.held.as_ref().map_or(&self.expected, |h| &h.after);
        if (!self.applied.is_empty() || !ops.is_empty()) && !patch::diff(current, &s.doc).is_empty() {
            self.flush(s, &mut ops);
            let before = self.expected.clone();
            let e = Edit {
                id: "edit".into(),
                params: Json::Null,
                sel: s.sel.clone(),
                pending: None,
                time: wordcraft_engine::cmd::now_unix(),
                before,
                after: s.doc.clone(),
                sel_after: s.sel.clone(),
            };
            self.record(s, e, &mut ops);
        }
        self.mirror(s);
        if ops.is_empty() {
            return None;
        }
        let jsons: Vec<Arc<str>> = ops.iter().filter_map(|op| serde_json::to_string(op).ok()).map(Arc::from).collect();
        Some(self.append(&jsons))
    }

    /// Applies the others' updates (and this member's own, which change nothing): `s` gets the
    /// shared document, its selection carried along.
    pub fn remote(&mut self, s: &mut Session, updates: Vec<Vec<u8>>, now: f64) -> Option<Vec<u8>> {
        // Held typing goes first: the shared document replaces the session's.
        let mine = self.held.is_some().then(|| self.local(s, now + TYPING_HOLD_MS)).flatten();
        {
            let mut txn = self.ydoc.transact_mut();
            for bytes in updates {
                match Update::decode_v1(&bytes) {
                    Ok(update) => {
                        if let Err(e) = txn.apply_update(update) {
                            log::warn!("ignored a shared update: {e}");
                        }
                    }
                    Err(e) => log::warn!("ignored an unreadable shared update: {e}"),
                }
            }
        }
        self.reconcile(s);
        mine
    }

    /// Shares typing held back, as one op (when replaying it gives the same document) or a patch.
    fn flush(&mut self, s: &mut Session, ops: &mut Vec<Op>) {
        let Some(h) = self.held.take() else { return };
        let e = Edit { params: serde_json::json!({"text": h.text}), after: h.after, ..h.first };
        self.record(s, e, ops);
    }

    /// Turns one edit into an op, applied here at once.
    fn record(&mut self, s: &mut Session, e: Edit, ops: &mut Vec<Op>) {
        // The first op of the chat also brings the document it started from (its paper).
        if self.applied.is_empty() && ops.is_empty() {
            let start = patch::diff(&base(), &e.before);
            if !start.is_empty() {
                let op = self.op("start", Json::Null, Selection::default(), None, s, e.time, Some(start));
                self.commit(op, base(), e.before.clone(), None, ops);
            }
        }
        let label = if e.id == "text.insert" { "Typing" } else { s.registry.get(&e.id).map_or("Edit", |c| c.label) };
        let label = label.trim_end_matches('…').to_string();
        let mut op = self.op(&e.id, e.params.clone(), e.sel.clone(), e.pending.clone(), s, e.time, None);
        let same = replayable(&op.c, &op.p) && patch::diff(&replay(&mut self.replica, &op, &[], e.before.clone()), &e.after).is_empty();
        if !same {
            let d = patch::diff(&e.before, &e.after);
            if d.is_empty() {
                return;
            }
            op = Op { c: label.clone(), p: Json::Null, d: Some(d), ..op };
        } else if patch::diff(&e.before, &e.after).is_empty() {
            return;
        }
        self.commit(op, e.before, e.after, Some(label), ops);
    }

    fn op(&self, c: &str, p: Json, sel: Selection, pp: Option<CharProps>, s: &Session, t: u64, d: Option<Patch>) -> Op {
        let n = self.counts.get(&self.client).copied().unwrap_or(0);
        Op {
            i: (self.client, n),
            k: self.counts.clone(),
            c: c.into(),
            p,
            sel,
            pp,
            a: s.author.clone(),
            ai: s.initials.clone(),
            t,
            o: prefs_diff(&s.prefs),
            d,
        }
    }

    /// Adds an op made here: `after` is what it left. An op too big to share is dropped, and the
    /// session goes back to the shared document (see `local`).
    fn commit(&mut self, op: Op, before: Document, after: Document, step: Option<String>, ops: &mut Vec<Op>) {
        let Ok(json) = serde_json::to_string(&op) else { return };
        if json.len() > self.max_op {
            self.refused = Some(format!("This change is too big to share in the chat ({} KB).", json.len() / 1000));
            self.revert = true;
            return;
        }
        if let Some(label) = step {
            push(&mut self.undo, Step { label, before, after: after.clone() });
            self.redo.clear();
        }
        self.applied.push(json.into());
        self.ids.push(op.i);
        *self.counts.entry(op.i.0).or_default() += 1;
        self.states.insert(self.applied.len(), after.clone());
        self.prune();
        self.expected = after;
        ops.push(op);
    }

    /// Undo or Redo: this member's own steps, found in the document as it is now.
    fn undo_redo(&mut self, s: &mut Session, e: &Edit, ops: &mut Vec<Op>) {
        let redo = e.id == "edit.redo";
        let count = if e.id == "edit.undoTo" { e.params.get("count").and_then(Json::as_u64).unwrap_or(1) as usize } else { 1 };
        self.refused = None;
        for _ in 0..count.clamp(1, MAX_UNDO_STEPS) {
            let (from, to) = if redo { (&mut self.redo, &mut self.undo) } else { (&mut self.undo, &mut self.redo) };
            let Some(step) = from.pop() else { break };
            let (old, new) = if redo { (&step.before, &step.after) } else { (&step.after, &step.before) };
            let now = self.expected.clone();
            let d = locate(&patch::diff(old, new), old, &now);
            push(to, step);
            let Some(d) = d else {
                self.refused = Some(
                    if redo { "Someone changed this since; it can't be redone." } else { "Someone changed this since; it can't be undone." }.into(),
                );
                continue;
            };
            let op = self.op(&e.id, Json::Null, e.sel.clone(), None, s, e.time, Some(d));
            let after = replay(&mut self.replica, &op, &[], now.clone());
            if !patch::diff(&now, &after).is_empty() {
                self.commit(op, now, after.clone(), None, ops);
                s.doc = after;
                s.touch();
                s.clamp_selection();
            }
        }
    }

    fn append(&mut self, ops: &[Arc<str>]) -> Vec<u8> {
        let mut txn = self.ydoc.transact_mut();
        let sv = txn.state_vector();
        for op in ops {
            self.ops.push_back(&mut txn, op.as_ref());
        }
        txn.encode_state_as_update_v1(&sv)
    }

    /// Brings the shared document in line with the ops' order and hands it to `s`.
    fn reconcile(&mut self, s: &mut Session) {
        let log: Vec<Arc<str>> = {
            let txn = self.ydoc.transact();
            self.ops.iter(&txn).filter_map(|v| if let Out::Any(Any::String(op)) = v { Some(op) } else { None }).collect()
        };
        let same = self.applied.iter().zip(&log).take_while(|(a, b)| a == b).count();
        if same == self.applied.len() && same == log.len() {
            return;
        }
        self.applied.truncate(same);
        self.ids.truncate(same);
        self.states.retain(|n, _| *n <= same);
        self.counts.clear();
        for (c, _) in &self.ids {
            *self.counts.entry(*c).or_default() += 1;
        }
        for json in log.get(same..).unwrap_or_default() {
            let op = serde_json::from_str::<Op>(json).ok();
            let id = op.as_ref().map_or((0, u32::MAX), |op| op.i);
            let n = self.applied.len();
            self.applied.push(json.clone());
            self.ids.push(id);
            *self.counts.entry(id.0).or_default() += 1;
            let doc = self.state_at(n);
            let next = match op {
                Some(op) => self.step(n, &op, doc),
                None => {
                    log::warn!("skipped an unreadable shared op");
                    doc
                }
            };
            self.states.insert(n + 1, next);
            self.prune();
        }
        let new = self.state_at(self.applied.len());
        let old = std::mem::replace(&mut s.doc, new.clone());
        let keep = |p: &Pos| rebase::map_pos(&old, &new, p, &|id| id);
        s.sel = Selection { anchor: keep(&s.sel.anchor), focus: keep(&s.sel.focus) };
        s.touch();
        s.clamp_selection();
        self.expected = new;
        self.mirror(s);
    }

    /// The document after `n` ops: cached, or replayed from the nearest cached state before it.
    fn state_at(&mut self, n: usize) -> Document {
        let (from, mut doc) = match self.states.range(..=n).next_back() {
            Some((k, d)) => (*k, d.clone()),
            None => (0, base()),
        };
        for j in from..n.min(self.applied.len()) {
            doc = match self.applied.get(j).and_then(|json| serde_json::from_str::<Op>(json).ok()) {
                Some(op) => self.step(j, &op, doc),
                None => doc,
            };
        }
        doc
    }

    /// Replays op `j` on `doc`, carrying its positions through the ops before it that its author
    /// hadn't seen.
    fn step(&mut self, j: usize, op: &Op, doc: Document) -> Document {
        let unseen: Vec<usize> =
            (j.saturating_sub(WINDOW)..j).filter(|&i| self.ids.get(i).is_some_and(|(c, n)| op.k.get(c).is_none_or(|seen| *n >= *seen))).collect();
        let pairs: Vec<(Document, Document)> = unseen.iter().map(|&i| (self.state_at(i), self.state_at(i + 1))).collect();
        replay(&mut self.replica, op, &pairs, doc)
    }

    /// Drops cached states no longer needed: the last `RECENT`, and the last `CHECKPOINTS`
    /// checkpoints, stay (and the blank start).
    fn prune(&mut self) {
        let n = self.applied.len();
        let oldest_checkpoint = (n / CHECKPOINT_EVERY).saturating_sub(CHECKPOINTS) * CHECKPOINT_EVERY;
        self.states.retain(|k, _| *k == 0 || *k + RECENT >= n || (k.is_multiple_of(CHECKPOINT_EVERY) && *k >= oldest_checkpoint));
    }

    /// This member's own steps as the session's undo and redo lists.
    fn mirror(&self, s: &mut Session) {
        let labels = |steps: &[Step]| steps.iter().map(|st| st.label.clone()).collect::<Vec<_>>();
        s.host_history = Some((labels(&self.undo), labels(&self.redo)));
    }
}

fn push(steps: &mut Vec<Step>, step: Step) {
    steps.push(step);
    if steps.len() > MAX_UNDO_STEPS {
        steps.remove(0);
    }
}

/// Commands an op may run elsewhere; any other change travels as a patch. Nothing that reads or
/// writes files, runs recorded macros or names a path.
pub fn replayable(c: &str, p: &Json) -> bool {
    let file = c.starts_with("file.") && c != "file.open";
    // An opened file travels as its bytes; its `path` only names it.
    let path = if c == "file.open" { p.get("data").is_none() } else { p.get("path").is_some() };
    !file && !path && !matches!(c, "tools.macros" | "tools.recordMacro" | "edit.repeat")
}

/// The preferences that change what typing and editing do; the others (the view, proofing, local
/// folders) never leave this member.
const EDITING_PREFS: &[&str] = &["autocorrect", "typingReplacesSelection", "overtype", "detectLanguage"];

/// The editing preferences that differ from the defaults, as an object (`null` when none do).
fn prefs_diff(p: &Prefs) -> Json {
    let (Ok(Json::Object(now)), Ok(Json::Object(def))) = (serde_json::to_value(p), serde_json::to_value(Prefs::default())) else { return Json::Null };
    let diff: serde_json::Map<String, Json> = now.into_iter().filter(|(k, v)| EDITING_PREFS.contains(&k.as_str()) && def.get(k) != Some(v)).collect();
    if diff.is_empty() { Json::Null } else { Json::Object(diff) }
}

fn prefs_from(o: &Json) -> Prefs {
    let Some(o) = o.as_object() else { return Prefs::default() };
    let Ok(Json::Object(mut def)) = serde_json::to_value(Prefs::default()) else { return Prefs::default() };
    def.extend(o.iter().filter(|(k, _)| EDITING_PREFS.contains(&k.as_str())).map(|(k, v)| (k.clone(), v.clone())));
    serde_json::from_value(Json::Object(def)).unwrap_or_default()
}

/// Ids of parts and comments the unseen ops made.
fn made(pairs: &[(Document, Document)]) -> (BTreeSet<u32>, BTreeSet<u32>) {
    let (mut parts, mut comments) = (BTreeSet::new(), BTreeSet::new());
    for (a, b) in pairs {
        parts.extend(b.parts.keys().filter(|k| !a.parts.contains_key(k)));
        comments.extend(b.comments.keys().filter(|k| !a.comments.contains_key(k)));
    }
    (parts, comments)
}

/// Where an id its author picked lands once ids others made concurrently are taken: past each of
/// them (ids are given out as the largest plus one).
fn bump(id: u32, taken: &BTreeSet<u32>) -> u32 {
    taken.iter().fold(id, |id, t| if *t <= id { id.saturating_add(1) } else { id })
}

/// Runs `op` on `doc` as its author ran it, on the replica; `pairs` are the documents before and
/// after each op its author hadn't seen. An op that can't run leaves `doc` as it was.
fn replay(r: &mut Session, op: &Op, pairs: &[(Document, Document)], doc: Document) -> Document {
    let (parts, comments) = made(pairs);
    let story = |s: StoryRef| match s {
        StoryRef::Part(id) => StoryRef::Part(bump(id, &parts)),
        body => body,
    };
    let map = |p: &Pos| {
        let p = Pos { story: story(p.story), ..p.clone() };
        pairs.iter().fold(p, |p, (a, b)| rebase::map_pos(a, b, &p, &|id| id))
    };
    // Hostile input: paths of a sane depth only.
    if op.sel.anchor.path.0.len() > MAX_PATH || op.sel.focus.path.0.len() > MAX_PATH {
        log::warn!("skipped a shared `{}` with a bad position", op.c);
        return doc;
    }
    if let Some(d) = &op.d {
        let at = |s: StoryRef, i: usize| {
            let p = map(&Pos { story: s, path: Path::top(i), off: 0 });
            (p.story, p.path.0.first().map_or(i, |i| *i as usize))
        };
        return match patch::apply(&doc, d, &at) {
            Some(out) => out,
            None => {
                log::info!("a shared `{}` did nothing: the same things changed since", op.c);
                doc
            }
        };
    }
    let spec_ok = r.registry.get(&op.c).is_some_and(|s| s.mutates) || matches!(op.c.as_str(), "file.open");
    if !spec_ok || !replayable(&op.c, &op.p) {
        log::warn!("skipped a shared `{}` this app can't run", op.c);
        return doc;
    }
    let mut p = op.p.clone();
    if op.c.starts_with("review.")
        && let Some(id) = p.get("id").and_then(Json::as_u64).and_then(|id| u32::try_from(id).ok())
        && let Some(o) = p.as_object_mut()
    {
        o.insert("id".into(), bump(id, &comments).into());
    }
    r.doc = doc;
    r.touch();
    r.sel = Selection { anchor: r.doc.clamp(&map(&op.sel.anchor)), focus: r.doc.clamp(&map(&op.sel.focus)) };
    r.pending = op.pp.clone();
    r.author = op.a.clone();
    r.initials = op.ai.clone();
    r.prefs = prefs_from(&op.o);
    r.clipboard = None;
    r.painter = None;
    r.math = None;
    r.column = None;
    r.also_selected.clear();
    r.view = Default::default();
    wordcraft_engine::cmd::set_clock(Some(op.t));
    if let Err(e) = r.run(&op.c, &p) {
        log::info!("shared `{}` did nothing here: {e}", op.c);
    }
    wordcraft_engine::cmd::set_clock(None);
    r.reset_history();
    r.ui_requests.clear();
    r.doc.clone()
}

/// `d` (made from `old`) with its block runs found in `now` instead: by pointer or content, the
/// occurrence nearest where they were in `old`. `None` when one of them isn't there any more.
fn locate(d: &Patch, old: &Document, now: &Document) -> Option<Patch> {
    let mut out = d.clone();
    for s in &mut out.b {
        let st = patch::story(s.s);
        let (o, n) = (old.story(st)?, now.story(st)?);
        let at = if s.n > 0 {
            let run = o.get(s.at..s.at + s.n)?;
            nearest(n, s.at, |i| n.get(i..i + run.len()).is_some_and(|w| w.iter().zip(run).all(|(x, y)| Arc::ptr_eq(x, y) || x == y)))?
        } else if s.at == 0 {
            0
        } else {
            // Blocks inserted: after the block they followed.
            let prev = o.get(s.at - 1)?;
            nearest(n, s.at - 1, |i| n.get(i).is_some_and(|x| Arc::ptr_eq(x, prev) || x == prev))? + 1
        };
        s.at = at;
    }
    Some(out)
}

/// The index nearest `around` (searching outwards) where `hit` holds.
fn nearest(blocks: &[Arc<wordcraft_doc::Block>], around: usize, hit: impl Fn(usize) -> bool) -> Option<usize> {
    (0..=blocks.len()).flat_map(|d| [around.checked_add(d), around.checked_sub(d).filter(|_| d > 0)]).flatten().find(|&i| i <= blocks.len() && hit(i))
}

#[cfg(test)]
mod tests;

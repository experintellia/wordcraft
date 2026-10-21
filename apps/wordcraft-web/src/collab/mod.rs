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
//! **Concurrent edits merge.** Every character has an identity ([`ids`]), and an op's selection
//! travels as the characters just before its ends: replayed, it lands just after them, whatever
//! its author hadn't seen, so two members typing at the same spot each keep their words together.
//! When such a character is gone, its positions are carried through the ops that come before it
//! but its author hadn't seen ([`rebase`]; an op records how many ops of each member its author
//! had seen). Parts (headers, notes, text boxes) and comments
//! made concurrently get other ids in the final order, and ids an op names are moved past them.
//! All of it is deterministic, so every member ends with the same document.
//!
//! **Undo takes back only your own changes**: each of this member's steps keeps the document before
//! and after it; Undo finds what the step left in the document as it is now and puts back what was
//! there before, refused when someone changed one of the same paragraphs, tables or fields since.

pub mod ids;
pub mod patch;
pub mod rebase;

use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;

use serde::{Deserialize, Serialize};
use serde_json::Value as Json;
use wordcraft_doc::edit::Fragment;
use wordcraft_doc::props::{CharProps, ParaProps};
use wordcraft_doc::{Document, Pos, StoryRef};
use wordcraft_engine::{Edit, Prefs, Selection, Session};
use yrs::updates::decoder::Decode as _;
use yrs::{Any, Array as _, ArrayRef, Doc, Out, ReadTxn as _, Transact as _, Update};

use ids::Ids;
use patch::Patch;

/// Ops further back than this are taken as seen by every later op.
// ponytail: an op made after this many unseen ops (a long offline session) lands unrebased;
// raise it (and `RECENT`), or rebase through a checkpoint, if long offline edits get common.
const WINDOW: usize = 256;
/// Cached states: after each of the last `RECENT` ops (every state an op's rebase can need), and
/// every `CHECKPOINT_EVERY` ops (the last `CHECKPOINTS`); anything else is replayed from the
/// nearest one before it.
const RECENT: usize = WINDOW + 1;
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
    /// What a paste read from its author's clipboard, and the formatting Paste Formatting
    /// applied: an op that reads them runs again elsewhere (and merges like typing).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    cb: Option<Fragment>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    fp: Option<(CharProps, ParaProps, bool)>,
    /// The selection's anchor and focus as the characters just before them ([`ids`]).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    y: Option<[Option<ids::Id>; 2]>,
    /// A range's first character: its start stays just before it, so what others type in front
    /// of the range isn't taken into it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    ys: Option<ids::Id>,
}

/// The document after some ops, and its characters' ids.
#[derive(Clone)]
struct State {
    doc: Document,
    ids: Ids,
}

impl State {
    fn base() -> State {
        let doc = base();
        let ids = Ids::fresh(&doc, (0, 0));
        State { doc, ids }
    }

    /// The state `op` left, `doc`; `sel` is where it ran.
    fn next(&self, op: &Op, doc: Document, sel: Option<&Selection>) -> State {
        let ids = self.ids.next(&self.doc, &doc, sel, op.i);
        State { doc, ids }
    }
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
    states: BTreeMap<usize, State>,
    /// Runs the ops, renewed for each one ([`Session::renew`]).
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
    /// Ops ([`op_hash`]) that crashed this app while replaying (a panic aborts on the web): left
    /// out, so one bad op can't stop the chat's document from opening. Every member crashes on
    /// such an op once and leaves it out from then on, so they still agree.
    pub skip: BTreeSet<u64>,
}

thread_local! {
    static REPLAYING: std::cell::Cell<Option<u64>> = const { std::cell::Cell::new(None) };
}

/// The op being replayed ([`op_hash`]), for a panic hook to remember it.
pub fn replaying() -> Option<u64> {
    REPLAYING.with(std::cell::Cell::get)
}

/// An op's identity for [`Collab::skip`].
pub fn op_hash(json: &str) -> u64 {
    patch::hash(json)
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
            states: [(0, State::base())].into(),
            replica,
            client,
            expected: base(),
            held: None,
            undo: vec![],
            redo: vec![],
            refused: None,
            max_op: usize::MAX,
            revert: false,
            skip: BTreeSet::new(),
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

    /// The edits made in `s` since the last call (and typing held long enough), as updates for
    /// the others, one per op (each fits `max_op`). `now` is a clock in milliseconds.
    pub fn local(&mut self, s: &mut Session, now: f64) -> Vec<Vec<u8>> {
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
                clipboard: None,
                painter: None,
                time: wordcraft_engine::cmd::now_unix(),
                before,
                after: s.doc.clone(),
                sel_after: s.sel.clone(),
            };
            self.record(s, e, &mut ops);
        }
        self.mirror(s);
        ops.iter().filter_map(|op| serde_json::to_string(op).ok()).map(|json| self.append(&json)).collect()
    }

    /// Applies the others' updates (and this member's own, which change nothing): `s` gets the
    /// shared document, its selection carried along.
    pub fn remote(&mut self, s: &mut Session, updates: Vec<Vec<u8>>, now: f64) -> Vec<Vec<u8>> {
        // Held typing goes first: the shared document replaces the session's.
        let mine = if self.held.is_some() { self.local(s, now + TYPING_HOLD_MS) } else { vec![] };
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
        op.cb = e.clipboard.clone();
        op.fp = e.painter.clone();
        let same = replayable(&op.c, &op.p) && patch::diff(&replay(&mut self.replica, &op, &[], None, e.before.clone()).0, &e.after).is_empty();
        if !same {
            let d = patch::diff(&e.before, &e.after);
            if d.is_empty() {
                return;
            }
            op = Op { c: label.clone(), p: Json::Null, d: Some(d), cb: None, fp: None, ..op };
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
            pp,
            a: s.author.clone(),
            ai: s.initials.clone(),
            t,
            o: prefs_diff(&s.prefs),
            y: self.states.get(&self.applied.len()).and_then(|st| Some([st.ids.anchor(&st.doc, &sel.anchor)?, st.ids.anchor(&st.doc, &sel.focus)?])),
            ys: (sel.anchor != sel.focus).then(|| self.states.get(&self.applied.len()).and_then(|st| st.ids.at(&st.doc, &sel.ordered().0))).flatten(),
            sel,
            d,
            cb: None,
            fp: None,
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
        let prev = self.state_at(self.applied.len());
        let sel = op.d.is_none().then(|| Selection { anchor: before.clamp(&op.sel.anchor), focus: before.clamp(&op.sel.focus) });
        if let Some(label) = step {
            push(&mut self.undo, Step { label, before, after: after.clone() });
            self.redo.clear();
        }
        self.applied.push(json.into());
        self.ids.push(op.i);
        *self.counts.entry(op.i.0).or_default() += 1;
        self.states.insert(self.applied.len(), prev.next(&op, after.clone(), sel.as_ref()));
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
            let after = replay(&mut self.replica, &op, &[], None, now.clone()).0;
            if !patch::diff(&now, &after).is_empty() {
                self.commit(op, now, after.clone(), None, ops);
                s.doc = after;
                s.touch();
                s.clamp_selection();
            }
        }
    }

    fn append(&mut self, op: &str) -> Vec<u8> {
        let mut txn = self.ydoc.transact_mut();
        let sv = txn.state_vector();
        self.ops.push_back(&mut txn, op);
        txn.encode_state_as_update_v1(&sv)
    }

    /// What this app's Yjs document holds, per Yjs client.
    pub fn state_vector(&self) -> yrs::StateVector {
        self.ydoc.transact().state_vector()
    }

    /// This run's Yjs client id (its items are the ones `local` returns).
    pub fn yjs_client(&self) -> yrs::ClientID {
        self.ydoc.client_id()
    }

    /// Everything this app holds that `sv` doesn't, as one update.
    pub fn encode_since(&self, sv: &yrs::StateVector) -> Vec<u8> {
        self.ydoc.transact().encode_state_as_update_v1(sv)
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
        let old_state = self.states.get(&self.applied.len()).cloned();
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
            let state = self.state_at(n);
            let next = match op {
                Some(op) => self.step(n, &op, state),
                None => {
                    log::warn!("skipped an unreadable shared op");
                    state
                }
            };
            self.states.insert(n + 1, next);
            self.prune();
        }
        let state = self.state_at(self.applied.len());
        let new = state.doc.clone();
        let old = std::mem::replace(&mut s.doc, new.clone());
        // The selection stays after the character it was after; else where the text says.
        let old_ids = old_state.filter(|st| patch::diff(&st.doc, &old).is_empty()).map(|st| st.ids);
        let keep = |p: &Pos| {
            old_ids
                .as_ref()
                .and_then(|ids| state.ids.find(&new, p.story, ids.anchor(&old, p)?))
                .or_else(|| rebase::map_pos(&old, &new, p, &|id| id))
                .unwrap_or_else(|| new.start_of(StoryRef::Body))
        };
        s.sel = Selection { anchor: keep(&s.sel.anchor), focus: keep(&s.sel.focus) };
        s.touch();
        s.clamp_selection();
        self.expected = new;
        self.mirror(s);
    }

    /// The document after `n` ops: cached, or replayed from the nearest cached state before it.
    fn state_at(&mut self, n: usize) -> State {
        let (from, mut state) = match self.states.range(..=n).next_back() {
            Some((k, st)) => (*k, st.clone()),
            None => (0, State::base()),
        };
        for j in from..n.min(self.applied.len()) {
            state = match self.applied.get(j).and_then(|json| serde_json::from_str::<Op>(json).ok()) {
                Some(op) => self.step(j, &op, state),
                None => state,
            };
            // Kept until the next prune: an op's rebase asks for neighbouring states.
            self.states.insert(j + 1, state.clone());
        }
        state
    }

    /// Replays op `j` on `doc`, carrying its positions through the ops before it that its author
    /// hadn't seen.
    fn step(&mut self, j: usize, op: &Op, state: State) -> State {
        let hash = self.applied.get(j).map(|json| op_hash(json));
        if hash.is_some_and(|h| self.skip.contains(&h)) {
            log::warn!("left out a shared `{}` that crashed this app before", op.c);
            return state;
        }
        let unseen: Vec<usize> =
            (j.saturating_sub(WINDOW)..j).filter(|&i| self.ids.get(i).is_some_and(|(c, n)| op.k.get(c).is_none_or(|seen| *n >= *seen))).collect();
        let pairs: Vec<(Document, Document)> = unseen.iter().map(|&i| (self.state_at(i).doc, self.state_at(i + 1).doc)).collect();
        REPLAYING.with(|r| r.set(hash));
        let (doc, sel) = replay(&mut self.replica, op, &pairs, Some(&state.ids), state.doc.clone());
        REPLAYING.with(|r| r.set(None));
        state.next(op, doc, sel.as_ref())
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
    // Spelling commands read this member's dictionary and ignored words.
    let proofing = matches!(c, "review.applySuggestion" | "review.addToDictionary" | "review.ignoreAll" | "review.ignore");
    !file && !path && !proofing && !matches!(c, "tools.macros" | "tools.recordMacro" | "edit.repeat")
}

/// The preferences that change what typing and editing do; the others (the view, proofing, local
/// folders) never leave this member.
const EDITING_PREFS: &[&str] = &["autocorrect", "typingReplacesSelection", "overtype", "detectLanguage"];

/// The editing preferences that differ from the defaults, as an object (`null` when none do).
/// AutoCorrect's lists (the member's own replacements and exceptions) never travel: an op that one
/// of them changed doesn't replay the same, so its author shares it as a patch.
const PRIVATE_LISTS: &[&str] = &["entries", "removed", "exceptions", "exceptionsRemoved", "initialCapsExceptions"];

fn editing_prefs(p: &Prefs) -> Option<serde_json::Map<String, Json>> {
    let Ok(Json::Object(mut o)) = serde_json::to_value(p) else { return None };
    o.retain(|k, _| EDITING_PREFS.contains(&k.as_str()));
    if let Some(Json::Object(ac)) = o.get_mut("autocorrect") {
        ac.retain(|k, _| !PRIVATE_LISTS.contains(&k.as_str()));
    }
    Some(o)
}

fn prefs_diff(p: &Prefs) -> Json {
    let (Some(now), Some(def)) = (editing_prefs(p), editing_prefs(&Prefs::default())) else { return Json::Null };
    let diff: serde_json::Map<String, Json> = now.into_iter().filter(|(k, v)| EDITING_PREFS.contains(&k.as_str()) && def.get(k) != Some(v)).collect();
    if diff.is_empty() { Json::Null } else { Json::Object(diff) }
}

fn prefs_from(o: &Json) -> Prefs {
    let Some(o) = o.as_object() else { return Prefs::default() };
    let Ok(Json::Object(mut def)) = serde_json::to_value(Prefs::default()) else { return Prefs::default() };
    for (k, v) in o.iter().filter(|(k, _)| EDITING_PREFS.contains(&k.as_str())) {
        // Object preferences (AutoCorrect) take the op's keys over the defaults; its lists stay
        // the defaults.
        match (def.get_mut(k), v) {
            (Some(Json::Object(d)), Json::Object(v)) => {
                d.extend(v.iter().filter(|(k, _)| !PRIVATE_LISTS.contains(&k.as_str())).map(|(k, v)| (k.clone(), v.clone())))
            }
            _ => drop(def.insert(k.clone(), v.clone())),
        }
    }
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
/// after each op its author hadn't seen, `ids` those of `doc`'s characters. An op that can't run
/// leaves `doc` as it was. With the document, the selection the op ran with (for a command).
fn replay(r: &mut Session, op: &Op, pairs: &[(Document, Document)], ids: Option<&Ids>, doc: Document) -> (Document, Option<Selection>) {
    let (parts, comments) = made(pairs);
    let story = |s: StoryRef| match s {
        StoryRef::Part(id) => StoryRef::Part(bump(id, &parts)),
        body => body,
    };
    // Just after the character it was after, when that's still here; else carried through what
    // the author hadn't seen. `None`: an unseen op deleted the story the position is in.
    let map = |p: &Pos, y: Option<Option<ids::Id>>| {
        let p = Pos { story: story(p.story), ..p.clone() };
        if pairs.is_empty() {
            return Some(p);
        }
        if let (Some(ids), Some(y)) = (ids, y)
            && let Some(q) = ids.find(&doc, p.story, y)
        {
            return Some(q);
        }
        pairs.iter().try_fold(p, |p, (a, b)| rebase::map_pos(a, b, &p, &|id| id))
    };
    // Hostile input: paths of a sane depth only.
    if op.sel.anchor.path.0.len() > MAX_PATH || op.sel.focus.path.0.len() > MAX_PATH {
        log::warn!("skipped a shared `{}` with a bad position", op.c);
        return (doc, None);
    }
    if let Some(d) = &op.d {
        let at = |s: StoryRef, i: usize| {
            let s = story(s);
            let i = pairs.iter().try_fold(i, |i, (a, b)| rebase::map_block(a, b, s, i))?;
            Some((s, i))
        };
        return match patch::apply(&doc, d, &at) {
            Some(out) => (out, None),
            None => {
                log::info!("a shared `{}` did nothing: the same things changed since", op.c);
                (doc, None)
            }
        };
    }
    let spec_ok = r.registry.get(&op.c).is_some_and(|s| s.mutates) || matches!(op.c.as_str(), "file.open");
    if !spec_ok || !replayable(&op.c, &op.p) {
        log::warn!("skipped a shared `{}` this app can't run", op.c);
        return (doc, None);
    }
    let mut p = op.p.clone();
    if op.c.starts_with("review.")
        && let Some(id) = p.get("id").and_then(Json::as_u64).and_then(|id| u32::try_from(id).ok())
        && let Some(o) = p.as_object_mut()
    {
        o.insert("id".into(), bump(id, &comments).into());
    }
    let (Some(mut anchor), Some(mut focus)) = (map(&op.sel.anchor, op.y.map(|y| y[0])), map(&op.sel.focus, op.y.map(|y| y[1]))) else {
        log::info!("a shared `{}` did nothing: what it edited was deleted", op.c);
        return (doc, None);
    };
    // A range's start: just before its first character, wherever that is now.
    if !pairs.is_empty()
        && let (Some(ids), Some(first)) = (ids, op.ys)
    {
        let start_is_anchor = op.sel.anchor <= op.sel.focus;
        let st = story(if start_is_anchor { op.sel.anchor.story } else { op.sel.focus.story });
        if let Some(q) = ids.find_before(&doc, st, first) {
            *(if start_is_anchor { &mut anchor } else { &mut focus }) = q;
        }
    }
    // A fresh session for every op: nothing an earlier op left behind (or this member did) counts.
    r.renew(doc);
    let sel = Selection { anchor: r.doc.clamp(&anchor), focus: r.doc.clamp(&focus) };
    r.sel = sel.clone();
    r.pending = op.pp.clone();
    r.clipboard = op.cb.clone();
    r.painter = op.fp.clone();
    r.author = op.a.clone();
    r.initials = op.ai.clone();
    r.prefs = prefs_from(&op.o);
    wordcraft_engine::cmd::set_clock(Some(op.t));
    if let Err(e) = r.run(&op.c, &p) {
        log::info!("shared `{}` did nothing here: {e}", op.c);
    }
    wordcraft_engine::cmd::set_clock(None);
    (r.doc.clone(), Some(sel))
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

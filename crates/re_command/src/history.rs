//! Transactions, undo and redo, and the append-only log (ADR-0003).
//!
//! A [`Transaction`] is atomic: if any operation fails, the ones already
//! applied are undone and a `rejected` entry is logged. [`History`] keeps the
//! undo and redo stacks and the log of JSON lines that records the origin of
//! every transaction.
//!
//! A transaction with `set_doc` operations is also checked as a whole: every
//! open document it edited must pass its kind's validator once all its
//! operations ran (ADR-0044), or it is rolled back like any failing
//! operation; undo and redo are checked the same way. Each history entry
//! remembers the [epoch](re_scene::OpenDoc::epoch) of every document it
//! edited: once a document is reopened (or closed), undo and redo refuse the
//! entries recorded against it and drop them, instead of applying JSON
//! pointers to a different document.
//!
//! A transaction with a [`coalesce`](Transaction::coalesce) key merges into
//! the undo step before it when that step has the same key and origin and
//! nothing came between: a gizmo drag sends a live `set_field` each frame,
//! and the whole drag is one undo step. [`History::seal`] ends the run (the
//! button is released), as does any other transaction, an undo or a redo.
//! The log still records every transaction.

use re_scene::{IdGen, World};
use serde::{Deserialize, Serialize};

use crate::error::{OpError, TxError};
use crate::op::{Op, apply};

/// Who issued a transaction. Written to the log as `"user"`,
/// `{"script": "name"}` or `{"agent": "name"}`.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq, Hash)]
#[serde(rename_all = "snake_case")]
pub enum Origin {
    /// A person, through the editor or the CLI.
    User,
    /// A script in the `tools` context, by script path or name.
    Script(String),
    /// A named agent (for example over MCP).
    Agent(String),
}

/// A labelled list of operations applied atomically.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub struct Transaction {
    /// A short human-readable label (shown in undo menus and the log).
    pub label: String,
    /// Who issued it.
    pub origin: Origin,
    /// The operations, applied in order.
    pub ops: Vec<Op>,
    /// Merges this transaction into the previous undo step when that step
    /// has the same key and origin and is not sealed (see the module docs).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub coalesce: Option<String>,
}

impl Transaction {
    /// A transaction from its parts.
    pub fn new(label: impl Into<String>, origin: Origin, ops: Vec<Op>) -> Self {
        Transaction {
            label: label.into(),
            origin,
            ops,
            coalesce: None,
        }
    }

    /// The same transaction with a coalescing key.
    pub fn coalescing(mut self, key: impl Into<String>) -> Self {
        self.coalesce = Some(key.into());
        self
    }
}

/// The kind of a [`LogEntry`].
#[derive(Serialize, Deserialize, Clone, Copy, Debug, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum LogKind {
    /// A transaction was applied.
    Do,
    /// A transaction was undone (the entry lists the inverse operations).
    Undo,
    /// A transaction was redone.
    Redo,
    /// A transaction failed and changed nothing.
    Rejected,
}

/// One line of the transaction log.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub struct LogEntry {
    /// Sequence number, from 1.
    pub seq: u64,
    /// What happened.
    pub kind: LogKind,
    /// The transaction's label.
    pub label: String,
    /// The transaction's origin.
    pub origin: Origin,
    /// The operations as applied (with generated ids filled in), or as
    /// submitted for a rejected transaction.
    pub ops: Vec<Op>,
    /// The error of a rejected transaction.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

struct Applied {
    label: String,
    origin: Origin,
    forward: Vec<Op>,
    inverse: Vec<Op>,
    /// The open documents the operations edit, with the epoch they had.
    docs: Vec<(String, u64)>,
    /// The key later transactions merge into this step by; `None` once
    /// sealed.
    coalesce: Option<String>,
}

impl Applied {
    /// Whether `tx`, applied with `forward`, merges into this step.
    fn takes(&self, tx: &Transaction, docs: &[(String, u64)]) -> bool {
        tx.coalesce.is_some()
            && self.coalesce == tx.coalesce
            && self.origin == tx.origin
            && docs.iter().all(|(d, e)| {
                self.docs
                    .iter()
                    .find(|(sd, _)| sd == d)
                    .is_none_or(|(_, se)| se == e)
            })
    }

    /// Folds a later transaction's operations into this step: undoing it
    /// undoes both. When both are only `set_field`s (a drag), each field
    /// keeps its latest value and its earliest inverse, so a step that sets
    /// a field every frame stays one operation per field.
    fn absorb(&mut self, forward: Vec<Op>, inverse: Vec<Op>, docs: Vec<(String, u64)>) {
        for d in docs {
            if !self.docs.iter().any(|(sd, _)| *sd == d.0) {
                self.docs.push(d);
            }
        }
        if only_set_fields(&self.forward) && only_set_fields(&forward) {
            let mut fresh = Vec::new();
            for op in forward {
                let key = field_of(&op);
                match self.forward.iter_mut().find(|f| field_of(f) == key) {
                    Some(f) => *f = op,
                    None => {
                        fresh.push(key);
                        self.forward.push(op);
                    }
                }
            }
            // A field this step did not set yet is undone with the later
            // transaction's inverse for it: its `set_field` and the override
            // restore that follows it on a generated entity.
            let mut merged = Vec::new();
            let mut keep = false;
            for op in inverse {
                if let Op::SetField { .. } = op {
                    keep = fresh.contains(&field_of(&op));
                }
                if keep {
                    merged.push(op);
                }
            }
            merged.append(&mut self.inverse);
            self.inverse = merged;
            return;
        }
        self.forward.extend(forward);
        let mut merged = inverse;
        merged.append(&mut self.inverse);
        self.inverse = merged;
    }
}

/// True when `ops` are all `set_field` operations.
fn only_set_fields(ops: &[Op]) -> bool {
    ops.iter().all(|op| matches!(op, Op::SetField { .. }))
}

/// The (entity, component, field) a `set_field` sets; `None` for any other
/// operation.
fn field_of(op: &Op) -> Option<(String, String, String)> {
    match op {
        Op::SetField {
            id,
            component,
            field,
            ..
        } => Some((id.clone(), component.clone(), field.clone())),
        _ => None,
    }
}

/// The open documents `ops` edit, each once, with their current epoch.
fn doc_epochs(world: &World, ops: &[Op]) -> Vec<(String, u64)> {
    let mut out: Vec<(String, u64)> = Vec::new();
    for op in ops {
        if let Op::SetDoc { doc, .. } = op {
            let doc = re_scene::doc_path(doc).unwrap_or_else(|_| doc.clone());
            if !out.iter().any(|(d, _)| *d == doc) {
                let epoch = world.doc_epoch(&doc).unwrap_or(0);
                out.push((doc, epoch));
            }
        }
    }
    out
}

/// Undo and redo stacks plus the append-only transaction log.
#[derive(Default)]
pub struct History {
    undo: Vec<Applied>,
    redo: Vec<Applied>,
    log: Vec<LogEntry>,
}

impl History {
    /// Applies a transaction atomically: if any operation fails, the ones
    /// before it are rolled back, the world is unchanged and a `rejected`
    /// entry is logged. On success returns the operations as applied and
    /// clears the redo stack.
    pub fn execute(&mut self, world: &mut World, tx: Transaction) -> Result<Vec<Op>, TxError> {
        match apply_all(world, &tx.ops) {
            Ok((forward, inverse)) => {
                self.record(LogKind::Do, &tx.label, &tx.origin, &forward, None);
                let docs = doc_epochs(world, &forward);
                self.redo.clear();
                if let Some(top) = self.undo.last_mut()
                    && top.takes(&tx, &docs)
                {
                    top.absorb(forward.clone(), inverse, docs);
                    return Ok(forward);
                }
                self.undo.push(Applied {
                    label: tx.label,
                    origin: tx.origin,
                    docs,
                    forward: forward.clone(),
                    inverse,
                    coalesce: tx.coalesce,
                });
                Ok(forward)
            }
            Err(err) => {
                self.record(
                    LogKind::Rejected,
                    &tx.label,
                    &tx.origin,
                    &tx.ops,
                    Some(err.to_string()),
                );
                Err(err)
            }
        }
    }

    /// Runs a transaction and undoes it again, returning the operations as
    /// they would be applied. The undo and redo stacks, the log and the id
    /// generator are left unchanged, and so is the world's
    /// [`state_hash`](World::state_hash) (overrides keep their order and
    /// procedural sources keep their generation state), and so are open
    /// documents, with their revisions and dirty flags.
    ///
    /// The run happens on the world itself, so `Entity` handles do not all
    /// survive: a dry run of a `despawn` re-creates the despawned subtree
    /// with new handles (stable ids, fields, child order and entity
    /// references between registered components are restored). Anything
    /// outside the registry that holds an `Entity` of such a subtree must
    /// look it up again by stable id.
    pub fn dry_run(world: &mut World, tx: &Transaction) -> Result<Vec<Op>, TxError> {
        let ids = world.ecs.get_resource::<IdGen>().cloned();
        let marks = edits_docs(&tx.ops).then(|| world.doc_marks());
        let result = apply_all(world, &tx.ops).and_then(|(forward, inverse)| {
            rollback(world, std::slice::from_ref(&inverse), 0)?;
            Ok(forward)
        });
        if let Some(ids) = ids {
            world.ecs.insert_resource(ids);
        }
        if let Some(marks) = marks {
            world.restore_doc_marks(&marks);
        }
        result
    }

    /// Ends a coalescing run: the next transaction starts a new undo step
    /// whatever its key (a drag's button was released).
    pub fn seal(&mut self) {
        if let Some(top) = self.undo.last_mut() {
            top.coalesce = None;
        }
    }

    /// A document `applied` edited that was reopened or closed since:
    /// every entry that edited it is dropped from both stacks, and the
    /// refusal is returned.
    fn check_docs_still_open(&mut self, world: &World, applied: &Applied) -> Result<(), TxError> {
        let Some((doc, _)) = applied
            .docs
            .iter()
            .find(|(doc, epoch)| world.doc_epoch(doc) != Some(*epoch))
        else {
            return Ok(());
        };
        let doc = doc.clone();
        let edits = |a: &Applied| a.docs.iter().any(|(d, _)| *d == doc);
        self.undo.retain(|a| !edits(a));
        self.redo.retain(|a| !edits(a));
        Err(TxError {
            index: 0,
            error: OpError::StaleDocument { doc },
        })
    }

    /// Undoes the last transaction. Returns `Ok(false)` when there is nothing
    /// to undo. When a document the transaction edited was reopened or
    /// closed since, nothing is applied: every entry that edited that
    /// document is dropped and the error says so.
    pub fn undo(&mut self, world: &mut World) -> Result<bool, TxError> {
        let Some(applied) = self.undo.pop() else {
            return Ok(false);
        };
        self.check_docs_still_open(world, &applied)?;
        match apply_all(world, &applied.inverse) {
            Ok(_) => {
                self.record(
                    LogKind::Undo,
                    &applied.label,
                    &applied.origin,
                    &applied.inverse,
                    None,
                );
                self.redo.push(applied);
                self.seal();
                Ok(true)
            }
            Err(e) => {
                self.undo.push(applied);
                Err(e)
            }
        }
    }

    /// Redoes the last undone transaction. Returns `Ok(false)` when there is
    /// nothing to redo. Refused, like [`undo`](History::undo), when a
    /// document it edited was reopened or closed since.
    pub fn redo(&mut self, world: &mut World) -> Result<bool, TxError> {
        let Some(applied) = self.redo.pop() else {
            return Ok(false);
        };
        self.check_docs_still_open(world, &applied)?;
        match apply_all(world, &applied.forward) {
            Ok((forward, inverse)) => {
                self.record(
                    LogKind::Redo,
                    &applied.label,
                    &applied.origin,
                    &forward,
                    None,
                );
                self.undo.push(Applied {
                    label: applied.label,
                    origin: applied.origin,
                    docs: applied.docs,
                    forward,
                    inverse,
                    coalesce: None,
                });
                Ok(true)
            }
            Err(e) => {
                self.redo.push(applied);
                Err(e)
            }
        }
    }

    /// True if there is a transaction to undo.
    pub fn can_undo(&self) -> bool {
        !self.undo.is_empty()
    }

    /// True if there is a transaction to redo.
    pub fn can_redo(&self) -> bool {
        !self.redo.is_empty()
    }

    /// The operations an [`undo`](History::undo) would apply, if any.
    pub fn peek_undo(&self) -> Option<&[Op]> {
        self.undo.last().map(|a| a.inverse.as_slice())
    }

    /// The operations a [`redo`](History::redo) would apply, if any.
    pub fn peek_redo(&self) -> Option<&[Op]> {
        self.redo.last().map(|a| a.forward.as_slice())
    }

    /// The label of the transaction an undo would revert.
    pub fn undo_label(&self) -> Option<&str> {
        self.undo.last().map(|a| a.label.as_str())
    }

    /// The [coalesce](Transaction::coalesce) key of the step an undo would
    /// revert, while later transactions with that key still merge into it.
    pub fn undo_key(&self) -> Option<&str> {
        self.undo.last().and_then(|a| a.coalesce.as_deref())
    }

    /// The label of the transaction a redo would reapply.
    pub fn redo_label(&self) -> Option<&str> {
        self.redo.last().map(|a| a.label.as_str())
    }

    /// The whole log.
    pub fn log(&self) -> &[LogEntry] {
        &self.log
    }

    /// The sequence number of the last log entry (0 when empty).
    pub fn last_seq(&self) -> u64 {
        self.log.last().map_or(0, |e| e.seq)
    }

    /// Log entries after `seq`, as JSON lines (what an agent polls).
    pub fn log_since_jsonl(&self, seq: u64) -> String {
        self.log
            .iter()
            .filter(|e| e.seq > seq)
            .map(|e| serde_json::to_string(e).unwrap_or_default() + "\n")
            .collect()
    }

    fn record(
        &mut self,
        kind: LogKind,
        label: &str,
        origin: &Origin,
        ops: &[Op],
        error: Option<String>,
    ) {
        let seq = self.log.len() as u64 + 1;
        self.log.push(LogEntry {
            seq,
            kind,
            label: label.to_string(),
            origin: origin.clone(),
            ops: ops.to_vec(),
            error,
        });
    }
}

/// True if any of `ops` edits an open document.
fn edits_docs(ops: &[Op]) -> bool {
    ops.iter().any(|o| matches!(o, Op::SetDoc { .. }))
}

/// Checks every open document `ops` edited with its kind's validator. The
/// error names the last operation that edited a document that does not
/// validate (documents in the order of their last edit).
fn check_docs(world: &World, ops: &[Op]) -> Result<(), TxError> {
    let mut last: Vec<(String, usize)> = Vec::new();
    for (index, op) in ops.iter().enumerate() {
        if let Op::SetDoc { doc, .. } = op {
            let doc = re_scene::doc_path(doc).unwrap_or_else(|_| doc.clone());
            last.retain(|(d, _)| *d != doc);
            last.push((doc, index));
        }
    }
    for (doc, index) in last {
        if let Some(Err(message)) = world.validate_doc(&doc) {
            return Err(TxError {
                index,
                error: OpError::InvalidDocument { doc, message },
            });
        }
    }
    Ok(())
}

/// Applies operations in order, returning (forward as applied, inverse in
/// undo order). Rolls back on error, and when an edited open document does
/// not validate afterwards. A rolled-back transaction leaves open
/// documents' revisions and dirty flags as they were.
fn apply_all(world: &mut World, ops: &[Op]) -> Result<(Vec<Op>, Vec<Op>), TxError> {
    let marks = edits_docs(ops).then(|| world.doc_marks());
    let mut forward = Vec::with_capacity(ops.len());
    let mut inverse: Vec<Vec<Op>> = Vec::with_capacity(ops.len());
    let mut failure = None;
    for (index, op) in ops.iter().enumerate() {
        match apply(world, op) {
            Ok((fwd, inv)) => {
                forward.push(fwd);
                inverse.push(inv);
            }
            Err(error) => {
                failure = Some(TxError { index, error });
                break;
            }
        }
    }
    if failure.is_none() && marks.is_some() {
        failure = check_docs(world, ops).err();
    }
    if let Some(err) = failure {
        inverse.reverse();
        let rolled_back = rollback(world, &inverse, err.index);
        if let Some(marks) = &marks {
            world.restore_doc_marks(marks);
        }
        rolled_back?;
        return Err(err);
    }
    let inverse = inverse.into_iter().rev().flatten().collect();
    Ok((forward, inverse))
}

/// Applies inverse operations, group by group in the order given. An inverse
/// that does not apply means the world no longer matches what was recorded;
/// that is reported as [`OpError::RollbackFailed`] (the remaining inverses
/// are still applied, best effort) instead of crashing the engine thread.
fn rollback(world: &mut World, groups: &[Vec<Op>], index: usize) -> Result<(), TxError> {
    let mut first: Option<String> = None;
    for o in groups.iter().flatten() {
        if let Err(e) = apply(world, o)
            && first.is_none()
        {
            first = Some(e.to_string());
        }
    }
    match first {
        None => Ok(()),
        Some(message) => Err(TxError {
            index,
            error: OpError::RollbackFailed(message),
        }),
    }
}

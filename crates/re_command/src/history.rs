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
//! operation. Undo and redo restore states that were valid, so they do not
//! check again.

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
}

impl Transaction {
    /// A transaction from its parts.
    pub fn new(label: impl Into<String>, origin: Origin, ops: Vec<Op>) -> Self {
        Transaction {
            label: label.into(),
            origin,
            ops,
        }
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
        match apply_all(world, &tx.ops, true) {
            Ok((forward, inverse)) => {
                self.record(LogKind::Do, &tx.label, &tx.origin, &forward, None);
                self.undo.push(Applied {
                    label: tx.label,
                    origin: tx.origin,
                    forward: forward.clone(),
                    inverse,
                });
                self.redo.clear();
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
        let result = apply_all(world, &tx.ops, true).and_then(|(forward, inverse)| {
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

    /// Undoes the last transaction. Returns `Ok(false)` when there is nothing to undo.
    pub fn undo(&mut self, world: &mut World) -> Result<bool, TxError> {
        let Some(applied) = self.undo.pop() else {
            return Ok(false);
        };
        match apply_all(world, &applied.inverse, false) {
            Ok(_) => {
                self.record(
                    LogKind::Undo,
                    &applied.label,
                    &applied.origin,
                    &applied.inverse,
                    None,
                );
                self.redo.push(applied);
                Ok(true)
            }
            Err(e) => {
                self.undo.push(applied);
                Err(e)
            }
        }
    }

    /// Redoes the last undone transaction. Returns `Ok(false)` when there is nothing to redo.
    pub fn redo(&mut self, world: &mut World) -> Result<bool, TxError> {
        let Some(applied) = self.redo.pop() else {
            return Ok(false);
        };
        match apply_all(world, &applied.forward, false) {
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
                    forward,
                    inverse,
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
/// undo order). Rolls back on error, and when `validate` is set and an edited
/// open document does not validate afterwards. A rolled-back transaction
/// leaves open documents' revisions and dirty flags as they were.
fn apply_all(world: &mut World, ops: &[Op], validate: bool) -> Result<(Vec<Op>, Vec<Op>), TxError> {
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
    if failure.is_none() && validate && marks.is_some() {
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

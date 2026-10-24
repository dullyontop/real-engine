//! The command registry (ADR-0003): named commands with JSON Schema inputs,
//! a tier (ADR-0020), and handlers that edit the world through transactions.
//! The CLI (`real do`), the MCP server, the editor's palette and Luau tools
//! all dispatch through it.
//!
//! Dry runs are implemented once, here: a mutating command called with
//! `dry_run` runs its transaction, records the changes and undoes it without
//! touching the undo and redo stacks or the log.

use std::sync::Arc;

use indexmap::IndexMap;
use re_core::{Diagnostic, Location};
use re_scene::World;
use schemars::JsonSchema;
use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};
use serde_json::Value as Json;

use crate::codes;
use crate::history::{History, Origin, Transaction};
use crate::op::Op;

/// What a command needs from its host: the world and its history.
/// Implemented by `re_app::App`, by [`EditState`], and by
/// test fixtures.
pub trait EditTarget {
    /// The world to edit.
    fn world(&mut self) -> &mut World;
    /// The undo history and log.
    fn history(&mut self) -> &mut History;
}

/// A command's permission tier (ADR-0020): `read` < `edit` < `destructive`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Tier {
    /// Changes nothing.
    Read,
    /// Changes the project in an undoable way.
    Edit,
    /// Can lose work (for example deleting files).
    Destructive,
}

impl Tier {
    /// `"read"`, `"edit"` or `"destructive"`.
    pub fn as_str(self) -> &'static str {
        match self {
            Tier::Read => "read",
            Tier::Edit => "edit",
            Tier::Destructive => "destructive",
        }
    }
}

/// MCP-style tool annotations (ARCHITECTURE §6), derived from the tier with
/// `idempotentHint` set per command.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CommandAnnotations {
    /// True for `read` commands.
    pub read_only_hint: bool,
    /// True for `destructive` commands.
    pub destructive_hint: bool,
    /// True if repeating the call with the same arguments has no further effect.
    pub idempotent_hint: bool,
    /// Always false: commands act only on the project.
    pub open_world_hint: bool,
}

impl CommandAnnotations {
    /// The annotations of a command of `tier`.
    pub fn for_tier(tier: Tier, idempotent: bool) -> Self {
        CommandAnnotations {
            read_only_hint: tier == Tier::Read,
            destructive_hint: tier == Tier::Destructive,
            idempotent_hint: idempotent,
            open_world_hint: false,
        }
    }
}

/// The description of one command.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct CommandSpec {
    /// The stable dotted name (`entity.set`).
    pub name: String,
    /// One sentence saying what it does.
    pub summary: String,
    /// The permission tier.
    pub tier: Tier,
    /// JSON Schema of the arguments (from schemars; the root is an object).
    pub input_schema: Json,
    /// Tool annotations.
    pub annotations: CommandAnnotations,
    /// JSON Schema of the outcome's `data` on success (from schemars), for
    /// commands registered with [`CommandRegistry::register_typed_output`].
    /// `None` means the data is not described.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub output_schema: Option<Json>,
}

/// The result of a dispatch.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct CommandOutcome {
    /// True if the command succeeded.
    pub ok: bool,
    /// The operations applied (or, for a dry run, that would be applied).
    #[serde(default)]
    pub changes: Vec<Op>,
    /// Errors (when `ok` is false) and warnings.
    #[serde(default)]
    pub diagnostics: Vec<Diagnostic>,
    /// The command's result data (`null` when it has none).
    #[serde(default)]
    pub data: Json,
}

impl CommandOutcome {
    /// A failed outcome with one diagnostic.
    pub fn error(diagnostic: Diagnostic) -> Self {
        CommandOutcome {
            ok: false,
            changes: Vec::new(),
            diagnostics: vec![diagnostic],
            data: Json::Null,
        }
    }
}

/// The policy hook run before every dispatch. `re_agent` implements it with
/// the agent policy (tiers, path scopes, budgets). A dry run needs only the
/// `read` tier.
pub trait PolicyCheck {
    /// `Ok` to allow the call, or a diagnostic saying why it is refused.
    fn check(&self, spec: &CommandSpec, args: &Json, dry_run: bool) -> Result<(), Diagnostic>;
}

/// A policy that allows everything (tests, the CLI run by a person).
#[derive(Clone, Copy, Debug, Default)]
pub struct AllowAll;

impl PolicyCheck for AllowAll {
    fn check(&self, _spec: &CommandSpec, _args: &Json, _dry_run: bool) -> Result<(), Diagnostic> {
        Ok(())
    }
}

/// The state of one dispatch, handed to the handler.
pub struct Call {
    name: String,
    dry_run: bool,
    origin: Origin,
    changes: Vec<Op>,
    diagnostics: Vec<Diagnostic>,
    specs: Arc<[CommandSpec]>,
}

impl Call {
    /// The command's name.
    pub fn name(&self) -> &str {
        &self.name
    }

    /// True for a dry run: [`Call::transact`] then rolls back, and handlers
    /// that change state another way must only report what they would do.
    pub fn dry_run(&self) -> bool {
        self.dry_run
    }

    /// Who issued the call.
    pub fn origin(&self) -> &Origin {
        &self.origin
    }

    /// Every registered command's spec.
    pub fn specs(&self) -> &[CommandSpec] {
        &self.specs
    }

    /// Runs `ops` as one transaction with the call's origin (or, in a dry
    /// run, runs and rolls it back), records the applied operations as the
    /// outcome's changes, and returns them. A rejected transaction becomes an
    /// `RE2001` diagnostic.
    pub fn transact<C: EditTarget + ?Sized>(
        &mut self,
        target: &mut C,
        label: impl Into<String>,
        ops: Vec<Op>,
    ) -> Result<Vec<Op>, Diagnostic> {
        self.transact_coalescing(target, label, ops, None)
    }

    /// [`transact`](Self::transact) with a [coalesce](Transaction::coalesce)
    /// key: the transaction merges into the previous undo step when that
    /// step has the same key and origin.
    pub fn transact_coalescing<C: EditTarget + ?Sized>(
        &mut self,
        target: &mut C,
        label: impl Into<String>,
        ops: Vec<Op>,
        coalesce: Option<String>,
    ) -> Result<Vec<Op>, Diagnostic> {
        let mut tx = Transaction::new(label, self.origin.clone(), ops);
        tx.coalesce = coalesce;
        let result = if self.dry_run {
            History::dry_run(target.world(), &tx)
        } else {
            let mut history = std::mem::take(target.history());
            let r = history.execute(target.world(), tx);
            *target.history() = history;
            r
        };
        match result {
            Ok(applied) => {
                self.changes.extend(applied.iter().cloned());
                Ok(applied)
            }
            Err(e) => {
                let mut d = e.to_diagnostic();
                if d.location.is_none() {
                    d.location = Some(Location::command(self.name.clone()));
                }
                Err(d)
            }
        }
    }

    /// Records operations as changes (for handlers that change state without
    /// [`Call::transact`], such as `undo`).
    pub fn record_changes(&mut self, ops: impl IntoIterator<Item = Op>) {
        self.changes.extend(ops);
    }

    /// Adds a warning or note to the outcome.
    pub fn push_diagnostic(&mut self, diagnostic: Diagnostic) {
        self.diagnostics.push(diagnostic);
    }
}

/// A command handler: gets the target, the arguments (without `dry_run`) and
/// the call state; returns the result data.
pub type Handler<C> =
    Arc<dyn Fn(&mut C, &Json, &mut Call) -> Result<Json, Diagnostic> + Send + Sync>;

/// The command registry for targets of type `C`.
pub struct CommandRegistry<C: EditTarget> {
    commands: IndexMap<String, (CommandSpec, Handler<C>)>,
    specs: Arc<[CommandSpec]>,
}

impl<C: EditTarget> Default for CommandRegistry<C> {
    fn default() -> Self {
        Self::new()
    }
}

/// The JSON Schema of `A`, as a plain object with a `dry_run` property added
/// for mutating tiers.
pub fn input_schema<A: JsonSchema>(tier: Tier) -> Json {
    let mut schema = serde_json::to_value(schemars::schema_for!(A)).unwrap_or(Json::Null);
    if let Some(obj) = schema.as_object_mut() {
        obj.entry("type").or_insert_with(|| Json::from("object"));
        let props = obj
            .entry("properties")
            .or_insert_with(|| Json::Object(Default::default()));
        if tier != Tier::Read
            && let Some(props) = props.as_object_mut()
        {
            props.insert(
                "dry_run".into(),
                serde_json::json!({
                    "type": "boolean",
                    "description": "Return the changes the command would make, without making them (needs only the read tier)."
                }),
            );
        }
    }
    schema
}

impl<C: EditTarget> CommandRegistry<C> {
    /// An empty registry.
    pub fn new() -> Self {
        CommandRegistry {
            commands: IndexMap::new(),
            specs: Arc::from(Vec::new()),
        }
    }

    /// Registers (or replaces) a command with a raw handler.
    pub fn register(&mut self, spec: CommandSpec, handler: Handler<C>) {
        self.commands.insert(spec.name.clone(), (spec, handler));
        self.specs = self.commands.values().map(|(s, _)| s.clone()).collect();
    }

    /// Registers a command whose arguments deserialize into `A` (its input
    /// schema comes from schemars). Arguments that do not parse give an
    /// `RE2005` diagnostic.
    pub fn register_typed<A, F>(
        &mut self,
        name: &str,
        summary: &str,
        tier: Tier,
        idempotent: bool,
        handler: F,
    ) where
        A: DeserializeOwned + JsonSchema + 'static,
        F: Fn(&mut C, A, &mut Call) -> Result<Json, Diagnostic> + Send + Sync + 'static,
    {
        let spec = CommandSpec {
            name: name.to_string(),
            summary: summary.to_string(),
            tier,
            input_schema: input_schema::<A>(tier),
            annotations: CommandAnnotations::for_tier(tier, idempotent),
            output_schema: None,
        };
        let command = name.to_string();
        let handler: Handler<C> = Arc::new(move |target, args, call| {
            let args = if args.is_null() {
                Json::Object(Default::default())
            } else {
                args.clone()
            };
            let parsed: A = serde_json::from_value(args).map_err(|e| {
                Diagnostic::error(
                    codes::INVALID_ARGUMENTS,
                    format!("invalid arguments for {command}: {e}"),
                )
                .at(Location::command(command.clone()))
                .with_hint(format!(
                    "see the input schema with `command.describe {{\"name\": \"{command}\"}}`"
                ))
            })?;
            handler(target, parsed, call)
        });
        self.register(spec, handler);
    }

    /// Registers a command like [`CommandRegistry::register_typed`] whose
    /// result data is described by `O`: its schema becomes the spec's
    /// [`CommandSpec::output_schema`]. The handler still returns JSON; `O`
    /// only documents its shape.
    pub fn register_typed_output<A, O, F>(
        &mut self,
        name: &str,
        summary: &str,
        tier: Tier,
        idempotent: bool,
        handler: F,
    ) where
        A: DeserializeOwned + JsonSchema + 'static,
        O: JsonSchema,
        F: Fn(&mut C, A, &mut Call) -> Result<Json, Diagnostic> + Send + Sync + 'static,
    {
        self.register_typed::<A, F>(name, summary, tier, idempotent, handler);
        let schema = serde_json::to_value(schemars::schema_for!(O)).unwrap_or(Json::Null);
        if let Some((spec, _)) = self.commands.get_mut(name) {
            spec.output_schema = Some(schema);
        }
        self.specs = self.commands.values().map(|(s, _)| s.clone()).collect();
    }

    /// The spec of the command named `name`.
    pub fn spec(&self, name: &str) -> Option<&CommandSpec> {
        self.commands.get(name).map(|(s, _)| s)
    }

    /// Every spec, in registration order.
    pub fn specs(&self) -> &[CommandSpec] {
        &self.specs
    }

    /// Dispatches a command as [`Origin::User`].
    pub fn dispatch(
        &self,
        target: &mut C,
        name: &str,
        args: Json,
        dry_run: bool,
        policy: &dyn PolicyCheck,
    ) -> CommandOutcome {
        self.dispatch_as(target, name, args, dry_run, policy, Origin::User)
    }

    /// Dispatches a command: looks it up, takes `dry_run` from the call or
    /// from a `"dry_run": true` argument, runs the policy check, then the
    /// handler. Never panics on bad input; every failure is a diagnostic in
    /// the outcome.
    pub fn dispatch_as(
        &self,
        target: &mut C,
        name: &str,
        mut args: Json,
        dry_run: bool,
        policy: &dyn PolicyCheck,
        origin: Origin,
    ) -> CommandOutcome {
        let Some((spec, handler)) = self.commands.get(name) else {
            let suggestion = re_reflect::suggest(name, self.commands.keys().map(String::as_str));
            let mut d = Diagnostic::error(
                codes::UNKNOWN_COMMAND,
                match &suggestion {
                    Some(s) => format!("unknown command {name:?} (did you mean {s:?}?)"),
                    None => format!("unknown command {name:?}"),
                },
            )
            .at(Location::command(name.to_string()));
            d = d.with_hint("list the commands with `describe`");
            return CommandOutcome::error(d);
        };
        let mut dry_run = dry_run;
        if let Some(obj) = args.as_object_mut()
            && let Some(flag) = obj.remove("dry_run")
        {
            match flag {
                Json::Bool(b) => dry_run |= b,
                other => {
                    return CommandOutcome::error(
                        Diagnostic::error(
                            codes::INVALID_ARGUMENTS,
                            format!("invalid arguments for {name}: `dry_run` must be a boolean, found {other}"),
                        )
                        .at(Location::command(name.to_string())),
                    );
                }
            }
        }
        if let Err(d) = policy.check(spec, &args, dry_run) {
            return CommandOutcome::error(d);
        }
        let mut call = Call {
            name: name.to_string(),
            dry_run,
            origin,
            changes: Vec::new(),
            diagnostics: Vec::new(),
            specs: self.specs.clone(),
        };
        match handler(target, &args, &mut call) {
            Ok(data) => CommandOutcome {
                ok: true,
                changes: call.changes,
                diagnostics: call.diagnostics,
                data,
            },
            Err(d) => {
                let mut diagnostics = call.diagnostics;
                diagnostics.insert(0, d);
                CommandOutcome {
                    ok: false,
                    changes: call.changes,
                    diagnostics,
                    data: Json::Null,
                }
            }
        }
    }
}

/// A world plus its history: the simplest [`EditTarget`] (tests, tools, the
/// CLI's one-shot commands).
#[derive(Default)]
pub struct EditState {
    /// The world.
    pub world: World,
    /// Its history.
    pub history: History,
}

impl EditState {
    /// A new world with the built-in components and an empty history.
    pub fn new() -> Self {
        Self::default()
    }
}

impl EditTarget for EditState {
    fn world(&mut self) -> &mut World {
        &mut self.world
    }
    fn history(&mut self) -> &mut History {
        &mut self.history
    }
}

/// An empty argument object (for commands that take none).
#[derive(Debug, Default, Clone, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct NoArgs {}

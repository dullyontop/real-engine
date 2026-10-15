//! The compat scene conventions: how a Roblox-style place is laid out as a
//! native scene, so the importer (WP16), the data model (WP11) and the
//! services (WP12) agree on it.
//!
//! * The data model is one root entity with stable id [`GAME_ID`] (`"game"`)
//!   and class `DataModel`.
//! * Each service is a child of `game` whose stable id and name are its class
//!   name (`Workspace`, `ReplicatedStorage`), with a [`ServiceTag`].
//! * Every instance carries a [`ClassMarker`] naming its class, plus the
//!   components [`ClassRegistry::instance_components`] lists.
//! * A script instance carries a `ScriptRef` whose `context` follows its class
//!   and run context ([`script_context`]): `server` for a `Script` (or
//!   `client`, for run context `Client`), `client` for a `LocalScript`, and
//!   `module` for a `ModuleScript`. The context says which VM the script
//!   belongs to, not whether it starts: where it sits in the tree decides that
//!   (a `Script` under `ServerStorage` never starts; WP11). A `Script` with run
//!   context `Plugin` runs nowhere in a place and is not imported.
//! * Class-specific property values live in host components `Compat_<Class>`
//!   ([`ClassRegistry::register_components`]), which a scene file stores by
//!   field name without declaring their schemas.
//! * A project in the compat layout says so in `real.toml`:
//!   `[compat]` with `api = "roblox"` ([`CompatConfig`]).

use std::collections::BTreeMap;

use indexmap::IndexMap;
use re_core::Diagnostic;
use re_reflect::{Doc, Value, component};
use re_scene::ecs as bevy_ecs;
use re_scene::{Entity, EntityDoc, ScriptContext, ScriptRef, World};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::class::{ClassMarker, ClassRegistry, own_component_name};
use crate::codes;
use crate::error::CompatError;

/// The stable id of the data model's root entity.
pub const GAME_ID: &str = "game";

/// The class of the root entity.
pub const ROOT_CLASS: &str = "DataModel";

component! {
    /// Marks the entity as a service: the one instance of its class, a child
    /// of `game`.
    pub struct ServiceTag {}
}

/// The `ScriptRef` context a script instance runs in, from its class and its
/// `RunContext` item name (`Legacy`, `Server`, `Client`, `Plugin`; `None` reads
/// as `Legacy`). `None` for a class that is not a script, or a run context that
/// runs nowhere here (`Plugin`). A `LocalScript` always runs on the client.
pub fn script_context(class: &str, run_context: Option<&str>) -> Option<ScriptContext> {
    match class {
        "Script" => match run_context.unwrap_or("Legacy") {
            "Legacy" | "Server" => Some(ScriptContext::Server),
            "Client" => Some(ScriptContext::Client),
            _ => None,
        },
        "LocalScript" => Some(ScriptContext::Client),
        "ModuleScript" => Some(ScriptContext::Module),
        _ => None,
    }
}

/// The `RunContext` item of a script instance, read from the own component
/// of the class that declares the property, if the instance has it.
fn run_context(
    world: &World,
    classes: &ClassRegistry,
    entity: Entity,
    class: &str,
) -> Option<String> {
    let m = classes.member(class, "RunContext").ok()?;
    let crate::class::Member::Property(p) = m.member else {
        return None;
    };
    let crate::class::Storage::Own { index, .. } = p.storage else {
        return None;
    };
    let key = world.registry.try_key(&own_component_name(&m.class.name))?;
    match world.registry.get_field(&world.ecs, entity, key, index)? {
        Value::Enum(v) => Some(v),
        _ => None,
    }
}

/// The scene-file form of a new instance of `class`: its [`ClassMarker`] and
/// every component [`ClassRegistry::instance_components`] lists, all at their
/// defaults.
pub fn instance_doc(
    classes: &ClassRegistry,
    class: &str,
    id: &str,
    name: &str,
    parent: Option<&str>,
) -> Result<EntityDoc, CompatError> {
    let mut components = IndexMap::new();
    let mut marker = IndexMap::new();
    marker.insert("class".to_string(), Doc::Str(class.to_string()));
    components.insert("ClassMarker".to_string(), marker);
    for c in classes.instance_components(class)? {
        components.insert(c, IndexMap::new());
    }
    if classes.try_class(class).is_some_and(|c| c.service) {
        components.insert("ServiceTag".to_string(), IndexMap::new());
    }
    Ok(EntityDoc {
        id: id.to_string(),
        name: name.to_string(),
        parent: parent.map(str::to_string),
        components,
        overrides: Vec::new(),
    })
}

/// Spawns an instance of `class` named `name` under `parent` (which needs a
/// stable id), with stable id `id` or a fresh one. The class layer's
/// components must be registered ([`ClassRegistry::register_components`]).
pub fn spawn_instance(
    world: &mut World,
    classes: &ClassRegistry,
    class: &str,
    id: Option<&str>,
    name: &str,
    parent: Option<Entity>,
) -> Result<Entity, CompatError> {
    let parent_id = match parent {
        None => None,
        Some(p) => Some(
            world
                .id_of(p)
                .ok_or_else(|| CompatError::Scene("the parent has no stable id".into()))?
                .to_string(),
        ),
    };
    let id = match id {
        Some(id) => id.to_string(),
        None => world.new_id(),
    };
    let doc = instance_doc(classes, class, &id, name, parent_id.as_deref())?;
    let spawned = world
        .spawn_docs(&[doc], None)
        .map_err(|e| CompatError::Scene(e.to_string()))?;
    Ok(spawned[0])
}

/// Spawns an empty place: the `game` root and one child per service class
/// named in `services`, in that order. Registers the class layer's
/// components first. Returns the root.
pub fn create_place(
    world: &mut World,
    classes: &ClassRegistry,
    services: &[&str],
) -> Result<Entity, CompatError> {
    classes.register_components(world)?;
    let game = spawn_instance(world, classes, ROOT_CLASS, Some(GAME_ID), GAME_ID, None)?;
    for &s in services {
        if !classes.class(s)?.service {
            return Err(CompatError::Scene(format!("{s} is not a service class")));
        }
        spawn_instance(world, classes, s, Some(s), s, Some(game))?;
    }
    Ok(game)
}

/// Checks a loaded compat scene against the conventions. Returns one
/// `RE9004` error per problem, located at the entity; empty when the scene
/// follows them. Entities without a [`ClassMarker`] are native entities and
/// are not checked.
pub fn validate(world: &mut World, classes: &ClassRegistry) -> Vec<Diagnostic> {
    let mut out = Vec::new();
    let mut q = world.ecs.query::<(
        Entity,
        &ClassMarker,
        Option<&ServiceTag>,
        Option<&ScriptRef>,
    )>();
    let mut rows: Vec<(String, Entity, String, bool, Option<ScriptContext>)> = q
        .iter(&world.ecs)
        .map(|(e, m, s, r)| {
            (
                String::new(),
                e,
                m.class.clone(),
                s.is_some(),
                r.map(|r| r.context),
            )
        })
        .collect();
    for r in &mut rows {
        r.0 = world.id_of(r.1).unwrap_or_default().to_string();
    }
    rows.sort_by(|a, b| a.0.cmp(&b.0).then(a.1.cmp(&b.1)));
    let game = world.entity(GAME_ID).ok();
    let mut err = |id: &str, msg: String| {
        out.push(Diagnostic::error(codes::SCENE_CONVENTION, msg).at(re_core::Location::entity(id)));
    };
    match game {
        None => err(
            GAME_ID,
            format!("the scene has no root entity with id {GAME_ID:?}"),
        ),
        Some(g) => {
            let class = world.ecs.get::<ClassMarker>(g).map(|m| m.class.as_str());
            if class != Some(ROOT_CLASS) {
                err(
                    GAME_ID,
                    format!("{GAME_ID:?} must be a {ROOT_CLASS} instance"),
                );
            }
            if re_scene::hierarchy::parent(&world.ecs, g).is_some() {
                err(GAME_ID, format!("{GAME_ID:?} must be a root entity"));
            }
        }
    }
    for (id, e, class, tagged, script) in rows {
        let Some(def) = classes.try_class(&class) else {
            err(&id, format!("unknown class {class:?}"));
            continue;
        };
        if def.service != tagged {
            err(
                &id,
                if def.service {
                    format!("{class} is a service but has no ServiceTag")
                } else {
                    format!("{class} is not a service but has a ServiceTag")
                },
            );
        }
        if def.service {
            if id != class {
                err(&id, format!("a {class} service must have the id {class:?}"));
            }
            if game.is_none() || re_scene::hierarchy::parent(&world.ecs, e) != game {
                err(
                    &id,
                    format!("the {class} service must be a child of {GAME_ID:?}"),
                );
            }
        }
        let rc = run_context(world, classes, e, &class);
        let want = script_context(&class, rc.as_deref());
        if script != want {
            let name = |c: Option<ScriptContext>| match c {
                Some(c) => format!("{c:?}").to_lowercase(),
                None => "none".to_string(),
            };
            err(
                &id,
                format!(
                    "a {class} instance must have script context {}, found {}",
                    name(want),
                    name(script)
                ),
            );
        }
    }
    out
}

/// The API a compat project's scripts are written against.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "lowercase")]
pub enum CompatApi {
    /// The Roblox-style API.
    Roblox,
}

/// The `[compat]` section of `real.toml`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct CompatConfig {
    /// The API the project's scripts use.
    pub api: CompatApi,
    /// When signal handlers run: `deferred` (the default) or `immediate`.
    #[serde(default)]
    pub signal_behavior: crate::signal::SignalBehavior,
    /// Animation id substitutes (`[compat.animations]`): an asset id
    /// (`"1234"` or `"rbxassetid://1234"`) to `builtin:<clip>` or the dotted
    /// path of a `KeyframeSequence` in the place. Keys are kept as asset
    /// numbers once loaded.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub animations: BTreeMap<String, String>,
}

impl CompatConfig {
    /// The `[compat]` section of a `real.toml`'s text, if it has one. Other
    /// sections are not checked here.
    pub fn from_project_toml(text: &str) -> Result<Option<CompatConfig>, CompatError> {
        let doc: toml::Table = text
            .parse()
            .map_err(|e: toml::de::Error| CompatError::Scene(format!("real.toml: {e}")))?;
        match doc.get("compat") {
            None => Ok(None),
            Some(v) => {
                let mut cfg: CompatConfig =
                    v.clone().try_into().map_err(|e: toml::de::Error| {
                        CompatError::Scene(format!("real.toml [compat]: {e}"))
                    })?;
                cfg.animations = normalize_animations(&cfg.animations)?;
                Ok(Some(cfg))
            }
        }
    }

    /// The section as the importer writes it.
    pub fn to_toml(&self) -> String {
        let api = match self.api {
            CompatApi::Roblox => "roblox",
        };
        let mut out = format!("[compat]\napi = \"{api}\"\n");
        if self.signal_behavior != crate::signal::SignalBehavior::default() {
            out.push_str(&format!(
                "signal_behavior = \"{}\"\n",
                self.signal_behavior.as_str()
            ));
        }
        if !self.animations.is_empty() {
            out.push_str("\n[compat.animations]\n");
            for (k, v) in &self.animations {
                out.push_str(&format!("{k:?} = {v:?}\n"));
            }
        }
        out
    }
}

/// Checks `[compat.animations]` and keys it by asset number: `"1234"` and
/// `"rbxassetid://1234"` are the same id, so naming both is refused, as is a
/// key with no asset number or a value that is neither `builtin:<clip>` nor a
/// dotted path.
fn normalize_animations(
    table: &BTreeMap<String, String>,
) -> Result<BTreeMap<String, String>, CompatError> {
    let bad = |message: String| CompatError::Runtime {
        code: codes::BAD_ANIMATION_MAP,
        message: format!("real.toml [compat.animations]: {message}"),
    };
    let mut out = BTreeMap::new();
    for (k, v) in table {
        let digits = k.strip_prefix("rbxassetid://").unwrap_or(k);
        if digits.is_empty() || !digits.bytes().all(|b| b.is_ascii_digit()) {
            return Err(bad(format!(
                "key {k:?} is not an asset id (\"1234\" or \"rbxassetid://1234\")"
            )));
        }
        let ok = match v.strip_prefix("builtin:") {
            Some(name) => crate::anim::clips::builtin_names().any(|n| n == name),
            None => v.split('.').all(|seg| {
                !seg.is_empty()
                    && seg
                        .chars()
                        .all(|c| c.is_alphanumeric() || c == '_' || c == ' ')
            }),
        };
        if !ok {
            return Err(bad(format!(
                "{k:?} = {v:?}: expected \"builtin:<clip>\" naming a built-in clip, or a dotted path such as \"game.ReplicatedStorage.Wave\""
            )));
        }
        let n = digits.trim_start_matches('0');
        let n = if n.is_empty() { "0" } else { n };
        if out.insert(n.to_string(), v.clone()).is_some() {
            return Err(bad(format!("asset id {n} is listed twice")));
        }
    }
    Ok(out)
}

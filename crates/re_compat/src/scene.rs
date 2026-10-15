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
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "lowercase")]
pub enum CompatApi {
    /// The Roblox-style API.
    #[default]
    Roblox,
}

/// The `[compat]` section of `real.toml`. Build one with `..Default::default()`
/// so later fields need no change.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct CompatConfig {
    /// The API the project's scripts use.
    pub api: CompatApi,
    /// When signal handlers run: `deferred` (the default) or `immediate`.
    #[serde(default)]
    pub signal_behavior: crate::signal::SignalBehavior,
    /// What `game` and `RunService` report about the session
    /// (`[compat.game]`); Studio-like by default.
    #[serde(default, skip_serializing_if = "GameInfo::is_default")]
    pub game: GameInfo,
    /// Seeds `Random.new()` called without a seed (the client context uses
    /// `seed + 1`), so runs repeat. TOML integers are signed, so a seed
    /// written to `real.toml` must be at most `i64::MAX`.
    #[serde(default, skip_serializing_if = "is_zero")]
    pub seed: u64,
    /// Asset substitutes: an asset id (`"rbxassetid://123"` or `"123"`, both
    /// stored as [`asset_key`] gives them) to a project-relative file path.
    /// Sounds, images and textures resolve through it; an id that is not
    /// here, or whose file is missing, is `RE9350` when it is used.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub assets: BTreeMap<String, String>,
    /// Particles plus beam and trail segments drawn per frame (default
    /// [`DEFAULT_EFFECT_BUDGET`]); the excess is `RE9351`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub effect_budget: Option<u32>,
    /// How many players join when the game starts (`players = 2`); the first
    /// is the client's local player. One unless set.
    #[serde(default = "one_player")]
    pub players: u32,
}

fn one_player() -> u32 {
    1
}

impl Default for CompatConfig {
    fn default() -> Self {
        CompatConfig {
            api: CompatApi::default(),
            signal_behavior: Default::default(),
            game: GameInfo::default(),
            seed: 0,
            assets: BTreeMap::new(),
            effect_budget: None,
            players: one_player(),
        }
    }
}

fn is_zero(n: &u64) -> bool {
    *n == 0
}

/// The `[compat.game]` section: the values scripts read from `game`
/// (`PlaceId`, `JobId`, ...) and `RunService:IsStudio()`. The defaults are
/// those of an unpublished place in a Studio play test.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(default, deny_unknown_fields)]
pub struct GameInfo {
    /// `game.PlaceId`.
    pub place_id: i64,
    /// `game.GameId`.
    pub game_id: i64,
    /// `game.CreatorId`.
    pub creator_id: i64,
    /// `game.JobId`.
    pub job_id: String,
    /// `game.PrivateServerId`.
    pub private_server_id: String,
    /// `game.PrivateServerOwnerId`.
    pub private_server_owner_id: i64,
    /// `RunService:IsStudio()`.
    pub studio: bool,
}

impl Default for GameInfo {
    fn default() -> Self {
        GameInfo {
            place_id: 0,
            game_id: 0,
            creator_id: 0,
            job_id: String::new(),
            private_server_id: String::new(),
            private_server_owner_id: 0,
            studio: true,
        }
    }
}

impl GameInfo {
    /// True for the Studio-like defaults, which `real.toml` leaves out.
    pub fn is_default(&self) -> bool {
        *self == GameInfo::default()
    }
}

/// The effect budget when `[compat] effect_budget` is absent.
pub const DEFAULT_EFFECT_BUDGET: u32 = 20_000;

/// The one key of an asset id in [`CompatConfig::assets`]: `rbxassetid://N`
/// and the asset URL form become the bare `N`; any other id is kept as given.
pub fn asset_key(id: &str) -> &str {
    let id = id.trim();
    id.strip_prefix("rbxassetid://")
        .or_else(|| id.strip_prefix("http://www.roblox.com/asset/?id="))
        .or_else(|| id.strip_prefix("https://www.roblox.com/asset/?id="))
        .unwrap_or(id)
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
                let cfg: CompatConfig = v.clone().try_into().map_err(|e: toml::de::Error| {
                    CompatError::Scene(format!("real.toml [compat]: {e}"))
                })?;
                cfg.validated().map(Some)
            }
        }
    }

    /// The section with its asset keys normalised ([`asset_key`]), refusing
    /// two keys for one id, a path that is absolute or leaves the project,
    /// and an effect budget of 0.
    fn validated(mut self) -> Result<CompatConfig, CompatError> {
        if self.players == 0 {
            return Err(CompatError::Runtime {
                code: crate::codes::PLAYER_STEP,
                message: "real.toml [compat]: players must be at least 1".into(),
            });
        }
        let bad = |m: String| CompatError::Scene(format!("real.toml [compat]: {m}"));
        let mut assets = BTreeMap::new();
        for (id, path) in std::mem::take(&mut self.assets) {
            let key = asset_key(&id).to_string();
            if key.is_empty() {
                return Err(bad(format!("assets: the id {id:?} is empty")));
            }
            let p = std::path::Path::new(&path);
            if path.is_empty()
                || p.is_absolute()
                || path.starts_with('/')
                || path.starts_with('\\')
                || p.components().any(|c| {
                    !matches!(
                        c,
                        std::path::Component::Normal(_) | std::path::Component::CurDir
                    )
                })
                || path.split(['/', '\\']).any(|part| part == "..")
            {
                return Err(bad(format!(
                    "assets: {id:?} = {path:?} must be a path inside the project, \
                     relative and without `..`"
                )));
            }
            if assets.insert(key.clone(), path).is_some() {
                return Err(bad(format!(
                    "assets: {id:?} and another entry name the same id; \
                     \"rbxassetid://{key}\" and \"{key}\" are one id, give it once"
                )));
            }
        }
        self.assets = assets;
        if self.effect_budget == Some(0) {
            return Err(bad("effect_budget must be at least 1".into()));
        }
        Ok(self)
    }

    /// The effect budget: `effect_budget`, or [`DEFAULT_EFFECT_BUDGET`].
    pub fn effect_budget(&self) -> u32 {
        self.effect_budget.unwrap_or(DEFAULT_EFFECT_BUDGET)
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
        if self.seed != 0 {
            out.push_str(&format!("seed = {}\n", self.seed));
        }
        if self.players != 1 {
            out.push_str(&format!("players = {}\n", self.players));
        }
        if let Some(b) = self.effect_budget {
            out.push_str(&format!("effect_budget = {b}\n"));
        }
        // Tables last: plain `key = value` lines go above this point, or they
        // would land inside `[compat.game]`.
        if self.game != GameInfo::default() {
            let g = toml::to_string(&self.game).expect("plain values serialize");
            out.push_str("\n[compat.game]\n");
            out.push_str(&g);
        }
        if !self.assets.is_empty() {
            out.push_str("\n[compat.assets]\n");
            for (id, path) in &self.assets {
                out.push_str(&format!(
                    "{} = {}\n",
                    toml::Value::String(id.clone()),
                    toml::Value::String(path.clone())
                ));
            }
        }
        out
    }
}

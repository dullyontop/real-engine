//! Playing a compat project (`real.toml` `[compat]`, docs/M2_PLAN.md WP16):
//! the Roblox-style layer of `re_compat` in a server and a client context.
//!
//! When the game first runs a frame, [`CompatPlay::start`] splits the App's
//! World into two ([`re_compat::CompatWorlds`]): the App keeps the
//! authoritative server world (physics runs there, and it is what is saved
//! and hashed) and [`CompatPlay`] holds the local player's client world, which
//! is what is rendered. Each context gets a [`CompatVm`]; the server's
//! scripts start, the `[compat] players` join, then the client's scripts
//! start. A frame follows WP12's order:
//!
//! ```text
//! pump(server)  server phases (with physics)  replicate  pump(client)  client phases
//! ```
//!
//! The client's phases run once per fixed step the server ran, without
//! physics; then tweens and sounds advance and the sound commands go to the
//! mixer. Native scripts (`re_script`'s `game` context) do not run in a
//! compat project: its scripts are the instances' `ScriptRef`s.

use std::collections::{BTreeMap, HashMap};
use std::sync::Arc;

use glam::{Quat, Vec3};
use re_compat::players::{self, PlayerSpec};
use re_compat::runtime::scripts::StartOptions;
use re_compat::{CompatConfig, CompatSetup, CompatVm, CompatWorlds, ReplicationSet};
use re_core::{Diagnostic, Location};
use re_scene::World;
use re_script::{ContextKind, OutputLine, RuntimeConfig, ScriptHost};

use crate::codes;
use crate::config::AppConfig;

/// The part of a park label that counts as stuck when a run ends
/// (`<path>:WaitForChild("<name>")`): a thread still waiting in
/// `WaitForChild` never got the instance it needs.
pub const STUCK_LABEL: &str = ":WaitForChild(";

/// The seed of the server world's id generator (the client world's has its
/// own, in `re_compat::net`).
const SERVER_ID_SEED: u64 = 0x7365_7276_6572_0001;

/// The `[compat]` section of `config`, if the project has one.
///
/// # Errors
///
/// `RE0501` when the section does not read as a [`CompatConfig`].
pub fn compat_config(config: &AppConfig) -> Result<Option<CompatConfig>, Diagnostic> {
    let Some(table) = &config.compat else {
        return Ok(None);
    };
    toml::Value::Table(table.clone())
        .try_into::<CompatConfig>()
        .map(Some)
        .map_err(|e| {
            Diagnostic::error(
                codes::CONFIG_INVALID,
                format!("the [compat] section of real.toml is invalid: {e}"),
            )
            .at(Location::file(crate::config::CONFIG_FILE))
        })
}

/// What a compat run reports beyond a native one ([`crate::RunReport::compat`]).
#[derive(Clone, Debug, Default, PartialEq, serde::Serialize)]
pub struct CompatRunInfo {
    /// Errors scripts raised and did not catch, with their context
    /// (`server` or `client`), in the order they were raised.
    pub uncaught: Vec<(String, Diagnostic)>,
    /// Threads still parked in `WaitForChild` at the end:
    /// `(context, what it waits for, the waiting thread)`.
    pub stuck_yields: Vec<(String, String, String)>,
    /// Members scripts reached that the layer does not have (`RE9001`), with
    /// how often, over both contexts.
    pub unsupported: BTreeMap<String, u32>,
}

/// The compat side of a running game.
pub struct CompatPlay {
    /// The local player's world, the one that is rendered.
    pub client: World,
    set: ReplicationSet,
    pub(crate) server_vm: CompatVm,
    pub(crate) client_vm: CompatVm,
    /// The players joined so far (`Player1` to `Player<players>`).
    pub(crate) players: u32,
    uncaught: Vec<(ContextKind, Diagnostic)>,
}

impl std::fmt::Debug for CompatPlay {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("CompatPlay").finish_non_exhaustive()
    }
}

/// The setup every compat run shares (the class layer and its bindings),
/// built once per process.
pub fn setup() -> Result<Arc<CompatSetup>, Diagnostic> {
    static SETUP: std::sync::OnceLock<Result<Arc<CompatSetup>, Diagnostic>> =
        std::sync::OnceLock::new();
    SETUP
        .get_or_init(|| CompatSetup::standard().map_err(Diagnostic::from))
        .clone()
}

impl CompatPlay {
    /// Splits `world` (the App's, which becomes the server world) and makes
    /// the two contexts; `persist_data` is [`App::persist_data`](crate::App::persist_data). Nothing runs yet: [`CompatPlay::start_scripts`]
    /// starts the game.
    ///
    /// # Errors
    ///
    /// The scene breaks the compat conventions, or a VM cannot be made.
    pub fn new(
        files: &re_core::files::ProjectFiles,
        config: &AppConfig,
        compat: &CompatConfig,
        world: &mut World,
        clip_seconds: HashMap<String, f64>,
        persist_data: bool,
    ) -> Result<CompatPlay, Diagnostic> {
        let setup = setup()?;
        // Instances scripts make get ids from a fixed sequence, so a run
        // and its replay end with the same state hash.
        // The split works on a fork, so a scene that breaks the compat
        // conventions leaves `world` as it was.
        let mut server = world.fork();
        server
            .ecs
            .insert_resource(re_scene::IdGen::seeded(SERVER_ID_SEED));
        install_default_scripts(&mut server, &setup.classes)?;
        let worlds = CompatWorlds::new(&setup, server)?;
        let CompatWorlds {
            server,
            mut client,
            set,
        } = worlds;
        *world = server;
        let runtime = RuntimeConfig::with_files(files.clone()).with_limits(
            config.scripts.cpu_budget_ms.max(0.0).ceil() as u64,
            config.scripts.memory_limit_mb as usize,
        );
        let server_vm =
            CompatVm::with_config(&setup, ContextKind::Server, world, &runtime, compat)?;
        let client_vm =
            CompatVm::with_config(&setup, ContextKind::Client, &mut client, &runtime, compat)?;
        let step = 1.0 / f64::from(config.sim.fixed_hz.max(1));
        let data = re_compat::services::data::DataSettings::from_config_in(
            &compat.data,
            files,
            step,
            persist_data,
        )
        .map_err(|m| Diagnostic::error(re_compat::codes::DATA_FILE, m))?;
        re_compat::services::data::configure(server_vm.context().lua(), data);
        let clips = Arc::new(clip_seconds);
        for vm in [&server_vm, &client_vm] {
            let clips = Arc::clone(&clips);
            let lookup: Arc<re_compat::sound::ClipSecondsFn> =
                Arc::new(move |path: &str| clips.get(path).copied());
            re_compat::sound::configure(
                vm.context().lua(),
                re_compat::sound::SoundSettings::from_config(compat, Some(lookup)),
            );
            re_compat::anim::set_substitutes(vm.context().lua(), compat.animations.clone());
            let model_files = files.clone();
            let load: Arc<re_compat::render::LoadModelFn> =
                Arc::new(move |path: &str| load_model(&model_files, path));
            re_compat::render::configure_models(
                vm.context().lua(),
                re_compat::render::ModelSettings::from_config(compat, Some(load)),
            );
        }
        Ok(CompatPlay {
            client,
            set,
            server_vm,
            client_vm,
            players: 0,
            uncaught: Vec::new(),
        })
    }

    /// The VM of a context.
    pub fn vm(&self, kind: ContextKind) -> &CompatVm {
        match kind {
            ContextKind::Client => &self.client_vm,
            _ => &self.server_vm,
        }
    }

    /// Keeps a context's script errors for [`CompatPlay::info`] and returns
    /// them.
    pub fn keep(&mut self, kind: ContextKind, diags: Vec<Diagnostic>) -> Vec<Diagnostic> {
        for d in diags.iter().filter(|d| d.is_error()) {
            // A callback that fails every frame is one entry.
            if !self.uncaught.iter().any(|(k, u)| *k == kind && u == d) {
                self.uncaught.push((kind, d.clone()));
            }
        }
        diags
    }

    /// One replication pass, server to client.
    pub fn replicate(&mut self, server: &mut World) {
        re_compat::net::replicate(server, &mut self.client, &self.set);
    }

    /// What the run reports: uncaught errors, stuck yields and unsupported
    /// members.
    pub fn info(&self) -> CompatRunInfo {
        let name = |k: ContextKind| k.name().to_string();
        let mut stuck = Vec::new();
        for (kind, vm) in [
            (ContextKind::Server, &self.server_vm),
            (ContextKind::Client, &self.client_vm),
        ] {
            let waits = re_script::sched::parked_waits(vm.context().lua()).unwrap_or_default();
            for (label, thread) in waits {
                if label.contains(STUCK_LABEL) {
                    stuck.push((name(kind), label, thread));
                }
            }
        }
        let mut unsupported = self.server_vm.unsupported();
        for (k, n) in self.client_vm.unsupported() {
            *unsupported.entry(k).or_default() += n;
        }
        CompatRunInfo {
            uncaught: self
                .uncaught
                .iter()
                .map(|(k, d)| (name(*k), d.clone()))
                .collect(),
            stuck_yields: stuck,
            unsupported,
        }
    }

    /// Drains what both contexts printed.
    pub fn take_output(&self) -> Vec<OutputLine> {
        let mut out = self.server_vm.take_output();
        out.extend(self.client_vm.take_output());
        out
    }
}

/// Starts a context's scripts.
pub fn start_scripts(vm: &CompatVm, host: &mut dyn ScriptHost) -> Vec<Diagnostic> {
    vm.start_scripts(
        host,
        StartOptions {
            templates_as_player: false,
        },
    )
}

/// Joins player `n` (1-based; player 1 is the local player).
pub fn add_player(
    vm: &CompatVm,
    host: &mut dyn ScriptHost,
    n: u32,
) -> (Result<re_scene::Entity, Diagnostic>, Vec<Diagnostic>) {
    players::add_player(vm, host, &PlayerSpec::numbered(n))
}

/// The client's listener for this frame, and the sound commands its
/// scripts queued, for the mixer.
pub fn client_sounds(vm: &CompatVm, listener: Option<(Vec3, Quat)>) -> Vec<re_types::AudioCmd> {
    let lua = vm.context().lua();
    if let Some((p, r)) = listener {
        re_compat::sound::set_listener(lua, p, r);
    }
    re_compat::sound::take_commands(lua)
}

/// Adds the default player and character scripts
/// ([`re_compat::humanoid::scripts::DEFAULT_SCRIPTS`]) that the place does
/// not replace with its own (a child of the same name in the same
/// container), once the class layer has parts and `Workspace.CurrentCamera`.
/// Their `ScriptRef`s name `.real/defaults/`, which the engine layer serves
/// ([`crate::package::engine_layer`]); nothing is written to the project.
pub fn install_default_scripts(
    world: &mut World,
    classes: &re_compat::ClassRegistry,
) -> Result<(), Diagnostic> {
    use re_compat::instance::tree;
    let fail = |m: String| Diagnostic::error(codes::BOOT_FAILED, m);
    let Ok(game) = world.entity(re_compat::GAME_ID) else {
        return Ok(());
    };
    // The defaults drive a character made of parts and the camera; until the
    // class layer has parts, `Workspace.CurrentCamera` and the camera's own
    // members (WP15), a character is an empty `Model` and they would only
    // wait for it, or fail on the camera.
    let needs = [
        ("Workspace", "CurrentCamera"),
        ("Camera", "CameraType"),
        ("Camera", "CameraSubject"),
        ("Camera", "CFrame"),
        ("Camera", "Focus"),
    ];
    if classes.class("Part").is_err() || needs.iter().any(|(c, m)| classes.member(c, m).is_err()) {
        return Ok(());
    }
    // A starter character the project names but ships no rig in gets the
    // engine's rig, so its players and the scripts that clone it have one.
    re_compat::humanoid::rig::fill_starter_character(world, classes, game)
        .map_err(|e| fail(format!("cannot fill StarterPlayer.StarterCharacter: {e}")))?;
    for script in re_compat::humanoid::scripts::DEFAULT_SCRIPTS {
        let Some(starter) = tree::find_child(world, game, "StarterPlayer", false) else {
            continue;
        };
        let Some(container) = tree::find_child(world, starter, script.container, false) else {
            continue;
        };
        if tree::find_child(world, container, script.name, false).is_some() {
            continue;
        }
        let rel = crate::package::default_script_path(script.container, script.name);
        let e = re_compat::scene::spawn_instance(
            world,
            classes,
            script.class,
            None,
            script.name,
            Some(container),
        )
        .map_err(|e| fail(format!("cannot add the default {}: {e}", script.name)))?;
        world.ecs.entity_mut(e).insert(re_scene::ScriptRef {
            path: rel,
            context: re_compat::script_context(script.class, None)
                .unwrap_or(re_scene::ScriptContext::Server),
        });
    }
    Ok(())
}

/// A `MeshPart` model from a `.glb` (or a `.gltf` with embedded buffers) of
/// the project: the meshes of its first skin that has any, with that skin,
/// or else every mesh of the default scene with its node transforms baked
/// in.
///
/// # Errors
///
/// The file is missing or unreadable, or does not import.
pub fn load_model(
    files: &re_core::files::ProjectFiles,
    path: &str,
) -> Result<re_compat::render::Model, String> {
    use re_compat::render::{Model, ModelMesh};
    let bytes = files.read(path).map_err(|e| e.to_string())?.bytes;
    let g = re_assets::import_gltf(&bytes, None, re_assets::AssetId::new_source(), path)
        .map_err(|d| d.message.clone())?;
    for (i, skin) in g.skins.iter().enumerate() {
        let meshes: Vec<ModelMesh> = g
            .skinned_meshes(i)
            .into_iter()
            .map(|m| ModelMesh {
                data: g.meshes[m].mesh.clone(),
                weights: g.meshes[m].skin_weights.clone(),
            })
            .collect();
        if !meshes.is_empty() {
            return Ok(Model::new(meshes, Some(skin.clone())));
        }
    }
    let mut meshes = Vec::new();
    let mut stack: Vec<(usize, glam::Mat4)> =
        g.roots.iter().map(|&r| (r, glam::Mat4::IDENTITY)).collect();
    while let Some((n, parent)) = stack.pop() {
        let node = &g.nodes[n];
        let world = parent
            * glam::Mat4::from_scale_rotation_translation(
                node.scale,
                node.rotation,
                node.translation,
            );
        let normal = glam::Mat3::from_mat4(world).inverse().transpose();
        for &m in &node.meshes {
            let mut data = g.meshes[m].mesh.clone();
            for v in &mut data.vertices {
                v.pos = world.transform_point3(Vec3::from(v.pos)).to_array();
                v.normal = (normal * Vec3::from(v.normal))
                    .normalize_or_zero()
                    .to_array();
            }
            meshes.push(ModelMesh {
                data,
                weights: None,
            });
        }
        stack.extend(node.children.iter().map(|&c| (c, world)));
    }
    Ok(Model::new(meshes, None))
}

#[cfg(test)]
mod tests {
    use re_core::files::{Form, LayerKind, MemLayer, ProjectFiles, ProjectPath};

    use super::*;

    /// A triangle (0,0,0), (2,0,0), (2,1,0) drawn by a node under a parent
    /// node at x = 10, as a `.glb`.
    fn triangle_glb() -> Vec<u8> {
        let mut bin: Vec<u8> = [0.0f32, 0.0, 0.0, 2.0, 0.0, 0.0, 2.0, 1.0, 0.0]
            .iter()
            .flat_map(|f| f.to_le_bytes())
            .collect();
        bin.extend([0u16, 1, 2, 0].iter().flat_map(|i| i.to_le_bytes()));
        let json = r#"{"asset":{"version":"2.0"},"scene":0,"scenes":[{"nodes":[0]}],
"nodes":[{"name":"Root","translation":[10,0,0],"children":[1]},{"name":"Tri","mesh":0}],
"meshes":[{"primitives":[{"attributes":{"POSITION":0},"indices":1}]}],
"accessors":[{"bufferView":0,"componentType":5126,"count":3,"type":"VEC3","min":[0,0,0],"max":[2,1,0]},
{"bufferView":1,"componentType":5123,"count":3,"type":"SCALAR"}],
"bufferViews":[{"buffer":0,"byteOffset":0,"byteLength":36},{"buffer":0,"byteOffset":36,"byteLength":6}],
"buffers":[{"byteLength":44}]}"#;
        let mut j = json.as_bytes().to_vec();
        while !j.len().is_multiple_of(4) {
            j.push(b' ');
        }
        let mut out = Vec::new();
        out.extend_from_slice(b"glTF");
        out.extend_from_slice(&2u32.to_le_bytes());
        out.extend_from_slice(&((12 + 8 + j.len() + 8 + bin.len()) as u32).to_le_bytes());
        out.extend_from_slice(&(j.len() as u32).to_le_bytes());
        out.extend_from_slice(b"JSON");
        out.extend_from_slice(&j);
        out.extend_from_slice(&(bin.len() as u32).to_le_bytes());
        out.extend_from_slice(b"BIN\0");
        out.extend_from_slice(&bin);
        out
    }

    #[test]
    fn a_model_without_a_skin_bakes_its_node_transforms() {
        let mut layer = MemLayer::new(LayerKind::Project);
        layer.insert(
            ProjectPath::new("assets/tri.glb").unwrap(),
            triangle_glb(),
            Form::Source,
        );
        let files = ProjectFiles::for_tests(layer);
        let m = load_model(&files, "assets/tri.glb").unwrap();
        assert!(m.skin.is_none());
        assert_eq!(m.meshes.len(), 1);
        assert!(m.meshes[0].weights.is_none());
        assert_eq!(m.bounds.min, Vec3::new(10.0, 0.0, 0.0));
        assert_eq!(m.bounds.max, Vec3::new(12.0, 1.0, 0.0));
        assert!(load_model(&files, "assets/gone.glb").is_err());
    }
}

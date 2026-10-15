//! The compatibility runtime: [`CompatVm`], one `server` or `client` script
//! context bound to one world, and [`Bindings`], where each area of the layer
//! registers its behaviour.
//!
//! A VM is built from a [`CompatSetup`] (the class registry plus the bindings,
//! built once and shared by the server and client VMs) and a world that
//! follows the compat scene conventions. At creation it:
//! * registers the class layer's components and the event observers in the
//!   world, and turns on the world's attribute and tag change feeds (the VM is
//!   their one consumer);
//! * creates the VM ([`re_script::Context::compat`]) with the globals of every
//!   area (`game`, `workspace`, `Instance`, ...) before the sandbox freezes
//!   them;
//! * makes the instance events ([`crate::signal::events`]) dispatch at the
//!   end of every resumption cycle (the scheduler's settle hook).
//!
//! The VM keeps no reference to the world: every entry point takes the host
//! ([`ScriptHost`]) for the duration of the call, as the native runtime does.

mod codec;

use std::collections::HashMap;
use std::rc::Rc;
use std::sync::Arc;

use mlua::{Lua, MultiValue, Value as LuaValue};
use re_core::{Code, Diagnostic};
use re_scene::{Entity, World};
use re_script::{Context, ContextKind, OutputLine, RuntimeConfig, ScriptHost, sched};

pub use codec::{EnumFromLuaFn, EnumToLuaFn, FromUserdataFn, ToLuaFn, ValueCodec};

use crate::class::{Accessor, ClassMarker, ClassRegistry, ClassRegistryBuilder, Converter};
use crate::data::Status;
use crate::error::CompatError;
use crate::signal::SignalBehavior;
use crate::{GAME_ID, ROOT_CLASS};

/// A method's implementation: `(lua, self, arguments after self)`. A yielding
/// method returns the values [`re_script::sched::park`] gave it to park its
/// caller.
pub type MethodFn = dyn Fn(&Lua, Entity, MultiValue) -> mlua::Result<MultiValue> + Send + Sync;

/// Makes the value of a global when a VM is created.
pub type GlobalFn = dyn Fn(&Lua, &GlobalEnv) -> mlua::Result<LuaValue> + Send + Sync;

/// What a global's constructor knows about the VM being created.
#[derive(Clone, Copy, Debug)]
pub struct GlobalEnv {
    /// `server` or `client`.
    pub kind: ContextKind,
    /// The `game` root.
    pub game: Entity,
    /// The `Workspace` service.
    pub workspace: Entity,
}

#[derive(Clone)]
pub(crate) struct MethodImpl {
    pub(crate) f: Arc<MethodFn>,
    pub(crate) yields: bool,
}

/// What the areas of the layer register: property accessors and converters
/// (for the class registry), method implementations, globals and value
/// conversions. [`Bindings::standard`] collects every area's registrations.
pub struct Bindings {
    builder: Option<ClassRegistryBuilder>,
    methods: HashMap<(String, String), MethodImpl>,
    globals: Vec<(String, Arc<GlobalFn>)>,
    /// The Luau ↔ value conversions.
    pub codec: ValueCodec,
}

impl Bindings {
    /// Bindings over the classes of `builder`, with nothing registered yet.
    pub fn new(builder: ClassRegistryBuilder) -> Bindings {
        Bindings {
            builder: Some(builder),
            methods: HashMap::new(),
            globals: Vec::new(),
            codec: ValueCodec::default(),
        }
    }

    /// The built-in data files with every area's registrations.
    pub fn standard() -> Result<Bindings, CompatError> {
        let mut b = Bindings::new(ClassRegistryBuilder::builtin()?);
        crate::instance::register(&mut b);
        crate::services::core::register(&mut b);
        crate::datatypes::register(&mut b);
        crate::tween::register(&mut b);
        crate::sound::register(&mut b);
        crate::gui::register(&mut b);
        Ok(b)
    }

    fn with_builder(&mut self, f: impl FnOnce(ClassRegistryBuilder) -> ClassRegistryBuilder) {
        let b = self
            .builder
            .take()
            .expect("the builder is present until build");
        self.builder = Some(f(b));
    }

    /// Registers the accessor of a builtin property.
    pub fn accessor(&mut self, class: &str, property: &str, a: Accessor) {
        self.with_builder(|b| b.accessor(class, property, a));
    }

    /// Registers a converter for `"via": name` properties.
    pub fn converter(&mut self, name: &str, c: Converter) {
        self.with_builder(|b| b.converter(name, c));
    }

    /// Changes the class registry builder (to add data files in tests).
    pub fn classes(&mut self, f: impl FnOnce(ClassRegistryBuilder) -> ClassRegistryBuilder) {
        self.with_builder(f);
    }

    /// Registers the implementation of method `name` of `class`.
    pub fn method(&mut self, class: &str, name: &str, f: Arc<MethodFn>) {
        self.methods.insert(
            (class.to_string(), name.to_string()),
            MethodImpl { f, yields: false },
        );
    }

    /// Registers a method that may park its caller ([`re_script::sched::park`]).
    pub fn yielding_method(&mut self, class: &str, name: &str, f: Arc<MethodFn>) {
        self.methods.insert(
            (class.to_string(), name.to_string()),
            MethodImpl { f, yields: true },
        );
    }

    /// Registers a global, made when each VM is created.
    pub fn global(&mut self, name: &str, f: Arc<GlobalFn>) {
        self.globals.push((name.to_string(), f));
    }
}

/// The class registry and the bindings, built once and shared by every VM.
pub struct CompatSetup {
    /// The class registry, with every accessor and converter.
    pub classes: Arc<ClassRegistry>,
    methods: HashMap<(String, String), MethodImpl>,
    globals: Vec<(String, Arc<GlobalFn>)>,
    /// The Luau ↔ value conversions.
    pub codec: ValueCodec,
}

impl std::fmt::Debug for CompatSetup {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("CompatSetup")
            .field("methods", &self.methods.len())
            .field("globals", &self.globals.len())
            .finish()
    }
}

impl CompatSetup {
    /// The standard setup ([`Bindings::standard`]).
    pub fn standard() -> Result<Arc<CompatSetup>, CompatError> {
        CompatSetup::new(Bindings::standard()?)
    }

    /// Builds the class registry and checks the bindings against it: every
    /// `implemented` or `partial` method has an implementation, and every
    /// implementation belongs to a declared method.
    pub fn new(mut b: Bindings) -> Result<Arc<CompatSetup>, CompatError> {
        let classes = b.builder.take().expect("builder present").build()?;
        for c in classes.classes() {
            for m in &c.methods {
                if matches!(m.status, Status::Implemented | Status::Partial)
                    && !b.methods.contains_key(&(c.name.clone(), m.name.clone()))
                {
                    return Err(CompatError::InvalidData(format!(
                        "{}.{} is {} but has no implementation",
                        c.name,
                        m.name,
                        m.status.as_str()
                    )));
                }
            }
        }
        for (class, name) in b.methods.keys() {
            let ok = classes
                .try_class(class)
                .is_some_and(|c| c.methods.iter().any(|m| &m.name == name));
            if !ok {
                return Err(CompatError::InvalidData(format!(
                    "an implementation is registered for {class}.{name}, which is not a declared method"
                )));
            }
        }
        Ok(Arc::new(CompatSetup {
            classes: Arc::new(classes),
            methods: b.methods,
            globals: b.globals,
            codec: b.codec,
        }))
    }

    pub(crate) fn method(&self, class: &str, name: &str) -> Option<&MethodImpl> {
        self.methods.get(&(class.to_string(), name.to_string()))
    }
}

/// What every function of the layer reads about its VM.
pub(crate) struct State {
    pub(crate) kind: ContextKind,
    pub(crate) behavior: SignalBehavior,
    pub(crate) game: Entity,
    pub(crate) setup: Arc<CompatSetup>,
}

/// The VM's [`State`].
pub(crate) fn state(lua: &Lua) -> mlua::Result<Rc<State>> {
    lua.app_data_ref::<Rc<State>>()
        .map(|s| s.clone())
        .ok_or_else(|| mlua::Error::runtime("this VM is not a compat VM"))
}

/// A Luau error carrying `code` (`[RE####] message`).
pub(crate) fn lua_error(code: Code, message: impl std::fmt::Display) -> mlua::Error {
    mlua::Error::runtime(format!("[{code}] {message}"))
}

/// A [`CompatError`] as a Luau error carrying its code.
pub(crate) fn compat_error(e: CompatError) -> mlua::Error {
    lua_error(e.code(), e)
}

/// Runs `f` with the world of the bound host.
pub(crate) fn with_world<R>(lua: &Lua, f: impl FnOnce(&mut World) -> R) -> mlua::Result<R> {
    re_script::host::with_host(lua, |h| Ok(f(h.world())))
}

/// One `server` or `client` script context of the compatibility layer, bound
/// to one world.
pub struct CompatVm {
    ctx: Context,
    state: Rc<State>,
}

impl std::fmt::Debug for CompatVm {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("CompatVm")
            .field("kind", &self.state.kind)
            .finish()
    }
}

impl CompatVm {
    /// Creates the VM of context `kind` for `world`, which must hold a compat
    /// scene (a `game` root of class `DataModel`). Missing core services that
    /// the globals name (`Workspace`) are created.
    ///
    /// # Errors
    ///
    /// `kind` is not a compat context, the world has no `game` root (`RE9004`),
    /// or a global failed to build.
    pub fn new(
        setup: &Arc<CompatSetup>,
        kind: ContextKind,
        world: &mut World,
        config: &RuntimeConfig,
        behavior: SignalBehavior,
    ) -> Result<CompatVm, Diagnostic> {
        let classes = &setup.classes;
        classes.register_components(world)?;
        crate::signal::events::prepare_world(world);
        let game = world.entity(GAME_ID).map_err(|_| {
            CompatError::Scene(format!("the world has no root entity with id {GAME_ID:?}"))
        })?;
        if world.ecs.get::<ClassMarker>(game).map(|m| m.class.as_str()) != Some(ROOT_CLASS) {
            return Err(
                CompatError::Scene(format!("{GAME_ID:?} must be a {ROOT_CLASS} instance")).into(),
            );
        }
        let workspace = crate::services::core::ensure_service(world, classes, game, "Workspace")?;
        let state = Rc::new(State {
            kind,
            behavior,
            game,
            setup: setup.clone(),
        });
        let env = GlobalEnv {
            kind,
            game,
            workspace,
        };
        let install = |lua: &Lua| -> mlua::Result<()> {
            lua.set_app_data(state.clone());
            crate::instance::install(lua)?;
            crate::signal::install(lua)?;
            crate::signal::events::install(lua)?;
            let globals = lua.globals();
            for (name, f) in &setup.globals {
                globals.raw_set(name.as_str(), f(lua, &env)?)?;
            }
            Ok(())
        };
        let ctx = Context::compat(config, kind, &install).map_err(|e| e.to_diagnostic())?;
        sched::set_settle_hook(ctx.lua(), Rc::new(crate::signal::events::settle))
            .map_err(|e| re_script::ScriptError::from_mlua(kind.name(), &e).to_diagnostic())?;
        Ok(CompatVm { ctx, state })
    }

    /// `server` or `client`.
    pub fn kind(&self) -> ContextKind {
        self.state.kind
    }

    /// The underlying script context.
    pub fn context(&self) -> &Context {
        &self.ctx
    }

    /// The setup the VM was built from.
    pub fn setup(&self) -> &Arc<CompatSetup> {
        &self.state.setup
    }

    /// Binds `host`, runs `f`, then settles (deferred threads and instance
    /// events) and returns the diagnostics of every error raised meanwhile.
    pub fn run(
        &self,
        host: &mut dyn ScriptHost,
        f: impl FnOnce(&Lua) -> mlua::Result<()>,
    ) -> Vec<Diagnostic> {
        self.ctx.run(host, f)
    }

    /// Runs Luau `source` as a new script thread (it may yield; it continues
    /// in later steps). `name` labels it in errors. Its `script` global is nil.
    pub fn exec(&self, host: &mut dyn ScriptHost, name: &str, source: &str) -> Vec<Diagnostic> {
        self.run(host, |lua| {
            let env = re_script::vm::environment(lua, LuaValue::Nil)?;
            let f = lua
                .load(source)
                .set_name(format!("={name}"))
                .set_environment(env)
                .into_function()?;
            sched::spawn(lua, f, MultiValue::new(), name)?;
            Ok(())
        })
    }

    /// Advances the simulation time by `dt` seconds, resumes the threads that
    /// are due and settles. The runtime's phase driver calls the finer steps.
    pub fn step(&self, host: &mut dyn ScriptHost, dt: f64) -> Vec<Diagnostic> {
        self.run(host, |lua| {
            sched::advance(lua, dt)?;
            sched::resume_due(lua)?;
            Ok(())
        })
    }

    /// Drains what `print` and `warn` wrote.
    pub fn take_output(&self) -> Vec<OutputLine> {
        self.ctx.take_output()
    }

    /// The labels of the threads parked in yielding calls (`WaitForChild`,
    /// `Signal:Wait`), in park order.
    pub fn parked(&self) -> Vec<String> {
        sched::parked_labels(self.ctx.lua()).unwrap_or_default()
    }

    /// The `game` root.
    pub fn game(&self) -> Entity {
        self.state.game
    }
}

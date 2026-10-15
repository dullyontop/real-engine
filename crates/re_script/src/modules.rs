//! `require` confined to the project's `scripts/` tree, and module hot reload.
//!
//! mlua's default `require` resolves against the process working directory and
//! accepts absolute paths, so a script could load any file on the machine. The
//! engine installs [`ProjectRequirer`] instead (through
//! `Lua::create_require_function`). It navigates a virtual path relative to the
//! project root:
//!
//! * `require("./sibling")` and `require("../shared/util")`, relative to the
//!   requiring file (Luau's require-by-string rules);
//! * `require("@self/child")` from an `init.luau`, which stands for its folder;
//! * aliases from `.luaurc` files, such as the project's
//!   `{ "aliases": { "shared": "./scripts/shared" } }`. The type checker reads the
//!   same file, so both agree.
//!
//! Navigation may climb to the project root (that is where `.luaurc` lives), never
//! above it. A module that resolves outside `scripts/` is refused with `RE3004`,
//! and so is one whose real path (after symlinks) leaves `scripts/`.

use std::cell::RefCell;
use std::collections::BTreeSet;
use std::io::Result as IoResult;
use std::path::{Component, Path, PathBuf};
use std::rc::Rc;
use std::result::Result as StdResult;

use mlua::luau::{NavigateError, Require};
use mlua::{Function, Lua, Result, Table, Value};

use crate::codes;
use crate::vm::ContextKind;

/// mlua's registry table of loaded modules, keyed by [`Require::cache_key`].
pub(crate) const LOADER_CACHE: &str = "__MLUA_LOADER_CACHE";

/// Where `require` in an `exec` snippet resolves from.
const EXEC_LOCATION: &str = "scripts/__exec__.luau";

/// The module keys (project-relative paths with `/`) loaded in one context.
pub(crate) type Loaded = Rc<RefCell<BTreeSet<String>>>;

/// A project path as a `/`-separated string (the chunk name and cache key).
pub(crate) fn slash(p: &Path) -> String {
    p.components()
        .map(|c| c.as_os_str().to_string_lossy())
        .collect::<Vec<_>>()
        .join("/")
}

/// The `require` resolver of one context.
pub struct ProjectRequirer {
    root: Rc<PathBuf>,
    kind: ContextKind,
    /// The current virtual location, relative to the root (a file or a folder).
    rel: PathBuf,
    /// The module file `rel` resolves to.
    resolved: Option<PathBuf>,
    loaded: Loaded,
}

impl ProjectRequirer {
    pub(crate) fn new(root: PathBuf, kind: ContextKind, loaded: Loaded) -> ProjectRequirer {
        ProjectRequirer {
            root: Rc::new(root),
            kind,
            rel: PathBuf::new(),
            resolved: None,
            loaded,
        }
    }

    fn resolve(&self, rel: &Path) -> StdResult<Option<PathBuf>, NavigateError> {
        let mut found = Vec::new();
        if rel.file_name().is_some_and(|n| n != "init") {
            for ext in ["luau", "lua"] {
                let candidate = rel.with_extension(ext);
                if self.root.join(&candidate).is_file() {
                    found.push(candidate);
                }
            }
        }
        if self.root.join(rel).is_dir() {
            for init in ["init.luau", "init.lua"] {
                let candidate = rel.join(init);
                if self.root.join(&candidate).is_file() {
                    found.push(candidate);
                }
            }
        }
        match found.len() {
            0 => Ok(None),
            1 => Ok(found.pop()),
            _ => Err(NavigateError::Ambiguous),
        }
    }

    fn set(&mut self, rel: PathBuf) -> StdResult<(), NavigateError> {
        self.resolved = self.resolve(&rel)?;
        self.rel = rel;
        Ok(())
    }
}

/// A chunk name such as `@scripts/game/main.luau` as a relative path; anything
/// absolute or with `..` is refused, so a hand-made name cannot point outside.
pub(crate) fn chunk_to_rel(chunk_name: &str) -> Option<PathBuf> {
    let name = chunk_name.strip_prefix('@')?;
    let path = Path::new(name);
    (path.components().all(|c| matches!(c, Component::Normal(_))) && name.starts_with("scripts/"))
        .then(|| path.to_path_buf())
}

impl Require for ProjectRequirer {
    fn is_require_allowed(&self, chunk_name: &str) -> bool {
        chunk_name.starts_with('=') || chunk_to_rel(chunk_name).is_some()
    }

    fn reset(&mut self, chunk_name: &str) -> StdResult<(), NavigateError> {
        // Code run with `exec` (chunk `=name`) requires as if it were a file
        // directly in scripts/.
        let mut rel = if chunk_name.starts_with('=') {
            PathBuf::from(EXEC_LOCATION)
        } else {
            chunk_to_rel(chunk_name).ok_or(NavigateError::NotFound)?
        };
        // An init module stands for its folder: `@self/x` is inside the folder and
        // `./x` is a sibling of the folder.
        if rel
            .file_name()
            .is_some_and(|n| n == "init.luau" || n == "init.lua")
        {
            rel.pop();
        }
        self.resolved = Some(rel.clone());
        self.rel = rel;
        Ok(())
    }

    fn jump_to_alias(&mut self, path: &str) -> StdResult<(), NavigateError> {
        // Only called for alias values that do not resolve relative to their
        // `.luaurc`; treat them as relative to the project root.
        let rel: PathBuf = Path::new(path)
            .components()
            .filter(|c| !matches!(c, Component::CurDir))
            .collect();
        if !rel.components().all(|c| matches!(c, Component::Normal(_))) {
            return Err(NavigateError::NotFound);
        }
        self.set(rel)
    }

    fn to_parent(&mut self) -> StdResult<(), NavigateError> {
        let mut rel = self.rel.clone();
        if !rel.pop() {
            return Err(NavigateError::NotFound); // already at the project root
        }
        self.set(rel)
    }

    fn to_child(&mut self, name: &str) -> StdResult<(), NavigateError> {
        if name.contains(['/', '\\']) || name == ".." || name == "." || name.is_empty() {
            return Err(NavigateError::NotFound);
        }
        self.set(self.rel.join(name))
    }

    fn has_module(&self) -> bool {
        self.resolved
            .as_ref()
            .is_some_and(|p| self.root.join(p).is_file())
    }

    fn cache_key(&self) -> String {
        self.resolved.as_deref().map(slash).unwrap_or_default()
    }

    fn has_config(&self) -> bool {
        let dir = self.root.join(&self.rel);
        dir.is_dir() && dir.join(".luaurc").is_file()
    }

    fn config(&self) -> IoResult<Vec<u8>> {
        std::fs::read(self.root.join(&self.rel).join(".luaurc"))
    }

    fn loader(&self, lua: &Lua) -> Result<Function> {
        let rel = self
            .resolved
            .clone()
            .ok_or_else(|| mlua::Error::runtime("module not resolved"))?;
        let key = slash(&rel);
        let abs = self.root.join(&rel);
        let scripts = self.root.join("scripts");
        let inside = match (abs.canonicalize(), scripts.canonicalize()) {
            (Ok(a), Ok(s)) => a.starts_with(s),
            _ => false,
        };
        if !key.starts_with("scripts/") || !inside {
            return Err(crate::api::error(
                codes::REQUIRE,
                format!("require of {key:?} is refused: modules must be inside scripts/"),
            ));
        }
        let source = std::fs::read(&abs).map_err(|e| {
            crate::api::error(codes::SCRIPT_READ, format!("cannot read {key}: {e}"))
        })?;
        self.loaded.borrow_mut().insert(key.clone());
        let env = crate::vm::new_env(lua, &key, self.kind, None)?;
        lua.load(source)
            .set_name(format!("@{key}"))
            .set_compiler(crate::compile::compiler())
            .set_environment(env)
            .into_function()
    }
}

/// What a hot reload did to one module.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Reload {
    /// The chunk returned the table it was given (`local M = ... or {}`): its
    /// functions were redefined on the same table and its state kept, so every
    /// holder of the module sees the new code.
    InPlace,
    /// The chunk built a new value. Later `require` calls get it; code that
    /// already holds the old value keeps running the old version.
    Replaced,
}

/// Reloads module `key` from `source`, giving the chunk its current value as
/// `...`. A compile or runtime error leaves the old version in place.
pub(crate) fn reload_module(lua: &Lua, key: &str, source: &[u8], env: Table) -> Result<Reload> {
    let chunk = lua
        .load(source)
        .set_name(format!("@{key}"))
        .set_compiler(crate::compile::compiler())
        .set_environment(env)
        .into_function()?;
    let cache: Table = lua.named_registry_value(LOADER_CACHE)?;
    let old: Value = cache.raw_get(key)?;
    let fresh: Value = crate::scheduler::call_budgeted(lua, || chunk.call(old.clone()))?;
    // mlua stores `true` for a module that returns nothing.
    let fresh = if fresh.is_nil() {
        Value::Boolean(true)
    } else {
        fresh
    };
    if fresh == old {
        return Ok(Reload::InPlace);
    }
    cache.raw_set(key, fresh)?;
    Ok(Reload::Replaced)
}

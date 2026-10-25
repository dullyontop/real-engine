//! Shader files from the project, and from the engine's shader directory, on
//! the renderer (ADR-0038).
//!
//! The project's `shaders/*.wgsl` are read through [`ProjectFiles`] when the app
//! opens, in every run (a packaged game reads them from its package). A file
//! named like one of the engine's ([`re_render::shader::SOURCES`]) replaces it;
//! any other is a new file for shaders to `#include`. A development run
//! ([`App::enable_hot_reload`]) watches them, and the directory that
//! `RE_ENGINE_SHADERS` names ([`App::watch_engine_shaders`]; engine developers
//! point it at `crates/re_render/src/shaders`), whose files replace the built-in
//! ones beneath the project's. The renderer rebuilds what a change touches and
//! keeps its last good pipelines when a file does not build
//! ([`Renderer::reload_shaders`]); the problems are the app's diagnostics, at
//! the file that has them. `real verify` checks the project's files without a
//! GPU ([`check`]).
//!
//! [`App::enable_hot_reload`]: crate::App::enable_hot_reload
//! [`App::watch_engine_shaders`]: crate::App::watch_engine_shaders

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use re_assets::AssetChange;
use re_core::files::ProjectFiles;
use re_core::{Diagnostic, Location};
use re_render::Renderer;
use re_render::shader::{ShaderSources, is_engine_file};

/// The project folder whose `*.wgsl` files the renderer uses.
pub const SHADER_DIR: &str = "shaders";

/// The environment variable that names the engine's shader directory, which a
/// development run reads and watches.
pub const ENGINE_SHADERS_VAR: &str = "RE_ENGINE_SHADERS";

/// The name of the shader file at the project path `path`: a `.wgsl` file
/// directly in `shaders/`.
pub fn project_name(path: &str) -> Option<&str> {
    let name = path.strip_prefix(SHADER_DIR)?.strip_prefix('/')?;
    file_name(name)
}

/// `name` when it is a `.wgsl` file name (not a path).
fn file_name(name: &str) -> Option<&str> {
    (name.len() > ".wgsl".len() && name.ends_with(".wgsl") && !name.contains(['/', '\\']))
        .then_some(name)
}

/// `real verify`'s shader checks, which need no GPU: a warning (`RE6033`) for
/// each project file that replaces an engine file, and the problems the
/// renderer would report for every shader the project's files change (`RE6030`
/// and `RE6031`). Also returns how many shader files the project has.
pub fn check(files: &ProjectFiles) -> (usize, Vec<Diagnostic>) {
    let (shaders, mut problems) = ShaderFiles::load(files);
    let mut sources = ShaderSources::default();
    for (name, source) in &shaders.project {
        if is_engine_file(name) {
            let path = format!("{SHADER_DIR}/{name}");
            problems.push(
                Diagnostic::warning(
                    re_render::codes::SHADER_REPLACED,
                    format!("`{path}` replaces the engine's `{name}`"),
                )
                .at(Location::file(&path))
                .with_hint(
                    "the engine's shader files can change in any release; remove the file once \
                     the change is in the engine, or rename it if it is a new file to include",
                ),
            );
        }
        sources.set(name, Some(source.clone()));
    }
    problems.extend(sources.check().into_iter().map(|d| shaders.locate(d)));
    (shaders.project.len(), problems)
}

/// The shader files the app gives the renderer, besides its built-in ones.
#[derive(Clone, Debug, Default)]
pub(crate) struct ShaderFiles {
    /// The project's `shaders/*.wgsl`, by file name.
    project: BTreeMap<String, String>,
    /// The engine shader directory's `*.wgsl`, by file name.
    engine: BTreeMap<String, String>,
    /// That directory, as `RE_ENGINE_SHADERS` named it.
    engine_dir: Option<PathBuf>,
}

impl ShaderFiles {
    /// Reads the project's shader files. One that cannot be read is reported
    /// (`RE6031`) and left out.
    pub(crate) fn load(files: &ProjectFiles) -> (ShaderFiles, Vec<Diagnostic>) {
        let mut shaders = ShaderFiles::default();
        let mut problems = Vec::new();
        for path in files.list(SHADER_DIR, ".wgsl") {
            if let Some(name) = project_name(path.as_str()) {
                problems.extend(shaders.read_project(files, name));
            }
        }
        (shaders, problems)
    }

    /// Reads the project file `name` again (after a change): a file that is
    /// gone is dropped, and one that cannot be read keeps its last version.
    pub(crate) fn read_project(&mut self, files: &ProjectFiles, name: &str) -> Option<Diagnostic> {
        let path = format!("{SHADER_DIR}/{name}");
        match files.read_str(&path) {
            Ok(source) => {
                self.project.insert(name.to_owned(), source);
                None
            }
            Err(e) if e.is_not_found() => {
                self.project.remove(name);
                None
            }
            Err(e) => Some(unreadable(&path, &e)),
        }
    }

    /// Reads every `*.wgsl` file of the engine shader directory `dir`, in place
    /// of the files read from any earlier one.
    pub(crate) fn read_engine_dir(&mut self, dir: &Path) -> Vec<Diagnostic> {
        self.engine_dir = Some(dir.to_path_buf());
        self.engine.clear();
        let mut problems = Vec::new();
        let mut names: Vec<String> = match std::fs::read_dir(dir) {
            Ok(entries) => entries
                .filter_map(|e| e.ok()?.file_name().into_string().ok())
                .filter(|n| file_name(n).is_some())
                .collect(),
            Err(e) => {
                problems.push(unreadable(&slash(dir), &e));
                Vec::new()
            }
        };
        names.sort();
        for name in names {
            problems.extend(self.read_engine(&name));
        }
        problems
    }

    /// Applies the changes the engine shader directory's watcher reported.
    pub(crate) fn engine_changed(&mut self, changes: &[AssetChange]) -> Vec<Diagnostic> {
        let mut names: Vec<&str> = Vec::new();
        for change in changes {
            match change {
                AssetChange::Created(p) | AssetChange::Modified(p) | AssetChange::Removed(p) => {
                    names.push(p);
                }
                AssetChange::Renamed { from, to } => names.extend([from.as_str(), to.as_str()]),
            }
        }
        names.retain(|n| file_name(n).is_some());
        names.sort_unstable();
        names.dedup();
        names
            .into_iter()
            .filter_map(|n| self.read_engine(n))
            .collect()
    }

    fn read_engine(&mut self, name: &str) -> Option<Diagnostic> {
        let dir = self.engine_dir.as_ref()?;
        let path = dir.join(name);
        match std::fs::read_to_string(&path) {
            Ok(source) => {
                self.engine.insert(name.to_owned(), source);
                None
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                self.engine.remove(name);
                None
            }
            Err(e) => Some(unreadable(&slash(&path), &e)),
        }
    }

    /// Gives `renderer` these files in place of the ones it has set, and
    /// rebuilds what that changes. Returns the problems, at the files that have
    /// them.
    pub(crate) fn install(&self, renderer: &mut Renderer) -> Vec<Diagnostic> {
        let mut files: BTreeMap<&str, &str> = self
            .engine
            .iter()
            .map(|(n, s)| (n.as_str(), s.as_str()))
            .collect();
        files.extend(self.project.iter().map(|(n, s)| (n.as_str(), s.as_str())));
        // Files an earlier owner of the renderer set (another project's).
        let stale: Vec<String> = renderer
            .shader_sources()
            .set_files()
            .map(|(name, _)| name)
            .filter(|name| !files.contains_key(name))
            .map(str::to_owned)
            .collect();
        if files.is_empty() && stale.is_empty() {
            return Vec::new();
        }
        for name in stale {
            renderer.set_shader_source(&name, None);
        }
        for (name, source) in files {
            renderer.set_shader_source(name, Some(source.to_owned()));
        }
        let report = renderer.reload_shaders();
        if !report.rebuilt.is_empty() {
            tracing::info!(rebuilt = ?report.rebuilt, "shaders rebuilt");
        }
        report
            .problems
            .into_iter()
            .map(|d| self.locate(d))
            .collect()
    }

    /// `d` with its file locations, and its related diagnostics', at the files a
    /// run read: `shaders/<name>` for the project's files and the engine
    /// directory's path for its own. Built-in files keep their names.
    fn locate(&self, mut d: Diagnostic) -> Diagnostic {
        if let Some(Location::File { file, .. }) = &mut d.location {
            if self.project.contains_key(file.as_str()) {
                *file = format!("{SHADER_DIR}/{file}");
            } else if let Some(dir) = &self.engine_dir
                && self.engine.contains_key(file.as_str())
            {
                *file = slash(&dir.join(file.as_str()));
            }
        }
        d.related = std::mem::take(&mut d.related)
            .into_iter()
            .map(|r| self.locate(r))
            .collect();
        d
    }
}

/// `RE6031` for a shader file that cannot be read.
fn unreadable(path: &str, error: &dyn std::fmt::Display) -> Diagnostic {
    Diagnostic::error(
        re_render::codes::SHADER_COMPILE,
        format!("`{path}` cannot be read: {error}"),
    )
    .at(Location::file(path))
}

fn slash(path: &Path) -> String {
    path.to_string_lossy().replace('\\', "/")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_wgsl_files_directly_in_the_folder_are_shader_files() {
        assert_eq!(project_name("shaders/sky.wgsl"), Some("sky.wgsl"));
        assert_eq!(project_name("shaders/old/sky.wgsl"), None);
        assert_eq!(project_name("shaders/sky.wgsl.bak"), None);
        assert_eq!(project_name("shaders/.wgsl"), None);
        assert_eq!(project_name("shadersx/sky.wgsl"), None);
        assert_eq!(project_name("scripts/sky.wgsl"), None);
    }

    #[test]
    fn locations_name_the_file_a_run_read() {
        let mut shaders = ShaderFiles::default();
        shaders.project.insert("sky.wgsl".into(), String::new());
        shaders.engine.insert("post.wgsl".into(), String::new());
        shaders.engine_dir = Some(PathBuf::from("crates/re_render/src/shaders"));
        let at = |file: &str| {
            let d = Diagnostic::error(re_render::codes::SHADER_COMPILE, "x")
                .at(Location::file_at(file, 3, 4))
                .with_related(
                    Diagnostic::note(re_render::codes::SHADER_COMPILE, "y")
                        .at(Location::file(file)),
                );
            let d = shaders.locate(d);
            let file_of = |l: &Option<Location>| match l {
                Some(Location::File { file, .. }) => file.clone(),
                _ => panic!("no file"),
            };
            (file_of(&d.location), file_of(&d.related[0].location))
        };
        let both = |f: &str| (f.to_owned(), f.to_owned());
        assert_eq!(at("sky.wgsl"), both("shaders/sky.wgsl"));
        assert_eq!(
            at("post.wgsl"),
            both("crates/re_render/src/shaders/post.wgsl")
        );
        assert_eq!(at("forward.wgsl"), both("forward.wgsl"));
    }
}

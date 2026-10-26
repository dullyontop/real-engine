//! Project templates and generator recipes as data (ADR-0046).
//!
//! * A **template** is a directory: `template.toml` and the files a new
//!   project starts with. Files named in its `render` list have `{{param}}`
//!   replaced; nothing else is evaluated, so a template cannot run code. A
//!   template may list recipes to apply after the copy, and checks.
//! * A **recipe** is one TOML file: parameters, steps (each one registered
//!   command with arguments) and checks. Steps can only call registered
//!   commands at `read` or `edit` tier, so a recipe can do nothing an agent
//!   or `real do` could not, and the policy checks every step.
//! * A **check** is read-only: `real verify` passes (or one stage does), a
//!   read-tier command's result at a JSON pointer, or the `verify.diff`
//!   against the state before the steps ran.
//!
//! The built-in templates and recipes are the repository's `templates/`
//! directory, embedded by this crate's build script. [`apply_recipe`] runs a
//! recipe through any command runner, so `re_cli` (the `recipe.apply`
//! command, `real new`) and tests share it.
//!
//! Codes `RE8019`–`RE8025`.

use std::collections::BTreeMap;
use std::path::Path;

use re_command::{CommandOutcome, CommandSpec, Op, Tier};
use re_core::{Diagnostic, Location};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value as Json, json};

use crate::codes;

/// The built-in files: `(path under templates/, contents)`.
mod builtin {
    include!(concat!(env!("OUT_DIR"), "/templates.rs"));
}

/// The parameter every template has: the project name.
pub const NAME_PARAM: &str = "name";

/// The type of a parameter.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "lowercase")]
pub enum ParamKind {
    /// Any text.
    String,
    /// A whole number.
    Int,
    /// A number.
    Float,
    /// `true` or `false`.
    Bool,
    /// An RGBA colour: three or four numbers from 0 to 1 (alpha defaults
    /// to 1).
    Color,
}

impl ParamKind {
    /// Its name in `template.toml`.
    pub fn as_str(self) -> &'static str {
        match self {
            ParamKind::String => "string",
            ParamKind::Int => "int",
            ParamKind::Float => "float",
            ParamKind::Bool => "bool",
            ParamKind::Color => "color",
        }
    }
}

/// One parameter of a template or recipe.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Param {
    /// The name, used as `{{name}}`.
    pub name: String,
    /// Its type.
    #[serde(rename = "type")]
    pub kind: ParamKind,
    /// The value when none is given; a parameter without one is required.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub default: Option<Json>,
    /// What it does, for people and agents.
    #[serde(default)]
    pub doc: String,
    /// The smallest allowed value (numbers).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub min: Option<f64>,
    /// The largest allowed value (numbers).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max: Option<f64>,
}

/// One step of a recipe: a registered command and its arguments.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Step {
    /// The command's dotted name (`scene.apply`).
    pub command: String,
    /// Its arguments, with `{{param}}` in strings.
    #[serde(default = "empty_object")]
    pub args: Json,
}

fn empty_object() -> Json {
    Json::Object(Map::new())
}

/// A check as written in TOML: exactly one of the kinds below.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Check {
    /// `"ok"`: `real verify` passes with every stage ok.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub verify: Option<String>,
    /// That verify stage passes.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub verify_stage: Option<String>,
    /// A read-tier command to run.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub command: Option<String>,
    /// Its arguments.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub args: Option<Json>,
    /// A JSON pointer into its outcome (`{ ok, changes, diagnostics, data }`),
    /// such as `/data/total`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pointer: Option<String>,
    /// The value there equals this.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub equals: Option<Json>,
    /// The number there is at least this.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub at_least: Option<f64>,
    /// The number there is at most this.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub at_most: Option<f64>,
    /// The string there contains this, or the array there contains this
    /// value.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub contains: Option<Json>,
    /// This verify stage changed (or did not) since the steps began.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub diff_stage: Option<String>,
    /// With `diff_stage`: whether it must have changed (default true).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub changed: Option<bool>,
    /// No verify stage that passed before the steps fails after them.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub no_regressions: Option<bool>,
}

/// A check, validated.
#[derive(Clone, Debug, PartialEq)]
pub enum CheckKind {
    /// Every verify stage passes.
    Verify,
    /// One verify stage passes.
    VerifyStage(String),
    /// A read-tier command's outcome at a pointer.
    Command {
        /// The command.
        command: String,
        /// Its arguments.
        args: Json,
        /// Where to look.
        pointer: String,
        /// What must hold there.
        expect: Expect,
    },
    /// A stage changed (`true`) or did not (`false`) in the verify diff.
    DiffStage(String, bool),
    /// No stage regressed in the verify diff.
    NoRegressions,
}

/// What a command check expects at its pointer.
#[derive(Clone, Debug, PartialEq)]
pub enum Expect {
    /// Equal to the value.
    Equals(Json),
    /// A number at least this.
    AtLeast(f64),
    /// A number at most this.
    AtMost(f64),
    /// A string containing it, or an array with the value.
    Contains(Json),
}

impl Check {
    /// The check's kind, or why it is malformed (`RE8019`).
    ///
    /// # Errors
    ///
    /// `RE8019` unless exactly one kind is given, with its fields.
    pub fn kind(&self) -> Result<CheckKind, String> {
        let mut kinds = Vec::new();
        if let Some(v) = &self.verify {
            if v != "ok" {
                return Err(format!("`verify` must be \"ok\", not {v:?}"));
            }
            kinds.push(CheckKind::Verify);
        }
        if let Some(stage) = &self.verify_stage {
            kinds.push(CheckKind::VerifyStage(stage.clone()));
        }
        if let Some(command) = &self.command {
            let pointer = self
                .pointer
                .clone()
                .ok_or_else(|| format!("the check on `{command}` needs a `pointer`"))?;
            let mut expects = Vec::new();
            if let Some(v) = &self.equals {
                expects.push(Expect::Equals(v.clone()));
            }
            if let Some(v) = self.at_least {
                expects.push(Expect::AtLeast(v));
            }
            if let Some(v) = self.at_most {
                expects.push(Expect::AtMost(v));
            }
            if let Some(v) = &self.contains {
                expects.push(Expect::Contains(v.clone()));
            }
            if expects.len() != 1 {
                return Err(format!(
                    "the check on `{command}` needs exactly one of `equals`, `at_least`, `at_most`, `contains`"
                ));
            }
            kinds.push(CheckKind::Command {
                command: command.clone(),
                args: self.args.clone().unwrap_or_else(empty_object),
                pointer,
                expect: expects.remove(0),
            });
        } else if self.pointer.is_some()
            || self.equals.is_some()
            || self.at_least.is_some()
            || self.at_most.is_some()
            || self.contains.is_some()
            || self.args.is_some()
        {
            return Err(
                "`pointer`, `args` and the expectations belong to a `command` check".into(),
            );
        }
        if let Some(stage) = &self.diff_stage {
            kinds.push(CheckKind::DiffStage(
                stage.clone(),
                self.changed.unwrap_or(true),
            ));
        } else if self.changed.is_some() {
            return Err("`changed` belongs to a `diff_stage` check".into());
        }
        match self.no_regressions {
            Some(true) => kinds.push(CheckKind::NoRegressions),
            Some(false) => {
                return Err("`no_regressions = false` checks nothing; leave it out".into());
            }
            None => {}
        }
        match kinds.len() {
            1 => Ok(kinds.remove(0)),
            0 => Err("a check needs one of `verify`, `verify_stage`, `command`, `diff_stage`, `no_regressions`".into()),
            _ => Err("a check has exactly one of `verify`, `verify_stage`, `command`, `diff_stage`, `no_regressions`".into()),
        }
    }

    /// One line for people: what the check checks.
    pub fn describe(&self) -> String {
        match self.kind() {
            Ok(CheckKind::Verify) => "verify passes".into(),
            Ok(CheckKind::VerifyStage(s)) => format!("verify stage `{s}` passes"),
            Ok(CheckKind::Command {
                command,
                pointer,
                expect,
                ..
            }) => {
                let what = match expect {
                    Expect::Equals(v) => format!("equals {v}"),
                    Expect::AtLeast(v) => format!("is at least {v}"),
                    Expect::AtMost(v) => format!("is at most {v}"),
                    Expect::Contains(v) => format!("contains {v}"),
                };
                format!("`{command}` {pointer} {what}")
            }
            Ok(CheckKind::DiffStage(s, true)) => format!("verify stage `{s}` changed"),
            Ok(CheckKind::DiffStage(s, false)) => format!("verify stage `{s}` did not change"),
            Ok(CheckKind::NoRegressions) => "no verify stage regressed".into(),
            Err(e) => format!("malformed check: {e}"),
        }
    }
}

/// A recipe applied by a template, with its arguments.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct RecipeUse {
    /// The recipe's name.
    pub recipe: String,
    /// Its arguments, with `{{param}}` naming the template's parameters.
    #[serde(default = "empty_object")]
    pub args: Json,
}

/// A recipe (`templates/recipes/<name>.toml`).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Recipe {
    /// Its name (the file name without `.toml`).
    pub name: String,
    /// One sentence saying what it makes.
    pub summary: String,
    /// Its parameters.
    #[serde(default)]
    pub params: Vec<Param>,
    /// The commands it runs, in order.
    pub steps: Vec<Step>,
    /// What must hold afterwards.
    #[serde(default)]
    pub checks: Vec<Check>,
}

/// `template.toml`.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct TemplateManifest {
    /// Its name (the directory name).
    pub name: String,
    /// One sentence saying what the project starts as.
    pub summary: String,
    /// Its parameters (besides `name`, which every template has).
    #[serde(default)]
    pub params: Vec<Param>,
    /// Files (relative to the template) whose `{{param}}` are replaced.
    #[serde(default)]
    pub render: Vec<String>,
    /// Empty folders the project starts with.
    #[serde(default)]
    pub folders: Vec<String>,
    /// Recipes applied after the copy, in order.
    #[serde(default)]
    pub recipes: Vec<RecipeUse>,
    /// What must hold for a new project (run by the tests and
    /// `real new --check`).
    #[serde(default)]
    pub checks: Vec<Check>,
    /// A built-in generator that writes the files instead (`roblox-place`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub generator: Option<String>,
}

/// A template: its manifest and files.
#[derive(Clone, Debug, PartialEq)]
pub struct Template {
    /// The manifest.
    pub manifest: TemplateManifest,
    /// Its files, by path relative to the template (not `template.toml`).
    pub files: BTreeMap<String, Vec<u8>>,
}

/// The templates and recipes on offer.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Library {
    /// The templates, by name.
    pub templates: BTreeMap<String, Template>,
    /// The recipes, by name.
    pub recipes: BTreeMap<String, Recipe>,
}

fn invalid(file: &str, message: impl Into<String>) -> Diagnostic {
    Diagnostic::error(codes::INVALID_TEMPLATE, message).at(Location::file(file))
}

/// Parses a recipe file named `<name>.toml`.
///
/// # Errors
///
/// `RE8019` when it is not a valid recipe.
pub fn parse_recipe(file: &str, text: &str) -> Result<Recipe, Diagnostic> {
    let recipe: Recipe = toml::from_str(text).map_err(|e| invalid(file, format!("{file}: {e}")))?;
    let stem = Path::new(file)
        .file_stem()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_default();
    if recipe.name != stem {
        return Err(invalid(
            file,
            format!(
                "{file} names the recipe `{}`; it must be `{stem}`, the file name",
                recipe.name
            ),
        ));
    }
    validate_params(file, &recipe.params, false)?;
    if recipe.steps.is_empty() {
        return Err(invalid(file, format!("{file} has no steps")));
    }
    for check in &recipe.checks {
        check
            .kind()
            .map_err(|e| invalid(file, format!("{file}: {e}")))?;
    }
    // Every `{{param}}` in the steps and checks names a parameter.
    let probe: Map<String, Json> = recipe
        .params
        .iter()
        .map(|p| (p.name.clone(), Json::Null))
        .collect();
    for step in &recipe.steps {
        render_json(&step.args, &probe)
            .map_err(|d| invalid(file, format!("{file}: {}", d.message)))?;
    }
    Ok(recipe)
}

/// Parses a template's manifest and takes its files.
///
/// # Errors
///
/// `RE8019` when the manifest is invalid or names files that are missing.
pub fn parse_template(
    dir_name: &str,
    manifest: &str,
    files: BTreeMap<String, Vec<u8>>,
) -> Result<Template, Diagnostic> {
    let file = format!("{dir_name}/template.toml");
    let manifest: TemplateManifest =
        toml::from_str(manifest).map_err(|e| invalid(&file, format!("{file}: {e}")))?;
    if manifest.name != dir_name {
        return Err(invalid(
            &file,
            format!(
                "{file} names the template `{}`; it must be `{dir_name}`, the directory name",
                manifest.name
            ),
        ));
    }
    validate_params(&file, &manifest.params, true)?;
    for check in &manifest.checks {
        check
            .kind()
            .map_err(|e| invalid(&file, format!("{file}: {e}")))?;
    }
    for path in &manifest.render {
        if !files.contains_key(path) {
            return Err(invalid(
                &file,
                format!("{file} renders `{path}`, which the template does not have"),
            ));
        }
    }
    if manifest.generator.is_some() && !files.is_empty() {
        return Err(invalid(
            &file,
            format!("{file} has a generator and files; a generated template has only its manifest"),
        ));
    }
    for path in files.keys().chain(&manifest.folders) {
        if !safe_relative(path) {
            return Err(invalid(
                &file,
                format!("{file}: `{path}` must be a relative path inside the project"),
            ));
        }
    }
    Ok(Template { manifest, files })
}

/// True for a relative path with no `..`, root or drive.
fn safe_relative(path: &str) -> bool {
    let p = Path::new(path);
    !path.is_empty()
        && !path.contains('\\')
        && p.components()
            .all(|c| matches!(c, std::path::Component::Normal(_)))
}

fn validate_params(file: &str, params: &[Param], template: bool) -> Result<(), Diagnostic> {
    let mut seen = std::collections::BTreeSet::new();
    for p in params {
        let ident = p
            .name
            .chars()
            .next()
            .is_some_and(|c| c.is_ascii_alphabetic() || c == '_')
            && p.name
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || c == '_');
        if !ident {
            return Err(invalid(
                file,
                format!(
                    "{file}: parameter `{}` must be letters, digits and `_`",
                    p.name
                ),
            ));
        }
        if template && p.name == NAME_PARAM {
            return Err(invalid(
                file,
                format!(
                    "{file}: `{NAME_PARAM}` is every template's project name; do not declare it"
                ),
            ));
        }
        if !seen.insert(p.name.as_str()) {
            return Err(invalid(
                file,
                format!("{file}: parameter `{}` is declared twice", p.name),
            ));
        }
        if let Some(d) = &p.default {
            check_value(p, d)
                .map_err(|e| invalid(file, format!("{file}: the default of `{}` {e}", p.name)))?;
        }
    }
    Ok(())
}

impl Library {
    /// The built-in templates and recipes (the repository's `templates/`).
    ///
    /// # Errors
    ///
    /// `RE8019` for an invalid built-in file (a test keeps them valid).
    pub fn builtin() -> Result<Library, Diagnostic> {
        Library::from_files(
            builtin::FILES
                .iter()
                .map(|(p, b)| ((*p).to_string(), b.to_vec())),
        )
    }

    /// A library from `(path under templates/, contents)` pairs:
    /// `recipes/<name>.toml` and `<template>/...`.
    ///
    /// # Errors
    ///
    /// `RE8019` for an invalid file.
    pub fn from_files(
        files: impl IntoIterator<Item = (String, Vec<u8>)>,
    ) -> Result<Library, Diagnostic> {
        let mut library = Library::default();
        let mut dirs: BTreeMap<String, BTreeMap<String, Vec<u8>>> = BTreeMap::new();
        for (path, bytes) in files {
            if path == "README.md" {
                continue;
            }
            if let Some(file) = path.strip_prefix("recipes/") {
                if !file.ends_with(".toml") || file.contains('/') {
                    continue;
                }
                let text = String::from_utf8(bytes)
                    .map_err(|_| invalid(&path, format!("{path} is not UTF-8")))?;
                let recipe = parse_recipe(&path, &text)?;
                library.recipes.insert(recipe.name.clone(), recipe);
            } else if let Some((dir, rest)) = path.split_once('/') {
                dirs.entry(dir.to_string())
                    .or_default()
                    .insert(rest.to_string(), bytes);
            }
        }
        for (dir, mut files) in dirs {
            let manifest = files
                .remove("template.toml")
                .ok_or_else(|| invalid(&dir, format!("{dir}/ has no template.toml")))?;
            let manifest = String::from_utf8(manifest)
                .map_err(|_| invalid(&dir, format!("{dir}/template.toml is not UTF-8")))?;
            let template = parse_template(&dir, &manifest, files)?;
            library.templates.insert(dir, template);
        }
        for template in library.templates.values() {
            for used in &template.manifest.recipes {
                if !library.recipes.contains_key(&used.recipe) {
                    return Err(invalid(
                        &template.manifest.name,
                        format!(
                            "template `{}` applies the unknown recipe `{}`",
                            template.manifest.name, used.recipe
                        ),
                    ));
                }
            }
        }
        Ok(library)
    }

    /// Reads a template directory from disk (`real new --template PATH`).
    ///
    /// # Errors
    ///
    /// `RE8019` when it cannot be read or is invalid.
    pub fn read_template_dir(dir: &Path) -> Result<Template, Diagnostic> {
        let shown = dir.display().to_string();
        let name = dir
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .ok_or_else(|| invalid(&shown, format!("{shown} is not a template directory")))?;
        let mut files = BTreeMap::new();
        let mut stack = vec![dir.to_path_buf()];
        while let Some(d) = stack.pop() {
            let entries = std::fs::read_dir(&d)
                .map_err(|e| invalid(&shown, format!("cannot read {}: {e}", d.display())))?;
            for entry in entries {
                let entry = entry
                    .map_err(|e| invalid(&shown, format!("cannot read {}: {e}", d.display())))?;
                let path = entry.path();
                let kind = entry
                    .file_type()
                    .map_err(|e| invalid(&shown, format!("cannot read {}: {e}", path.display())))?;
                // A link could bring in a file from outside the template.
                if kind.is_symlink() {
                    return Err(invalid(
                        &shown,
                        format!(
                            "{} is a link; a template holds only its own files",
                            path.display()
                        ),
                    ));
                }
                if kind.is_dir() {
                    stack.push(path);
                } else {
                    let rel = path
                        .strip_prefix(dir)
                        .unwrap_or(&path)
                        .components()
                        .map(|c| c.as_os_str().to_string_lossy().into_owned())
                        .collect::<Vec<_>>()
                        .join("/");
                    let bytes = std::fs::read(&path).map_err(|e| {
                        invalid(&shown, format!("cannot read {}: {e}", path.display()))
                    })?;
                    files.insert(rel, bytes);
                }
            }
        }
        let manifest = files
            .remove("template.toml")
            .ok_or_else(|| invalid(&shown, format!("{shown} has no template.toml")))?;
        let manifest = String::from_utf8(manifest)
            .map_err(|_| invalid(&shown, format!("{shown}/template.toml is not UTF-8")))?;
        parse_template(&name, &manifest, files)
    }

    /// The recipe called `name` (`RE8020`).
    ///
    /// # Errors
    ///
    /// `RE8020` with the closest name as a hint.
    pub fn recipe(&self, name: &str) -> Result<&Recipe, Diagnostic> {
        self.recipes
            .get(name)
            .ok_or_else(|| unknown("recipe", name, self.recipes.keys().map(String::as_str)))
    }

    /// The template called `name` (`RE8020`).
    ///
    /// # Errors
    ///
    /// `RE8020` with the closest name as a hint.
    pub fn template(&self, name: &str) -> Result<&Template, Diagnostic> {
        self.templates
            .get(name)
            .ok_or_else(|| unknown("template", name, self.templates.keys().map(String::as_str)))
    }
}

fn unknown<'a>(what: &str, name: &str, known: impl Iterator<Item = &'a str> + Clone) -> Diagnostic {
    let list = known.clone().collect::<Vec<_>>().join(", ");
    let d = Diagnostic::error(codes::UNKNOWN_TEMPLATE, format!("unknown {what} `{name}`"));
    match re_reflect::suggest(name, known) {
        Some(s) => d.with_hint(format!("did you mean `{s}`? The {what}s are: {list}")),
        None => d.with_hint(format!("the {what}s are: {list}")),
    }
}

/// Checks `value` against `param`'s type and range; the reason on failure.
fn check_value(param: &Param, value: &Json) -> Result<(), String> {
    let number = |v: &Json| v.as_f64();
    match param.kind {
        ParamKind::String if value.is_string() => {}
        ParamKind::Bool if value.is_boolean() => {}
        ParamKind::Int if value.is_i64() || value.is_u64() => {}
        ParamKind::Float if value.is_number() => {}
        ParamKind::Color => {
            let ok = value.as_array().is_some_and(|a| {
                (a.len() == 3 || a.len() == 4)
                    && a.iter()
                        .all(|c| c.as_f64().is_some_and(|c| (0.0..=1.0).contains(&c)))
            });
            if !ok {
                return Err(format!(
                    "must be a colour [r, g, b] or [r, g, b, a] with values from 0 to 1, not {value}"
                ));
            }
        }
        kind => {
            return Err(format!(
                "must be {}, not {value}",
                match kind {
                    ParamKind::String => "a string",
                    ParamKind::Bool => "true or false",
                    ParamKind::Int => "a whole number",
                    _ => "a number",
                }
            ));
        }
    }
    if let Some(n) = number(value) {
        if let Some(min) = param.min
            && n < min
        {
            return Err(format!("must be at least {min}, not {n}"));
        }
        if let Some(max) = param.max
            && n > max
        {
            return Err(format!("must be at most {max}, not {n}"));
        }
    }
    Ok(())
}

/// Turns a command-line value (`--set seed=7`) into JSON for `param`: a
/// number, `true`/`false`, a JSON array for colours, or the text itself for
/// strings.
pub fn parse_cli_value(param: &Param, text: &str) -> Json {
    match param.kind {
        ParamKind::String => Json::String(text.to_string()),
        _ => serde_json::from_str(text).unwrap_or_else(|_| Json::String(text.to_string())),
    }
}

/// The values for `params` from `given`: defaults filled in, colours with
/// alpha, every value checked (`RE8020` unknown or missing, `RE8021` wrong
/// type or out of range). `extra` names accepted besides `params` (a
/// template's `name`).
///
/// # Errors
///
/// As above.
pub fn resolve_params(
    params: &[Param],
    given: &Map<String, Json>,
    extra: &[&str],
) -> Result<Map<String, Json>, Diagnostic> {
    for key in given.keys() {
        if !extra.contains(&key.as_str()) && !params.iter().any(|p| &p.name == key) {
            let names = params
                .iter()
                .map(|p| p.name.as_str())
                .chain(extra.iter().copied());
            let mut d = Diagnostic::error(
                codes::UNKNOWN_TEMPLATE,
                format!("unknown parameter `{key}`"),
            );
            let list = names.clone().collect::<Vec<_>>().join(", ");
            d = match re_reflect::suggest(key, names) {
                Some(s) => d.with_hint(format!("did you mean `{s}`? The parameters are: {list}")),
                None => d.with_hint(format!(
                    "the parameters are: {}",
                    if list.is_empty() { "none" } else { &list }
                )),
            };
            return Err(d);
        }
    }
    let mut values = Map::new();
    for key in extra {
        if let Some(v) = given.get(*key) {
            values.insert((*key).to_string(), v.clone());
        }
    }
    for p in params {
        let value = match given.get(&p.name).or(p.default.as_ref()) {
            Some(v) => v.clone(),
            None => {
                return Err(Diagnostic::error(
                    codes::UNKNOWN_TEMPLATE,
                    format!("parameter `{}` is required ({})", p.name, p.doc),
                )
                .with_hint(format!(
                    "give it a value, such as `{}={}`",
                    p.name,
                    example(p.kind)
                )));
            }
        };
        check_value(p, &value).map_err(|e| {
            Diagnostic::error(
                codes::BAD_PARAM_VALUE,
                format!("parameter `{}` {e}", p.name),
            )
        })?;
        let value = match (p.kind, value) {
            (ParamKind::Color, Json::Array(mut a)) if a.len() == 3 => {
                a.push(json!(1.0));
                Json::Array(a)
            }
            (ParamKind::Float, v) => json!(v.as_f64().unwrap_or_default()),
            (_, v) => v,
        };
        values.insert(p.name.clone(), value);
    }
    Ok(values)
}

fn example(kind: ParamKind) -> &'static str {
    match kind {
        ParamKind::String => "text",
        ParamKind::Int => "3",
        ParamKind::Float => "1.5",
        ParamKind::Bool => "true",
        ParamKind::Color => "[0.8, 0.2, 0.2]",
    }
}

/// Finds the `{{ name }}` or `{{ name | filter }}` markers in `text`:
/// `(start, end, name, filter)`. Only identifiers count, so Luau's nested
/// table constructors (`{{1, 2}}`) are left alone.
fn markers(text: &str) -> Vec<(usize, usize, &str, Option<&str>)> {
    let mut out = Vec::new();
    let mut from = 0;
    while let Some(i) = text[from..].find("{{") {
        let start = from + i;
        let Some(j) = text[start + 2..].find("}}") else {
            break;
        };
        let end = start + 2 + j + 2;
        let inner = &text[start + 2..end - 2];
        let (name, filter) = match inner.split_once('|') {
            Some((n, f)) => (n.trim(), Some(f.trim())),
            None => (inner.trim(), None),
        };
        let ident = name
            .chars()
            .next()
            .is_some_and(|c| c.is_ascii_alphabetic() || c == '_')
            && name.chars().all(|c| c.is_ascii_alphanumeric() || c == '_');
        if ident {
            out.push((start, end, name, filter));
            from = end;
        } else {
            // One byte on, so `{{{x}}}` still finds `{{x}}`.
            from = start + 1;
        }
    }
    out
}

/// Replaces `{{param}}` in a text file. `{{param|toml}}` and `{{param|json}}`
/// write a quoted string literal for that format; plain `{{param}}` writes
/// the value as text (strings without quotes).
///
/// # Errors
///
/// `RE8020` for a marker naming no parameter or an unknown filter.
pub fn render_text(text: &str, values: &Map<String, Json>) -> Result<String, Diagnostic> {
    let mut out = String::with_capacity(text.len());
    let mut last = 0;
    for (start, end, name, filter) in markers(text) {
        let value = values.get(name).ok_or_else(|| {
            Diagnostic::error(
                codes::UNKNOWN_TEMPLATE,
                format!("`{{{{{name}}}}}` names no parameter"),
            )
        })?;
        let plain = match value {
            Json::String(s) => s.clone(),
            other => other.to_string(),
        };
        let rendered = match filter {
            None => plain,
            Some("json") => serde_json::to_string(&plain).expect("a string serializes"),
            Some("toml") => crate::policy::toml_string(&plain),
            Some(f) => {
                return Err(Diagnostic::error(
                    codes::UNKNOWN_TEMPLATE,
                    format!("unknown filter `{f}` in `{{{{{name}|{f}}}}}`"),
                )
                .with_hint("the filters are `toml` and `json`"));
            }
        };
        out.push_str(&text[last..start]);
        out.push_str(&rendered);
        last = end;
    }
    out.push_str(&text[last..]);
    Ok(out)
}

/// Replaces `{{param}}` in JSON arguments: a string that is exactly
/// `"{{param}}"` becomes the parameter's typed value; other strings get the
/// value as text.
///
/// # Errors
///
/// `RE8020` for a marker naming no parameter.
pub fn render_json(value: &Json, values: &Map<String, Json>) -> Result<Json, Diagnostic> {
    Ok(match value {
        Json::String(s) => {
            let found = markers(s);
            match found.as_slice() {
                [(0, end, name, None)] if *end == s.len() => {
                    values.get(*name).cloned().ok_or_else(|| {
                        Diagnostic::error(
                            codes::UNKNOWN_TEMPLATE,
                            format!("`{s}` names no parameter"),
                        )
                    })?
                }
                _ => Json::String(render_text(s, values)?),
            }
        }
        Json::Array(a) => Json::Array(
            a.iter()
                .map(|v| render_json(v, values))
                .collect::<Result<_, _>>()?,
        ),
        Json::Object(m) => Json::Object(
            m.iter()
                .map(|(k, v)| Ok((k.clone(), render_json(v, values)?)))
                .collect::<Result<_, Diagnostic>>()?,
        ),
        other => other.clone(),
    })
}

impl Template {
    /// The values for this template's parameters, with the project `name`.
    ///
    /// # Errors
    ///
    /// As [`resolve_params`].
    pub fn values(
        &self,
        name: &str,
        given: &Map<String, Json>,
    ) -> Result<Map<String, Json>, Diagnostic> {
        if given.contains_key(NAME_PARAM) {
            return Err(Diagnostic::error(
                codes::BAD_PARAM_VALUE,
                "`name` is the project's name, not a template parameter",
            )
            .with_hint("pass it with `real new --name`"));
        }
        let mut given = given.clone();
        given.insert(NAME_PARAM.into(), Json::String(name.to_string()));
        resolve_params(&self.manifest.params, &given, &[NAME_PARAM])
    }

    /// Writes the template's folders and files into `dir` (which must be
    /// empty or missing), rendering the files it lists. Returns the files
    /// written. A generated template writes nothing here; its generator
    /// does.
    ///
    /// # Errors
    ///
    /// `RE8020` from rendering; `RE8019` when a file cannot be written.
    pub fn write(&self, dir: &Path, values: &Map<String, Json>) -> Result<Vec<String>, Diagnostic> {
        let fail = |what: &str, e: std::io::Error| {
            Diagnostic::error(codes::INVALID_TEMPLATE, format!("cannot write {what}: {e}"))
        };
        for folder in &self.manifest.folders {
            std::fs::create_dir_all(dir.join(folder)).map_err(|e| fail(folder, e))?;
        }
        let mut written = Vec::new();
        for (path, bytes) in &self.files {
            let target = dir.join(path);
            if let Some(parent) = target.parent() {
                std::fs::create_dir_all(parent).map_err(|e| fail(path, e))?;
            }
            let bytes = if self.manifest.render.contains(path) {
                let text = String::from_utf8(bytes.clone()).map_err(|_| {
                    invalid(
                        path,
                        format!("{path} is not UTF-8, so it cannot be rendered"),
                    )
                })?;
                render_text(&text, values)?.into_bytes()
            } else {
                bytes.clone()
            };
            std::fs::write(&target, bytes).map_err(|e| fail(path, e))?;
            written.push(path.clone());
        }
        Ok(written)
    }
}

/// How [`apply_recipe`] runs.
#[derive(Clone, Copy, Debug, Default)]
pub struct ApplyOptions {
    /// Run every step as a dry run and skip the checks.
    pub dry_run: bool,
    /// Take a checkpoint first, and restore it when a step or check fails
    /// (`recipe.apply`). Without it (`real new`, on a fresh directory) a
    /// failure leaves what the earlier steps did, and diff checks are not
    /// run.
    pub atomic: bool,
}

/// What one step did.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct StepReport {
    /// The command.
    pub command: String,
    /// The operations it applied (or would, in a dry run).
    pub changes: usize,
}

/// One check's result.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct CheckReport {
    /// What it checks.
    pub check: String,
    /// Whether it held (`null`: not run).
    pub ok: Option<bool>,
    /// What was found, or why it was not run.
    pub detail: String,
}

/// The result of [`apply_recipe`].
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct RecipeReport {
    /// The recipe.
    pub recipe: String,
    /// The parameter values used.
    pub values: Map<String, Json>,
    /// The checkpoint taken first (atomic runs).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub checkpoint: Option<String>,
    /// Each step, in order.
    pub steps: Vec<StepReport>,
    /// Each check.
    pub checks: Vec<CheckReport>,
    /// Every operation the steps applied (or would, in a dry run), in
    /// order.
    pub changes: Vec<Op>,
    /// A dry run of several steps: each step previews against the project
    /// as it was, so a step that needs an earlier step's result previews
    /// only partly (or fails) there.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub partial_preview: bool,
}

/// Runs `recipe` with `given` arguments through `run` (a command dispatcher:
/// name, arguments, dry run). `specs` are the registered commands: steps
/// must be among them at `read` or `edit` tier, and command checks at `read`.
///
/// # Errors
///
/// `RE8020`/`RE8021` for the arguments, `RE8022` for a step command that is
/// missing or above `edit`, `RE8023` when a step fails and `RE8024` when a
/// check fails (after restoring the checkpoint, when atomic). The failed
/// step's own diagnostic follows as a note in the message.
pub fn apply_recipe(
    recipe: &Recipe,
    given: &Map<String, Json>,
    specs: &[CommandSpec],
    options: ApplyOptions,
    run: &mut dyn FnMut(&str, Json, bool) -> CommandOutcome,
) -> Result<RecipeReport, Diagnostic> {
    let values = resolve_params(&recipe.params, given, &[])?;
    let tier_of = |name: &str| specs.iter().find(|s| s.name == name).map(|s| s.tier);
    for step in &recipe.steps {
        // The checkpoint is the recipe's own; a step must not move it.
        if step.command.starts_with("checkpoint.") {
            return Err(Diagnostic::error(
                codes::RECIPE_STEP_REFUSED,
                format!(
                    "recipe `{}` runs `{}`; only `recipe.apply` itself takes and restores checkpoints",
                    recipe.name, step.command
                ),
            ));
        }
        match tier_of(&step.command) {
            Some(Tier::Read | Tier::Edit) => {}
            Some(Tier::Destructive) => {
                return Err(Diagnostic::error(
                    codes::RECIPE_STEP_REFUSED,
                    format!(
                        "recipe `{}` runs `{}`, which is destructive; recipes may only read and edit",
                        recipe.name, step.command
                    ),
                ));
            }
            None => {
                return Err(Diagnostic::error(
                    codes::RECIPE_STEP_REFUSED,
                    format!("recipe `{}` runs `{}`, which is not a registered command here", recipe.name, step.command),
                )
                .with_hint("`describe` lists the commands; a project's Luau tools load only in a project that has them"));
            }
        }
    }
    for check in &recipe.checks {
        if let Ok(CheckKind::Command { command, .. }) = check.kind()
            && tier_of(&command) != Some(Tier::Read)
        {
            return Err(Diagnostic::error(
                codes::RECIPE_STEP_REFUSED,
                format!(
                    "recipe `{}` checks with `{command}`, which is not a read-tier command here",
                    recipe.name
                ),
            ));
        }
    }
    let steps: Vec<(String, Json)> = recipe
        .steps
        .iter()
        .map(|s| Ok((s.command.clone(), render_json(&s.args, &values)?)))
        .collect::<Result<_, Diagnostic>>()?;

    let mut report = RecipeReport {
        recipe: recipe.name.clone(),
        values,
        partial_preview: options.dry_run && recipe.steps.len() > 1,
        ..RecipeReport::default()
    };
    if options.atomic && !options.dry_run {
        let out = run(
            "checkpoint.create",
            json!({ "label": format!("before recipe {}", recipe.name) }),
            false,
        );
        if !out.ok {
            return Err(nested(
                codes::RECIPE_STEP_FAILED,
                format!("recipe `{}` could not take a checkpoint first", recipe.name),
                &out,
            ));
        }
        report.checkpoint = out.data["checkpoint"].as_str().map(str::to_string);
    }
    // Restores the checkpoint, if any, and says how that went.
    let restore = |run: &mut dyn FnMut(&str, Json, bool) -> CommandOutcome, cp: &Option<String>| {
        let cp = cp.as_ref()?;
        let out = run("checkpoint.restore", json!({ "checkpoint": cp }), false);
        Some(if out.ok {
            "nothing changed: the project was restored to the checkpoint taken before the recipe"
                .to_string()
        } else {
            format!(
                "restoring the checkpoint {cp} failed ({}), so the project may be partly changed; `checkpoint.restore {{\"checkpoint\": \"{cp}\"}}` tries again",
                failure_text(&out)
            )
        })
    };

    for (i, (command, args)) in steps.into_iter().enumerate() {
        let out = run(&command, args, options.dry_run);
        if !out.ok {
            let restored = restore(run, &report.checkpoint);
            let mut d = nested(
                codes::RECIPE_STEP_FAILED,
                format!(
                    "recipe `{}` step {} (`{command}`) failed",
                    recipe.name,
                    i + 1
                ),
                &out,
            );
            if let Some(hint) = restored {
                d = d.with_hint(hint);
            }
            return Err(d);
        }
        report.steps.push(StepReport {
            command,
            changes: out.changes.len(),
        });
        report.changes.extend(out.changes);
    }

    let mut verify: Option<Json> = None;
    let mut diff: Option<Json> = None;
    for check in &recipe.checks {
        let described = check.describe();
        let kind = check.kind().map_err(|e| invalid(&recipe.name, e))?;
        if options.dry_run {
            report.checks.push(CheckReport {
                check: described,
                ok: None,
                detail: "not run in a dry run".into(),
            });
            continue;
        }
        let result = evaluate(
            &kind,
            &values_for(&report),
            &mut verify,
            &mut diff,
            report.checkpoint.as_deref(),
            run,
        );
        match result {
            Ok(Some(detail)) => report.checks.push(CheckReport {
                check: described,
                ok: Some(true),
                detail,
            }),
            Ok(None) => report.checks.push(CheckReport {
                check: described,
                ok: None,
                detail: "needs the checkpoint of an atomic run".into(),
            }),
            Err(detail) => {
                let restored = restore(run, &report.checkpoint);
                let mut d = Diagnostic::error(
                    codes::RECIPE_CHECK_FAILED,
                    format!(
                        "recipe `{}`: check failed: {described}: {detail}",
                        recipe.name
                    ),
                );
                if let Some(hint) = restored {
                    d = d.with_hint(hint);
                }
                return Err(d);
            }
        }
    }
    Ok(report)
}

/// Runs a template's checks on a project just made from it (`real new
/// --check` and the tests). `values` are the template's (from
/// [`Template::values`]); `specs` are the project's commands, and `run`
/// dispatches as for [`apply_recipe`]. Diff checks are not run: a new project
/// has no checkpoint before it.
///
/// # Errors
///
/// `RE8019` before anything runs when a check names a command that is not a
/// read-tier command here (checks only read), and `RE8024` for the first
/// check that fails.
pub fn check_template(
    template: &Template,
    values: &Map<String, Json>,
    specs: &[CommandSpec],
    run: &mut dyn FnMut(&str, Json, bool) -> CommandOutcome,
) -> Result<Vec<CheckReport>, Diagnostic> {
    let name = &template.manifest.name;
    let tier_of = |command: &str| specs.iter().find(|s| s.name == command).map(|s| s.tier);
    for check in &template.manifest.checks {
        if let Ok(CheckKind::Command { command, .. }) = check.kind()
            && tier_of(&command) != Some(Tier::Read)
        {
            return Err(Diagnostic::error(
                codes::INVALID_TEMPLATE,
                format!(
                    "template `{name}` checks with `{command}`, which is not a read-tier command here"
                ),
            )
            .with_hint("a check only reads: use a read-tier command such as `query` or `entity.get`"));
        }
    }
    let (mut verify, mut diff) = (None, None);
    let mut reports = Vec::new();
    for check in &template.manifest.checks {
        let described = check.describe();
        let kind = check.kind().map_err(|e| invalid(name, e))?;
        match evaluate(&kind, values, &mut verify, &mut diff, None, run) {
            Ok(Some(detail)) => reports.push(CheckReport {
                check: described,
                ok: Some(true),
                detail,
            }),
            Ok(None) => reports.push(CheckReport {
                check: described,
                ok: None,
                detail: "a new project has no checkpoint to compare with".into(),
            }),
            Err(detail) => {
                return Err(Diagnostic::error(
                    codes::RECIPE_CHECK_FAILED,
                    format!("template `{name}`: check failed: {described}: {detail}"),
                ));
            }
        }
    }
    Ok(reports)
}

/// Runs every check in `checks` without stopping at a failure (`real agent
/// eval`). `checkpoint` is what diff checks compare with: `run` gets
/// `verify.diff` with it. A check that cannot run reports `ok: None`.
pub fn evaluate_checks(
    checks: &[Check],
    values: &Map<String, Json>,
    checkpoint: Option<&str>,
    run: &mut dyn FnMut(&str, Json, bool) -> CommandOutcome,
) -> Vec<CheckReport> {
    let (mut verify, mut diff) = (None, None);
    checks
        .iter()
        .map(|check| {
            let described = check.describe();
            let (ok, detail) = match check.kind() {
                Err(e) => (Some(false), e),
                Ok(kind) => {
                    match evaluate(&kind, values, &mut verify, &mut diff, checkpoint, run) {
                        Ok(Some(found)) => (Some(true), found),
                        Ok(None) => (None, "needs a checkpoint to compare with".to_string()),
                        Err(why) => (Some(false), why),
                    }
                }
            };
            CheckReport {
                check: described,
                ok,
                detail,
            }
        })
        .collect()
}

fn values_for(report: &RecipeReport) -> Map<String, Json> {
    report.values.clone()
}

/// A diagnostic for a failed nested command, quoting its first error.
fn nested(code: re_core::Code, message: String, out: &CommandOutcome) -> Diagnostic {
    match out.diagnostics.first() {
        Some(inner) => {
            Diagnostic::error(code, format!("{message}: {} {}", inner.code, inner.message))
                .with_hint(format!(
                    "`real explain {}` explains the step's error",
                    inner.code
                ))
        }
        None => Diagnostic::error(code, message),
    }
}

/// Runs one check: `Ok(Some(found))` when it holds, `Ok(None)` when it
/// cannot run (diff checks without a checkpoint), `Err(why)` when it fails.
fn evaluate(
    kind: &CheckKind,
    values: &Map<String, Json>,
    verify: &mut Option<Json>,
    diff: &mut Option<Json>,
    checkpoint: Option<&str>,
    run: &mut dyn FnMut(&str, Json, bool) -> CommandOutcome,
) -> Result<Option<String>, String> {
    let outcome_json = |out: &CommandOutcome| serde_json::to_value(out).unwrap_or(Json::Null);
    match kind {
        CheckKind::Verify | CheckKind::VerifyStage(_) => {
            if verify.is_none() {
                let out = run("verify", json!({}), false);
                if !out.ok {
                    return Err(failure_text(&out));
                }
                *verify = Some(out.data);
            }
            let report = verify.as_ref().expect("set above");
            match kind {
                CheckKind::Verify => {
                    let failed: Vec<&str> = stages(report)
                        .filter(|s| s["ok"] != true)
                        .filter_map(|s| s["name"].as_str())
                        .collect();
                    if report["ok"] == true && failed.is_empty() {
                        Ok(Some("every stage passes".into()))
                    } else {
                        Err(format!("stages failing: {}", failed.join(", ")))
                    }
                }
                CheckKind::VerifyStage(name) => {
                    match stages(report).find(|s| s["name"] == name.as_str()) {
                        Some(s) if s["ok"] == true => Ok(Some(format!("stage `{name}` passes"))),
                        Some(_) => Err(format!("stage `{name}` fails")),
                        None => Err(format!("verify has no stage `{name}`")),
                    }
                }
                _ => unreachable!(),
            }
        }
        CheckKind::Command {
            command,
            args,
            pointer,
            expect,
        } => {
            let args = render_json(args, values).map_err(|d| d.message)?;
            let out = run(command, args, false);
            if !out.ok {
                return Err(failure_text(&out));
            }
            let whole = outcome_json(&out);
            let found = whole
                .pointer(pointer)
                .ok_or_else(|| format!("the result has nothing at {pointer}"))?;
            let holds = match expect {
                Expect::Equals(v) => numbers_equal(found, v) || found == v,
                Expect::AtLeast(v) => found.as_f64().is_some_and(|f| f >= *v),
                Expect::AtMost(v) => found.as_f64().is_some_and(|f| f <= *v),
                Expect::Contains(v) => match (found, v) {
                    (Json::String(s), Json::String(part)) => s.contains(part.as_str()),
                    (Json::Array(a), v) => a.contains(v),
                    _ => false,
                },
            };
            if holds {
                Ok(Some(format!("{pointer} is {found}")))
            } else {
                Err(format!("{pointer} is {found}"))
            }
        }
        CheckKind::DiffStage(_, _) | CheckKind::NoRegressions => {
            let Some(cp) = checkpoint else {
                return Ok(None);
            };
            if diff.is_none() {
                let out = run("verify.diff", json!({ "checkpoint": cp }), false);
                if !out.ok {
                    return Err(failure_text(&out));
                }
                *diff = Some(out.data);
            }
            let diff = diff.as_ref().expect("set above");
            let stages: Vec<&Json> = diff["diff"]["stages"]
                .as_array()
                .map(|a| a.iter().collect())
                .unwrap_or_default();
            match kind {
                CheckKind::DiffStage(name, want) => {
                    let changed = stages.iter().any(|s| s["name"] == name.as_str());
                    if changed == *want {
                        Ok(Some(format!(
                            "stage `{name}` {}",
                            if changed { "changed" } else { "did not change" }
                        )))
                    } else {
                        Err(format!(
                            "stage `{name}` {}",
                            if changed { "changed" } else { "did not change" }
                        ))
                    }
                }
                CheckKind::NoRegressions => {
                    let regressed: Vec<&str> = stages
                        .iter()
                        .filter(|s| s["ok_before"] == true && s["ok_after"] != true)
                        .filter_map(|s| s["name"].as_str())
                        .collect();
                    if regressed.is_empty() {
                        Ok(Some("no stage regressed".into()))
                    } else {
                        Err(format!(
                            "stages that passed before and fail now: {}",
                            regressed.join(", ")
                        ))
                    }
                }
                _ => unreachable!(),
            }
        }
    }
}

fn stages(report: &Json) -> impl Iterator<Item = &Json> {
    report["stages"].as_array().into_iter().flatten()
}

fn numbers_equal(a: &Json, b: &Json) -> bool {
    matches!((a.as_f64(), b.as_f64()), (Some(x), Some(y)) if x == y)
}

fn failure_text(out: &CommandOutcome) -> String {
    out.diagnostics
        .first()
        .map(|d| format!("{} {}", d.code, d.message))
        .unwrap_or_else(|| "it failed".into())
}

/// A template or recipe as `template.list` and `recipe.list` describe it.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct Listing {
    /// Its name.
    pub name: String,
    /// What it makes.
    pub summary: String,
    /// Its parameters (a template's `name` is not listed: it is the
    /// project's name).
    pub params: Vec<Param>,
    /// What its checks check.
    pub checks: Vec<String>,
    /// A recipe's steps, as command names; a template's recipes.
    pub uses: Vec<String>,
}

impl Library {
    /// Every template, for `template.list` and `real new --list-templates`.
    pub fn template_listings(&self) -> Vec<Listing> {
        self.templates
            .values()
            .map(|t| Listing {
                name: t.manifest.name.clone(),
                summary: t.manifest.summary.clone(),
                params: t.manifest.params.clone(),
                checks: t.manifest.checks.iter().map(Check::describe).collect(),
                uses: t
                    .manifest
                    .recipes
                    .iter()
                    .map(|r| r.recipe.clone())
                    .collect(),
            })
            .collect()
    }

    /// Every recipe, for `recipe.list`.
    pub fn recipe_listings(&self) -> Vec<Listing> {
        self.recipes
            .values()
            .map(|r| Listing {
                name: r.name.clone(),
                summary: r.summary.clone(),
                params: r.params.clone(),
                checks: r.checks.iter().map(Check::describe).collect(),
                uses: r.steps.iter().map(|s| s.command.clone()).collect(),
            })
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn param(name: &str, kind: ParamKind) -> Param {
        Param {
            name: name.into(),
            kind,
            default: None,
            doc: String::new(),
            min: None,
            max: None,
        }
    }

    fn spec(name: &str, tier: Tier) -> CommandSpec {
        CommandSpec {
            name: name.into(),
            summary: String::new(),
            tier,
            input_schema: json!({ "type": "object" }),
            annotations: re_command::CommandAnnotations::for_tier(tier, false),
            output_schema: None,
        }
    }

    fn recipe(steps: &[&str]) -> Recipe {
        Recipe {
            name: "r".into(),
            summary: String::new(),
            params: Vec::new(),
            steps: steps
                .iter()
                .map(|c| Step {
                    command: (*c).to_string(),
                    args: json!({}),
                })
                .collect(),
            checks: Vec::new(),
        }
    }

    fn outcome(ok: bool, data: Json) -> CommandOutcome {
        CommandOutcome {
            ok,
            changes: Vec::new(),
            diagnostics: if ok {
                Vec::new()
            } else {
                vec![Diagnostic::error(codes::TOOL_FAILED, "no")]
            },
            data,
        }
    }

    #[test]
    fn a_template_check_may_only_read() {
        let manifest = |command: &str| {
            format!(
                "name = \"t\"\nsummary = \"s\"\n\n[[checks]]\ncommand = \"{command}\"\npointer = \"/ok\"\nequals = true\n"
            )
        };
        let specs = [spec("query", Tier::Read), spec("entity.spawn", Tier::Edit)];
        // An edit-tier check is refused before anything runs.
        let edit = parse_template("t", &manifest("entity.spawn"), BTreeMap::new()).unwrap();
        let mut run = |_: &str, _: Json, _: bool| -> CommandOutcome { panic!("nothing runs") };
        let err = check_template(&edit, &Map::new(), &specs, &mut run).unwrap_err();
        assert_eq!(err.code, codes::INVALID_TEMPLATE, "{err:?}");
        assert!(err.message.contains("entity.spawn"), "{}", err.message);
        // So is a command the project does not have.
        let unknown = parse_template("t", &manifest("no.such"), BTreeMap::new()).unwrap();
        let err = check_template(&unknown, &Map::new(), &specs, &mut run).unwrap_err();
        assert_eq!(err.code, codes::INVALID_TEMPLATE, "{err:?}");
        // A read-tier check runs.
        let read = parse_template("t", &manifest("query"), BTreeMap::new()).unwrap();
        let mut ran = Vec::new();
        let mut run = |name: &str, _: Json, dry_run: bool| {
            ran.push((name.to_string(), dry_run));
            outcome(true, Json::Null)
        };
        let reports = check_template(&read, &Map::new(), &specs, &mut run).unwrap();
        assert_eq!(reports.len(), 1);
        assert_eq!(ran, [("query".to_string(), false)]);
    }

    #[test]
    fn a_step_may_not_move_the_checkpoint() {
        let specs = [
            spec("checkpoint.restore", Tier::Edit),
            spec("entity.spawn", Tier::Edit),
        ];
        let mut run = |_: &str, _: Json, _: bool| -> CommandOutcome { panic!("nothing runs") };
        let err = apply_recipe(
            &recipe(&["entity.spawn", "checkpoint.restore"]),
            &Map::new(),
            &specs,
            ApplyOptions {
                dry_run: false,
                atomic: true,
            },
            &mut run,
        )
        .unwrap_err();
        assert_eq!(err.code, codes::RECIPE_STEP_REFUSED);
    }

    #[test]
    fn a_failed_restore_is_reported() {
        let specs = [spec("entity.spawn", Tier::Edit)];
        let mut calls = Vec::new();
        let mut run = |name: &str, _: Json, _: bool| {
            calls.push(name.to_string());
            match name {
                "checkpoint.create" => outcome(true, json!({ "checkpoint": "c1" })),
                _ => outcome(false, Json::Null),
            }
        };
        let err = apply_recipe(
            &recipe(&["entity.spawn"]),
            &Map::new(),
            &specs,
            ApplyOptions {
                dry_run: false,
                atomic: true,
            },
            &mut run,
        )
        .unwrap_err();
        assert_eq!(err.code, codes::RECIPE_STEP_FAILED);
        assert_eq!(
            calls,
            ["checkpoint.create", "entity.spawn", "checkpoint.restore"]
        );
        let hint = err.hint.unwrap();
        assert!(
            hint.contains("restoring the checkpoint c1 failed"),
            "{hint}"
        );
    }

    #[test]
    fn the_builtin_library_loads() {
        let lib = Library::builtin().unwrap();
        let names: Vec<_> = lib.templates.keys().map(String::as_str).collect();
        assert_eq!(names, ["native", "roblox", "terrain"]);
        let names: Vec<_> = lib.recipes.keys().map(String::as_str).collect();
        assert_eq!(names, ["falling-box", "spinner", "terrain-shape"]);
        for t in lib.templates.values() {
            for r in &t.manifest.recipes {
                lib.recipe(&r.recipe).unwrap();
            }
        }
    }

    #[test]
    fn markers_skip_luau_tables() {
        let v: Map<String, Json> = [("n".to_string(), json!("Ann \"A\""))]
            .into_iter()
            .collect();
        assert_eq!(render_text("x = {{n}}", &v).unwrap(), "x = Ann \"A\"");
        // The toml crate picks a literal string when the text has quotes.
        assert_eq!(
            render_text("x = {{ n | toml }}", &v).unwrap(),
            "x = 'Ann \"A\"'"
        );
        assert_eq!(
            render_text("x = {{n|json}}", &v).unwrap(),
            "x = \"Ann \\\"A\\\"\""
        );
        assert_eq!(
            render_text("local t = {{1, 2}, {3}}", &v).unwrap(),
            "local t = {{1, 2}, {3}}"
        );
        assert_eq!(render_text("t = {{{n}}}", &v).unwrap(), "t = {Ann \"A\"}");
        assert_eq!(
            render_text("{{missing}}", &v).unwrap_err().code,
            codes::UNKNOWN_TEMPLATE
        );
        assert_eq!(
            render_text("{{n|yaml}}", &v).unwrap_err().code,
            codes::UNKNOWN_TEMPLATE
        );
    }

    #[test]
    fn whole_string_markers_keep_their_type() {
        let v: Map<String, Json> = [
            ("x".to_string(), json!(2.5)),
            ("c".to_string(), json!([1, 0, 0, 1])),
        ]
        .into_iter()
        .collect();
        let args = json!({ "t": ["{{x}}", 1.0, "{{ x }}"], "label": "at {{x}}", "color": "{{c}}" });
        assert_eq!(
            render_json(&args, &v).unwrap(),
            json!({ "t": [2.5, 1.0, 2.5], "label": "at 2.5", "color": [1, 0, 0, 1] })
        );
    }

    #[test]
    fn params_are_typed_ranged_and_defaulted() {
        let mut seed = param("seed", ParamKind::Int);
        seed.default = Some(json!(1));
        let mut height = param("height", ParamKind::Float);
        height.min = Some(1.0);
        height.max = Some(10.0);
        let color = param("color", ParamKind::Color);
        let params = [seed, height, color];
        let given: Map<String, Json> = [
            ("height".to_string(), json!(2)),
            ("color".to_string(), json!([1, 0.5, 0])),
        ]
        .into_iter()
        .collect();
        let v = resolve_params(&params, &given, &[]).unwrap();
        assert_eq!(
            Json::Object(v),
            json!({ "seed": 1, "height": 2.0, "color": [1, 0.5, 0, 1.0] })
        );

        let bad = |k: &str, val: Json| {
            let mut g = given.clone();
            g.insert(k.into(), val);
            resolve_params(&params, &g, &[]).unwrap_err().code
        };
        assert_eq!(bad("height", json!(11)), codes::BAD_PARAM_VALUE);
        assert_eq!(bad("seed", json!(1.5)), codes::BAD_PARAM_VALUE);
        assert_eq!(bad("color", json!([2, 0, 0])), codes::BAD_PARAM_VALUE);
        assert_eq!(bad("sed", json!(1)), codes::UNKNOWN_TEMPLATE);
        let mut missing = given.clone();
        missing.remove("height");
        assert_eq!(
            resolve_params(&params, &missing, &[]).unwrap_err().code,
            codes::UNKNOWN_TEMPLATE
        );
    }

    #[test]
    fn checks_have_exactly_one_kind() {
        let ok: Check =
            toml::from_str("command = \"query\"\npointer = \"/data/total\"\nat_least = 1").unwrap();
        assert!(matches!(ok.kind(), Ok(CheckKind::Command { .. })));
        for bad in [
            "",
            "verify = \"ok\"\nverify_stage = \"boot\"",
            "verify = \"yes\"",
            "command = \"query\"\npointer = \"/data\"",
            "command = \"query\"\nequals = 1",
            "pointer = \"/data\"",
            "changed = true",
            "no_regressions = false",
        ] {
            let c: Check = toml::from_str(bad).unwrap();
            assert!(c.kind().is_err(), "{bad:?}");
        }
    }

    #[test]
    fn unsafe_paths_are_refused() {
        for p in ["../x", "/etc/passwd", "a/../../b", "", "a\\b"] {
            assert!(!safe_relative(p), "{p}");
        }
        assert!(safe_relative("scripts/game/a.luau"));
        assert!(safe_relative(".gitignore"));
    }
}

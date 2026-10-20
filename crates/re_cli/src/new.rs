//! `real new DIR`: a new project with `real.toml`, an entry scene and the
//! standard folders (ARCHITECTURE §8).

use std::path::Path;

#[cfg(any(test, not(feature = "tooling")))]
use re_app::AppConfig;
use re_core::Diagnostic;

use crate::codes;

/// The entry scene of a new project: a camera, a sun, static ground and one
/// dynamic box that falls onto it. Written in canonical form (load, then save).
#[cfg(any(test, not(feature = "tooling")))]
const STARTER_SCENE: &str = r#"{
  "format": "real-engine/scene",
  "version": 1,
  "entities": [
    { "id": "box", "name": "Box", "components": {
        "Collider": { "shape": "Box", "half_extents": [0.5, 0.5, 0.5] },
        "MaterialRef": { "asset": "materials/box.material.json" },
        "MeshRef": { "asset": "builtin:cube" },
        "RigidBody": { "kind": "Dynamic", "mass": 10.0 },
        "Transform": { "translation": [0.0, 4.0, 0.0], "rotation": [0.2, 0.3, 0.1, 0.93] } } },
    { "id": "camera", "name": "Camera", "components": {
        "Camera": { "active": true },
        "Transform": { "translation": [0.0, 5.0, 12.0], "rotation": [-0.19509032, 0.0, 0.0, 0.98078528] } } },
    { "id": "ground", "name": "Ground", "components": {
        "Collider": { "shape": "Box", "half_extents": [0.5, 0.5, 0.5] },
        "MaterialRef": { "asset": "materials/ground.material.json" },
        "MeshRef": { "asset": "builtin:cube" },
        "Transform": { "translation": [0.0, -0.5, 0.0], "scale": [40.0, 1.0, 40.0] } } },
    { "id": "sun", "name": "Sun", "components": {
        "Light": { "kind": "Directional", "intensity": 1.0, "shadows": true },
        "Transform": { "rotation": [-0.5, 0.3, 0.1, 0.8] } } }
  ]
}"#;

#[cfg(any(test, not(feature = "tooling")))]
fn material(base_color: [f32; 4], roughness: f32) -> String {
    let [r, g, b, a] = base_color;
    format!(
        "{{\n  \"$schema\": \"../schemas/material.schema.json\",\n  \"base_color\": [{r:?}, {g:?}, {b:?}, {a:?}],\n  \"roughness\": {roughness:?}\n}}\n"
    )
}

#[cfg(any(test, not(feature = "tooling")))]
const FOLDERS: &[&str] = &[
    "scenes",
    "prefabs",
    "materials",
    "graphs",
    "scripts/game",
    "scripts/tools",
    "scripts/shared",
    "assets",
    "tests/replays",
    "tests/goldens",
];

#[cfg(any(test, not(feature = "tooling")))]
fn canonical_scene() -> String {
    let doc = re_scene::scene::from_json(STARTER_SCENE).expect("the starter scene parses");
    let mut world = re_scene::World::new();
    world.load(&doc).expect("the starter scene loads");
    let mut saved = world.save(re_scene::SaveOptions::default());
    saved.schema = Some("../schemas/scene.schema.json".into());
    re_scene::scene::to_json(&saved)
}

/// What `real new` makes besides the directory.
#[derive(Clone, Copy, Debug)]
pub struct NewOptions<'a> {
    /// The project name (the directory name when `None`).
    pub name: Option<&'a str>,
    /// A built-in template's name, or a template directory (ADR-0046).
    pub template: &'a str,
    /// `KEY=VALUE` template parameters.
    pub set: &'a [String],
    /// Run the template's checks on the new project.
    pub check: bool,
}

/// The name a project in `dir` gets when none is given.
fn default_name(dir: &Path) -> String {
    dir.file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| "game".into())
}

/// Creates the project at `dir` from a template (ADR-0046). In a build
/// without tooling only the default `native` template, with no parameters
/// and no checks, is available.
///
/// # Errors
///
/// As [`create`]; the template codes `RE8019`–`RE8024`; `RE0902` with a
/// hint to use the tooling build for anything else in a runtime-only build.
pub fn create_from(dir: &Path, options: NewOptions<'_>) -> Result<Vec<String>, Diagnostic> {
    #[cfg(feature = "tooling")]
    {
        templated::create(dir, options)
    }
    #[cfg(not(feature = "tooling"))]
    {
        if options.template == "native" && options.set.is_empty() && !options.check {
            return create(dir, options.name);
        }
        Err(Diagnostic::error(
            codes::WRITE_FAILED,
            "this build of `real` has only the plain `native` template",
        )
        .with_hint(
            "use the default (tooling) build of `real` for templates, `--set` and `--check`",
        ))
    }
}

/// `real new --list-templates`: the templates and their parameters, as text
/// or JSON.
///
/// # Errors
///
/// `RE8019` when a built-in template is invalid; `RE0902` in a runtime-only
/// build.
pub fn list_templates(json: bool) -> Result<String, Diagnostic> {
    #[cfg(feature = "tooling")]
    {
        templated::list(json)
    }
    #[cfg(not(feature = "tooling"))]
    {
        let _ = json;
        Err(Diagnostic::error(
            codes::WRITE_FAILED,
            "this build of `real` has only the plain `native` template",
        )
        .with_hint("use the default (tooling) build of `real`"))
    }
}

/// Templates from the library in `templates/` (ADR-0046).
#[cfg(feature = "tooling")]
mod templated {
    use std::path::Path;

    use re_agent::templates::{
        ApplyOptions, Library, Template, apply_recipe, check_template, parse_cli_value, render_json,
    };
    use re_command::{AllowAll, Origin};
    use re_core::Diagnostic;
    use serde_json::{Map, Value as Json};

    use super::{NewOptions, default_name, refuse_non_empty};

    /// The generator of the `roblox` template.
    const ROBLOX_PLACE: &str = "roblox-place";

    /// A built-in template by name, or a template directory by path (one
    /// with a slash or backslash, or `.` or `..`), so a folder in the
    /// working directory cannot stand in for a built-in name.
    fn find(lib: &Library, template: &str) -> Result<Template, Diagnostic> {
        let is_path = template.contains(['/', '\\']) || matches!(template, "." | "..");
        if is_path {
            let path = Path::new(template);
            let path = std::fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf());
            return Library::read_template_dir(&path);
        }
        lib.template(template).cloned()
    }

    /// `--set KEY=VALUE`, each value parsed as its parameter's type.
    fn given(template: &Template, set: &[String]) -> Result<Map<String, Json>, Diagnostic> {
        let mut given = Map::new();
        for item in set {
            let Some((key, value)) = item.split_once('=') else {
                return Err(Diagnostic::error(
                    re_agent::codes::BAD_PARAM_VALUE,
                    format!("`--set {item}` is not KEY=VALUE"),
                ));
            };
            let value = match template.manifest.params.iter().find(|p| p.name == key) {
                Some(param) => parse_cli_value(param, value),
                // Unknown keys are reported, with a suggestion, when resolved.
                None => Json::String(value.to_string()),
            };
            given.insert(key.to_string(), value);
        }
        Ok(given)
    }

    pub(super) fn create(dir: &Path, options: NewOptions<'_>) -> Result<Vec<String>, Diagnostic> {
        let lib = Library::builtin()?;
        let template = find(&lib, options.template)?;
        let given = given(&template, options.set)?;
        let name = options
            .name
            .map_or_else(|| default_name(dir), str::to_string);
        let values = template.values(&name, &given)?;
        refuse_non_empty(dir)?;
        let written = match template.manifest.generator.as_deref() {
            None => template.write(dir, &values)?,
            Some(ROBLOX_PLACE) => super::create_roblox(dir, Some(&name))?,
            Some(other) => {
                return Err(Diagnostic::error(
                    re_agent::codes::INVALID_TEMPLATE,
                    format!(
                        "template `{}` names an unknown generator `{other}`",
                        template.manifest.name
                    ),
                )
                .with_hint(format!("the generator is `{ROBLOX_PLACE}`")));
            }
        };
        if template.manifest.recipes.is_empty() && !options.check {
            return Ok(written);
        }
        let mut agent_app = crate::tooling::mcp::open_for_agents(dir)?;
        let app = &mut agent_app.app;
        let specs = app.commands().specs().to_vec();
        let mut run = |command: &str, args: Json, dry_run: bool| {
            app.dispatch(command, args, dry_run, &AllowAll, Origin::User)
        };
        for used in &template.manifest.recipes {
            let recipe = lib.recipe(&used.recipe)?;
            let args = match render_json(&used.args, &values)? {
                Json::Object(map) => map,
                _ => Map::new(),
            };
            // On a fresh directory: no checkpoint, so a failure leaves
            // what was made for the person to look at.
            apply_recipe(recipe, &args, &specs, ApplyOptions::default(), &mut run)?;
        }
        if options.check {
            for report in check_template(&template, &values, &mut run)? {
                println!("check: {} ({})", report.check, report.detail);
            }
        }
        Ok(written)
    }

    pub(super) fn list(json: bool) -> Result<String, Diagnostic> {
        let lib = Library::builtin()?;
        let listings = lib.template_listings();
        if json {
            return Ok(serde_json::to_string_pretty(&listings).expect("listings serialize"));
        }
        let mut out = String::new();
        for t in listings {
            out.push_str(&format!("{}: {}\n", t.name, t.summary));
            for p in t.params {
                let default = p
                    .default
                    .map(|d| format!(" (default {d})"))
                    .unwrap_or_default();
                out.push_str(&format!(
                    "  --set {}=<{}>{default}  {}\n",
                    p.name,
                    p.kind.as_str(),
                    p.doc
                ));
            }
        }
        Ok(out)
    }
}

#[cfg(feature = "tooling")]
fn create_roblox(dir: &Path, name: Option<&str>) -> Result<Vec<String>, Diagnostic> {
    let classes = crate::tooling::import::classes()?;
    re_import::new_project(dir, name, &classes)
}

fn refuse_non_empty(dir: &Path) -> Result<(), Diagnostic> {
    if dir.exists()
        && std::fs::read_dir(dir)
            .map(|mut d| d.next().is_some())
            .unwrap_or(true)
    {
        return Err(Diagnostic::error(
            codes::PROJECT_EXISTS,
            format!("{} already exists and is not empty", dir.display()),
        )
        .with_hint("choose a new directory; `real new` never overwrites files"));
    }
    Ok(())
}

/// Creates the plain native project at `dir`, named `name` (the directory
/// name when `None`), for builds without the template library;
/// `templates/native` makes the same files (a test keeps them the same).
/// Returns the files written, relative to `dir`.
///
/// # Errors
///
/// `RE0901` when `dir` exists and is not empty; `RE0902` when a file cannot be
/// written.
#[cfg(any(test, not(feature = "tooling")))]
pub fn create(dir: &Path, name: Option<&str>) -> Result<Vec<String>, Diagnostic> {
    refuse_non_empty(dir)?;
    let fail = |what: &str, e: std::io::Error| {
        Diagnostic::error(codes::WRITE_FAILED, format!("cannot write {what}: {e}"))
    };
    let name = name.map_or_else(|| default_name(dir), str::to_string);
    for folder in FOLDERS {
        std::fs::create_dir_all(dir.join(folder)).map_err(|e| fail(folder, e))?;
    }
    let files = [
        ("real.toml", AppConfig::template(&name)),
        ("scenes/main.scene.json", canonical_scene()),
        (
            ".luaurc",
            "{\n  \"languageMode\": \"strict\",\n  \"aliases\": {\n    \"shared\": \"scripts/shared\"\n  }\n}\n".to_string(),
        ),
        (".gitignore", ".real/*\n!.real/agent-policy.toml\nout/\n".to_string()),
        (
            "materials/box.material.json",
            material([0.9, 0.45, 0.15, 1.0], 0.6),
        ),
        (
            "materials/ground.material.json",
            material([0.35, 0.5, 0.3, 1.0], 0.9),
        ),
    ];
    let mut written = Vec::new();
    for (path, text) in files {
        std::fs::write(dir.join(path), text).map_err(|e| fail(path, e))?;
        written.push(path.to_string());
    }
    Ok(written)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn starter_scene_is_canonical_and_opens() {
        let dir = tempfile::tempdir().unwrap();
        let project = dir.path().join("demo");
        create(&project, None).unwrap();
        let text = std::fs::read_to_string(project.join("scenes/main.scene.json")).unwrap();
        let doc = re_scene::scene::from_json(&text).unwrap();
        let mut w = re_scene::World::new();
        w.load(&doc).unwrap();
        assert_eq!(
            re_scene::scene::to_json(&w.save(re_scene::SaveOptions::default())),
            text
        );
        let mut app = re_app::App::open_project(&project).unwrap();
        let report = app.run_headless(Some(60), None).unwrap();
        assert!(report.ok(), "{:?}", report.diagnostics);
        // The box fell onto the ground.
        let y = match app.world().get("box", "Transform", "translation").unwrap() {
            re_reflect::Value::Vec3(v) => v.y,
            other => panic!("{other:?}"),
        };
        assert!(y < 3.9 && y > 0.3, "box at y = {y}");
        for m in ["box", "ground"] {
            let path = format!("materials/{m}.material.json");
            re_assets::load_material_in(&project, &path).unwrap();
        }
    }

    /// Every file under `dir`, with its bytes, and every folder.
    fn tree(dir: &Path) -> Vec<(String, Option<Vec<u8>>)> {
        let mut out = Vec::new();
        let mut stack = vec![dir.to_path_buf()];
        while let Some(d) = stack.pop() {
            for entry in std::fs::read_dir(&d).unwrap() {
                let path = entry.unwrap().path();
                let rel = path
                    .strip_prefix(dir)
                    .unwrap()
                    .to_string_lossy()
                    .replace('\\', "/");
                if path.is_dir() {
                    out.push((rel, None));
                    stack.push(path);
                } else {
                    out.push((rel, Some(std::fs::read(&path).unwrap())));
                }
            }
        }
        out.sort();
        out
    }

    #[cfg(feature = "tooling")]
    #[test]
    fn the_native_template_makes_the_plain_project() {
        let dir = tempfile::tempdir().unwrap();
        create(&dir.path().join("plain"), Some("Demo")).unwrap();
        let options = NewOptions {
            name: Some("Demo"),
            template: "native",
            set: &[],
            check: false,
        };
        create_from(&dir.path().join("templated"), options).unwrap();
        assert_eq!(
            tree(&dir.path().join("plain")),
            tree(&dir.path().join("templated"))
        );
        // The template quotes the name as TOML, so any name loads.
        let quoted = dir.path().join("quoted");
        let options = NewOptions {
            name: Some("My \"Game\" \\ 2"),
            ..options
        };
        create_from(&quoted, options).unwrap();
        assert_eq!(
            AppConfig::load(&quoted).unwrap().project.name,
            "My \"Game\" \\ 2"
        );
    }

    /// ADR-0046: every built-in template makes a project that passes
    /// `real verify` and the template's other checks.
    #[cfg(feature = "tooling")]
    #[test]
    fn every_template_makes_a_project_that_passes_its_checks() {
        let dir = tempfile::tempdir().unwrap();
        for t in ["native", "roblox", "terrain"] {
            let options = NewOptions {
                name: None,
                template: t,
                set: &[],
                check: true,
            };
            create_from(&dir.path().join(t), options).unwrap_or_else(|d| panic!("{t}: {d:?}"));
        }
        let set = ["seed=42".to_string(), "height=40".to_string()];
        let options = NewOptions {
            name: None,
            template: "terrain",
            set: &set,
            check: true,
        };
        create_from(&dir.path().join("terrain42"), options).unwrap();
        let scene =
            std::fs::read_to_string(dir.path().join("terrain42/scenes/main.scene.json")).unwrap();
        assert!(scene.contains("42"), "{scene}");
    }

    /// ADR-0046: template scenes are stored in canonical form.
    #[cfg(feature = "tooling")]
    #[test]
    fn template_scenes_are_canonical() {
        use re_agent::templates::{Library, render_text};
        let lib = Library::builtin().unwrap();
        let mut seen = 0;
        for t in lib.templates.values() {
            let values = t.values("game", &serde_json::Map::new()).unwrap();
            for (path, bytes) in &t.files {
                if !path.ends_with(".scene.json") {
                    continue;
                }
                let text = render_text(std::str::from_utf8(bytes).unwrap(), &values).unwrap();
                let doc = re_scene::scene::from_json(&text).unwrap();
                let mut world = re_scene::World::new();
                world.load(&doc).unwrap();
                let mut saved = world.save(re_scene::SaveOptions::default());
                saved.schema = doc.schema.clone();
                assert_eq!(
                    re_scene::scene::to_json(&saved),
                    text,
                    "{}/{path}",
                    t.manifest.name
                );
                seen += 1;
            }
        }
        assert_eq!(seen, 2);
    }

    #[cfg(feature = "tooling")]
    #[test]
    fn template_errors_are_reported() {
        let dir = tempfile::tempdir().unwrap();
        let make = |template: &str, set: &[String]| {
            let options = NewOptions {
                name: None,
                template,
                set,
                check: false,
            };
            create_from(&dir.path().join("x"), options)
                .unwrap_err()
                .code
        };
        assert_eq!(make("terain", &[]), re_agent::codes::UNKNOWN_TEMPLATE);
        assert_eq!(
            make("terrain", &["sed=1".into()]),
            re_agent::codes::UNKNOWN_TEMPLATE
        );
        assert_eq!(
            make("terrain", &["seed=lots".into()]),
            re_agent::codes::BAD_PARAM_VALUE
        );
        assert_eq!(
            make("terrain", &["height=500".into()]),
            re_agent::codes::BAD_PARAM_VALUE
        );
        assert_eq!(
            make("terrain", &["seed".into()]),
            re_agent::codes::BAD_PARAM_VALUE
        );
        assert_eq!(
            make("native", &["name=x".into()]),
            re_agent::codes::BAD_PARAM_VALUE
        );
        // Nothing was written for any of them.
        assert!(!dir.path().join("x").exists());
    }

    #[test]
    fn refuses_a_non_empty_directory() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("x"), "").unwrap();
        assert_eq!(
            create(dir.path(), None).unwrap_err().code,
            codes::PROJECT_EXISTS
        );
    }
}

//! The import steps (WP16): what `re_import` made of the sample, re-import,
//! variants of the input, and compiling the scripts.
//!
//! * `import.ok`: the import succeeded with no failed instance.
//! * `import.declared` / `import.created` / `import.adopted` (`{class: n}`):
//!   the instances the input declares, or the import created or adopted, by
//!   class, exactly (services excluded).
//! * `import.paths` (`{"exist": [{path, class, tags?}], "absent": [{path,
//!   class?}]}`): instances in the imported scene.
//! * `import.sources` (`{"list": FILE}`, a JSON list of `{path, class,
//!   file}`): each script exists with exactly that file's bytes, and the scene
//!   has no other script.
//! * `import.source_totals` (`{"bytes": n, "lines": n}`): over the input's
//!   scripts; every source ends with a newline.
//! * `import.reimport`: importing again creates, removes and changes nothing.
//! * `import.variant` (`{"edits": [...], "into": "fresh" | "imported",
//!   "before": {...}, "expect": {...}}`): imports an edited copy of the input.
//!   Edits: `{"delete_lines": [a, b]}`, `{"replace_lines": [a, b], "with":
//!   text}`, `{"insert_before_line": n, "text": text}`, `{"replace": {"find":
//!   s, "with": s}}`. `before.add_script` (`{parent, name, class}`) puts a
//!   script the import did not create into the project first. Expectations:
//!   `error_code` (and `error_line`): it fails and the project is unchanged;
//!   `identical_to_import`: the project's files equal the plain import's;
//!   `created`, `removed` (`[{path, class}]`) and `failed` counts; `survives`:
//!   the added script is still there.
//!   `diagnostic` (`{code, file_suffix, line}`): the report has that warning.
//! * `import.compile` (`{"input": bool}`): every script in the project (and
//!   the input itself, when asked) compiles.
//! * `import.order` (`{"list": FILE}`, a JSON list of `{path, class}` in the
//!   input's order): each parent's listed children appear in that order.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use re_import::{ImportOptions, ImportReport};
use re_scene::scene::SceneDoc;
use serde_json::Value as J;

use super::Ctx;

pub fn step(name: &str) -> Option<super::Step> {
    Some(match name {
        "import.ok" => ok,
        "import.declared" => declared,
        "import.created" => created,
        "import.adopted" => adopted,
        "import.paths" => paths,
        "import.sources" => sources,
        "import.source_totals" => source_totals,
        "import.reimport" => reimport,
        "import.variant" => variant,
        "import.compile" => compile,
        "import.order" => order,
        _ => return None,
    })
}

fn ok(ctx: &mut Ctx, _: &J) -> Result<(), String> {
    let r = ctx.import()?;
    if !r.failed.is_empty() {
        return Err(format!(
            "{} instance(s) failed, first: {:?}",
            r.failed.len(),
            r.failed[0]
        ));
    }
    if !r.ok() {
        return Err(r.render_human());
    }
    Ok(())
}

fn want_counts(args: &J) -> Result<BTreeMap<String, u32>, String> {
    serde_json::from_value(args.clone()).map_err(|e| format!("args: {e}"))
}

fn count(items: &[re_import::Item]) -> BTreeMap<String, u32> {
    let mut out = BTreeMap::new();
    for i in items.iter().filter(|i| i.path.len() > 1) {
        *out.entry(i.class.clone()).or_default() += 1;
    }
    out
}

fn same(what: &str, got: BTreeMap<String, u32>, want: BTreeMap<String, u32>) -> Result<(), String> {
    if got == want {
        Ok(())
    } else {
        Err(format!("{what}: got {got:?}, want {want:?}"))
    }
}

fn declared(ctx: &mut Ctx, args: &J) -> Result<(), String> {
    let want = want_counts(args)?;
    let got = ctx.import()?.classes.clone();
    same("declared", got, want)
}

fn created(ctx: &mut Ctx, args: &J) -> Result<(), String> {
    let want = want_counts(args)?;
    same("created", count(&ctx.import()?.created), want)
}

fn adopted(ctx: &mut Ctx, args: &J) -> Result<(), String> {
    let want = want_counts(args)?;
    same("adopted", count(&ctx.import()?.adopted), want)
}

/// The scene's entities by path from the service down (several per path when
/// names repeat).
struct Scene {
    doc: SceneDoc,
}

impl Scene {
    fn read(project: &Path) -> Result<Scene, String> {
        let text = std::fs::read_to_string(project.join("scenes/main.scene.json"))
            .map_err(|e| e.to_string())?;
        Ok(Scene {
            doc: re_scene::scene::from_json(&text).map_err(|e| e.to_string())?,
        })
    }

    fn by_id(&self, id: &str) -> Option<&re_scene::scene::EntityDoc> {
        self.doc.entities.iter().find(|e| e.id == id)
    }

    fn path(&self, e: &re_scene::scene::EntityDoc) -> Vec<String> {
        let mut out = vec![];
        let mut at = Some(e);
        while let Some(x) = at {
            if x.id == "game" {
                break;
            }
            out.push(x.name.clone());
            at = x.parent.as_deref().and_then(|p| self.by_id(p));
        }
        out.reverse();
        out
    }

    fn str_field(e: &re_scene::scene::EntityDoc, c: &str, f: &str) -> Option<String> {
        match e.components.get(c)?.get(f)? {
            re_reflect::Doc::Str(s) => Some(s.clone()),
            _ => None,
        }
    }

    fn class(e: &re_scene::scene::EntityDoc) -> String {
        Self::str_field(e, "ClassMarker", "class").unwrap_or_default()
    }

    fn tags(e: &re_scene::scene::EntityDoc) -> Vec<String> {
        match e.components.get("Tags").and_then(|t| t.get("tags")) {
            Some(re_reflect::Doc::List(l)) => l
                .iter()
                .filter_map(|d| match d {
                    re_reflect::Doc::Str(s) => Some(s.clone()),
                    _ => None,
                })
                .collect(),
            _ => vec![],
        }
    }

    fn find(&self, path: &[String], class: Option<&str>) -> Vec<&re_scene::scene::EntityDoc> {
        self.doc
            .entities
            .iter()
            .filter(|e| self.path(e) == path && class.is_none_or(|c| Self::class(e) == c))
            .collect()
    }
}

fn strings(v: &J) -> Vec<String> {
    v.as_array()
        .map(|a| {
            a.iter()
                .filter_map(|x| x.as_str().map(str::to_string))
                .collect()
        })
        .unwrap_or_default()
}

fn paths(ctx: &mut Ctx, args: &J) -> Result<(), String> {
    ctx.import()?;
    let scene = Scene::read(&ctx.project())?;
    let mut problems = Vec::new();
    for want in args
        .get("exist")
        .and_then(J::as_array)
        .into_iter()
        .flatten()
    {
        let path = strings(&want["path"]);
        let class = want.get("class").and_then(J::as_str);
        let found = scene.find(&path, class);
        let Some(e) = found.first() else {
            problems.push(format!("missing {path:?} ({class:?})"));
            continue;
        };
        if let Some(tags) = want.get("tags") {
            let tags = strings(tags);
            if Scene::tags(e) != tags {
                problems.push(format!(
                    "{path:?} has tags {:?}, want {tags:?}",
                    Scene::tags(e)
                ));
            }
        }
    }
    for gone in args
        .get("absent")
        .and_then(J::as_array)
        .into_iter()
        .flatten()
    {
        let path = strings(&gone["path"]);
        let class = gone.get("class").and_then(J::as_str);
        if !scene.find(&path, class).is_empty() {
            problems.push(format!("{path:?} ({class:?}) should not exist"));
        }
    }
    if problems.is_empty() {
        Ok(())
    } else {
        Err(problems.join("; "))
    }
}

fn is_script(class: &str) -> bool {
    matches!(class, "Script" | "LocalScript" | "ModuleScript")
}

fn sources(ctx: &mut Ctx, args: &J) -> Result<(), String> {
    ctx.import()?;
    let list_path = ctx.kit_path(args["list"].as_str().ok_or("args.list")?);
    let base = list_path.parent().unwrap_or(Path::new(".")).to_path_buf();
    let list: Vec<J> =
        serde_json::from_str(&std::fs::read_to_string(&list_path).map_err(|e| e.to_string())?)
            .map_err(|e| e.to_string())?;
    let project = ctx.project();
    let scene = Scene::read(&project)?;
    let mut problems = Vec::new();
    let mut matched = std::collections::HashSet::new();
    for entry in &list {
        let path = strings(&entry["path"]);
        let class = entry["class"].as_str().unwrap_or_default();
        let file = entry["file"].as_str().unwrap_or_default();
        let want = std::fs::read(base.join(file)).map_err(|e| format!("{file}: {e}"))?;
        let candidates = scene.find(&path, Some(class));
        let hit = candidates.iter().find(|e| {
            !matched.contains(&e.id)
                && Scene::str_field(e, "ScriptRef", "path")
                    .and_then(|p| std::fs::read(project.join(p)).ok())
                    .is_some_and(|got| got == want)
        });
        match hit {
            Some(e) => {
                matched.insert(e.id.clone());
            }
            None if candidates.is_empty() => problems.push(format!("missing {path:?} ({class})")),
            None => problems.push(format!("{path:?} ({class}) differs from {file}")),
        }
    }
    let scripts = scene
        .doc
        .entities
        .iter()
        .filter(|e| is_script(&Scene::class(e)))
        .count();
    if scripts != list.len() {
        problems.push(format!(
            "the scene has {scripts} scripts, the list {}",
            list.len()
        ));
    }
    if problems.is_empty() {
        Ok(())
    } else {
        let n = problems.len();
        problems.truncate(10);
        Err(format!("{n} problem(s): {}", problems.join("; ")))
    }
}

fn script_files(project: &Path) -> Result<Vec<(String, String)>, String> {
    let scene = Scene::read(project)?;
    let mut out = Vec::new();
    for e in &scene.doc.entities {
        if let Some(p) = Scene::str_field(e, "ScriptRef", "path") {
            let text =
                std::fs::read_to_string(project.join(&p)).map_err(|err| format!("{p}: {err}"))?;
            out.push((p, text));
        }
    }
    Ok(out)
}

fn source_totals(ctx: &mut Ctx, args: &J) -> Result<(), String> {
    ctx.import()?;
    let files = script_files(&ctx.project())?;
    let bytes: u64 = files.iter().map(|(_, t)| t.len() as u64).sum();
    let lines: u64 = files
        .iter()
        .map(|(_, t)| t.matches('\n').count() as u64)
        .sum();
    let unterminated: Vec<&String> = files
        .iter()
        .filter(|(_, t)| !t.ends_with('\n'))
        .map(|(p, _)| p)
        .collect();
    let mut problems = Vec::new();
    if args
        .get("bytes")
        .and_then(J::as_u64)
        .is_some_and(|b| b != bytes)
    {
        problems.push(format!("{bytes} source bytes, want {}", args["bytes"]));
    }
    if args
        .get("lines")
        .and_then(J::as_u64)
        .is_some_and(|l| l != lines)
    {
        problems.push(format!("{lines} source lines, want {}", args["lines"]));
    }
    if !unterminated.is_empty() {
        problems.push(format!(
            "{} source(s) without a final newline",
            unterminated.len()
        ));
    }
    if problems.is_empty() {
        Ok(())
    } else {
        Err(problems.join("; "))
    }
}

/// Every file under `dir`, relative and sorted, with its bytes.
fn tree(dir: &Path) -> Vec<(String, Vec<u8>)> {
    fn walk(base: &Path, dir: &Path, out: &mut Vec<(String, Vec<u8>)>) {
        let Ok(rd) = std::fs::read_dir(dir) else {
            return;
        };
        let mut entries: Vec<PathBuf> = rd.filter_map(|e| e.ok().map(|e| e.path())).collect();
        entries.sort();
        for p in entries {
            if p.is_dir() {
                walk(base, &p, out);
            } else {
                let rel = p
                    .strip_prefix(base)
                    .unwrap()
                    .to_string_lossy()
                    .replace('\\', "/");
                out.push((rel, std::fs::read(&p).unwrap_or_default()));
            }
        }
    }
    let mut out = Vec::new();
    walk(dir, dir, &mut out);
    out
}

fn copy_dir(from: &Path, to: &Path) -> std::io::Result<()> {
    std::fs::create_dir_all(to)?;
    for e in std::fs::read_dir(from)? {
        let p = e?.path();
        let dest = to.join(p.file_name().unwrap());
        if p.is_dir() {
            copy_dir(&p, &dest)?;
        } else {
            std::fs::copy(&p, &dest)?;
        }
    }
    Ok(())
}

fn reimport(ctx: &mut Ctx, _: &J) -> Result<(), String> {
    let first = ctx.import()?.clone();
    let project = ctx.work.path().join("reimport/project");
    copy_dir(&ctx.project(), &project).map_err(|e| e.to_string())?;
    let before = tree(&project);
    let again = re_import::import(&ctx.input(), &project, &ctx.classes, &ctx.options())
        .map_err(|d| d.render_human())?;
    let mut problems = Vec::new();
    if !again.created.is_empty() {
        problems.push(format!("created {}", again.created.len()));
    }
    if !again.removed.is_empty() {
        problems.push(format!("removed {}", again.removed.len()));
    }
    if !again.failed.is_empty() {
        problems.push(format!("failed {}", again.failed.len()));
    }
    let want = first.created.len() + first.adopted.len();
    if again.adopted.len() != want {
        problems.push(format!("adopted {}, want {want}", again.adopted.len()));
    }
    if again.adopted.iter().any(|i| i.updated) {
        problems.push("a script was updated".into());
    }
    if tree(&project) != before {
        problems.push("the project's files changed".into());
    }
    if problems.is_empty() {
        Ok(())
    } else {
        Err(problems.join("; "))
    }
}

fn edit(text: &str, edits: &J) -> Result<String, String> {
    let mut lines: Vec<String> = text.split_inclusive('\n').map(str::to_string).collect();
    let mut joined: Option<String> = None;
    for e in edits.as_array().ok_or("edits must be a list")? {
        if let Some(s) = joined.take() {
            lines = s.split_inclusive('\n').map(str::to_string).collect();
        }
        let range = |k: &str| -> Option<(usize, usize)> {
            let a = e.get(k)?.as_array()?;
            Some((a.first()?.as_u64()? as usize, a.get(1)?.as_u64()? as usize))
        };
        if let Some((a, b)) = range("delete_lines") {
            lines.drain(a - 1..b);
        } else if let Some((a, b)) = range("replace_lines") {
            let with = e["with"].as_str().ok_or("replace_lines needs with")?;
            lines.splice(a - 1..b, [with.to_string()]);
        } else if let Some(n) = e.get("insert_before_line").and_then(J::as_u64) {
            let t = e["text"].as_str().ok_or("insert_before_line needs text")?;
            lines.insert(n as usize - 1, t.to_string());
        } else if let Some(r) = e.get("replace") {
            let find = r["find"].as_str().ok_or("replace.find")?;
            let with = r["with"].as_str().ok_or("replace.with")?;
            let all = lines.concat();
            if !all.contains(find) {
                return Err(format!("edit: {find:?} is not in the input"));
            }
            joined = Some(all.replacen(find, with, 1));
        } else {
            return Err(format!("unknown edit {e}"));
        }
    }
    Ok(joined.unwrap_or_else(|| lines.concat()))
}

/// Puts a script the import did not create into the project.
fn add_script(
    project: &Path,
    spec: &J,
    classes: &re_compat::ClassRegistry,
) -> Result<String, String> {
    let mut scene = Scene::read(project)?;
    let parent_path = strings(&spec["parent"]);
    let parent = scene
        .find(&parent_path, None)
        .first()
        .map(|e| e.id.clone())
        .ok_or_else(|| format!("no {parent_path:?} to add a script to"))?;
    let name = spec["name"].as_str().unwrap_or("KitScript");
    let class = spec["class"].as_str().unwrap_or("ModuleScript");
    let id = "kit-own-script".to_string();
    let mut doc = re_compat::scene::instance_doc(classes, class, &id, name, Some(&parent))
        .map_err(|e| e.to_string())?;
    let ctx = re_compat::script_context(class, None).ok_or("not a script class")?;
    let file = "scripts/kit-own-script.luau".to_string();
    let mut sr = indexmap::IndexMap::new();
    sr.insert("path".to_string(), re_reflect::Doc::Str(file.clone()));
    sr.insert(
        "context".to_string(),
        re_reflect::Doc::Str(format!("{ctx:?}")),
    );
    doc.components.insert("ScriptRef".into(), sr);
    scene.doc.entities.push(doc);
    std::fs::write(project.join(&file), "return nil\n").map_err(|e| e.to_string())?;
    std::fs::write(
        project.join("scenes/main.scene.json"),
        re_scene::scene::to_json(&scene.doc),
    )
    .map_err(|e| e.to_string())?;
    Ok(id)
}

fn variant(ctx: &mut Ctx, args: &J) -> Result<(), String> {
    let base = ctx.import()?.clone();
    let n = ctx.state.len();
    ctx.state
        .insert(format!("import.variant.{n}"), Box::new(()));
    let dir = ctx.work.path().join(format!("variant-{n}"));
    std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
    let text = std::fs::read_to_string(ctx.input()).map_err(|e| e.to_string())?;
    let input = dir.join("input.luau");
    std::fs::write(&input, edit(&text, &args["edits"])?).map_err(|e| e.to_string())?;
    let project = dir.join("project");
    let into = args.get("into").and_then(J::as_str).unwrap_or("fresh");
    if into == "imported" {
        copy_dir(&ctx.project(), &project).map_err(|e| e.to_string())?;
    }
    let mut added = None;
    if let Some(spec) = args.get("before").and_then(|b| b.get("add_script")) {
        added = Some(add_script(&project, spec, &ctx.classes)?);
    }
    let before = tree(&project);
    let opts = ImportOptions {
        dry_run: false,
        keep_stale: args
            .get("keep_stale")
            .and_then(J::as_bool)
            .unwrap_or(ctx.expect.import.keep_stale),
        ..ImportOptions::default()
    };
    let result = re_import::import(&input, &project, &ctx.classes, &opts);
    let expect = &args["expect"];
    let mut problems = Vec::new();
    if let Some(code) = expect.get("error_code").and_then(J::as_str) {
        match &result {
            Ok(_) => problems.push(format!("imported; want {code}")),
            Err(d) => {
                if d.code.to_string() != code {
                    problems.push(format!("failed with {}, want {code}", d.code));
                }
                if let Some(line) = expect.get("error_line").and_then(J::as_u64) {
                    let got = match &d.location {
                        Some(re_core::Location::File { line, .. }) => line.map(u64::from),
                        _ => None,
                    };
                    if got != Some(line) {
                        problems.push(format!("error at line {got:?}, want {line}"));
                    }
                }
            }
        }
        if tree(&project) != before {
            problems.push("the project changed".into());
        }
        return if problems.is_empty() {
            Ok(())
        } else {
            Err(problems.join("; "))
        };
    }
    let report: ImportReport = result.map_err(|d| d.render_human())?;
    if expect.get("identical_to_import").and_then(J::as_bool) == Some(true) {
        let strip = |t: Vec<(String, Vec<u8>)>| -> Vec<(String, Vec<u8>)> {
            t.into_iter().filter(|(p, _)| p != "real.toml").collect()
        };
        if strip(tree(&project)) != strip(tree(&ctx.project())) {
            problems.push("the project differs from the plain import".into());
        }
        if report.created.len() != base.created.len() {
            problems.push(format!(
                "created {}, the plain import {}",
                report.created.len(),
                base.created.len()
            ));
        }
    }
    if let Some(c) = expect.get("created").and_then(J::as_u64)
        && report.created.len() as u64 != c
    {
        problems.push(format!("created {}, want {c}", report.created.len()));
    }
    if let Some(f) = expect.get("failed").and_then(J::as_u64)
        && report.failed.len() as u64 != f
    {
        problems.push(format!("failed {}, want {f}", report.failed.len()));
    }
    if let Some(removed) = expect.get("removed").and_then(J::as_array) {
        let mut want: Vec<(Vec<String>, String)> = removed
            .iter()
            .map(|r| {
                (
                    strings(&r["path"]),
                    r["class"].as_str().unwrap_or_default().to_string(),
                )
            })
            .collect();
        let mut got: Vec<(Vec<String>, String)> = report
            .removed
            .iter()
            .map(|i| (i.path.clone(), i.class.clone()))
            .collect();
        want.sort();
        got.sort();
        if got != want {
            problems.push(format!("removed {got:?}, want {want:?}"));
        }
    }
    if let Some(want) = expect.get("diagnostic") {
        let code = want["code"].as_str().unwrap_or_default();
        let suffix = want["file_suffix"].as_str().unwrap_or_default();
        let line = want.get("line").and_then(J::as_u64);
        let hit = report.diagnostics.iter().any(|d| {
            d.code.to_string() == code
                && match &d.location {
                    Some(re_core::Location::File { file, line: l, .. }) => {
                        file.ends_with(suffix) && (line.is_none() || l.map(u64::from) == line)
                    }
                    _ => false,
                }
        });
        if !hit {
            problems.push(format!(
                "no {code} at {suffix}:{line:?}; got {:?}",
                report
                    .diagnostics
                    .iter()
                    .map(|d| (d.code.to_string(), d.location.clone()))
                    .collect::<Vec<_>>()
            ));
        }
    }
    if expect.get("survives").and_then(J::as_bool) == Some(true) {
        let scene = Scene::read(&project)?;
        if added.as_ref().is_none_or(|id| scene.by_id(id).is_none()) {
            problems.push("the script the import did not create is gone".into());
        }
    }
    if problems.is_empty() {
        Ok(())
    } else {
        Err(problems.join("; "))
    }
}

fn compile(ctx: &mut Ctx, args: &J) -> Result<(), String> {
    ctx.import()?;
    let mut problems = Vec::new();
    for (path, text) in script_files(&ctx.project())? {
        if let Some(d) = re_script::compile_check(&text, &path).into_iter().next() {
            problems.push(format!("{path}: {}", d.message));
        }
    }
    if args.get("input").and_then(J::as_bool) == Some(true) {
        let text = std::fs::read_to_string(ctx.input()).map_err(|e| e.to_string())?;
        if let Some(d) = re_script::compile_check(&text, "input").into_iter().next() {
            problems.push(format!("the input: {}", d.message));
        }
    }
    if problems.is_empty() {
        Ok(())
    } else {
        let n = problems.len();
        problems.truncate(5);
        Err(format!(
            "{n} file(s) do not compile: {}",
            problems.join("; ")
        ))
    }
}

fn order(ctx: &mut Ctx, args: &J) -> Result<(), String> {
    ctx.import()?;
    let list_path = ctx.kit_path(args["list"].as_str().ok_or("args.list")?);
    let list: Vec<J> =
        serde_json::from_str(&std::fs::read_to_string(&list_path).map_err(|e| e.to_string())?)
            .map_err(|e| e.to_string())?;
    let scene = Scene::read(&ctx.project())?;
    // Each parent path → its listed children, in list order.
    let mut want: BTreeMap<Vec<String>, Vec<(String, String)>> = BTreeMap::new();
    for e in &list {
        let path = strings(&e["path"]);
        let (name, parent) = path.split_last().ok_or("an empty path")?;
        want.entry(parent.to_vec()).or_default().push((
            name.clone(),
            e["class"].as_str().unwrap_or_default().to_string(),
        ));
    }
    let mut problems = Vec::new();
    for (parent, kids) in want {
        let Some(p) = scene.find(&parent, None).first().map(|e| e.id.clone()) else {
            problems.push(format!("missing {parent:?}"));
            continue;
        };
        let got: Vec<(String, String)> = scene
            .doc
            .entities
            .iter()
            .filter(|e| e.parent.as_deref() == Some(p.as_str()))
            .map(|e| (e.name.clone(), Scene::class(e)))
            .filter(|k| kids.contains(k))
            .collect();
        if got != kids {
            problems.push(format!(
                "children of {parent:?} are in the order {got:?}, want {kids:?}"
            ));
        }
    }
    if problems.is_empty() {
        Ok(())
    } else {
        Err(problems.join("; "))
    }
}

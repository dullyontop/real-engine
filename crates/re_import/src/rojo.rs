//! The Rojo-style project reader: a `*.project.json` tree and the files its
//! `$path`s map, with Rojo's naming rules.
//!
//! * The project's `tree` must be a place (`"$className": "DataModel"`); each
//!   child of it is a service.
//! * A node is `$className` (default `Folder`), `$path`, `$properties`,
//!   `$attributes`, and its other keys as children.
//! * A directory is a `Folder`, or the script its `init.luau`,
//!   `init.server.luau` or `init.client.luau` makes (`.lua` too), with
//!   `init.meta.json` for its class and properties.
//! * `X.server.luau` is a `Script`, `X.client.luau` a `LocalScript` and `X.luau`
//!   a `ModuleScript`; `X.model.json` is an instance tree (`ClassName`,
//!   `Properties`, `Attributes`, `Children` with `Name`); `X.meta.json` adds
//!   properties to `X`.
//! * Other files are skipped with a warning. Entries are read in name order.
//!
//! Instances an earlier import made under a service that the project no
//! longer declares are removed.

use std::path::{Path, PathBuf};

use re_core::{Diagnostic, Location};
use serde_json::{Map, Value as J};

use crate::codes;
use crate::tree::{Kind, StaleScope, Tree};

fn invalid(file: &Path, msg: impl Into<String>) -> Diagnostic {
    Diagnostic::error(codes::ROJO_INVALID, msg).at(Location::file(file.display().to_string()))
}

fn read_json(file: &Path) -> Result<J, Diagnostic> {
    let text =
        std::fs::read_to_string(file).map_err(|e| invalid(file, format!("cannot read: {e}")))?;
    serde_json::from_str(&text).map_err(|e| {
        Diagnostic::error(codes::ROJO_INVALID, format!("not valid JSON: {e}")).at(
            Location::file_at(
                file.display().to_string(),
                e.line() as u32,
                e.column() as u32,
            ),
        )
    })
}

/// Reads the project file `project`.
pub fn read(project: &Path) -> Result<Tree, Diagnostic> {
    let doc = read_json(project)?;
    let base = project.parent().unwrap_or(Path::new(".")).to_path_buf();
    let tree_json = doc
        .get("tree")
        .and_then(J::as_object)
        .ok_or_else(|| invalid(project, "the project has no \"tree\" object"))?;
    if tree_json.get("$className").and_then(J::as_str) != Some("DataModel") {
        return Err(invalid(
            project,
            "the project's tree is not a place: its \"$className\" must be \"DataModel\"",
        )
        .with_hint(
            "only place projects can be imported; a model project has no services to put it in",
        ));
    }
    let mut r = Reader {
        tree: Tree::new(),
        base,
        project: project.to_path_buf(),
    };
    for (name, node) in tree_json {
        if name.starts_with('$') {
            continue;
        }
        let obj = node
            .as_object()
            .ok_or_else(|| invalid(project, format!("tree.{name} is not an object")))?;
        let class = obj.get("$className").and_then(J::as_str).unwrap_or(name);
        let svc = r.tree.service(class, None);
        r.tree.nodes[svc].file = Some(project.display().to_string());
        r.node_body(svc, obj, &format!("tree.{name}"))?;
        r.tree.scopes.push(StaleScope::Subtree(svc));
    }
    Ok(r.tree)
}

struct Reader {
    tree: Tree,
    base: PathBuf,
    project: PathBuf,
}

fn props(obj: Option<&J>) -> Vec<(String, J)> {
    obj.and_then(J::as_object)
        .map(|m| m.iter().map(|(k, v)| (k.clone(), v.clone())).collect())
        .unwrap_or_default()
}

/// The script class and instance name of a file, if it is a script.
fn script_file(file_name: &str) -> Option<(&'static str, &str)> {
    for (suffix, class) in [
        (".server.luau", "Script"),
        (".server.lua", "Script"),
        (".client.luau", "LocalScript"),
        (".client.lua", "LocalScript"),
        (".luau", "ModuleScript"),
        (".lua", "ModuleScript"),
    ] {
        if let Some(stem) = file_name.strip_suffix(suffix) {
            return Some((class, stem));
        }
    }
    None
}

impl Reader {
    fn rel(&self, p: &Path) -> String {
        p.strip_prefix(&self.base)
            .unwrap_or(p)
            .display()
            .to_string()
    }

    /// `$path`, `$properties`, `$attributes` and the children of a tree node.
    fn node_body(&mut self, node: usize, obj: &Map<String, J>, at: &str) -> Result<(), Diagnostic> {
        if let Some(p) = obj.get("$path") {
            let rel = p
                .as_str()
                .ok_or_else(|| invalid(&self.project, format!("{at}.$path is not a string")))?;
            let path = self.base.join(rel);
            self.path_into(node, &path)?;
        }
        self.tree.nodes[node]
            .properties
            .extend(props(obj.get("$properties")));
        self.tree.nodes[node]
            .attributes
            .extend(props(obj.get("$attributes")));
        for (name, child) in obj {
            if name.starts_with('$') {
                continue;
            }
            let cobj = child
                .as_object()
                .ok_or_else(|| invalid(&self.project, format!("{at}.{name} is not an object")))?;
            let class = match cobj.get("$className") {
                Some(J::String(c)) => c.clone(),
                Some(_) => {
                    return Err(invalid(
                        &self.project,
                        format!("{at}.{name}.$className is not a string"),
                    ));
                }
                None if cobj.contains_key("$path") => String::new(),
                None => "Folder".into(),
            };
            let c = self.tree.instance(&class, name, Kind::Declared, None);
            self.tree.nodes[c].file = Some(self.project.display().to_string());
            self.tree.set_parent(c, Some(node));
            self.node_body(c, cobj, &format!("{at}.{name}"))?;
            if self.tree.nodes[c].class.is_empty() {
                self.tree.nodes[c].class = "Folder".into();
            }
        }
        Ok(())
    }

    /// The instance a `$path` maps, merged into `node` (whose name is kept).
    fn path_into(&mut self, node: usize, path: &Path) -> Result<(), Diagnostic> {
        if path.is_dir() {
            self.dir_into(node, path)
        } else if path.is_file() {
            let fname = path
                .file_name()
                .map(|n| n.to_string_lossy().into_owned())
                .unwrap_or_default();
            if let Some(stem) = fname.strip_suffix(".model.json") {
                let _ = stem;
                let json = read_json(path)?;
                self.model_into(node, &json, path)
            } else if let Some((class, _)) = script_file(&fname) {
                self.set_class(node, class);
                self.tree.nodes[node].source = Some(self.read_source(path)?);
                self.tree.nodes[node].file = Some(self.rel(path));
                Ok(())
            } else {
                Err(invalid(
                    path,
                    "a $path file must be a script or a .model.json file",
                ))
            }
        } else {
            Err(invalid(
                &self.project,
                format!("$path {} does not exist", path.display()),
            ))
        }
    }

    fn set_class(&mut self, node: usize, class: &str) {
        let n = &mut self.tree.nodes[node];
        if n.kind != Kind::Service && (n.class.is_empty() || n.class == "Folder") {
            n.class = class.to_string();
        }
    }

    fn read_source(&self, path: &Path) -> Result<String, Diagnostic> {
        std::fs::read_to_string(path).map_err(|e| {
            invalid(
                path,
                format!("cannot read the script (it must be UTF-8): {e}"),
            )
        })
    }

    fn dir_into(&mut self, node: usize, dir: &Path) -> Result<(), Diagnostic> {
        let mut entries: Vec<PathBuf> = std::fs::read_dir(dir)
            .map_err(|e| invalid(dir, format!("cannot list: {e}")))?
            .filter_map(|e| e.ok().map(|e| e.path()))
            .collect();
        entries.sort();
        let name_of = |p: &Path| {
            p.file_name()
                .map(|n| n.to_string_lossy().into_owned())
                .unwrap_or_default()
        };
        // The init files say what the directory itself is.
        for p in &entries {
            let f = name_of(p);
            if let Some(("init", class)) = script_file(&f).map(|(c, s)| (s, c)) {
                self.set_class(node, class);
                self.tree.nodes[node].source = Some(self.read_source(p)?);
                self.tree.nodes[node].file = Some(self.rel(p));
            }
        }
        if self.tree.nodes[node].class.is_empty() {
            self.tree.nodes[node].class = "Folder".into();
        }
        let init_meta = dir.join("init.meta.json");
        if init_meta.is_file() {
            let meta = read_json(&init_meta)?;
            self.meta_into(node, &meta, &init_meta, true);
        }
        for p in &entries {
            let f = name_of(p);
            if f.starts_with('.') {
                continue;
            }
            if p.is_dir() {
                let c = self.tree.instance("", &f, Kind::Declared, None);
                self.tree.nodes[c].file = Some(self.rel(p));
                self.tree.set_parent(c, Some(node));
                self.dir_into(c, p)?;
                continue;
            }
            if f == "init.meta.json" || script_file(&f).is_some_and(|(_, s)| s == "init") {
                continue;
            }
            if let Some(stem) = f.strip_suffix(".model.json") {
                let json = read_json(p)?;
                let c = self.tree.instance("", stem, Kind::Declared, None);
                self.tree.nodes[c].file = Some(self.rel(p));
                self.tree.set_parent(c, Some(node));
                self.model_into(c, &json, p)?;
            } else if f.ends_with(".meta.json") {
                // Applied to its instance below.
            } else if let Some((class, stem)) = script_file(&f) {
                let c = self.tree.instance(class, stem, Kind::Declared, None);
                self.tree.nodes[c].source = Some(self.read_source(p)?);
                self.tree.nodes[c].file = Some(self.rel(p));
                self.tree.set_parent(c, Some(node));
            } else {
                self.tree.warnings.push(
                    Diagnostic::warning(
                        codes::ROJO_INVALID,
                        format!(
                            "skipped {}: the importer maps no instance to this kind of file",
                            self.rel(p)
                        ),
                    )
                    .at(Location::file(p.display().to_string())),
                );
            }
        }
        // `X.meta.json` adds to the instance `X`.
        for p in &entries {
            let f = name_of(p);
            let Some(stem) = f.strip_suffix(".meta.json") else {
                continue;
            };
            if stem == "init" {
                continue;
            }
            let target = self
                .tree
                .live_children(node)
                .find(|&c| self.tree.nodes[c].name == stem);
            match target {
                Some(c) => {
                    let meta = read_json(p)?;
                    let is_dir = dir.join(stem).is_dir();
                    self.meta_into(c, &meta, p, is_dir);
                }
                None => self.tree.warnings.push(
                    Diagnostic::warning(
                        codes::ROJO_INVALID,
                        format!("skipped {}: there is no {stem} next to it", self.rel(p)),
                    )
                    .at(Location::file(p.display().to_string())),
                ),
            }
        }
        Ok(())
    }

    fn meta_into(&mut self, node: usize, meta: &J, file: &Path, may_set_class: bool) {
        if may_set_class
            && let Some(c) = meta.get("className").and_then(J::as_str)
            && self.tree.nodes[node].source.is_none()
        {
            self.tree.nodes[node].class = c.to_string();
        }
        self.tree.nodes[node]
            .properties
            .extend(props(meta.get("properties")));
        self.tree.nodes[node]
            .attributes
            .extend(props(meta.get("attributes")));
        let _ = file;
    }

    fn model_into(&mut self, node: usize, json: &J, file: &Path) -> Result<(), Diagnostic> {
        let obj = json
            .as_object()
            .ok_or_else(|| invalid(file, "a model file must hold an object"))?;
        let class = obj
            .get("ClassName")
            .and_then(J::as_str)
            .ok_or_else(|| invalid(file, "a model instance needs a \"ClassName\""))?;
        if self.tree.nodes[node].kind != Kind::Service {
            self.tree.nodes[node].class = class.to_string();
        }
        self.tree.nodes[node]
            .properties
            .extend(props(obj.get("Properties")));
        self.tree.nodes[node]
            .attributes
            .extend(props(obj.get("Attributes")));
        if let Some(children) = obj.get("Children") {
            let list = children
                .as_array()
                .ok_or_else(|| invalid(file, "\"Children\" must be a list"))?;
            for child in list {
                let name = child
                    .get("Name")
                    .and_then(J::as_str)
                    .ok_or_else(|| invalid(file, "a child in a model file needs a \"Name\""))?;
                let c = self.tree.instance("", name, Kind::Declared, None);
                self.tree.nodes[c].file = Some(self.rel(file));
                self.tree.set_parent(c, Some(node));
                self.model_into(c, child, file)?;
            }
        }
        Ok(())
    }
}

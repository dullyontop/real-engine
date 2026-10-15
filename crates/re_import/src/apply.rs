//! Syncing a [`Tree`] into a project: match instances by path, create what is
//! missing, update scripts in place, remove what an earlier import created and
//! the input no longer declares, then check the scene against the compat
//! conventions and write it.

use std::collections::{BTreeMap, HashMap, HashSet};
use std::path::{Path, PathBuf};

use indexmap::IndexMap;
use re_compat::scene::{GAME_ID, create_place, instance_doc, script_context, validate};
use re_compat::{ClassRegistry, CompatApi, CompatConfig};
use re_core::{Diagnostic, Location};
use re_reflect::{Doc, Value};
use re_scene::scene::{EntityDoc, SceneDoc};
use re_scene::{SaveOptions, World};

use crate::codes;
use crate::names::{file_name, script_suffix};
use crate::report::{Failure, FileChanges, ImportReport, InputFormat, Item, display_path};
use crate::tree::{Kind, StaleScope, Tree};

/// Every id the importer gives an instance it creates starts with this; an
/// import removes only instances whose id does.
pub const IMPORT_ID_PREFIX: &str = "imp-";

/// The services a new compat place starts with, when the class layer knows
/// them. Other services are added when an input puts something in them, or
/// by the runtime when a script asks for them.
pub const PLACE_SERVICES: &[&str] = &[
    "Workspace",
    "Lighting",
    "ReplicatedStorage",
    "ServerScriptService",
    "ServerStorage",
    "StarterGui",
    "StarterPlayer",
];

/// The fixed children a new place's services get, when the class layer knows
/// their classes: `(service, class)`, with the class as name and id.
pub const PLACE_CHILDREN: &[(&str, &str)] = &[
    ("StarterPlayer", "StarterPlayerScripts"),
    ("StarterPlayer", "StarterCharacterScripts"),
];

/// The entry scene of a new project.
pub const ENTRY_SCENE: &str = "scenes/main.scene.json";

/// What an import does to the project directory.
pub struct Plan {
    pub report: ImportReport,
    /// The canonical scene text and where it goes, when it changes.
    pub scene: Option<(String, String)>,
    /// `real.toml`, for a new project.
    pub config: Option<String>,
    /// Script files to write.
    pub writes: Vec<(String, String)>,
    /// Script files to delete.
    pub deletes: Vec<String>,
}

/// The project an import writes into.
pub struct Project {
    pub dir: PathBuf,
    pub new: bool,
    pub entry_scene: String,
    pub scene: SceneDoc,
    pub name: String,
}

/// `real.toml` of a new compat project.
pub fn project_toml(name: &str) -> String {
    format!(
        "[project]\nname = {name:?}\nentry_scene = \"{ENTRY_SCENE}\"\n\n[sim]\nfixed_hz = 60\n\n{}",
        CompatConfig {
            api: CompatApi::Roblox,
            signal_behavior: Default::default(),
            players: 1,
        }
        .to_toml()
    )
}

fn unsuitable(dir: &Path, msg: String) -> Diagnostic {
    Diagnostic::error(codes::PROJECT_UNSUITABLE, msg).at(Location::file(dir.display().to_string()))
}

/// An empty place: the `game` root, the [`PLACE_SERVICES`] and
/// [`PLACE_CHILDREN`] the class layer knows.
pub fn place_scene(classes: &ClassRegistry) -> Result<SceneDoc, Diagnostic> {
    let mut w = World::new();
    let services: Vec<&str> = PLACE_SERVICES
        .iter()
        .copied()
        .filter(|s| classes.try_class(s).is_some_and(|c| c.service))
        .collect();
    let scene_err = |e: re_compat::CompatError| {
        Diagnostic::error(
            codes::PROJECT_UNSUITABLE,
            format!("cannot build an empty place: {e}"),
        )
    };
    create_place(&mut w, classes, &services).map_err(scene_err)?;
    for (service, class) in PLACE_CHILDREN {
        if let Ok(parent) = w.entity(service)
            && classes.try_class(class).is_some()
        {
            re_compat::scene::spawn_instance(
                &mut w,
                classes,
                class,
                Some(class),
                class,
                Some(parent),
            )
            .map_err(scene_err)?;
        }
    }
    let mut doc = w.save(SaveOptions::default());
    doc.schema = Some("../schemas/scene.schema.json".into());
    Ok(doc)
}

/// Opens (or plans) the project at `dir`.
pub fn open_project(dir: &Path, classes: &ClassRegistry) -> Result<Project, Diagnostic> {
    let empty = !dir.exists()
        || std::fs::read_dir(dir)
            .map(|mut d| d.next().is_none())
            .unwrap_or(false);
    let name = dir
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| "game".into());
    if empty {
        return Ok(Project {
            dir: dir.to_path_buf(),
            new: true,
            entry_scene: ENTRY_SCENE.into(),
            scene: place_scene(classes)?,
            name,
        });
    }
    let toml_path = dir.join("real.toml");
    let text = std::fs::read_to_string(&toml_path).map_err(|e| {
        unsuitable(
            dir,
            format!(
                "{} is not empty and has no readable real.toml: {e}",
                dir.display()
            ),
        )
        .with_hint(
            "import into a new directory, or into a project `real new --template roblox` made",
        )
    })?;
    match CompatConfig::from_project_toml(&text) {
        Ok(Some(_)) => {}
        Ok(None) => {
            return Err(unsuitable(
                &toml_path,
                "the project is not a compat project: real.toml has no [compat] section".into(),
            )
            .with_hint("add `[compat]` with `api = \"roblox\"` to real.toml, or import into a new directory"));
        }
        Err(e) => return Err(unsuitable(&toml_path, e.to_string())),
    }
    let table: toml::Table = text
        .parse()
        .map_err(|e: toml::de::Error| unsuitable(&toml_path, e.to_string()))?;
    let entry_scene = table
        .get("project")
        .and_then(|p| p.get("entry_scene"))
        .and_then(|s| s.as_str())
        .unwrap_or(ENTRY_SCENE)
        .to_string();
    let scene_path = dir.join(&entry_scene);
    let scene = match std::fs::read_to_string(&scene_path) {
        Ok(t) => re_scene::scene::from_json(&t)
            .map_err(|e| unsuitable(&scene_path, format!("the entry scene does not parse: {e}")))?,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => place_scene(classes)?,
        Err(e) => {
            return Err(unsuitable(
                &scene_path,
                format!("cannot read the entry scene: {e}"),
            ));
        }
    };
    Ok(Project {
        dir: dir.to_path_buf(),
        new: false,
        entry_scene,
        scene,
        name,
    })
}

/// The scene's entities with their tree.
struct Docs {
    list: Vec<EntityDoc>,
    index: HashMap<String, usize>,
    children: HashMap<String, Vec<String>>,
    removed: HashSet<String>,
}

impl Docs {
    fn new(list: Vec<EntityDoc>) -> Self {
        let mut d = Docs {
            list: Vec::new(),
            index: HashMap::new(),
            children: HashMap::new(),
            removed: HashSet::new(),
        };
        for e in list {
            d.push(e);
        }
        d
    }

    fn push(&mut self, e: EntityDoc) {
        if let Some(p) = &e.parent {
            self.children
                .entry(p.clone())
                .or_default()
                .push(e.id.clone());
        }
        self.index.insert(e.id.clone(), self.list.len());
        self.list.push(e);
    }

    fn get(&self, id: &str) -> Option<&EntityDoc> {
        self.index.get(id).map(|&i| &self.list[i])
    }

    fn get_mut(&mut self, id: &str) -> Option<&mut EntityDoc> {
        self.index.get(id).map(|&i| &mut self.list[i])
    }

    fn live_children(&self, id: &str) -> Vec<String> {
        self.children
            .get(id)
            .map(|c| {
                c.iter()
                    .filter(|c| !self.removed.contains(*c))
                    .cloned()
                    .collect()
            })
            .unwrap_or_default()
    }

    fn class(&self, id: &str) -> String {
        match self
            .get(id)
            .and_then(|e| e.components.get("ClassMarker"))
            .and_then(|m| m.get("class"))
        {
            Some(Doc::Str(s)) => s.clone(),
            _ => String::new(),
        }
    }

    fn script_path(&self, id: &str) -> Option<String> {
        match self.get(id)?.components.get("ScriptRef")?.get("path")? {
            Doc::Str(s) if !s.is_empty() => Some(s.clone()),
            _ => None,
        }
    }

    /// The names from the service down.
    fn path(&self, id: &str) -> Vec<String> {
        let mut out = Vec::new();
        let mut at = self.get(id);
        while let Some(e) = at {
            if e.id == GAME_ID {
                break;
            }
            out.push(e.name.clone());
            at = e.parent.as_deref().and_then(|p| self.get(p));
        }
        out.reverse();
        out
    }

    fn subtree(&self, id: &str, out: &mut Vec<String>) {
        out.push(id.to_string());
        for c in self.live_children(id) {
            self.subtree(&c, out);
        }
    }
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

/// The id of the k-th instance of `class` named `name` the importer creates
/// under `parent`: the same every time, so importing twice into two new
/// projects gives the same files.
fn import_id(parent: &str, name: &str, class: &str, k: usize) -> String {
    let mut h = blake3::Hasher::new();
    for part in [parent, name, class, &k.to_string()] {
        h.update(part.as_bytes());
        h.update(&[0]);
    }
    format!("{IMPORT_ID_PREFIX}{}", hex(&h.finalize().as_bytes()[..8]))
}

struct Sync<'a> {
    tree: &'a Tree,
    classes: &'a ClassRegistry,
    project: &'a Project,
    docs: Docs,
    /// Tree node → entity id.
    mapped: HashMap<usize, String>,
    desired: HashSet<String>,
    created: Vec<Item>,
    adopted: Vec<Item>,
    removed: Vec<Item>,
    failed: Vec<Failure>,
    diagnostics: Vec<Diagnostic>,
    writes: Vec<(String, String)>,
    deletes: Vec<String>,
    /// Script paths taken by the scene or this import.
    taken: HashSet<String>,
}

/// Plans the import of `tree` into `project`.
pub fn plan(
    tree: &Tree,
    format: InputFormat,
    input_label: &str,
    project: &Project,
    classes: &ClassRegistry,
    remove_stale: bool,
    dry_run: bool,
) -> Result<Plan, Diagnostic> {
    let docs = Docs::new(project.scene.entities.clone());
    let mut taken = HashSet::new();
    for e in &docs.list {
        if let Some(p) = docs.script_path(&e.id) {
            taken.insert(p.to_lowercase());
        }
    }
    if docs.get(GAME_ID).is_none() {
        return Err(unsuitable(
            &project.dir.join(&project.entry_scene),
            format!("the entry scene has no {GAME_ID:?} root, so it is not a compat place"),
        ));
    }
    let mut s = Sync {
        tree,
        classes,
        project,
        docs,
        mapped: HashMap::new(),
        desired: HashSet::new(),
        created: Vec::new(),
        adopted: Vec::new(),
        removed: Vec::new(),
        failed: Vec::new(),
        diagnostics: tree.warnings.clone(),
        writes: Vec::new(),
        deletes: Vec::new(),
        taken,
    };
    for &service in &tree.services {
        s.service(service);
    }
    if remove_stale && tree.remove_stale {
        s.remove_stale();
    }
    let Sync {
        docs,
        mut created,
        mut adopted,
        removed,
        mut failed,
        mut diagnostics,
        writes,
        deletes,
        mapped,
        ..
    } = s;

    // Build the world: the scene, then tags, attributes and properties.
    let entities: Vec<EntityDoc> = docs
        .list
        .into_iter()
        .filter(|e| !docs.removed.contains(&e.id))
        .collect();
    let scene = SceneDoc {
        entities,
        ..project.scene.clone()
    };
    let mut world = World::new();
    classes.register_components(&mut world).map_err(|e| {
        Diagnostic::error(
            codes::PROJECT_UNSUITABLE,
            format!("cannot register the class layer: {e}"),
        )
    })?;
    world.load(&scene).map_err(|e| {
        unsuitable(
            &project.dir.join(&project.entry_scene),
            format!("the imported scene does not load: {e}"),
        )
    })?;
    let mut order: Vec<(&usize, &String)> = mapped.iter().collect();
    order.sort();
    let created_ids: HashSet<&str> = created.iter().map(|i| i.id.as_str()).collect();
    for (&node, id) in order {
        let n = &tree.nodes[node];
        let Ok(e) = world.entity(id) else { continue };
        let mut new_tags = n.tags.clone();
        if created_ids.contains(id.as_str()) {
            new_tags.extend(n.tags_if_created.iter().cloned());
        }
        if !new_tags.is_empty() {
            let mut tags = world
                .ecs
                .get::<re_scene::Tags>(e)
                .map(|t| t.tags.clone())
                .unwrap_or_default();
            for t in &new_tags {
                if !tags.contains(t) {
                    tags.push(t.clone());
                }
            }
            world.ecs.entity_mut(e).insert(re_scene::Tags { tags });
        }
        if !n.attributes.is_empty() {
            let mut values = world
                .ecs
                .get::<re_scene::Attributes>(e)
                .map(|a| a.values.clone())
                .unwrap_or_default();
            for (k, v) in &n.attributes {
                match json_value(v, None) {
                    Some(v) => {
                        values.insert(k.clone(), v);
                    }
                    None => failed.push(Failure {
                        path: tree.path(node),
                        class: n.class.clone(),
                        line: n.line,
                        message: format!("attribute {k}: the importer cannot read {v} yet"),
                    }),
                }
            }
            world
                .ecs
                .entity_mut(e)
                .insert(re_scene::Attributes { values });
        }
        for (prop, v) in &n.properties {
            let ty = match classes.member(&n.class, prop) {
                Ok(m) => match m.member {
                    re_compat::Member::Property(p) => Some(p.ty.clone()),
                    _ => None,
                },
                Err(_) => None,
            };
            let res = match json_value(v, ty.as_deref()) {
                Some(value) => classes
                    .set_property(&mut world, e, prop, value)
                    .map_err(|err| err.to_string()),
                None => Err(format!(
                    "the importer cannot read {v} as a {}",
                    ty.as_deref().unwrap_or("value")
                )),
            };
            if let Err(message) = res {
                failed.push(Failure {
                    path: tree.path(node),
                    class: n.class.clone(),
                    line: n.line,
                    message: format!("property {prop}: {message}"),
                });
            }
        }
    }
    let problems = validate(&mut world, classes);
    if !problems.is_empty() {
        let mut d = Diagnostic::error(
            codes::PROJECT_UNSUITABLE,
            format!(
                "the scene would break the compat conventions ({} problem(s)); nothing was imported",
                problems.len()
            ),
        )
        .at(Location::file(project.dir.join(&project.entry_scene).display().to_string()));
        for p in problems.iter().take(20) {
            d = d.with_related(p.clone());
        }
        return Err(d);
    }
    let mut saved = world.save(SaveOptions::default());
    saved.schema = project.scene.schema.clone();
    let scene_text = re_scene::scene::to_json(&saved);
    let scene_changed = project.new
        || std::fs::read_to_string(project.dir.join(&project.entry_scene))
            .ok()
            .as_deref()
            != Some(scene_text.as_str());

    // Compile each script the input declares.
    for (node, id) in &mapped {
        let n = &tree.nodes[*node];
        if let Some(src) = &n.source
            && script_context(&n.class, None).is_some()
        {
            let file =
                scene_script_path(&saved, id).unwrap_or_else(|| display_path(&tree.path(*node)));
            for d in re_script::compile_check(src, &file) {
                let mut w = Diagnostic::warning(
                    codes::SCRIPT_SYNTAX,
                    format!(
                        "{} does not compile: {}",
                        display_path(&tree.path(*node)),
                        d.message
                    ),
                );
                w.location = d.location.clone();
                diagnostics.push(w);
            }
        }
    }
    diagnostics.sort_by(|a, b| format!("{:?}", a.location).cmp(&format!("{:?}", b.location)));

    let mut classes_count: BTreeMap<String, u32> = BTreeMap::new();
    for &svc in &tree.services {
        count_declared(tree, svc, &mut classes_count);
    }
    created.sort_by_key(|i| i.line.unwrap_or(0));
    adopted.sort_by_key(|i| i.line.unwrap_or(0));
    let report = ImportReport {
        input: input_label.to_string(),
        format,
        project: project.dir.display().to_string(),
        dry_run,
        new_project: project.new,
        created,
        adopted,
        removed,
        failed,
        unrecognised: tree.unrecognised.clone(),
        classes: classes_count,
        files: FileChanges {
            written: {
                let mut w: Vec<String> = writes.iter().map(|(p, _)| p.clone()).collect();
                if project.new {
                    w.insert(0, "real.toml".into());
                }
                if scene_changed {
                    w.push(project.entry_scene.clone());
                }
                w
            },
            deleted: deletes.clone(),
        },
        diagnostics,
    };
    Ok(Plan {
        report,
        scene: scene_changed.then(|| (project.entry_scene.clone(), scene_text)),
        config: project.new.then(|| project_toml(&project.name)),
        writes,
        deletes,
    })
}

fn scene_script_path(scene: &SceneDoc, id: &str) -> Option<String> {
    let e = scene.entities.iter().find(|e| e.id == id)?;
    match e.components.get("ScriptRef")?.get("path")? {
        Doc::Str(s) => Some(s.clone()),
        _ => None,
    }
}

fn count_declared(tree: &Tree, node: usize, out: &mut BTreeMap<String, u32>) {
    for c in tree.live_children(node) {
        if tree.nodes[c].kind == Kind::Declared {
            *out.entry(tree.nodes[c].class.clone()).or_default() += 1;
        }
        count_declared(tree, c, out);
    }
}

/// A JSON value as a property or attribute value; `ty` is the property's type
/// as the class data writes it.
fn json_value(v: &serde_json::Value, ty: Option<&str>) -> Option<Value> {
    use serde_json::Value as J;
    Some(match v {
        J::Bool(b) => Value::Bool(*b),
        J::Number(n) => match (n.as_i64(), ty) {
            (Some(i), Some("int")) => Value::Int(i),
            _ => Value::Float(n.as_f64()?),
        },
        J::String(s) if ty.is_some_and(|t| t.starts_with("Enum.")) => Value::Enum(s.clone()),
        J::String(s) => Value::Str(s.clone()),
        J::Array(a) if a.len() == 3 && a.iter().all(J::is_number) => Value::Vec3(glam::Vec3::new(
            a[0].as_f64()? as f32,
            a[1].as_f64()? as f32,
            a[2].as_f64()? as f32,
        )),
        _ => return None,
    })
}

impl Sync<'_> {
    fn fail_subtree(&mut self, node: usize, message: &str) {
        let n = &self.tree.nodes[node];
        if n.kind == Kind::Declared {
            self.failed.push(Failure {
                path: self.tree.path(node),
                class: n.class.clone(),
                line: n.line,
                message: message.to_string(),
            });
        }
        let kids: Vec<usize> = self.tree.live_children(node).collect();
        for c in kids {
            self.fail_subtree(c, message);
        }
    }

    fn item(&self, node: usize, id: &str, updated: bool) -> Item {
        Item {
            path: self.tree.path(node),
            class: self.tree.nodes[node].class.clone(),
            id: id.to_string(),
            line: self.tree.nodes[node].line,
            updated,
        }
    }

    fn service(&mut self, node: usize) {
        let class = self.tree.nodes[node].class.clone();
        if self.tree.live_children(node).next().is_none() {
            // Only used as a handle (`ChangeHistoryService`, `CollectionService`),
            // or emptied by the input: never created, but a service the scene
            // has is still the root its stale scopes are counted from.
            if self.docs.get(&class).is_some() {
                self.mapped.insert(node, class);
            }
            return;
        }
        if self.docs.get(&class).is_none() {
            match self.classes.try_class(&class) {
                Some(c) if c.service => {
                    match instance_doc(self.classes, &class, &class, &class, Some(GAME_ID)) {
                        Ok(doc) => {
                            self.docs.push(doc);
                            self.created.push(self.item(node, &class, false));
                        }
                        Err(e) => {
                            self.fail_subtree(node, &e.to_string());
                            return;
                        }
                    }
                }
                Some(_) => {
                    self.fail_subtree(node, &format!("{class} is not a service"));
                    return;
                }
                None => {
                    self.fail_subtree(
                        node,
                        &format!("unknown service {class:?}: the class layer does not define it"),
                    );
                    return;
                }
            }
        }
        self.mapped.insert(node, class.clone());
        self.desired.insert(class.clone());
        self.visit(node, &class);
    }

    fn visit(&mut self, node: usize, id: &str) {
        let kids: Vec<usize> = self.tree.live_children(node).collect();
        let mut seen: HashMap<(String, String), usize> = HashMap::new();
        for c in kids {
            let n = &self.tree.nodes[c];
            let (name, class) = (n.name.clone(), n.class.clone());
            let existing = self.docs.live_children(id);
            let (k, found) = match n.kind {
                Kind::Container => (
                    0,
                    existing
                        .into_iter()
                        .find(|e| self.docs.get(e).is_some_and(|d| d.name == name)),
                ),
                _ => {
                    let k = seen.entry((name.clone(), class.clone())).or_insert(0);
                    let kk = *k;
                    *k += 1;
                    (
                        kk,
                        existing
                            .into_iter()
                            .filter(|e| {
                                self.docs.get(e).is_some_and(|d| d.name == name)
                                    && self.docs.class(e) == class
                            })
                            .nth(kk),
                    )
                }
            };
            let child_id = match found {
                Some(e) => {
                    let updated = self.script(c, &e, false);
                    self.adopted.push(self.item(c, &e, updated));
                    e
                }
                None => {
                    match self.classes.try_class(&class) {
                        None => {
                            self.fail_subtree(
                                c,
                                &format!(
                                    "unknown class {class:?}: the class layer does not define it"
                                ),
                            );
                            continue;
                        }
                        Some(def) if def.service => {
                            self.fail_subtree(
                                c,
                                &format!("{class} is a service and can only be a child of game"),
                            );
                            continue;
                        }
                        Some(_) => {}
                    }
                    let mut new_id = import_id(id, &name, &class, k);
                    let mut bump = 1;
                    while self.docs.get(&new_id).is_some() {
                        bump += 1;
                        new_id = import_id(id, &name, &class, k + bump * 1_000_000);
                    }
                    match instance_doc(self.classes, &class, &new_id, &name, Some(id)) {
                        Ok(doc) => self.docs.push(doc),
                        Err(e) => {
                            self.fail_subtree(c, &e.to_string());
                            continue;
                        }
                    }
                    self.script(c, &new_id, true);
                    self.created.push(self.item(c, &new_id, false));
                    new_id
                }
            };
            if self.tree.nodes[c].source.is_some() && script_context(&class, None).is_none() {
                self.failed.push(Failure {
                    path: self.tree.path(c),
                    class: class.clone(),
                    line: self.tree.nodes[c].line,
                    message: format!(
                        "Source is a script property and {class} is not a script; it was not set"
                    ),
                });
            }
            self.mapped.insert(c, child_id.clone());
            self.desired.insert(child_id.clone());
            self.visit(c, &child_id);
        }
    }

    /// Gives a script instance its file and source. Returns true when an
    /// existing script's source changed.
    fn script(&mut self, node: usize, id: &str, created: bool) -> bool {
        let n = &self.tree.nodes[node];
        let class = self.docs.class(id);
        let Some(ctx) = script_context(&class, None) else {
            return false;
        };
        let source = n.source.clone();
        let existing = self.docs.script_path(id);
        if let Some(p) = existing.as_ref().filter(|p| !safe_relative(p)) {
            self.failed.push(Failure {
                path: self.tree.path(node),
                class: class.clone(),
                line: n.line,
                message: format!(
                    "its script file {p:?} in the scene leaves the project directory; the source was not written"
                ),
            });
            return false;
        }
        let path = match &existing {
            Some(p) => p.clone(),
            None => {
                let p = self.new_script_path(node, &class);
                let doc = self.docs.get_mut(id).expect("the instance exists");
                let mut sr = IndexMap::new();
                sr.insert("path".to_string(), Doc::Str(p.clone()));
                sr.insert("context".to_string(), Doc::Str(format!("{ctx:?}")));
                doc.components.insert("ScriptRef".to_string(), sr);
                p
            }
        };
        let on_disk = std::fs::read_to_string(self.project.dir.join(&path)).ok();
        let text = match (&source, &on_disk) {
            (Some(s), _) => s.clone(),
            (None, Some(_)) => return false,
            (None, None) => String::new(),
        };
        if on_disk.as_deref() != Some(text.as_str()) {
            self.writes.push((path, text));
            return !created && existing.is_some();
        }
        false
    }

    fn new_script_path(&mut self, node: usize, class: &str) -> String {
        let path = self.tree.path(node);
        let mut dir = String::from("scripts");
        for p in &path[..path.len() - 1] {
            dir.push('/');
            dir.push_str(&file_name(p));
        }
        let stem = file_name(&path[path.len() - 1]);
        let suffix = script_suffix(class);
        let mut n = 1;
        loop {
            let candidate = if n == 1 {
                format!("{dir}/{stem}{suffix}")
            } else {
                format!("{dir}/{stem}~{n}{suffix}")
            };
            // Case-folded: two names that differ only in case would be one
            // file on macOS and Windows.
            let key = candidate.to_lowercase();
            if !self.taken.contains(&key) && !self.project.dir.join(&candidate).exists() {
                self.taken.insert(key);
                return candidate;
            }
            n += 1;
        }
    }

    fn remove_stale(&mut self) {
        let mut doomed: Vec<String> = Vec::new();
        for scope in &self.tree.scopes {
            match scope {
                StaleScope::Scripts => {
                    for e in &self.docs.list {
                        if e.id.starts_with(IMPORT_ID_PREFIX)
                            && !self.desired.contains(&e.id)
                            && script_context(&self.docs.class(&e.id), None).is_some()
                        {
                            doomed.push(e.id.clone());
                        }
                    }
                }
                StaleScope::Named { parent, name } => {
                    let Some(pid) = self.mapped.get(parent).cloned() else {
                        continue;
                    };
                    for c in self.docs.live_children(&pid) {
                        if self.docs.get(&c).is_some_and(|d| d.name == *name) {
                            if !c.starts_with(IMPORT_ID_PREFIX) && !self.desired.contains(&c) {
                                self.diagnostics.push(Diagnostic::warning(
                                    codes::INSTANCE_FAILED,
                                    format!(
                                        "kept {} ({}): the loader replaces it, but the importer only removes what an import created",
                                        display_path(&self.docs.path(&c)),
                                        self.docs.class(&c)
                                    ),
                                ));
                            }
                            let mut sub = Vec::new();
                            self.docs.subtree(&c, &mut sub);
                            doomed.extend(sub.into_iter().filter(|x| {
                                x.starts_with(IMPORT_ID_PREFIX) && !self.desired.contains(x)
                            }));
                        }
                    }
                }
                StaleScope::UndeclaredServices => {
                    for c in self.docs.live_children(GAME_ID) {
                        let class = self.docs.class(&c);
                        if self
                            .tree
                            .services
                            .iter()
                            .any(|&s| self.tree.nodes[s].class == class)
                        {
                            continue;
                        }
                        let mut sub = Vec::new();
                        self.docs.subtree(&c, &mut sub);
                        doomed.extend(sub.into_iter().filter(|x| {
                            x.starts_with(IMPORT_ID_PREFIX) && !self.desired.contains(x)
                        }));
                    }
                }
                StaleScope::Subtree(node) => {
                    let Some(pid) = self.mapped.get(node).cloned() else {
                        continue;
                    };
                    let mut sub = Vec::new();
                    self.docs.subtree(&pid, &mut sub);
                    doomed.extend(
                        sub.into_iter().filter(|x| {
                            x.starts_with(IMPORT_ID_PREFIX) && !self.desired.contains(x)
                        }),
                    );
                }
            }
        }
        let doomed_set: HashSet<String> = doomed.iter().cloned().collect();
        let mut done = HashSet::new();
        for id in doomed {
            if !done.insert(id.clone()) || self.docs.removed.contains(&id) {
                continue;
            }
            let mut sub = Vec::new();
            self.docs.subtree(&id, &mut sub);
            if let Some(keeper) = sub.iter().find(|x| !doomed_set.contains(*x)) {
                self.failed.push(Failure {
                    path: self.docs.path(&id),
                    class: self.docs.class(&id),
                    line: None,
                    message: format!(
                        "kept: it is stale, but it holds {}, which an import did not create",
                        display_path(&self.docs.path(keeper))
                    ),
                });
                continue;
            }
            for x in &sub {
                done.insert(x.clone());
                self.removed.push(Item {
                    path: self.docs.path(x),
                    class: self.docs.class(x),
                    id: x.clone(),
                    line: None,
                    updated: false,
                });
                if let Some(p) = self.docs.script_path(x)
                    && p.starts_with("scripts/")
                    && safe_relative(&p)
                {
                    self.deletes.push(p);
                }
            }
            for x in sub {
                self.docs.removed.insert(x);
            }
        }
        // A file another remaining instance still uses stays.
        let used: HashSet<String> = self
            .docs
            .list
            .iter()
            .filter(|e| !self.docs.removed.contains(&e.id))
            .filter_map(|e| self.docs.script_path(&e.id))
            .collect();
        self.deletes.retain(|p| !used.contains(p));
        self.deletes.sort();
        self.deletes.dedup();
    }
}

/// A script path from the scene that stays inside the project directory:
/// relative, `/`-separated, with no `..`, `.` or empty parts and no drive.
fn safe_relative(p: &str) -> bool {
    !p.is_empty()
        && !p.starts_with('/')
        && !p.contains('\\')
        && !p.contains(':')
        && p.split('/').all(|part| !matches!(part, "" | "." | ".."))
}

/// Writes a plan into the project directory: scripts first, then the scene,
/// so an interrupted write leaves the old scene pointing at files that exist.
pub fn write(dir: &Path, plan: &Plan) -> Result<(), Diagnostic> {
    let fail = |what: &str, e: std::io::Error| {
        Diagnostic::error(codes::WRITE_FAILED, format!("cannot write {what}: {e}"))
            .at(Location::file(dir.join(what).display().to_string()))
    };
    std::fs::create_dir_all(dir).map_err(|e| fail(".", e))?;
    if let Some(cfg) = &plan.config {
        std::fs::write(dir.join("real.toml"), cfg).map_err(|e| fail("real.toml", e))?;
    }
    for (path, text) in &plan.writes {
        let full = dir.join(path);
        if let Some(parent) = full.parent() {
            std::fs::create_dir_all(parent).map_err(|e| fail(path, e))?;
        }
        std::fs::write(&full, text).map_err(|e| fail(path, e))?;
    }
    if let Some((scene_path, scene_text)) = &plan.scene {
        let full = dir.join(scene_path);
        if let Some(parent) = full.parent() {
            std::fs::create_dir_all(parent).map_err(|e| fail(scene_path, e))?;
        }
        std::fs::write(&full, scene_text).map_err(|e| fail(scene_path, e))?;
    }
    for path in &plan.deletes {
        match std::fs::remove_file(dir.join(path)) {
            Ok(()) => {}
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(e) => return Err(fail(path, e)),
        }
    }
    Ok(())
}

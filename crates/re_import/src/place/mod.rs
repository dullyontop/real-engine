//! Place and model files (`.rbxl`, `.rbxm` binary; `.rbxlx`, `.rbxmx` XML),
//! read by our own reader (ADR-0048 §4) into the instance [`Tree`] the other
//! readers produce.
//!
//! A place's roots are its services; a model's roots go under `Workspace`, or
//! under the dotted path [`ImportOptions::parent`](crate::ImportOptions)
//! names. `Name`, `Source`, `Tags` and `AttributesSerialize` become the node's
//! name, script, tags and attributes; every other property is kept as the
//! file's value under its current name (`data/serialized_names.json` maps the
//! older names files still use) for [`apply`](crate::apply) to convert by the
//! property's type, and to report (`RE9811`) when the class layer cannot set
//! it.

pub mod binary;
pub mod dom;
pub mod xml;

use std::collections::HashMap;
use std::path::Path;
use std::sync::OnceLock;

use re_core::{Diagnostic, Location};

use crate::codes;
use crate::tree::{Kind, StaleScope, Tree, container_class};
use dom::{Dom, PlaceValue};

/// The extensions of place files (services at the root).
pub const PLACE_EXTENSIONS: &[&str] = &["rbxl", "rbxlx"];
/// The extensions of model files (instances at the root).
pub const MODEL_EXTENSIONS: &[&str] = &["rbxm", "rbxmx"];

/// The service a model goes under when no parent is given.
pub const DEFAULT_PARENT: &str = "Workspace";

#[derive(serde::Deserialize)]
struct Names {
    renames: HashMap<String, String>,
    ignored: Vec<String>,
}

fn names() -> &'static Names {
    static NAMES: OnceLock<Names> = OnceLock::new();
    NAMES.get_or_init(|| {
        serde_json::from_str(include_str!("../../data/serialized_names.json"))
            .expect("data/serialized_names.json parses")
    })
}

/// The property a saved name stands for on `class`, or `None` for metadata.
pub fn property_name<'a>(class: &str, saved: &'a str) -> Option<&'a str> {
    let n = names();
    if n.ignored.iter().any(|i| i == saved) {
        return None;
    }
    let qualified = format!("{class}.{saved}");
    Some(
        n.renames
            .get(&qualified)
            .or_else(|| n.renames.get(saved))
            .map_or(saved, String::as_str),
    )
}

/// The extension of `file`, lowercased.
fn extension(file: &Path) -> String {
    file.extension()
        .map(|e| e.to_string_lossy().to_lowercase())
        .unwrap_or_default()
}

/// True if `file` is named as a place or model file.
pub fn is_place_file(file: &Path) -> bool {
    let e = extension(file);
    PLACE_EXTENSIONS.contains(&e.as_str()) || MODEL_EXTENSIONS.contains(&e.as_str())
}

/// Reads a place or model file into the document it holds, binary or XML by
/// its first bytes.
pub fn read_dom(bytes: &[u8], label: &str) -> Result<Dom, Diagnostic> {
    if binary::is_binary(bytes) {
        return binary::read(bytes, label);
    }
    let text = std::str::from_utf8(bytes).map_err(|e| {
        Diagnostic::error(
            codes::PLACE_MALFORMED,
            format!(
                "{label} is not a readable place file: it is neither binary nor UTF-8 XML (byte {})",
                e.valid_up_to()
            ),
        )
        .at(Location::file(label.to_string()))
    })?;
    xml::read(text.strip_prefix('\u{feff}').unwrap_or(text), label)
}

/// Reads `file` into an instance tree. `parent` is the dotted path a model's
/// roots go under (`ReplicatedStorage.Maps`); it is refused for a place.
pub fn read(file: &Path, parent: Option<&str>) -> Result<Tree, Diagnostic> {
    let label = file.display().to_string();
    let bytes = std::fs::read(file).map_err(|e| {
        Diagnostic::error(codes::INPUT_UNREADABLE, format!("cannot read {label}: {e}"))
            .at(Location::file(label.clone()))
    })?;
    let dom = read_dom(&bytes, &label)?;
    let model = MODEL_EXTENSIONS.contains(&extension(file).as_str());
    if !model && parent.is_some() {
        return Err(Diagnostic::error(
            codes::INPUT_UNREADABLE,
            format!("{label} is a place, so its services cannot go under a parent"),
        )
        .with_hint("--parent is for model files (.rbxm, .rbxmx)"));
    }
    convert(dom, &label, model.then(|| parent.unwrap_or(DEFAULT_PARENT)))
}

/// Builds the tree. `model_parent` is the dotted parent of a model's roots;
/// `None` reads the roots as services.
pub fn convert(dom: Dom, label: &str, model_parent: Option<&str>) -> Result<Tree, Diagnostic> {
    let mut tree = Tree::new();
    tree.warnings = dom.warnings.clone();
    let mut c = Converter {
        dom: &dom,
        tree: &mut tree,
        label,
        nodes: vec![None; dom.instances.len()],
    };
    match model_parent {
        None => {
            for &r in &dom.roots {
                let class = &dom.instances[r].class;
                let svc = c.tree.service(class, None);
                c.nodes[r] = Some(svc);
                c.fill(r, svc);
                for &k in &dom.instances[r].children {
                    c.declare(k, svc);
                }
            }
            for i in 0..c.tree.services.len() {
                let s = c.tree.services[i];
                c.tree.scopes.push(StaleScope::Subtree(s));
            }
            c.tree.scopes.push(StaleScope::UndeclaredServices);
        }
        Some(path) => {
            let mut parts = path.split('.').filter(|p| !p.is_empty());
            let Some(first) = parts.next() else {
                return Err(Diagnostic::error(
                    codes::INPUT_UNREADABLE,
                    format!("{path:?} is not a parent path: name a service first"),
                ));
            };
            let mut at = c.tree.service(first, None);
            for p in parts {
                let class = container_class(&c.tree.nodes[at].class, p);
                at = c.tree.container(at, p, class, None);
            }
            for &r in &dom.roots {
                let n = c.declare(r, at);
                let name = c.tree.nodes[n].name.clone();
                let scope = StaleScope::Named { parent: at, name };
                if !c.tree.scopes.iter().any(|s| same_scope(s, &scope)) {
                    c.tree.scopes.push(scope);
                }
            }
        }
    }
    // Instance references, now that every instance has its node.
    let nodes = c.nodes;
    for n in &mut tree.nodes {
        for (_, v) in &mut n.place {
            if let PlaceValue::Ref(Some(i)) = v {
                *v = PlaceValue::Ref(nodes.get(*i).copied().flatten());
            }
        }
    }
    Ok(tree)
}

fn same_scope(a: &StaleScope, b: &StaleScope) -> bool {
    match (a, b) {
        (StaleScope::Named { parent: p, name: n }, StaleScope::Named { parent: q, name: m }) => {
            p == q && n == m
        }
        _ => false,
    }
}

struct Converter<'a> {
    dom: &'a Dom,
    tree: &'a mut Tree,
    label: &'a str,
    /// The node of each instance of the file.
    nodes: Vec<Option<usize>>,
}

fn text(v: &PlaceValue) -> Option<String> {
    match v {
        PlaceValue::String(b) | PlaceValue::SharedString(b) => {
            Some(String::from_utf8_lossy(b).into_owned())
        }
        _ => None,
    }
}

impl Converter<'_> {
    /// Declares instance `i` and its descendants under node `parent`.
    fn declare(&mut self, i: usize, parent: usize) -> usize {
        let inst = &self.dom.instances[i];
        let name = inst
            .get("Name")
            .and_then(text)
            .unwrap_or_else(|| inst.class.clone());
        let n = self.tree.instance(&inst.class, &name, Kind::Declared, None);
        self.tree.set_parent(n, Some(parent));
        self.nodes[i] = Some(n);
        self.fill(i, n);
        for &k in &inst.children {
            self.declare(k, n);
        }
        n
    }

    /// Copies the properties of instance `i` onto node `n`.
    fn fill(&mut self, i: usize, n: usize) {
        let inst = &self.dom.instances[i];
        for (saved, v) in &inst.properties {
            match saved.as_str() {
                "Name" => {}
                "Source" => self.tree.nodes[n].source = text(v),
                "Tags" => {
                    if let Some(t) = text(v) {
                        self.tree.nodes[n].tags = t
                            .split('\0')
                            .filter(|t| !t.is_empty())
                            .map(str::to_string)
                            .collect();
                    }
                }
                "AttributesSerialize" => {
                    if let PlaceValue::String(b) = v
                        && !b.is_empty()
                    {
                        self.attributes(n, &inst.class, b);
                    }
                }
                _ => {
                    if let Some(name) = property_name(&inst.class, saved) {
                        self.tree.nodes[n].place.push((name.to_string(), v.clone()));
                    }
                }
            }
        }
    }

    fn attributes(&mut self, n: usize, class: &str, blob: &[u8]) {
        match attributes(blob) {
            Ok((values, skipped)) => {
                self.tree.nodes[n].attributes.extend(values);
                for (name, ty) in skipped {
                    let path = crate::display_path(&self.tree.path(n));
                    self.tree.warnings.push(
                        Diagnostic::warning(
                            codes::PLACE_UNMAPPED,
                            format!(
                                "{}: attribute {name:?} of {path} ({class}) has type 0x{ty:02x}, which the importer does not convert; it is not imported",
                                self.label
                            ),
                        )
                        .at(Location::file(self.label.to_string())),
                    );
                }
            }
            Err(msg) => {
                let path = crate::display_path(&self.tree.path(n));
                self.tree.warnings.push(
                    Diagnostic::warning(
                        codes::PLACE_UNMAPPED,
                        format!(
                            "{}: the attributes of {path} ({class}) do not read ({msg}); none are imported",
                            self.label
                        ),
                    )
                    .at(Location::file(self.label.to_string())),
                );
            }
        }
    }
}

/// Decoded attributes, and the names and type bytes of those skipped.
pub type Attributes = (Vec<(String, serde_json::Value)>, Vec<(String, u8)>);

/// The attributes an `AttributesSerialize` blob holds (little-endian: a
/// count, then a length-prefixed name, a type byte and a value each), as the
/// JSON values the importer sets, with the names and types it skipped. A
/// value of a type it cannot size stops the read, so the whole blob must
/// read to its end or nothing is taken.
pub fn attributes(blob: &[u8]) -> Result<Attributes, String> {
    struct C<'a>(&'a [u8]);
    impl C<'_> {
        fn take(&mut self, n: usize) -> Result<&[u8], String> {
            if self.0.len() < n {
                return Err("it ends early".into());
            }
            let (a, b) = self.0.split_at(n);
            self.0 = b;
            Ok(a)
        }
        fn u32(&mut self) -> Result<u32, String> {
            Ok(u32::from_le_bytes(
                self.take(4)?.try_into().expect("4 bytes"),
            ))
        }
        fn f32(&mut self) -> Result<f32, String> {
            Ok(f32::from_le_bytes(
                self.take(4)?.try_into().expect("4 bytes"),
            ))
        }
        fn string(&mut self) -> Result<String, String> {
            let n = self.u32()? as usize;
            Ok(String::from_utf8_lossy(self.take(n)?).into_owned())
        }
    }
    use serde_json::json;
    let mut c = C(blob);
    let count = c.u32()?;
    let mut out = Vec::new();
    let mut skipped = Vec::new();
    for _ in 0..count {
        let name = c.string()?;
        let ty = c.take(1)?[0];
        let v = match ty {
            0x02 => Some(json!(c.string()?)),
            0x03 => Some(json!(c.take(1)?[0] != 0)),
            0x05 => Some(json!(c.f32()?)),
            0x06 => Some(json!(f64::from_le_bytes(
                c.take(8)?.try_into().expect("8 bytes")
            ))),
            0x11 => Some(json!([c.f32()?, c.f32()?, c.f32()?])),
            // Sized types the importer does not convert: skipped by size.
            0x09 => c.take(8).map(|_| None)?,  // UDim
            0x0A => c.take(16).map(|_| None)?, // UDim2
            0x0E => c.take(4).map(|_| None)?,  // BrickColor
            0x0F => c.take(12).map(|_| None)?, // Color3
            0x10 => c.take(8).map(|_| None)?,  // Vector2
            0x1B => c.take(8).map(|_| None)?,  // NumberRange
            0x1C => c.take(16).map(|_| None)?, // Rect
            0x17 => {
                let n = c.u32()? as usize;
                c.take(n.checked_mul(12).ok_or("a count overflows")?)?;
                None
            }
            0x19 => {
                let n = c.u32()? as usize;
                c.take(n.checked_mul(20).ok_or("a count overflows")?)?;
                None
            }
            other => return Err(format!("attribute {name:?} has type 0x{other:02x}")),
        };
        match v {
            Some(v) => out.push((name, v)),
            None => skipped.push((name, ty)),
        }
    }
    if !c.0.is_empty() {
        return Err(format!("{} bytes follow the last attribute", c.0.len()));
    }
    Ok((out, skipped))
}

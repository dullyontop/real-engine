//! Every GUI frame a test steps, written out with each `f32` to the bit and
//! compared, when the place is dropped, with
//! `tests/fixtures/gui/layout/<test file>/<test>.txt` (ADR-0058's exact
//! layout test; `RE_UPDATE_GOLDENS=1` writes the file instead). A frame
//! lists what the step returned, then the drawn elements and the property
//! values that differ from the frame before's: an element appears when it is
//! first drawn and again when its rectangle or clip changes, and a value
//! when the step writes it.

use std::collections::HashMap;
use std::fmt::Write as _;
use std::path::PathBuf;

use glam::Vec2;
use re_compat::gui::{GuiItem, GuiOutput, GuiRecord};
use re_compat::instance::tree;
use re_reflect::Value;
use re_scene::{Entity, World};

#[derive(Default)]
pub struct GuiLog {
    text: String,
    frames: usize,
    /// The frame before's drawn elements, with their paths and lines.
    items: Vec<(Entity, String, String)>,
    /// The frame before's values.
    values: HashMap<(Entity, &'static str), String>,
}

fn f(x: f32) -> String {
    format!("{x:?}")
}

fn v(p: Vec2) -> String {
    format!("({}, {})", f(p.x), f(p.y))
}

fn rect(r: egui::Rect) -> String {
    format!(
        "({}, {}, {}, {})",
        f(r.min.x),
        f(r.min.y),
        f(r.max.x),
        f(r.max.y)
    )
}

/// `e`'s names from below the data model, with the index among same-named
/// siblings where there are several.
fn path(w: &World, e: Entity) -> String {
    if !tree::alive(w, e) {
        return format!("{e:?} (gone)");
    }
    let mut parts = Vec::new();
    let mut at = e;
    loop {
        let name = tree::name(w, at);
        let parent = tree::parent(w, at);
        let mut part = name.clone();
        if let Some(p) = parent {
            let same: Vec<Entity> = tree::children(w, p)
                .into_iter()
                .filter(|&c| tree::name(w, c) == name)
                .collect();
            if same.len() > 1 {
                let i = same.iter().position(|&c| c == at).unwrap_or(0);
                part = format!("{name}#{i}");
            }
        }
        parts.push(part);
        match parent {
            Some(p) if tree::class(w, p) != Some("DataModel") => at = p,
            _ => break,
        }
    }
    parts.reverse();
    parts.join("/")
}

fn value(w: &World, x: &Value) -> String {
    match x {
        Value::Bool(b) => b.to_string(),
        Value::Int(i) => i.to_string(),
        Value::Float(x) => format!("{x:?}"),
        Value::Str(s) => format!("{s:?}"),
        Value::Entity(Some(e)) => path(w, *e),
        Value::Entity(None) => "nil".into(),
        Value::Record(_, fields) => {
            let fields: Vec<String> = fields.iter().map(|x| value(w, x)).collect();
            format!("({})", fields.join(", "))
        }
        other => format!("{other:?}"),
    }
}

fn item(it: &GuiItem) -> String {
    let mut line = format!("at {} size {}", v(it.min), v(it.size));
    if it.angle != 0.0 {
        let _ = write!(line, " angle {}", f(it.angle));
    }
    if let Some(c) = it.clip {
        let _ = write!(line, " clip {}", rect(c));
    }
    line
}

impl GuiLog {
    /// Adds one stepped frame.
    pub fn frame(&mut self, w: &World, out: &GuiOutput, r: &GuiRecord) {
        let s = &mut self.text;
        let opt = |e: Option<Entity>| e.map_or_else(|| "none".to_owned(), |e| path(w, e));
        let _ = writeln!(
            s,
            "frame {}: {} drawn, hovered {}, sinks {}, focus {}",
            self.frames,
            out.drawn,
            opt(out.hovered),
            out.sinks_pointer,
            opt(out.text_focus)
        );
        self.frames += 1;
        for sd in &out.surfaces {
            let _ = writeln!(
                s,
                "  surface {} canvas {} texture {:?} at {} pixels a point",
                path(w, sd.entity),
                v(sd.canvas),
                sd.texture_px,
                f(sd.pixels_per_point)
            );
        }
        for vd in &out.viewports {
            let _ = writeln!(s, "  viewport {} size {}", path(w, vd.entity), v(vd.size));
        }
        for d in &out.diagnostics {
            let _ = writeln!(s, "  [{}] {}", d.code, d.message);
        }
        let mut items = Vec::with_capacity(r.items.len());
        for it in &r.items {
            let p = path(w, it.entity);
            let line = item(it);
            if !self
                .items
                .iter()
                .any(|(e, q, l)| *e == it.entity && *q == p && *l == line)
            {
                let _ = writeln!(s, "  item {p} {line}");
            }
            items.push((it.entity, p, line));
        }
        for (e, p, _) in &self.items {
            if !items.iter().any(|(x, _, _)| x == e) {
                let _ = writeln!(s, "  gone {p}");
            }
        }
        self.items = items;
        let mut values = HashMap::with_capacity(r.writes.len());
        for (e, name, x) in &r.writes {
            let x = value(w, x);
            let last = values.get(&(*e, *name)).or(self.values.get(&(*e, *name)));
            if last != Some(&x) {
                let _ = writeln!(s, "  {}.{name} = {x}", path(w, *e));
            }
            values.insert((*e, *name), x);
        }
        self.values = values;
    }

    /// Compares the frames with the test's file, if the test stepped any.
    pub fn check(&self) {
        if self.frames == 0 {
            return;
        }
        let test = std::thread::current()
            .name()
            .unwrap_or("main")
            .replace("::", "-");
        let name = PLACES.with(|n| {
            let mut n = n.borrow_mut();
            let k = n.entry(test.clone()).or_insert(0);
            *k += 1;
            if *k == 1 {
                test.clone()
            } else {
                format!("{test}-{k}")
            }
        });
        let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
        let file = root
            .join("tests/fixtures/gui/layout")
            .join(env!("CARGO_CRATE_NAME"))
            .join(format!("{name}.txt"));
        if std::env::var_os("RE_UPDATE_GOLDENS").is_some_and(|v| !v.is_empty()) {
            std::fs::create_dir_all(file.parent().unwrap()).unwrap();
            std::fs::write(&file, &self.text).unwrap();
            println!("updated {}", file.display());
            return;
        }
        let expected = std::fs::read_to_string(&file)
            .unwrap_or_else(|e| {
                panic!(
                    "{}: {e}: run with RE_UPDATE_GOLDENS=1 and review the file",
                    file.display()
                )
            })
            .replace("\r\n", "\n");
        if expected == self.text {
            return;
        }
        let out = root
            .join("../../out/re_compat/gui-layout")
            .join(env!("CARGO_CRATE_NAME"));
        std::fs::create_dir_all(&out).unwrap();
        let saved = out.join(format!("{name}.txt"));
        std::fs::write(&saved, &self.text).unwrap();
        let (e, a): (Vec<&str>, Vec<&str>) =
            (expected.lines().collect(), self.text.lines().collect());
        let n = (0..e.len().max(a.len()))
            .find(|&i| e.get(i) != a.get(i))
            .unwrap_or(e.len());
        panic!(
            "{name}: line {} differs from {}\n  expected: {}\n  actual:   {}\n(the whole output is in {})",
            n + 1,
            file.display(),
            e.get(n).unwrap_or(&"(end)"),
            a.get(n).unwrap_or(&"(end)"),
            saved.display()
        );
    }
}

thread_local! {
    /// How many places each test has checked, so that a second place in one
    /// test gets a file of its own.
    static PLACES: std::cell::RefCell<HashMap<String, usize>> = Default::default();
}

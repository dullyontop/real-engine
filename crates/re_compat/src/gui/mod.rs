//! The 2D UI: `ScreenGui` and `BillboardGui` trees laid out, painted as
//! egui shapes and given pointer, keyboard and selection input.
//!
//! The host calls [`step`] once a frame inside its egui pass with the
//! containers to draw (the local player's `PlayerGui`), the viewport, the
//! camera and the frame's input. The step lays every enabled root out,
//! writes the read-only results (`AbsolutePosition`, `AbsoluteSize`,
//! `TextBounds` and the rest, only when they change), fires the input
//! events, and returns the shapes for the host to paint over the 3D view
//! (re_render's UI pass). Fonts come from [`install_fonts`].

mod fonts;
mod layout;
mod paint;
pub(crate) mod props;
mod text;

use std::sync::Arc;

use glam::Vec2;
use mlua::{Lua, MultiValue, Value as LuaValue};
use re_reflect::Value;
use re_scene::{Entity, World};

pub use layout::GuiCamera;

use crate::class::Accessor;
use crate::instance::tree;
use crate::runtime::{Bindings, state, with_world};
use crate::{CompatError, signal};
use layout::{Item, Kind, Layout, Root};
use paint::{Looks, Painter};
use props::{Scope, vec2_value};

/// Gives `ctx` egui's default fonts plus the bundled substitutes
/// (ADR-0035), replacing its fonts. Call it once before the first frame that
/// draws UI: text laid out before the fonts are in uses egui's own. A host
/// with fonts of its own uses [`add_fonts`] instead.
pub fn install_fonts(ctx: &egui::Context) {
    fonts::install(ctx);
}

/// Adds the bundled substitutes to a host's font definitions, keeping its
/// fonts; pass the result to `egui::Context::set_fonts`.
pub fn add_fonts(defs: &mut egui::FontDefinitions) {
    fonts::add_to(defs);
}

/// Paints the shapes of a [`step`] on a background layer of `ctx`, under
/// egui's own panels and windows.
pub fn paint(ctx: &egui::Context, shapes: Vec<egui::epaint::ClippedShape>) {
    let layer = egui::LayerId::new(egui::Order::Background, egui::Id::new("re_compat.gui"));
    let painter = ctx.layer_painter(layer);
    for s in shapes {
        painter.with_clip_rect(s.clip_rect).add(s.shape);
    }
}

/// The frame's input, in screen pixels.
#[derive(Clone, Debug, Default)]
pub struct GuiInput {
    /// Where the pointer is, `None` when it is outside the window.
    pub pointer: Option<Vec2>,
    /// True while the primary button is held.
    pub button1: bool,
    /// True while the secondary button is held.
    pub button2: bool,
    /// The wheel movement this frame, in pixels (positive y scrolls up).
    pub scroll: Vec2,
    /// The text typed this frame.
    pub text: String,
    /// True if Enter (Return) was pressed this frame.
    pub enter: bool,
    /// The number of Backspace presses this frame.
    pub backspace: u32,
    /// True if the selection was activated (a gamepad's A) this frame.
    pub select_activate: bool,
    /// A direction to move the selection in (a gamepad's stick or d-pad),
    /// y down.
    pub select_move: Option<Vec2>,
}

/// What one UI frame draws and what it is given.
#[derive(Clone, Debug, Default)]
pub struct GuiFrame {
    /// The window's size in pixels.
    pub viewport: Vec2,
    /// The height of the top bar that `IgnoreGuiInset` draws under.
    pub top_inset: f32,
    /// The containers whose `ScreenGui` children and `BillboardGui`
    /// descendants are drawn, back to front at equal `DisplayOrder`.
    pub roots: Vec<Entity>,
    /// The camera billboards are placed by; without one none are drawn.
    pub camera: Option<GuiCamera>,
    /// The frame's input.
    pub input: GuiInput,
}

/// What one UI frame produced.
#[derive(Clone, Debug, Default)]
pub struct GuiOutput {
    /// The shapes to paint, back to front, in screen pixels.
    pub shapes: Vec<egui::epaint::ClippedShape>,
    /// How many elements were drawn.
    pub drawn: usize,
    /// The topmost element under the pointer.
    pub hovered: Option<Entity>,
    /// True if the pointer is over an element that takes input (`Active`, a
    /// button or a text box), so the 3D view should not get the click.
    pub sinks_pointer: bool,
    /// The text box that has the keyboard, if any.
    pub text_focus: Option<Entity>,
}

/// What the input pass keeps from frame to frame.
#[derive(Default)]
struct Hub {
    hovered: Vec<Entity>,
    pressed1: Option<Entity>,
    pressed2: Option<Entity>,
    held1: bool,
    held2: bool,
    focused: Option<Entity>,
    /// Focus changes asked for by `CaptureFocus` and `ReleaseFocus` since the
    /// last frame: `Some(e)` to focus `e`, `None` to release.
    requests: Vec<Option<Entity>>,
    /// The read-only values written last frame, so an unchanged one is not
    /// written again.
    written: props::FxMap<(Entity, usize), Value>,
}

/// The properties the step writes that scripts can write too; they are
/// always written, the others only when they differ from last frame.
const SCRIPT_WRITABLE: &[&str] = &["CanvasPosition", "Text", "CursorPosition"];

fn hub_mut<R>(lua: &Lua, f: impl FnOnce(&mut Hub) -> R) -> R {
    if lua.app_data_ref::<Hub>().is_none() {
        lua.set_app_data(Hub::default());
    }
    let mut h = lua.app_data_mut::<Hub>().expect("installed above");
    f(&mut h)
}

/// An argument of a fired event.
#[derive(Clone, Debug)]
enum Arg {
    Int(i64),
    Bool(bool),
    Nil,
}

/// The `Font` property: the legacy name of `FontFace`.
fn font_accessor(class: &'static str) -> Accessor {
    Accessor {
        get: Arc::new(move |w, e| {
            let p = read_font_face(w, class, e);
            let (family, weight, style) = p.unwrap_or_default();
            Ok(Value::Enum(
                fonts::enum_of(fonts::family_stem(&family), &weight, &style).to_string(),
            ))
        }),
        set: Some(Arc::new(move |w, e, v| {
            let Value::Enum(item) = v else {
                return Err(CompatError::Runtime {
                    code: re_script::codes::BAD_ARGUMENT,
                    message: format!("Font must be an Enum.Font item, got {v:?}"),
                });
            };
            let Some((stem, weight, style)) = fonts::enum_face(&item) else {
                return Err(CompatError::Runtime {
                    code: re_script::codes::BAD_ARGUMENT,
                    message: format!("Enum.Font.{item} has no font face"),
                });
            };
            let id = re_reflect::RecordId::by_name("Font").ok_or_else(|| {
                CompatError::InvalidData("the Font record is not registered".into())
            })?;
            let face = Value::Record(
                id,
                vec![
                    Value::Str(fonts::family_id(stem)),
                    Value::Enum(weight.to_string()),
                    Value::Enum(style.to_string()),
                ],
            );
            crate::tween::set_own(w, e, class, "FontFace", face)
        })),
    }
}

fn read_font_face(w: &World, class: &str, e: Entity) -> Option<(String, String, String)> {
    let key = w
        .registry
        .try_key(&crate::class::own_component_name(class))?;
    let index = w.registry.schema(key).field_index("FontFace").ok()?;
    match w.registry.get_field(&w.ecs, e, key, index)? {
        Value::Record(_, fs) if fs.len() == 3 => {
            let s = |v: &Value| match v {
                Value::Str(s) | Value::Enum(s) => s.clone(),
                _ => String::new(),
            };
            Some((s(&fs[0]), s(&fs[1]), s(&fs[2])))
        }
        _ => None,
    }
}

/// The builtin-property accessors of the UI classes, by `(class, property)`.
pub fn accessors() -> Vec<(&'static str, &'static str, Accessor)> {
    ["TextLabel", "TextButton", "TextBox"]
        .into_iter()
        .map(|class| (class, "Font", font_accessor(class)))
        .collect()
}

/// Registers the `Font` accessors and the `TextBox` methods.
pub fn register(b: &mut Bindings) {
    for (class, prop, a) in accessors() {
        b.accessor(class, prop, a);
    }
    b.method(
        "TextBox",
        "CaptureFocus",
        Arc::new(|lua, e, _| {
            hub_mut(lua, |h| h.requests.push(Some(e)));
            Ok(MultiValue::new())
        }),
    );
    b.method(
        "TextBox",
        "ReleaseFocus",
        Arc::new(|lua, e, _| {
            hub_mut(lua, |h| {
                // Only the box that will have the focus once the pending
                // requests apply can release it.
                let will_have = h.requests.last().copied().unwrap_or(h.focused);
                if will_have == Some(e) {
                    h.requests.push(None);
                }
            });
            Ok(MultiValue::new())
        }),
    );
    b.method(
        "TextBox",
        "IsFocused",
        Arc::new(|lua, e, _| {
            let focused = hub_mut(lua, |h| match h.requests.last() {
                Some(r) => *r == Some(e),
                None => h.focused == Some(e),
            });
            Ok(MultiValue::from_vec(vec![LuaValue::Boolean(focused)]))
        }),
    );
}

/// Every `BillboardGui` in the world, found through its own component so
/// the step need not walk the containers (a container can be `Workspace`).
fn billboards(w: &mut World) -> Vec<Entity> {
    let Some(key) = w
        .registry
        .try_key(&crate::class::own_component_name("BillboardGui"))
    else {
        return Vec::new();
    };
    let id = w.registry.component_id(key);
    let mut q = re_scene::ecs::query::QueryBuilder::<Entity>::new(&mut w.ecs)
        .with_id(id)
        .build();
    q.iter(&w.ecs).collect()
}

/// The roots to draw, back to front: the enabled `ScreenGui` children of the
/// containers (or a container that is one), and the enabled `BillboardGui`s
/// inside them.
fn roots(s: &Scope, frame: &GuiFrame, boards_in_world: &[Entity]) -> Vec<Root> {
    let w = s.w;
    let mut screens: Vec<(i64, usize, Root)> = Vec::new();
    let mut boards: Vec<(f32, usize, Root)> = Vec::new();
    let viewport = egui::Rect::from_min_size(
        egui::Pos2::ZERO,
        egui::vec2(frame.viewport.x, frame.viewport.y),
    );
    let enabled = |e: Entity| s.props(e).bool("Enabled", true);
    let global = |e: Entity| s.props(e).item("ZIndexBehavior", "Sibling") == "Global";
    let inset = Vec2::new(0.0, frame.top_inset);
    let mut seen = std::collections::HashSet::new();
    let containers: Vec<Entity> = frame
        .roots
        .iter()
        .copied()
        .filter(|&c| tree::alive(w, c))
        .collect();
    for &c in &containers {
        let mut found = vec![c];
        found.extend(tree::children(w, c));
        for e in found {
            if !s.is_a(e, "ScreenGui") || !enabled(e) || !seen.insert(e) {
                continue;
            }
            let p = s.props(e);
            let origin = if p.bool("IgnoreGuiInset", false)
                || p.item("ScreenInsets", "CoreUISafeInsets") == "None"
            {
                Vec2::ZERO
            } else {
                inset
            };
            let n = screens.len();
            screens.push((
                p.int("DisplayOrder", 0),
                n,
                Root {
                    e,
                    origin,
                    abs_origin: inset,
                    size: (frame.viewport - origin).max(Vec2::ZERO),
                    global_z: global(e),
                    clip: Some(viewport),
                    input: true,
                },
            ));
        }
    }
    if let Some(cam) = frame.camera.as_ref() {
        for &e in boards_in_world {
            if !tree::alive(w, e) || !enabled(e) || !seen.insert(e) {
                continue;
            }
            let ancestors = tree::ancestors(w, e);
            if !containers.iter().any(|c| ancestors.contains(c)) {
                continue;
            }
            // Inside a disabled ScreenGui it is not drawn.
            if ancestors
                .iter()
                .any(|&a| s.is_a(a, "LayerCollector") && !enabled(a))
            {
                continue;
            }
            let Some((min, size, depth)) = layout::billboard_rect(s, e, cam, frame.viewport) else {
                continue;
            };
            let p = s.props(e);
            let clip = if p.bool("ClipsDescendants", false) {
                egui::Rect::from_min_size(egui::pos2(min.x, min.y), egui::vec2(size.x, size.y))
                    .intersect(viewport)
            } else {
                viewport
            };
            let n = boards.len();
            boards.push((
                depth,
                n,
                Root {
                    e,
                    origin: min,
                    abs_origin: inset,
                    size,
                    global_z: global(e),
                    clip: Some(clip),
                    input: p.bool("Active", false),
                },
            ));
        }
    }
    // Billboards, far to near, under the screen UI.
    boards.sort_by(|a, b| b.0.total_cmp(&a.0).then(a.1.cmp(&b.1)));
    screens.sort_by_key(|s| (s.0, s.1));
    boards
        .into_iter()
        .map(|b| b.2)
        .chain(screens.into_iter().map(|s| s.2))
        .collect()
}

/// True if screen point `p` is inside the drawn item.
fn contains(it: &Item, p: Vec2) -> bool {
    if it.clip.is_some_and(|c| !c.contains(egui::pos2(p.x, p.y))) {
        return false;
    }
    let q = it.rot.inverse().transform_point2(p);
    q.cmpge(it.min).all() && q.cmple(it.min + it.size).all()
}

/// Takes input: a button, a text box or an `Active` element.
fn takes_input(s: &Scope, it: &Item) -> bool {
    it.interactable
        && (it.kind == Kind::TextBox
            || s.is_a(it.e, "GuiButton")
            || s.props(it.e).bool("Active", false))
}

/// The `GuiService`, when it exists and has `SelectedObject`.
fn gui_service(s: &Scope, game: Entity) -> Option<Entity> {
    tree::children(s.w, game)
        .into_iter()
        .find(|&c| tree::class(s.w, c) == Some("GuiService"))
        .filter(|&g| s.classes.get_property(s.w, g, "SelectedObject").is_ok())
}

/// Lays out, paints and takes the input of one UI frame. Call it from the
/// host's egui pass on the client VM; see the module docs.
pub fn step(lua: &Lua, ctx: &egui::Context, frame: &GuiFrame) -> mlua::Result<GuiOutput> {
    let st = state(lua)?;
    let classes = st.setup.classes.clone();
    let game = st.game;
    let (requests, mut hub) = hub_mut(lua, |h| {
        let r = std::mem::take(&mut h.requests);
        (
            r,
            Hub {
                hovered: std::mem::take(&mut h.hovered),
                pressed1: h.pressed1,
                pressed2: h.pressed2,
                held1: h.held1,
                held2: h.held2,
                focused: h.focused,
                requests: Vec::new(),
                written: std::mem::take(&mut h.written),
            },
        )
    });
    let input = &frame.input;
    let mut events: Vec<(Entity, &'static str, Vec<Arg>)> = Vec::new();
    let out = with_world(lua, |w| {
        let classes = &*classes;
        let boards_in_world = billboards(w);
        let mut resolved = Vec::new();
        let mut selection = None;
        let out = {
            let scope = Scope::new(w, classes);
            let s = &scope;
            let w: &World = w;
            let mut lay = Layout::new(s, ctx);
            let roots = roots(s, frame, &boards_in_world);
            let mut items: Vec<Item> = Vec::new();
            for r in &roots {
                lay.root(r);
                let mut mine = std::mem::take(&mut lay.items);
                mine.sort_by_key(|it| it.key);
                items.extend(mine);
            }
            let mut writes = std::mem::take(&mut lay.writes);
            drop(lay);

            // Hover: every element under the pointer; the topmost takes input.
            let under: Vec<usize> = match input.pointer {
                Some(p) => (0..items.len())
                    .rev()
                    .filter(|&i| items[i].interactable && contains(&items[i], p))
                    .collect(),
                None => Vec::new(),
            };
            let target = under.iter().copied().find(|&i| takes_input(s, &items[i]));
            let pxy = |p: Option<Vec2>| {
                let p = p.unwrap_or_default() - Vec2::new(0.0, frame.top_inset);
                vec![Arg::Int(p.x.round() as i64), Arg::Int(p.y.round() as i64)]
            };
            let now: Vec<Entity> = under.iter().map(|&i| items[i].e).collect();
            for &e in &hub.hovered {
                if !now.contains(&e) && tree::alive(w, e) {
                    events.push((e, "MouseLeave", pxy(input.pointer)));
                }
            }
            for &e in &now {
                if hub.hovered.contains(&e) {
                    events.push((e, "MouseMoved", pxy(input.pointer)));
                } else {
                    events.push((e, "MouseEnter", pxy(input.pointer)));
                }
            }
            // MouseMoved only when the pointer moved is the original's rule; the
            // host's pointer is per frame, so it fires each frame it is over.
            hub.hovered = now;
            let target_e = target.map(|i| items[i].e);
            let is_button = |e: Entity| s.is_a(e, "GuiButton");

            // Buttons.
            let down1 = input.button1 && !hub.held1;
            let up1 = !input.button1 && hub.held1;
            let down2 = input.button2 && !hub.held2;
            let up2 = !input.button2 && hub.held2;
            if down1 {
                hub.pressed1 = target_e;
                if let Some(e) = target_e
                    && is_button(e)
                {
                    events.push((e, "MouseButton1Down", pxy(input.pointer)));
                }
            }
            if up1 {
                if let Some(e) = target_e
                    && is_button(e)
                {
                    events.push((e, "MouseButton1Up", pxy(input.pointer)));
                    if hub.pressed1 == Some(e) {
                        events.push((e, "MouseButton1Click", Vec::new()));
                        events.push((e, "Activated", vec![Arg::Nil, Arg::Int(1)]));
                    }
                }
                hub.pressed1 = None;
            }
            if down2 {
                hub.pressed2 = target_e;
                if let Some(e) = target_e
                    && is_button(e)
                {
                    events.push((e, "MouseButton2Down", pxy(input.pointer)));
                }
            }
            if up2 {
                if let Some(e) = target_e
                    && is_button(e)
                {
                    events.push((e, "MouseButton2Up", pxy(input.pointer)));
                    if hub.pressed2 == Some(e) {
                        events.push((e, "MouseButton2Click", Vec::new()));
                    }
                }
                hub.pressed2 = None;
            }
            hub.held1 = input.button1;
            hub.held2 = input.button2;

            // Text boxes: focus by click or CaptureFocus, typing, Enter.
            let focus_to = |hub: &mut Hub,
                            events: &mut Vec<(Entity, &'static str, Vec<Arg>)>,
                            writes: &mut Vec<(Entity, &'static str, Value)>,
                            next: Option<Entity>,
                            enter: bool| {
                if hub.focused == next {
                    return;
                }
                if let Some(old) = hub.focused.take()
                    && tree::alive(w, old)
                {
                    writes.push((old, "CursorPosition", Value::Int(-1)));
                    events.push((old, "FocusLost", vec![Arg::Bool(enter), Arg::Nil]));
                }
                if let Some(e) = next {
                    let p = s.props(e);
                    let mut text = p.string("Text");
                    if p.bool("ClearTextOnFocus", true) && p.bool("TextEditable", true) {
                        text.clear();
                        writes.push((e, "Text", Value::Str(String::new())));
                    }
                    writes.push((
                        e,
                        "CursorPosition",
                        Value::Int(text.chars().count() as i64 + 1),
                    ));
                    events.push((e, "Focused", Vec::new()));
                    hub.focused = Some(e);
                }
            };
            for r in requests {
                let r = r.filter(|&e| tree::alive(w, e));
                focus_to(&mut hub, &mut events, &mut writes, r, false);
            }
            if down1 {
                let next = target_e.filter(|&e| s.is_a(e, "TextBox"));
                focus_to(&mut hub, &mut events, &mut writes, next, false);
            }
            // A box that is gone or no longer drawn loses the focus.
            if let Some(e) = hub.focused
                && (!tree::alive(w, e)
                    || tree::is_destroyed(w, e)
                    || !items.iter().any(|it| it.e == e))
            {
                if tree::alive(w, e) && !tree::is_destroyed(w, e) {
                    focus_to(&mut hub, &mut events, &mut writes, None, false);
                } else {
                    hub.focused = None;
                }
            }
            if let Some(e) = hub.focused {
                {
                    let p = s.props(e);
                    let editable = p.bool("TextEditable", true);
                    let multi = p.bool("MultiLine", false);
                    let mut text = writes
                        .iter()
                        .rev()
                        .find(|(x, n, _)| *x == e && *n == "Text")
                        .and_then(|(_, _, v)| match v {
                            Value::Str(s) => Some(s.clone()),
                            _ => None,
                        })
                        .unwrap_or_else(|| p.string("Text"));
                    let before = text.clone();
                    if editable {
                        for _ in 0..input.backspace {
                            text.pop();
                        }
                        text.extend(input.text.chars().filter(|c| !c.is_control()));
                        if input.enter && multi {
                            text.push('\n');
                        }
                    }
                    if text != before {
                        writes.push((e, "Text", Value::Str(text.clone())));
                        writes.push((
                            e,
                            "CursorPosition",
                            Value::Int(text.chars().count() as i64 + 1),
                        ));
                    }
                    if input.enter && !multi {
                        focus_to(&mut hub, &mut events, &mut writes, None, true);
                    }
                }
            }

            // The wheel scrolls the topmost scrolling frame under the pointer.
            if input.scroll != Vec2::ZERO
                && let Some(&i) = under.iter().find(|&&i| {
                    items[i].kind == Kind::Scrolling
                        && s.props(items[i].e).bool("ScrollingEnabled", true)
                })
            {
                let it = &items[i];
                let p = s.props(it.e);
                let dir = p.item("ScrollingDirection", "XY");
                let (ax, ay) = match dir.as_str() {
                    "X" => (true, false),
                    "Y" => (false, true),
                    _ => (true, true),
                };
                let at = written_vec2(&writes, it.e, "CanvasPosition")
                    .unwrap_or_else(|| p.vec2("CanvasPosition", Vec2::ZERO));
                let canvas = it.canvas.unwrap_or(it.size) / it.scale.max(1e-6);
                let max = (canvas - it.size / it.scale.max(1e-6)).max(Vec2::ZERO);
                let mut d = -input.scroll / it.scale.max(1e-6);
                if !ax {
                    d.x = 0.0;
                }
                if !ay {
                    d.y = 0.0;
                }
                // A vertical wheel scrolls sideways in a frame that only scrolls
                // sideways.
                if ax && !ay && input.scroll.x == 0.0 {
                    d.x = -input.scroll.y / it.scale.max(1e-6);
                }
                let next = (at + d).clamp(Vec2::ZERO, max);
                writes.push((it.e, "CanvasPosition", vec2_value(next)));
            }

            // Gamepad selection.
            let service = gui_service(s, game);
            let mut selected =
                service.and_then(|s| match classes.get_property(w, s, "SelectedObject") {
                    Ok(Value::Entity(e)) => e.filter(|&e| tree::alive(w, e)),
                    _ => None,
                });
            if let Some(dir) = input.select_move
                && dir.length_squared() > 0.0
                && let Some(g) = service
            {
                let selectable: Vec<&Item> = items
                    .iter()
                    .filter(|it| it.interactable && s.props(it.e).bool("Selectable", false))
                    .collect();
                let from = selected
                    .and_then(|e| items.iter().find(|it| it.e == e))
                    .map(|it| it.rot.transform_point2(it.min + it.size * 0.5));
                let next = match from {
                    Some(c) => selectable
                        .iter()
                        .filter(|it| it.e != selected.unwrap_or(Entity::PLACEHOLDER))
                        .map(|it| (it.e, it.rot.transform_point2(it.min + it.size * 0.5) - c))
                        .filter(|(_, d)| d.dot(dir) > 0.0)
                        .min_by(|a, b| {
                            let score = |d: Vec2| {
                                let along = d.dot(dir.normalize());
                                let side = (d - dir.normalize() * along).length();
                                along + side * 2.0
                            };
                            score(a.1).total_cmp(&score(b.1))
                        })
                        .map(|x| x.0),
                    None => selectable.first().map(|it| it.e),
                };
                if let Some(n) = next {
                    selected = Some(n);
                    selection = Some((g, n));
                }
            }
            if input.select_activate
                && let Some(e) = selected
                && is_button(e)
                && items.iter().any(|it| it.e == e && it.interactable)
            {
                events.push((e, "MouseButton1Click", Vec::new()));
                events.push((e, "Activated", vec![Arg::Nil, Arg::Int(1)]));
            }
            // Paint.
            let looks = Looks {
                hovered: target_e,
                pressed: hub.pressed1.filter(|_| input.button1),
                selected,
                focused: hub.focused,
            };
            let mut painter = Painter {
                s,
                screen: egui::Rect::from_min_size(
                    egui::Pos2::ZERO,
                    egui::vec2(frame.viewport.x, frame.viewport.y),
                ),
                shapes: Vec::new(),
            };
            for it in &items {
                painter.item(it, &looks);
            }
            let shapes = std::mem::take(&mut painter.shapes);
            let out = GuiOutput {
                shapes,
                drawn: items.len(),
                hovered: under.first().map(|&i| items[i].e),
                sinks_pointer: target.is_some(),
                text_focus: hub.focused,
            };
            // Where each write goes, while the scope can say.
            let mut written = props::FxMap::default();
            for (e, name, v) in writes {
                if SCRIPT_WRITABLE.contains(&name) {
                    if let Some(at) = s.target(e, name) {
                        resolved.push((e, at, v));
                    }
                    continue;
                }
                let key = (e, name.as_ptr() as usize);
                if hub.written.get(&key) != Some(&v)
                    && let Some(at) = s.target(e, name)
                {
                    resolved.push((e, at, v.clone()));
                }
                written.insert(key, v);
            }
            hub.written = written;
            out
        };
        // Write back; an equal value changes nothing.
        for (e, at, v) in resolved {
            props::write(w, e, at, v);
        }
        if let Some((g, n)) = selection {
            let _ = classes.set_property(w, g, "SelectedObject", Value::Entity(Some(n)));
        }
        out
    })?;
    hub_mut(lua, |h| {
        let requests = std::mem::take(&mut h.requests);
        *h = hub;
        h.requests = requests;
    });
    for (e, name, args) in events {
        let args = args
            .into_iter()
            .map(|a| match a {
                Arg::Int(i) => LuaValue::Number(i as f64),
                Arg::Bool(b) => LuaValue::Boolean(b),
                Arg::Nil => LuaValue::Nil,
            })
            .collect();
        signal::fire(lua, e, name, MultiValue::from_vec(args))?;
    }
    signal::events::after_write(lua)?;
    Ok(out)
}

fn written_vec2(writes: &[(Entity, &'static str, Value)], e: Entity, name: &str) -> Option<Vec2> {
    writes
        .iter()
        .rev()
        .find(|(x, n, _)| *x == e && *n == name)
        .and_then(|(_, _, v)| match v {
            Value::Record(_, fs) if fs.len() == 2 => match (&fs[0], &fs[1]) {
                (Value::Float(x), Value::Float(y)) => Some(Vec2::new(*x as f32, *y as f32)),
                _ => None,
            },
            _ => None,
        })
}

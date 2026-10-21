//! Parts as draw items: each part on its own, or the parts of a static cell
//! merged into one mesh per material.

use std::collections::{BTreeMap, HashSet};

use glam::{Affine3A, Mat4, Quat, Vec3};
use re_physics::{PartGeom, PartShape};
use re_scene::Entity;
use re_types::mesh::MeshData;

use super::{
    CellRender, FrameItem, Hub, MaterialKey, RenderFrame, ShapeKey, looks, material_index, mesh,
};
use crate::gui::props::{FxMap, Rgba, Scope};
use crate::physics::{Cell, CellId};

/// What a part is drawn as.
#[derive(Clone, Copy, Debug)]
struct Info {
    e: Entity,
    geom: PartGeom,
    pos: Vec3,
    rot: Quat,
    key: MaterialKey,
    cast_shadow: bool,
}

impl Info {
    /// True when it can be merged into its cell's mesh.
    fn mergeable(&self) -> bool {
        self.key.alpha == 255
            && !matches!(
                super::MATERIALS[usize::from(self.key.material)],
                "Neon" | "ForceField"
            )
    }
}

fn shape_of(s: &Scope, e: Entity) -> PartShape {
    match crate::instance::tree::class(s.w, e) {
        Some("WedgePart") => PartShape::Wedge,
        Some("CornerWedgePart") => PartShape::CornerWedge,
        _ => match s.props(e).item("Shape", "Block").as_str() {
            "Ball" => PartShape::Ball,
            "Cylinder" => PartShape::Cylinder,
            "Wedge" => PartShape::Wedge,
            "CornerWedge" => PartShape::CornerWedge,
            _ => PartShape::Block,
        },
    }
}

fn byte(x: f32) -> u8 {
    (x.clamp(0.0, 1.0) * 255.0).round() as u8
}

/// The part's look, `None` when it is not drawn (fully transparent, or not a
/// drawn part).
fn info(s: &Scope, e: Entity) -> Option<Info> {
    if s.is_a(e, "Terrain") {
        return None;
    }
    let p = s.props(e);
    let t = p.f32("Transparency", 0.0).clamp(0.0, 1.0);
    let ltm = p.f32("LocalTransparencyModifier", 0.0).clamp(0.0, 1.0);
    let alpha = byte((1.0 - t) * (1.0 - ltm));
    if alpha == 0 {
        return None;
    }
    let pose = crate::physics::world_pose(s.w, e)?;
    let geom = PartGeom::new(shape_of(s, e), p.vec3("Size")).sanitized();
    let c = p.color("Color", Rgba([0.64, 0.64, 0.64, 1.0])).0;
    let material = material_index(&p.item("Material", "Plastic"));
    Some(Info {
        e,
        geom,
        pos: pose.pos,
        rot: pose.rot,
        key: MaterialKey {
            material,
            color: [byte(c[0]), byte(c[1]), byte(c[2])],
            alpha,
            reflectance: byte(p.f32("Reflectance", 0.0)),
        },
        cast_shadow: p.bool("CastShadow", true),
    })
}

fn shape_key(g: &PartGeom) -> ShapeKey {
    let s = g.size.to_array().map(f32::to_bits);
    ShapeKey(g.shape as u8, s)
}

/// What the view keeps about the parts from frame to frame, so a frame
/// visits only the parts that changed (ADR-0048 amendment).
#[derive(Default)]
pub(super) struct Retained {
    /// False until the first full pass.
    pub(super) ready: bool,
    /// The drawn parts in `Workspace` and how they look.
    infos: FxMap<Entity, Info>,
    /// The cell of every member of a static cell.
    cell_of: FxMap<Entity, CellId>,
    /// The members version each cell was built from.
    versions: FxMap<CellId, u64>,
    /// The parts merged into a cell's meshes, and that cell.
    merged: FxMap<Entity, CellId>,
    /// The drawn parts not merged into a cell, in entity order.
    loose: BTreeMap<Entity, Info>,
    /// The highlighted parts of the last frame.
    highlighted: HashSet<Entity>,
}

/// Adds the draw items of the parts, merging static cells. `dirty` names
/// the entities to look at again (every part on a full pass); everything
/// else is drawn as the last frame kept it. Returns the parts visited.
pub(super) fn draw(
    s: &Scope,
    hub: &mut Hub,
    dirty: &HashSet<Entity>,
    cells: &BTreeMap<CellId, Cell>,
    in_workspace: &dyn Fn(Entity) -> bool,
    highlighted: &HashSet<Entity>,
    out: &mut RenderFrame,
) -> u32 {
    let mut r = std::mem::take(&mut hub.parts);
    let mut visited = 0u32;
    // Parts whose cell must be rebuilt, and parts whose loose item may change.
    let mut touched_cells: HashSet<CellId> = HashSet::new();
    let mut touched: HashSet<Entity> = HashSet::new();
    for &e in dirty {
        let drawn = if s.w.ecs.get_entity(e).is_ok() && s.is_a(e, "BasePart") {
            visited += 1;
            if in_workspace(e) { info(s, e) } else { None }
        } else {
            None
        };
        let had = match drawn {
            Some(i) => r.infos.insert(e, i).is_some(),
            None => r.infos.remove(&e).is_some(),
        };
        if drawn.is_some() || had {
            touched.insert(e);
            if let Some(c) = r.cell_of.get(&e) {
                touched_cells.insert(*c);
            }
        }
    }
    for e in r.highlighted.symmetric_difference(highlighted) {
        touched.insert(*e);
        if let Some(c) = r.cell_of.get(e) {
            touched_cells.insert(*c);
        }
    }
    r.highlighted.clone_from(highlighted);

    // Static cells: rebuilt when their members or a member's look changed.
    for (id, cell) in cells {
        let fresh = r.versions.get(id) == Some(&cell.version)
            && hub.cells.contains_key(id)
            && !touched_cells.contains(id);
        if fresh {
            continue;
        }
        if let Some(old) = hub.cells.remove(id) {
            out.released_meshes.extend(old.batches.iter().map(|b| b.0));
            for m in &old.merged {
                r.merged.remove(m);
                touched.insert(*m);
            }
        }
        for m in &cell.members {
            r.cell_of.insert(*m, *id);
        }
        let members: Vec<&Info> = cell
            .members
            .iter()
            .filter_map(|e| r.infos.get(e))
            .filter(|i| i.mergeable() && !highlighted.contains(&i.e))
            .collect();
        let built = build_cell(hub, out, &members);
        for m in &built.merged {
            r.merged.insert(*m, *id);
            touched.insert(*m);
        }
        hub.cells.insert(*id, built);
        r.versions.insert(*id, cell.version);
    }
    let gone: Vec<CellId> = hub
        .cells
        .keys()
        .filter(|id| !cells.contains_key(id))
        .copied()
        .collect();
    for id in gone {
        if let Some(old) = hub.cells.remove(&id) {
            out.released_meshes.extend(old.batches.iter().map(|b| b.0));
            for m in &old.merged {
                r.merged.remove(m);
                touched.insert(*m);
            }
        }
        r.versions.remove(&id);
    }
    // A part that left a cell keeps a stale entry until it is looked at.
    for e in &touched {
        if r.cell_of.get(e).is_some_and(|c| !cells.contains_key(c)) {
            r.cell_of.remove(e);
        }
    }
    for e in touched {
        match r.infos.get(&e) {
            Some(i) if !r.merged.contains_key(&e) => {
                r.loose.insert(e, *i);
            }
            _ => {
                r.loose.remove(&e);
            }
        }
    }

    let first = out.items.len();
    for cell in cells.keys().filter_map(|id| hub.cells.get(id)) {
        for &(mesh, key, entity, cast_shadow) in &cell.batches {
            out.items.push(FrameItem {
                entity,
                mesh,
                material: key,
                world: Mat4::IDENTITY,
                cast_shadow,
            });
        }
    }
    let frame = hub.frame;
    for i in r.loose.values() {
        let sk = shape_key(&i.geom);
        let mesh = match hub.shapes.get_mut(&sk) {
            Some((key, last)) => {
                *last = frame;
                *key
            }
            None => {
                let key = hub.new_mesh(out, mesh::part(&i.geom));
                hub.shapes.insert(sk, (key, frame));
                key
            }
        };
        out.items.push(FrameItem {
            entity: re_scene::entity_id(i.e),
            mesh,
            material: i.key,
            world: Mat4::from_rotation_translation(i.rot, i.pos),
            cast_shadow: i.cast_shadow,
        });
    }
    let used: Vec<MaterialKey> = out.items[first..].iter().map(|i| i.material).collect();
    for key in used {
        hub.use_material(out, key);
    }
    r.ready = true;
    hub.parts = r;
    visited
}

/// One mesh per material (and shadow casting) of a cell's mergeable
/// members, with their colours in the vertices.
fn build_cell(hub: &mut Hub, out: &mut RenderFrame, members: &[&Info]) -> CellRender {
    let mut groups: BTreeMap<(MaterialKey, bool), (MeshData, Entity)> = BTreeMap::new();
    let mut merged = HashSet::new();
    for m in members {
        let key = MaterialKey {
            color: [255; 3],
            ..m.key
        };
        let lin = |c: u8| looks::srgb_to_linear(f32::from(c) / 255.0);
        let color = [
            lin(m.key.color[0]),
            lin(m.key.color[1]),
            lin(m.key.color[2]),
            1.0,
        ];
        let world = Affine3A::from_rotation_translation(m.rot, m.pos);
        let (data, _) = groups
            .entry((key, m.cast_shadow))
            .or_insert_with(|| (MeshData::default(), m.e));
        mesh::append(data, &m.geom, &world, color);
        merged.insert(m.e);
    }
    let batches = groups
        .into_iter()
        .map(|((key, shadow), (data, first))| {
            (
                hub.new_mesh(out, data),
                key,
                re_scene::entity_id(first),
                shadow,
            )
        })
        .collect();
    CellRender { batches, merged }
}

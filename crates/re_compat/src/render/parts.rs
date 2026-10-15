//! Parts as draw items: each part on its own, or the parts of a static cell
//! merged into one mesh per material.

use std::collections::{BTreeMap, HashSet};
use std::hash::{Hash, Hasher};

use glam::{Affine3A, Mat4, Quat, Vec3};
use re_physics::{PartGeom, PartShape};
use re_scene::Entity;
use re_types::mesh::MeshData;

use super::{
    CellRender, FrameItem, Hub, MaterialKey, RenderFrame, ShapeKey, looks, material_index, mesh,
};
use crate::gui::props::{Rgba, Scope};
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

    fn hash_into(&self, h: &mut impl Hasher) {
        self.e.to_bits().hash(h);
        (self.geom.shape as u8).hash(h);
        for f in self
            .geom
            .size
            .to_array()
            .into_iter()
            .chain(self.pos.to_array())
            .chain(self.rot.to_array())
        {
            f.to_bits().hash(h);
        }
        self.key.hash(h);
        self.cast_shadow.hash(h);
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

/// Adds the draw items of the parts, merging static cells.
pub(super) fn draw(
    s: &Scope,
    hub: &mut Hub,
    parts: &[Entity],
    cells: &BTreeMap<CellId, Cell>,
    in_workspace: &dyn Fn(Entity) -> bool,
    highlighted: &HashSet<Entity>,
    out: &mut RenderFrame,
) {
    let mut infos: crate::gui::props::FxMap<Entity, Info> = Default::default();
    let mut order = Vec::new();
    for &e in parts {
        if !in_workspace(e) {
            continue;
        }
        if let Some(i) = info(s, e) {
            infos.insert(e, i);
            order.push(e);
        }
    }

    // Static cells: rebuilt when a member or its look changed.
    let mut merged: HashSet<Entity> = HashSet::new();
    let mut live = HashSet::new();
    for (id, cell) in cells {
        live.insert(*id);
        let members: Vec<&Info> = cell
            .members
            .iter()
            .filter_map(|e| infos.get(e))
            .filter(|i| i.mergeable() && !highlighted.contains(&i.e))
            .collect();
        let mut h = crate::gui::props::Fx::default();
        for m in &members {
            m.hash_into(&mut h);
        }
        let signature = h.finish();
        let fresh = hub.cells.get(id).is_some_and(|c| c.signature == signature);
        if !fresh {
            let old = hub.cells.remove(id).unwrap_or_default();
            out.released_meshes.extend(old.batches.iter().map(|b| b.0));
            let r = build_cell(hub, out, signature, &members);
            hub.cells.insert(*id, r);
        }
        let r = &hub.cells[id];
        merged.extend(r.merged.iter().copied());
        let batches = r.batches.clone();
        for (mesh, key, entity, cast_shadow) in batches {
            hub.use_material(out, key);
            out.items.push(FrameItem {
                entity,
                mesh,
                material: key,
                world: Mat4::IDENTITY,
                cast_shadow,
            });
        }
    }
    let gone: Vec<CellId> = hub
        .cells
        .keys()
        .filter(|id| !live.contains(id))
        .copied()
        .collect();
    for id in gone {
        if let Some(old) = hub.cells.remove(&id) {
            out.released_meshes.extend(old.batches.iter().map(|b| b.0));
        }
    }

    for e in order {
        if merged.contains(&e) {
            continue;
        }
        let i = infos[&e];
        let sk = shape_key(&i.geom);
        let frame = hub.frame;
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
        hub.use_material(out, i.key);
        out.items.push(FrameItem {
            entity: re_scene::entity_id(e),
            mesh,
            material: i.key,
            world: Mat4::from_rotation_translation(i.rot, i.pos),
            cast_shadow: i.cast_shadow,
        });
    }
}

/// One mesh per material (and shadow casting) of a cell's mergeable
/// members, with their colours in the vertices.
fn build_cell(
    hub: &mut Hub,
    out: &mut RenderFrame,
    signature: u64,
    members: &[&Info],
) -> CellRender {
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
    CellRender {
        signature,
        batches,
        merged,
    }
}

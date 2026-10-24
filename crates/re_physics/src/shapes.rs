//! `ShapeDesc` → Jolt shape.

use oxijolt::{HeightFieldSettings, Shape, Vec3 as JVec3};
use re_core::Diagnostic;
use re_types::physics::ShapeDesc;

use crate::codes;
use crate::convert::jv;

/// A Jolt shape built from a [`ShapeDesc`], and any warning produced on the way.
pub(crate) struct Built {
    pub(crate) shape: Shape,
    pub(crate) warning: Option<Diagnostic>,
}

/// True for the shapes only static bodies may use (Jolt cannot collide them with each
/// other, and computes no mass for them).
pub(crate) fn is_static_only(shape: &ShapeDesc) -> bool {
    matches!(
        shape,
        ShapeDesc::TriangleMesh { .. } | ShapeDesc::Heightfield { .. }
    )
}

/// The shape's name as it appears in messages.
pub(crate) fn kind_name(shape: &ShapeDesc) -> &'static str {
    match shape {
        ShapeDesc::Box { .. } => "box",
        ShapeDesc::Sphere { .. } => "sphere",
        ShapeDesc::Capsule { .. } => "capsule",
        ShapeDesc::ConvexHull { .. } => "convex hull",
        ShapeDesc::TriangleMesh { .. } => "triangle mesh",
        ShapeDesc::Heightfield { .. } => "heightfield",
    }
}

/// Builds the Jolt shape for `desc`; fails with `RE5002`.
pub(crate) fn build(desc: &ShapeDesc) -> Result<Built, Diagnostic> {
    let invalid = |why: String| {
        Diagnostic::error(
            codes::SHAPE_INVALID,
            format!("invalid {} shape: {why}", kind_name(desc)),
        )
        .with_hint("sizes must be positive and finite, and meshes need valid triangle indices")
    };
    if !desc.is_well_formed() {
        return Err(invalid("its numbers are out of range".into()));
    }
    let mut warning = None;
    let shape = match desc {
        ShapeDesc::Box { half_extents } => Shape::new_box(jv(*half_extents)).map_err(|e| e.to_string()),
        ShapeDesc::Sphere { radius } => Shape::new_sphere(*radius).map_err(|e| e.to_string()),
        ShapeDesc::Capsule {
            half_height,
            radius,
        } => {
            if *half_height > 0.0 {
                Shape::new_capsule(*half_height, *radius).map_err(|e| e.to_string())
            } else {
                // A capsule without a cylinder part is a sphere; Jolt refuses half height 0.
                Shape::new_sphere(*radius).map_err(|e| e.to_string())
            }
        }
        ShapeDesc::ConvexHull { points } => {
            let points: Vec<JVec3> = points.iter().map(|p| jv(*p)).collect();
            Shape::new_convex_hull(&points).map_err(|e| e.to_string())
        }
        ShapeDesc::TriangleMesh {
            vertices,
            triangles,
        } => {
            let verts: Vec<JVec3> = vertices.iter().map(|p| jv(*p)).collect();
            match Shape::new_mesh(&verts, triangles) {
                Ok((shape, dropped)) => {
                    if !dropped.is_empty() {
                        warning = Some(
                            Diagnostic::warning(
                                codes::MESH_TRIANGLES_DROPPED,
                                format!(
                                    "the triangle mesh lost {} of {} triangles (degenerate or duplicate)",
                                    dropped.count(),
                                    triangles.len()
                                ),
                            )
                            .with_hint("remove zero-area triangles from the mesh"),
                        );
                    }
                    Ok(shape)
                }
                Err(e) => Err(e.to_string()),
            }
        }
        ShapeDesc::Heightfield {
            n,
            samples,
            spacing,
            offset,
            precision,
        } => {
            let mut settings = HeightFieldSettings::default()
                .scale(JVec3::new(*spacing, 1.0, *spacing))
                .offset(jv(*offset));
            if let Some(p) = precision {
                settings = settings
                    .bits_per_sample(u32::from(p.bits_per_sample))
                    .block_size(u32::from(p.block_size));
            }
            Shape::new_height_field(*n, samples, &settings)
        }
        .map_err(|e| e.to_string()),
    }
    .map_err(invalid)?;
    Ok(Built { shape, warning })
}

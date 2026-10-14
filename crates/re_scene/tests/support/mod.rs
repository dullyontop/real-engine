//! Test-only components, a test generator and the core spike's sample scene,
//! shared by the `re_scene` and `re_command` integration tests (the latter
//! include this file with `#[path]`).

#![allow(dead_code)]

use std::sync::Arc;

use glam::Vec3;
use indexmap::IndexMap;
use re_reflect::{Color, Doc, FieldValue, Value, component, field_enum};
use re_scene::ecs as bevy_ecs;
use re_scene::scene::{self, ComponentsDoc};
use re_scene::{
    BodyKind, Entity, EntityDoc, GenContext, GenNode, GeneratorInfo, SaveOptions, World,
};

field_enum! {
    /// Primitive shape of a `Primitive`.
    pub enum Shape { Box, Sphere, Cylinder, Wedge }
}

component! {
    /// A solid primitive shape (test component).
    pub struct Primitive {
        /// The shape.
        pub shape: Shape = Shape::Box,
        /// Size along each local axis, in metres.
        pub size: Vec3 = Vec3::ONE,
        /// The colour.
        pub color: Color = Color::rgba(0.64, 0.64, 0.64, 1.0),
    }
}

component! {
    /// Omnidirectional light (test component).
    pub struct PointLight {
        /// The colour.
        pub color: Color = Color::WHITE,
        /// Luminous intensity in candela.
        pub intensity: f32 = 800.0,
        /// Distance in metres beyond which the light has no effect.
        pub range: f32 = 20.0,
        /// Whether it casts shadows.
        pub shadows: bool = false,
    }
}

component! {
    /// Rigidly attaches this body to another entity (test component).
    pub struct Weld {
        /// The entity this one is welded to.
        pub other: Option<Entity> = None,
    }
}

component! {
    /// Parameters of the `pillar_grid` test generator.
    pub struct PillarGrid {
        /// Rows of pillars.
        pub rows: i32 = 3,
        /// Columns of pillars.
        pub cols: i32 = 3,
        /// Distance between pillar centres, in metres.
        pub spacing: f32 = 4.0,
        /// Base pillar height, in metres.
        pub height: f32 = 6.0,
        /// Maximum random height change, in metres.
        pub jitter: f32 = 1.0,
    }
}

/// A world with the built-ins plus the test components and the
/// `pillar_grid` generator.
pub fn world() -> World {
    let mut w = World::new();
    register(&mut w);
    w
}

/// Registers the test components and generator in `w`.
pub fn register(w: &mut World) {
    w.registry.register::<Primitive>(&mut w.ecs);
    w.registry.register::<PointLight>(&mut w.ecs);
    w.registry.register::<Weld>(&mut w.ecs);
    w.registry.register::<PillarGrid>(&mut w.ecs);
    w.generators.register(GeneratorInfo {
        name: "pillar_grid".into(),
        doc: "A rows x cols grid of static pillars, each with a cap.".into(),
        params_component: "PillarGrid".into(),
        run: Arc::new(pillar_grid),
        fingerprint_extra: None,
    });
}

fn pillar_grid(ctx: &GenContext) -> Result<Vec<GenNode>, re_core::Diagnostic> {
    let rows: i32 = ctx.param("rows")?;
    let cols: i32 = ctx.param("cols")?;
    let spacing: f32 = ctx.param("spacing")?;
    let height: f32 = ctx.param("height")?;
    let jitter: f32 = ctx.param("jitter")?;
    if !(0..=1000).contains(&rows) || !(0..=1000).contains(&cols) {
        return Err(ctx.error(format!(
            "rows and cols must be in 0..=1000, got {rows} x {cols}"
        )));
    }
    let mut out = Vec::new();
    for r in 0..rows {
        for c in 0..cols {
            let key = format!("pillar_{r}_{c}");
            let h = (height + jitter * (ctx.random(&key) * 2.0 - 1.0)).max(0.1);
            let x = (c as f32 - (cols - 1) as f32 / 2.0) * spacing;
            let z = (r as f32 - (rows - 1) as f32 / 2.0) * spacing;
            out.push(GenNode {
                key: key.clone(),
                name: format!("Pillar {r},{c}"),
                parent_key: None,
                components: vec![
                    (
                        "Transform".into(),
                        vec![("translation".into(), Value::Vec3(Vec3::new(x, h / 2.0, z)))],
                    ),
                    (
                        "Primitive".into(),
                        vec![
                            ("shape".into(), Shape::Cylinder.to_value()),
                            ("size".into(), Value::Vec3(Vec3::new(1.0, h, 1.0))),
                        ],
                    ),
                    (
                        "RigidBody".into(),
                        vec![("kind".into(), BodyKind::Static.to_value())],
                    ),
                ],
            });
            out.push(GenNode {
                key: format!("{key}/cap"),
                name: "Cap".into(),
                parent_key: Some(key),
                components: vec![
                    (
                        "Transform".into(),
                        vec![(
                            "translation".into(),
                            Value::Vec3(Vec3::new(0.0, h / 2.0 + 0.15, 0.0)),
                        )],
                    ),
                    (
                        "Primitive".into(),
                        vec![
                            ("size".into(), Value::Vec3(Vec3::new(1.4, 0.3, 1.4))),
                            (
                                "color".into(),
                                Value::Color(Color::rgba(0.8, 0.75, 0.6, 1.0)),
                            ),
                        ],
                    ),
                ],
            });
        }
    }
    Ok(out)
}

/// `[x, y, z]` as f32 text.
pub fn v3(x: f32, y: f32, z: f32) -> Doc {
    Doc::List(vec![Doc::F32(x), Doc::F32(y), Doc::F32(z)])
}

/// Components in file form.
pub fn comps(components: &[(&str, &[(&str, Doc)])]) -> ComponentsDoc {
    let mut c = IndexMap::new();
    for (name, fields) in components {
        let f: IndexMap<String, Doc> = fields
            .iter()
            .map(|(k, v)| (k.to_string(), v.clone()))
            .collect();
        c.insert(name.to_string(), f);
    }
    c
}

/// An entity doc named after its id in upper case (the spike's `spawn` helper).
pub fn entity(id: &str, parent: Option<&str>, components: &[(&str, &[(&str, Doc)])]) -> EntityDoc {
    EntityDoc {
        id: id.into(),
        name: id.to_uppercase(),
        parent: parent.map(Into::into),
        components: comps(components),
        overrides: Vec::new(),
    }
}

/// The core spike's small authored scene: world, ground, crate (with a lamp
/// child) and a weld referencing the crate.
pub fn sample_docs() -> Vec<EntityDoc> {
    vec![
        entity("world", None, &[]),
        entity(
            "ground",
            Some("world"),
            &[
                ("Transform", &[]),
                ("Primitive", &[("size", v3(100.0, 1.0, 100.0))]),
                ("RigidBody", &[]),
            ],
        ),
        entity(
            "crate",
            Some("world"),
            &[
                ("Transform", &[("translation", v3(0.0, 5.0, 0.0))]),
                ("Primitive", &[]),
                (
                    "RigidBody",
                    &[("kind", Doc::Str("Dynamic".into())), ("mass", Doc::Int(20))],
                ),
            ],
        ),
        entity(
            "lamp",
            Some("crate"),
            &[
                ("Transform", &[("translation", v3(0.0, 1.0, 0.0))]),
                (
                    "PointLight",
                    &[(
                        "color",
                        Doc::List(vec![Doc::F32(1.0), Doc::F32(0.8), Doc::F32(0.6)]),
                    )],
                ),
            ],
        ),
        entity(
            "weld",
            Some("world"),
            &[("Weld", &[("other", Doc::Str("crate".into()))])],
        ),
    ]
}

/// A procedural `pillar_grid` source under `world`.
pub fn grid_doc(rows: i64, cols: i64) -> EntityDoc {
    entity(
        "grid",
        Some("world"),
        &[
            ("Transform", &[]),
            (
                "Procedural",
                &[
                    ("generator", Doc::Str("pillar_grid".into())),
                    ("seed", Doc::Int(7)),
                ],
            ),
            (
                "PillarGrid",
                &[("rows", Doc::Int(rows)), ("cols", Doc::Int(cols))],
            ),
        ],
    )
}

/// The world's scene as JSON text.
pub fn scene_json(w: &mut World) -> String {
    scene::to_json(&w.save(SaveOptions::default()))
}

/// Ids of the children of `parent`, in child order.
pub fn ids_under(w: &World, parent: &str) -> Vec<String> {
    let p = w.entity(parent).unwrap();
    re_scene::hierarchy::children(&w.ecs, p)
        .iter()
        .map(|e| w.id_of(*e).unwrap().to_string())
        .collect()
}

/// Keeps the alias import used even when a test file uses no derive directly.
pub type EcsWorld = bevy_ecs::world::World;

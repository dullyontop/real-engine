//! Streaming hooks (ADR-0042): per-cell apply and unload on a streamed
//! procedural source, overrides across unloads, and stashed authored roots.

mod support;

use glam::Vec3;
use re_reflect::{Doc, Value};
use re_scene::{
    GenNode, Overrides, SaveOptions, World, cell_key, hierarchy, parse_cell_key, scene,
    stream::{in_cell, under},
};
use support::*;

/// The sample scene plus a streamed 1 x 2 pillar grid under `world`.
fn streamed_world() -> World {
    let mut w = world();
    w.spawn_docs(&sample_docs(), None).unwrap();
    let mut doc = grid_doc(1, 2);
    doc.components.insert("Streamed".into(), Default::default());
    w.spawn_docs(&[doc], None).unwrap();
    let report = w.update();
    assert!(report.diagnostics.is_empty(), "{:?}", report.diagnostics);
    w
}

/// `n` rocks in cell `cell`, each with a cap child, meshes `mesh:<cell>`.
fn rocks(cell: &str, n: usize) -> Vec<GenNode> {
    let mut out = Vec::new();
    for i in 0..n {
        let key = format!("{cell}/rock/{i}");
        out.push(GenNode {
            key: key.clone(),
            name: format!("Rock {i}"),
            parent_key: None,
            components: vec![
                (
                    "Transform".into(),
                    vec![(
                        "translation".into(),
                        Value::Vec3(Vec3::new(i as f32, 0.0, 0.0)),
                    )],
                ),
                (
                    "MeshRef".into(),
                    vec![("asset".into(), Value::Str(format!("mesh:{cell}")))],
                ),
            ],
        });
        out.push(GenNode {
            key: format!("{key}/cap"),
            name: "Cap".into(),
            parent_key: Some(key),
            components: vec![("Transform".into(), Vec::new())],
        });
    }
    out
}

#[test]
fn cell_keys() {
    assert_eq!(cell_key(3, -12), "c/3_-12");
    assert_eq!(parse_cell_key("c/3_-12"), Some([3, -12]));
    for bad in [
        "c/03_1", "c/+3_1", "c/3", "c/3_1/x", "d/3_1", "c/-0_1", "c/3_1_2",
    ] {
        assert_eq!(parse_cell_key(bad), None, "{bad}");
    }
    assert_eq!(in_cell("c/3_-2/rock/7"), Some("c/3_-2"));
    assert_eq!(in_cell("c/3_-2"), None);
    assert_eq!(in_cell("rock/c/3_-2/x"), None);
    assert!(under("c/1_2/a", "c/1_2"));
    assert!(!under("c/1_23/a", "c/1_2"));
}

#[test]
fn cells_load_update_and_unload_on_their_own() {
    let mut w = streamed_world();
    let grid = w.entity("grid").unwrap();
    let pillar = w.entity("grid/pillar_0_0").unwrap();

    let r = w.apply_cell(grid, "c/1_0", &rocks("c/1_0", 2)).unwrap();
    assert_eq!(r.spawned[0], "grid/c/1_0");
    assert_eq!(r.spawned.len(), 5);
    w.apply_cell(grid, "c/0_0", &rocks("c/0_0", 1)).unwrap();
    assert_eq!(w.loaded_cells(grid), ["c/0_0", "c/1_0"]);

    // The cell root holds the content; ids are `<source>/<key>`.
    let root = w.cell_root(grid, "c/1_0").unwrap();
    let rock = w.entity("grid/c/1_0/rock/1").unwrap();
    assert_eq!(hierarchy::parent(&w.ecs, rock), Some(root));
    assert!(w.entity("grid/c/1_0/rock/1/cap").is_ok());

    // Cell roots come after the source's other generated children, in key order.
    assert_eq!(
        ids_under(&w, "grid"),
        [
            "grid/pillar_0_0",
            "grid/pillar_0_1",
            "grid/c/0_0",
            "grid/c/1_0"
        ]
    );

    // Re-applying a cell diffs it: survivors keep their entity, the rest goes.
    let other = w.entity("grid/c/0_0/rock/0").unwrap();
    let survivor = w.entity("grid/c/1_0/rock/0").unwrap();
    let r = w.apply_cell(grid, "c/1_0", &rocks("c/1_0", 1)).unwrap();
    assert_eq!(w.entity("grid/c/1_0/rock/0").unwrap(), survivor);
    assert!(r.despawned.contains(&"grid/c/1_0/rock/1".to_string()));
    assert!(w.entity("grid/c/1_0/rock/1").is_err());
    assert_eq!(
        w.entity("grid/c/0_0/rock/0").unwrap(),
        other,
        "other cells untouched"
    );

    // Regenerating the whole source leaves its cells alone.
    w.set("grid", "PillarGrid", "cols", Value::Int(3)).unwrap();
    let r = w.update();
    assert!(r.diagnostics.is_empty() && r.orphans.is_empty(), "{r:?}");
    assert_eq!(w.entity("grid/pillar_0_0").unwrap(), pillar);
    assert_eq!(w.entity("grid/c/0_0/rock/0").unwrap(), other);
    assert_eq!(
        ids_under(&w, "grid"),
        [
            "grid/pillar_0_0",
            "grid/pillar_0_1",
            "grid/pillar_0_2",
            "grid/c/0_0",
            "grid/c/1_0"
        ]
    );

    // Unloading despawns the cell and releases what nothing else uses.
    let r = w.unload_cell(grid, "c/1_0").unwrap();
    assert_eq!(
        r.despawned,
        ["grid/c/1_0", "grid/c/1_0/rock/0", "grid/c/1_0/rock/0/cap"]
    );
    assert_eq!(r.released_assets, ["mesh:c/1_0"]);
    assert_eq!(w.loaded_cells(grid), ["c/0_0"]);
    assert!(w.unload_cell(grid, "c/1_0").unwrap().despawned.is_empty());

    // Nothing streamed is saved.
    assert!(!scene_json(&mut w).contains("c/0_0"));
}

#[test]
fn cell_content_is_checked_before_anything_changes() {
    let mut w = streamed_world();
    let grid = w.entity("grid").unwrap();
    let crate_ = w.entity("crate").unwrap();

    let err = w.apply_cell(grid, "c/1_0", &rocks("c/2_0", 1)).unwrap_err();
    assert_eq!(err.code().to_string(), "RE1207", "{err}");
    assert!(
        w.cell_root(grid, "c/1_0").is_none(),
        "a failed first load leaves no root"
    );
    assert!(w.apply_cell(grid, "c/01_0", &[]).is_err());
    assert!(w.apply_cell(crate_, "c/1_0", &[]).is_err(), "not a source");

    // A source without `Streamed` has no cells.
    let mut plain = world();
    plain.spawn_docs(&sample_docs(), None).unwrap();
    plain.spawn_docs(&[grid_doc(1, 1)], None).unwrap();
    plain.update();
    let g = plain.entity("grid").unwrap();
    assert!(plain.apply_cell(g, "c/0_0", &[]).is_err());
}

#[test]
fn overrides_survive_unloading_and_orphans_stay_in_their_cell() {
    let mut w = streamed_world();
    let grid = w.entity("grid").unwrap();
    let mut ov = Overrides::default();
    ov.set(
        "c/1_0/rock/0",
        "Transform",
        "translation",
        Some(Doc::List(vec![Doc::F32(9.0), Doc::F32(0.0), Doc::F32(0.0)])),
    );
    ov.set_deleted("c/1_0/rock/1", true);
    ov.set(
        "c/5_5/rock/0",
        "Transform",
        "translation",
        Some(Doc::List(vec![Doc::F32(1.0), Doc::F32(0.0), Doc::F32(0.0)])),
    );
    w.ecs.entity_mut(grid).insert(ov);
    w.regenerate(grid);
    assert!(
        w.regenerate(grid).orphans.is_empty(),
        "cell overrides are not whole-source orphans"
    );

    let translation = |w: &World| {
        w.get("grid/c/1_0/rock/0", "Transform", "translation")
            .unwrap()
    };
    for round in 0..2 {
        let r = w.apply_cell(grid, "c/1_0", &rocks("c/1_0", 2)).unwrap();
        assert!(r.orphans.is_empty(), "round {round}: {r:?}");
        assert_eq!(translation(&w), Value::Vec3(Vec3::new(9.0, 0.0, 0.0)));
        assert!(w.entity("grid/c/1_0/rock/1").is_err(), "deleted");
        w.unload_cell(grid, "c/1_0").unwrap();
        let ov = w.ecs.get::<Overrides>(grid).unwrap();
        assert_eq!(ov.0.len(), 3, "unloading records nothing");
    }

    // A suppression (play state) also survives unload and reload.
    w.apply_cell(grid, "c/2_0", &rocks("c/2_0", 2)).unwrap();
    w.suppress("grid/c/2_0/rock/0").unwrap();
    w.unload_cell(grid, "c/2_0").unwrap();
    w.apply_cell(grid, "c/2_0", &rocks("c/2_0", 2)).unwrap();
    assert!(w.entity("grid/c/2_0/rock/0").is_err());
    assert!(w.entity("grid/c/2_0/rock/1").is_ok());

    // An override whose key the cell no longer generates is that cell's orphan.
    let r = w.apply_cell(grid, "c/5_5", &rocks("c/5_5", 0)).unwrap();
    assert_eq!(r.orphans.len(), 1);
    assert_eq!(r.orphans[0].target, "c/5_5/rock/0");
}

#[test]
fn whole_regeneration_leaves_cells_alone() {
    let mut w = streamed_world();
    let grid = w.entity("grid").unwrap();
    w.apply_cell(grid, "c/0_0", &rocks("c/0_0", 1)).unwrap();
    w.set("grid", "PillarGrid", "rows", Value::Int(0)).unwrap();
    let r = w.update();
    assert!(r.diagnostics.is_empty());
    assert!(
        w.entity("grid/c/0_0/rock/0").is_ok(),
        "cells are not the generator's output"
    );
}

#[test]
fn stashed_roots_are_saved_and_come_back_exactly() {
    let mut w = world();
    w.spawn_docs(&sample_docs(), None).unwrap();
    // A streamed authored root with a child.
    let docs = vec![
        entity(
            "zz_tower",
            None,
            &[
                ("Transform", &[("translation", v3(300.0, 0.0, 0.0))]),
                ("Streamed", &[]),
            ],
        ),
        entity("zz_tower_top", Some("zz_tower"), &[("Transform", &[])]),
    ];
    w.spawn_docs(&docs, None).unwrap();
    for omit in [false, true] {
        let opts = SaveOptions {
            omit_defaults: omit,
        };
        let before = scene::to_json(&w.save(opts));
        let tower = w.entity("zz_tower").unwrap();
        w.stash(tower, "c/1_0").unwrap();
        assert!(w.entity("zz_tower").is_err() && w.entity("zz_tower_top").is_err());
        assert_eq!(w.stashed().unwrap().cell_of("zz_tower_top"), Some("c/1_0"));
        assert_eq!(
            scene::to_json(&w.save(opts)),
            before,
            "omit_defaults {omit}"
        );
        let fork = w.fork();
        assert!(fork.stashed().unwrap().root("zz_tower").is_some());
        let back = w.unstash_cell("c/1_0").unwrap();
        assert_eq!(back.len(), 1);
        assert_eq!(scene::to_json(&w.save(opts)), before);
    }

    // Play state is kept across the unload.
    let tower = w.entity("zz_tower").unwrap();
    w.set(
        "zz_tower_top",
        "Transform",
        "translation",
        Value::Vec3(Vec3::Y),
    )
    .unwrap();
    w.stash(tower, "c/1_0").unwrap();
    w.unstash_cell("c/1_0").unwrap();
    assert_eq!(
        w.get("zz_tower_top", "Transform", "translation").unwrap(),
        Value::Vec3(Vec3::Y)
    );
    assert!(w.unstash_cell("c/9_9").unwrap().is_empty());

    // Only authored, streamed roots stash.
    let lamp = w.entity("lamp").unwrap();
    assert!(w.stash(lamp, "c/0_0").is_err());
    let ground = w.entity("ground").unwrap();
    assert!(w.stash(ground, "c/0_0").is_err(), "no Streamed component");
}

#[test]
fn a_cell_root_keeps_its_overrides() {
    let mut w = streamed_world();
    let grid = w.entity("grid").unwrap();
    let mut ov = Overrides::default();
    ov.set(
        "c/1_0",
        "Transform",
        "translation",
        Some(Doc::List(vec![Doc::F32(0.0), Doc::F32(2.0), Doc::F32(0.0)])),
    );
    ov.set_deleted("c/2_0", true);
    w.ecs.entity_mut(grid).insert(ov);
    assert!(
        w.regenerate(grid).orphans.is_empty(),
        "cell root overrides belong to their cell"
    );

    for _ in 0..2 {
        let r = w.apply_cell(grid, "c/1_0", &rocks("c/1_0", 1)).unwrap();
        assert!(r.orphans.is_empty() && r.diagnostics.is_empty(), "{r:?}");
        assert_eq!(
            w.get("grid/c/1_0", "Transform", "translation").unwrap(),
            Value::Vec3(Vec3::new(0.0, 2.0, 0.0))
        );
        w.unload_cell(grid, "c/1_0").unwrap();
    }

    // A deleted cell root takes its content with it.
    let r = w.apply_cell(grid, "c/2_0", &rocks("c/2_0", 1)).unwrap();
    assert!(r.spawned.is_empty() && r.orphans.is_empty(), "{r:?}");
    assert!(w.cell_root(grid, "c/2_0").is_none());
    assert!(w.entity("grid/c/2_0/rock/0").is_err());
}

#[test]
fn stashed_ids_stay_taken_and_references_follow() {
    let mut w = world();
    w.spawn_docs(&sample_docs(), None).unwrap();
    let docs = vec![
        entity("zz_tower", None, &[("Transform", &[]), ("Streamed", &[])]),
        entity("zz_tower_top", Some("zz_tower"), &[("Transform", &[])]),
        entity(
            "zz_rope",
            None,
            &[("Weld", &[("other", Doc::Str("zz_tower_top".into()))])],
        ),
    ];
    w.spawn_docs(&docs, None).unwrap();
    let before = scene_json(&mut w);
    let tower = w.entity("zz_tower").unwrap();
    assert_eq!(
        w.stash(tower, "c/1_0").unwrap(),
        1,
        "zz_rope refers into it"
    );

    // The ids stay reserved while stashed.
    let err = w
        .spawn_docs(&[entity("zz_tower_top", None, &[])], None)
        .unwrap_err();
    assert_eq!(err.code().to_string(), "RE1201", "{err}");

    // The reference is saved as the id, in the world and in a copy.
    assert_eq!(scene_json(&mut w), before);
    let mut fork = w.fork();
    assert_eq!(scene_json(&mut fork), before);
    fork.unstash_cell("c/1_0").unwrap();
    assert_eq!(
        fork.get("zz_rope", "Weld", "other").unwrap(),
        Value::Entity(Some(fork.entity("zz_tower_top").unwrap()))
    );

    // And points at the entity again once it is back.
    w.unstash_cell("c/1_0").unwrap();
    let top = w.entity("zz_tower_top").unwrap();
    assert_eq!(
        w.get("zz_rope", "Weld", "other").unwrap(),
        Value::Entity(Some(top))
    );
    assert_eq!(scene_json(&mut w), before);
    assert!(w.stashed().unwrap().is_empty());
}

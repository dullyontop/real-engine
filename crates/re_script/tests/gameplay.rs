//! ADR-0057's gameplay scripting API: the late frame and drawn poses (§7) and
//! the correctness fixes (§11).

mod common;

use common::{Rig, STEP, project};
use re_core::Phase;
use re_scene::{HudText, entity_id};
use re_script::{ContextKind, RuntimeConfig, codes, vm::MAX_OUTPUT_LINES};
use re_types::glam::{Affine3A, Quat, Vec3};
use serde_json::json;

#[test]
fn late_frame_callbacks_run_once_a_frame_with_the_frame_time() {
    let mut rig = Rig::new();
    rig.exec(
        r#"
        time.on_late_frame(function(dt)
            print("late", dt)
            task.defer(function() print("deferred") end)
        end)
        "#,
    )
    .unwrap();
    rig.exec_in(
        ContextKind::Tools,
        r#"time.on_late_frame(function() print("tools late") end)"#,
    )
    .unwrap();
    let d = rig.step(STEP);
    assert!(d.is_empty(), "{d:#?}");
    assert!(rig.output().is_empty(), "nothing runs before LateFrame");
    let d = rig.phase(Phase::LateFrame, 0.025);
    assert!(d.is_empty(), "{d:#?}");
    // The game context first, its deferred thread in the phase's settle.
    assert_eq!(rig.output(), ["late\t0.025", "deferred", "tools late"]);
}

#[test]
fn drawn_transform_reads_the_drawn_pose_or_the_world() {
    let mut rig = Rig::new();
    rig.exec(
        r#"
        local p = world.spawn({ id = "p", components = { Transform = {
            translation = vector.create(1, 0, 0), scale = vector.create(2, 2, 2) } } })
        world.spawn({ id = "c", parent = p, components = { Transform = {
            translation = vector.create(0, 1, 0) } } })
        "#,
    )
    .unwrap();
    let read = r#"
        local t, r, s = world.get("c"):drawn_transform()
        return { t.x, t.y, t.z, r.w, s.x }
    "#;
    // Without a drawn pose, where the world has it, composed at the call.
    assert_eq!(rig.exec(read).unwrap(), json!([1, 2, 0, 1, 2]));

    let c = rig.world.ids().get("c").unwrap();
    rig.services.drawn.insert(
        entity_id(c),
        Affine3A::from_scale_rotation_translation(Vec3::splat(3.0), Quat::IDENTITY, Vec3::Z),
    );
    assert_eq!(rig.exec(read).unwrap(), json!([0, 0, 1, 1, 3]));

    // A chain of transforms ends at an ancestor without one, as propagation's does.
    rig.exec(
        r#"
        local bare = world.spawn({ id = "bare", parent = world.get("p") })
        world.spawn({ id = "g", parent = bare, components = { Transform = {
            translation = vector.create(0, 0, 5) } } })
        "#,
    )
    .unwrap();
    let got = rig
        .exec("local t = world.get('g'):drawn_transform() return { t.x, t.y, t.z }")
        .unwrap();
    assert_eq!(got, json!([0, 0, 5]));

    let d = rig.exec_err("local e = world.spawn({}) e:destroy() return e:drawn_transform()");
    assert_eq!(d.code, codes::STALE_HANDLE);
}

#[test]
fn frame_only_reads_fail_in_a_fixed_step() {
    let mut rig = Rig::new();
    rig.exec(
        r#"
        local e = world.spawn({ components = { Transform = {} } })
        time.on_frame(function() print("frame", (e:drawn_transform())) end)
        time.on_late_frame(function() print("late", (e:drawn_transform())) end)
        time.on_step(function()
            local _, err = pcall(function() return e:drawn_transform() end)
            print("step", string.find(tostring(err), "RE3026") ~= nil)
            _, err = pcall(audio.levels, "sfx")
            print("step", string.find(tostring(err), "RE3026") ~= nil)
        end)
        "#,
    )
    .unwrap();
    let mut d = rig.step(STEP);
    d.extend(rig.phase(Phase::LateFrame, STEP));
    assert!(d.is_empty(), "{d:#?}");
    assert_eq!(
        rig.output(),
        [
            "frame\tvector(0, 0, 0)",
            "step\ttrue",
            "step\ttrue",
            "late\tvector(0, 0, 0)"
        ]
    );
}

#[test]
fn ui_set_text_writes_the_text_at_once() {
    let mut rig = Rig::new();
    rig.exec(
        r#"
        local e = world.spawn({ id = "hud", components = { HudText = { text = "old" } } })
        ui.set_text(e, "new")
        "#,
    )
    .unwrap();
    let e = rig.world.ids().get("hud").unwrap();
    assert_eq!(rig.world.ecs.get::<HudText>(e).unwrap().text, "new");

    let d = rig.exec_err("ui.set_text(world.spawn({}), 'x')");
    assert_eq!(d.code, re_reflect::codes::MISSING_COMPONENT);
    let d = rig.exec_err("local e = world.spawn({}) e:destroy() ui.set_text(e, 'x')");
    assert_eq!(d.code, codes::STALE_HANDLE);
}

#[test]
fn output_keeps_the_newest_lines_and_counts_what_it_dropped() {
    let mut rig = Rig::with_config(Rig::budget(RuntimeConfig::new(project()), 60_000));
    let extra = 5;
    rig.exec(&format!(
        "for i = 1, {} do print(i) end",
        MAX_OUTPUT_LINES + extra
    ))
    .unwrap();
    let out = rig.rt.take_output();
    assert_eq!(out.len(), MAX_OUTPUT_LINES + 1);
    assert!(out[0].warn);
    assert!(
        out[0]
            .text
            .starts_with(&format!("{extra} earlier lines were dropped")),
        "{}",
        out[0].text
    );
    assert_eq!(out[1].text, (extra + 1).to_string());
    assert_eq!(
        out.last().unwrap().text,
        (MAX_OUTPUT_LINES + extra).to_string()
    );
    // The count starts again after a drain.
    rig.exec("print('again')").unwrap();
    assert_eq!(rig.output(), ["again"]);
}

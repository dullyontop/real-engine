//! Vehicles in the running game (ADR-0040): a `Vehicle` body with
//! `VehicleWheel` children becomes a physics vehicle, `physics.drive` moves it,
//! `physics.vehicle` reads it back, and the wheel poses are written to the
//! wheel entities.

use std::path::Path;

use re_app::{App, AppConfig};
use re_reflect::Value;

fn wheel(id: &str, x: f32, z: f32, front: bool) -> String {
    let steer = if front { 30.0 } else { 0.0 };
    format!(
        r#"{{ "id": "{id}", "parent": "car", "components": {{
        "VehicleWheel": {{ "radius": 0.35, "width": 0.2, "max_steer_deg": {steer}, "driven": {front} }},
        "Transform": {{ "translation": [{x}, -0.1, {z}] }} }} }}"#
    )
}

fn scene() -> String {
    let wheels = [
        wheel("fl", 0.9, 1.4, true),
        wheel("fr", -0.9, 1.4, true),
        wheel("rl", 0.9, -1.4, false),
        wheel("rr", -0.9, -1.4, false),
    ]
    .join(",\n");
    format!(
        r#"{{
  "format": "real-engine/scene",
  "version": 1,
  "entities": [
    {{ "id": "ground", "components": {{
        "Collider": {{ "shape": "Box", "half_extents": [100.0, 0.5, 100.0] }},
        "Transform": {{ "translation": [0.0, -0.5, 0.0] }} }} }},
    {{ "id": "car", "components": {{
        "Collider": {{ "shape": "Box", "half_extents": [0.9, 0.3, 2.0] }},
        "RigidBody": {{ "kind": "Dynamic", "mass": 1500.0, "center_of_mass_offset": [0.0, -0.3, 0.0] }},
        "Vehicle": {{}},
        "ScriptRef": {{ "path": "scripts/drive.luau" }},
        "Transform": {{ "translation": [0.0, 1.0, 0.0] }} }} }},
    {wheels}
  ]
}}
"#
    )
}

const DRIVE: &str = r#"local steps = 0
time.on_step(function(dt)
	steps += 1
	physics.drive(script.entity, { throttle = if steps > 30 then 1 else 0 })
	if steps == 150 then
		local v = physics.vehicle(script.entity)
		local grounded = 0
		for _, w in v.wheels do
			if w.on_ground then
				grounded += 1
			end
		end
		print(`speed {v.speed > 2} gear {v.gear} throttle {v.throttle} wheels {#v.wheels} grounded {grounded}`)
	end
end)
"#;

fn project() -> tempfile::TempDir {
    let dir = tempfile::tempdir().unwrap();
    let d = dir.path();
    std::fs::create_dir_all(d.join("scenes")).unwrap();
    std::fs::create_dir_all(d.join("scripts")).unwrap();
    std::fs::write(d.join("real.toml"), "[project]\nname = \"vehicle\"\n").unwrap();
    std::fs::write(d.join("scenes/main.scene.json"), scene()).unwrap();
    std::fs::write(d.join("scripts/drive.luau"), DRIVE).unwrap();
    dir
}

fn open(dir: &Path) -> App {
    App::open(dir, AppConfig::load(dir).unwrap()).unwrap()
}

fn vec3(app: &App, id: &str, field: &str) -> glam::Vec3 {
    match app.world().get(id, "Transform", field).unwrap() {
        Value::Vec3(v) => v,
        other => panic!("{other:?}"),
    }
}

#[test]
fn a_scripted_vehicle_drives_and_its_wheels_follow() {
    let dir = project();
    let mut app = open(dir.path());
    let report = app.run_headless(Some(150), None).unwrap();
    assert!(report.ok(), "{:?}", report.diagnostics);
    assert_eq!(
        report.output,
        ["[game] speed true gear 1 throttle 1 wheels 4 grounded 4"]
    );
    // Forward is +Z; the car drove a few metres and stayed upright.
    let car = vec3(&app, "car", "translation");
    assert!(car.z > 2.0, "car at {car}");
    // The wheels sit below their attachment points on the suspension, and spun.
    let fl = vec3(&app, "fl", "translation");
    assert!(
        (fl.x - 0.9).abs() < 0.05 && (fl.z - 1.4).abs() < 0.05,
        "fl at {fl}"
    );
    assert!(fl.y < -0.1, "fl at {fl}");
    let rot = match app.world().get("fl", "Transform", "rotation").unwrap() {
        Value::Quat(q) => q,
        other => panic!("{other:?}"),
    };
    assert!(
        rot.angle_between(glam::Quat::IDENTITY) > 0.01,
        "rotation {rot}"
    );
}

#[test]
fn vehicle_runs_are_deterministic() {
    let dir = project();
    let a = open(dir.path()).run_headless(Some(120), None).unwrap();
    let b = open(dir.path()).run_headless(Some(120), None).unwrap();
    assert!(a.ok(), "{:?}", a.diagnostics);
    assert_eq!(a.state_hash, b.state_hash);
}

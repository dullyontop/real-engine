//! What `Registry::set_field` costs with and without a change hook
//! (ADR-0048 amendment: a world that keeps no change journal pays nothing).
//! A manual run:
//!
//! ```sh
//! cargo run --locked -p re_reflect --release --example set_field_cost
//! ```
//!
//! It writes a native and a script-defined component 2,000,000 times each,
//! alternating two values so every write changes the field, and prints
//! nanoseconds per write: with no hook, then with a hook that records into a
//! resource. Run on a commit before the hook existed, the first five lines
//! give the baseline.

use bevy_ecs::prelude::Resource;
use bevy_ecs::world::World;
use re_reflect::{
    ComponentKey, ComponentSchema, FieldSchema, FieldType, Registry, Value, component,
};

component! {
    /// A component to write.
    pub struct Probe {
        /// The field written.
        pub x: f32 = 0.0,
    }
}

#[derive(Resource, Default)]
struct Seen(Vec<(bevy_ecs::entity::Entity, ComponentKey)>);

fn hook(w: &mut World, e: bevy_ecs::entity::Entity, k: ComponentKey) {
    if let Some(mut s) = w.get_resource_mut::<Seen>() {
        s.0.push((e, k));
    }
}

const WRITES: u32 = 2_000_000;

fn run(r: &Registry, w: &mut World, e: bevy_ecs::entity::Entity, key: ComponentKey) -> f64 {
    let t = re_core::time::Instant::now();
    for i in 0..WRITES {
        let v = Value::Float(f64::from(i & 1));
        r.set_field(w, e, key, 0, v)
            .expect("the entity has the component");
    }
    t.elapsed().as_nanos() as f64 / f64::from(WRITES)
}

fn main() {
    let mut w = World::new();
    let mut r = Registry::default();
    let native = r.register::<Probe>(&mut w);
    let script = r
        .register_script(
            &mut w,
            ComponentSchema {
                name: "ScriptProbe".into(),
                doc: String::new(),
                fields: vec![FieldSchema {
                    name: "x".into(),
                    ty: FieldType::F64,
                    doc: String::new(),
                    default: Value::Float(0.0),
                }],
                origin: re_reflect::ComponentOrigin::Script,
            },
        )
        .expect("a valid schema");
    let e = w.spawn(Probe::default()).id();
    r.insert(&mut w, e, script, vec![Value::Float(0.0)])
        .unwrap();
    // Warm up.
    run(&r, &mut w, e, native);
    for round in 0..5 {
        let a = run(&r, &mut w, e, native);
        let b = run(&r, &mut w, e, script);
        println!("no hook, round {round}: native {a:.1} ns, script {b:.1} ns");
    }
    r.set_change_hook(Some(hook));
    for round in 0..3 {
        w.insert_resource(Seen::default());
        let c = run(&r, &mut w, e, native);
        let d = run(&r, &mut w, e, script);
        w.remove_resource::<Seen>();
        println!("with hook, round {round}: native {c:.1} ns, script {d:.1} ns");
    }
}

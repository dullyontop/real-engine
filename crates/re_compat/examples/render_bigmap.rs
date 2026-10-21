//! Measures the 3D view's build on the large static map (ADR-0048
//! amendment, WP27 "incremental render"): loads the synthetic map
//! (`tests/fixtures/physics/bigmap.rs`) with a client VM, then times
//! `re_compat::render::build` alone (no GPU) in three runs:
//!
//! * **idle**: nothing changes between frames;
//! * **loose**: 100 unanchored parts fall each frame (physics moves them);
//! * **touch**: a script recolours one part of a static cell each frame,
//!   so that cell is rebuilt.
//!
//! ```sh
//! cargo run --release --locked -p re_compat --example render_bigmap -- --parts 250000 --frames 120
//! ```
//!
//! Options: `--parts N` (250000), `--frames N` (120), `--seed N` (1).
//! The budget: an idle build's mean at most 5 ms, and a loose or touch frame
//! at most 5 ms more.

#[path = "../tests/fixtures/physics/bigmap.rs"]
mod bigmap;

use std::time::Duration;

use glam::Vec2;
use re_compat::render::{self, RenderInput};
use re_compat::{CompatSetup, CompatVm, SignalBehavior, physics};
use re_core::time::Instant;
use re_scene::World;
use re_script::host::{NoServices, WorldHost};
use re_script::{ContextKind, RuntimeConfig};

const IDLE_BUDGET_MS: f64 = 5.0;
const CHANGE_BUDGET_MS: f64 = 5.0;

fn ms(d: Duration) -> f64 {
    d.as_secs_f64() * 1000.0
}

struct Run {
    world: World,
    services: NoServices,
    vm: CompatVm,
}

impl Run {
    fn build(&mut self) -> (Duration, render::RenderFrame) {
        let input = RenderInput {
            viewport: Vec2::new(1280.0, 720.0),
            top_inset: 0.0,
            dt: 1.0 / 60.0,
        };
        let mut host = WorldHost {
            world: &mut self.world,
            services: &mut self.services,
        };
        let mut out = None;
        let mut took = Duration::ZERO;
        let d = self.vm.run(&mut host, |lua| {
            let t = Instant::now();
            out = Some(render::build(lua, &input)?);
            took = t.elapsed();
            Ok(())
        });
        assert!(d.is_empty(), "{d:?}");
        (took, out.expect("the build ran"))
    }

    fn exec(&mut self, source: &str) {
        let mut host = WorldHost {
            world: &mut self.world,
            services: &mut self.services,
        };
        let d = self.vm.exec(&mut host, "bench", source);
        assert!(d.is_empty(), "{d:?}");
    }

    fn step(&mut self) {
        let d = physics::step(&mut self.world, 1.0 / 60.0);
        assert!(d.is_empty(), "{d:?}");
    }
}

fn mean(xs: &[f64]) -> f64 {
    xs.iter().sum::<f64>() / xs.len().max(1) as f64
}

fn main() {
    let mut parts = 250_000usize;
    let mut frames = 120usize;
    let mut seed = 1u64;
    let mut it = std::env::args().skip(1);
    while let Some(k) = it.next() {
        let v: u64 = it
            .next()
            .and_then(|v| v.parse().ok())
            .unwrap_or_else(|| panic!("{k} takes a number"));
        match k.as_str() {
            "--parts" => parts = v as usize,
            "--frames" => frames = v as usize,
            "--seed" => seed = v,
            other => panic!("unknown option {other}"),
        }
    }
    let setup = CompatSetup::standard().expect("the built-in classes load");
    let map = bigmap::generate(parts, seed);
    let doc = bigmap::scene(&map, &setup.classes).expect("the map is a valid scene");
    let mut world = World::new();
    setup.classes.register_components(&mut world).unwrap();
    world.load(&doc).expect("the scene loads");
    drop(doc);
    physics::prepare_world(&mut world, &setup.classes).unwrap();
    assert!(physics::step(&mut world, 0.0).is_empty());
    let dir = std::env::temp_dir().join(format!("re-render-bigmap-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let vm = CompatVm::new(
        &setup,
        ContextKind::Client,
        &mut world,
        &RuntimeConfig::new(&dir),
        SignalBehavior::Deferred,
    )
    .unwrap();
    let mut r = Run {
        world,
        services: NoServices::default(),
        vm,
    };

    let (first, f) = r.build();
    println!(
        "{parts} parts: first (full) build {:.1} ms, {} items, {} parts visited",
        ms(first),
        f.items.len(),
        f.stats.parts_visited
    );
    for _ in 0..5 {
        r.step();
        r.build();
    }

    let mut idle = Vec::new();
    let mut visited = 0u64;
    for _ in 0..frames {
        r.step();
        let (t, f) = r.build();
        idle.push(ms(t));
        visited += u64::from(f.stats.parts_visited);
    }
    let idle_mean = mean(&idle);
    println!("idle: build mean {idle_mean:.3} ms over {frames} frames, {visited} parts visited");

    r.exec(
        r#"
        for i = 1, 100 do
            local p = Instance.new("Part")
            p.Size = Vector3.new(1, 1, 1)
            p.Position = Vector3.new(i * 3, 400, 0)
            p.Parent = workspace
        end
        "#,
    );
    r.step();
    r.build();
    let mut loose = Vec::new();
    let mut visited = 0u64;
    for _ in 0..frames {
        r.step();
        let (t, f) = r.build();
        loose.push(ms(t));
        visited += u64::from(f.stats.parts_visited);
    }
    let loose_mean = mean(&loose);
    println!(
        "loose: build mean {loose_mean:.3} ms with 100 falling parts, {:.1} parts visited a frame",
        visited as f64 / frames as f64
    );
    r.exec("for _, p in workspace:GetChildren() do if p:IsA('BasePart') and not p.Anchored then p:Destroy() end end");
    r.step();
    r.build();

    let mut touch = Vec::new();
    for f in 0..frames {
        r.exec(&format!(
            "local p = workspace.Map:FindFirstChildWhichIsA('BasePart', true)
             p.Color = Color3.fromHSV({} / {frames}, 0.5, 0.8)",
            f
        ));
        r.step();
        let (t, _) = r.build();
        touch.push(ms(t));
    }
    let touch_mean = mean(&touch);
    println!("touch: build mean {touch_mean:.3} ms recolouring one cell part a frame");
    let pass = idle_mean <= IDLE_BUDGET_MS
        && loose_mean <= idle_mean + CHANGE_BUDGET_MS
        && touch_mean <= idle_mean + CHANGE_BUDGET_MS;
    println!(
        "{} (budgets: idle {IDLE_BUDGET_MS} ms, a change at most {CHANGE_BUDGET_MS} ms more)",
        if pass { "PASS" } else { "FAIL" }
    );
    let _ = std::fs::remove_dir_all(&dir);
}

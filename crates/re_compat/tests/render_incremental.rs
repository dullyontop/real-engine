//! WP27 "incremental render" (ADR-0048 amendment): the 3D view keeps its
//! parts from frame to frame and looks only at what the world's change
//! journal names. Idle frames visit no parts, and random script edits give
//! the same frame as a full rebuild.

use std::sync::Arc;

use glam::Vec2;
use re_compat::render::{self, RenderInput};
use re_compat::{CompatSetup, CompatVm, SignalBehavior, physics};
use re_scene::World;
use re_script::host::{NoServices, WorldHost};
use re_script::{ContextKind, RuntimeConfig};
use wp15::{Place, place_in};

#[path = "fixtures/physics/bigmap.rs"]
mod bigmap;
mod wp15;

fn input() -> RenderInput {
    RenderInput {
        viewport: Vec2::new(800.0, 600.0),
        top_inset: 0.0,
        dt: 1.0 / 60.0,
    }
}

/// The synthetic city of `parts` anchored parts with a client VM.
struct Map {
    world: World,
    services: NoServices,
    vm: CompatVm,
    _dir: tempfile::TempDir,
}

impl Map {
    fn new(parts: usize) -> Map {
        let setup: Arc<CompatSetup> = CompatSetup::standard().unwrap();
        let map = bigmap::generate(parts, 1);
        let doc = bigmap::scene(&map, &setup.classes).unwrap();
        let mut world = World::new();
        setup.classes.register_components(&mut world).unwrap();
        world.load(&doc).unwrap();
        physics::prepare_world(&mut world, &setup.classes).unwrap();
        assert!(physics::step(&mut world, 0.0).is_empty());
        let dir = tempfile::tempdir().unwrap();
        let vm = CompatVm::new(
            &setup,
            ContextKind::Client,
            &mut world,
            &RuntimeConfig::new(dir.path()),
            SignalBehavior::Deferred,
        )
        .unwrap();
        Map {
            world,
            services: NoServices::default(),
            vm,
            _dir: dir,
        }
    }

    fn render(&mut self) -> render::RenderFrame {
        let mut host = WorldHost {
            world: &mut self.world,
            services: &mut self.services,
        };
        let mut out = None;
        let d = self.vm.run(&mut host, |lua| {
            out = Some(render::build(lua, &input())?);
            Ok(())
        });
        assert!(d.is_empty(), "{d:#?}");
        out.unwrap()
    }

    fn step(&mut self) {
        assert!(physics::step(&mut self.world, 1.0 / 60.0).is_empty());
    }
}

fn idle_frames_visit_no_parts(parts: usize) {
    let mut m = Map::new(parts);
    let first = m.render();
    assert!(first.stats.full);
    // The parts, and the Terrain (a BasePart that is not drawn).
    assert_eq!(first.stats.parts_visited as usize, parts + 1);
    let items = first.items.len();
    assert!(
        items > 0 && items < parts,
        "{items} items for {parts} parts"
    );
    for _ in 0..3 {
        m.step();
        let f = m.render();
        assert!(!f.stats.full);
        assert_eq!(f.stats.parts_visited, 0);
        assert_eq!(f.items.len(), items);
        assert!(f.new_meshes.is_empty() && f.released_meshes.is_empty());
    }
}

#[test]
fn idle_frames_visit_no_parts_on_the_60k_map() {
    idle_frames_visit_no_parts(60_000);
}

#[test]
fn idle_frames_visit_no_parts_on_the_250k_map() {
    idle_frames_visit_no_parts(250_000);
}

const SETUP: &str = r#"
local function new(class, props, parent)
    local i = Instance.new(class)
    for k, v in pairs(props) do
        i[k] = v
    end
    i.Parent = parent
    return i
end
local ws = workspace
local rs = game:GetService("ReplicatedStorage")
S.parts = {}
S.homes = {}
S.homes[1] = ws
S.homes[2] = new("Model", {Name = "House"}, ws)
S.homes[3] = new("Folder", {Name = "Street"}, ws)
S.homes[4] = rs
S.homes[5] = new("Folder", {Name = "Inner"}, S.homes[3])
local mats = {Enum.Material.Plastic, Enum.Material.Wood, Enum.Material.Neon, Enum.Material.Slate, Enum.Material.Glass}
S.mats = mats
for i = 1, 60 do
    local anchored = i % 3 ~= 0
    S.parts[i] = new(i % 7 == 0 and "WedgePart" or "Part", {
        Name = "P" .. i,
        Anchored = anchored,
        Size = Vector3.new(1 + i % 4, 1 + i % 2, 2),
        Position = Vector3.new((i % 8) * 6, anchored and 0.5 or 10 + i, math.floor(i / 8) * 6),
        Color = Color3.fromHSV((i % 10) / 10, 0.7, 0.9),
        Material = mats[1 + i % #mats],
    }, S.homes[1 + i % 3])
end
new("Part", {Name = "Floor", Anchored = true, Size = Vector3.new(200, 1, 200), Position = Vector3.new(20, -1, 20)}, ws)
S.tween = game:GetService("TweenService")
function S.op(k, a, b)
    local n = #S.parts
    local p = S.parts[1 + a % n]
    if p == nil then return end
    if k == 0 then
        p.Color = Color3.fromHSV((b % 20) / 20, 0.5, 0.8)
    elseif k == 1 then
        p.Transparency = ({0, 0, 0.5, 1})[1 + b % 4]
    elseif k == 2 then
        p.Size = Vector3.new(1 + b % 5, 1 + b % 3, 1 + b % 2)
    elseif k == 3 then
        p.Material = S.mats[1 + b % #S.mats]
    elseif k == 4 then
        p.Parent = S.homes[1 + b % #S.homes]
    elseif k == 5 then
        p:Destroy()
        table.remove(S.parts, 1 + a % n)
    elseif k == 6 then
        local c = p:Clone()
        c.Parent = S.homes[1 + b % #S.homes]
        table.insert(S.parts, c)
    elseif k == 7 then
        S.tween:Create(p, TweenInfo.new(0.2), {Position = p.Position + Vector3.new(0, 1 + b % 3, 0)}):Play()
    elseif k == 8 then
        p.CFrame = CFrame.new((b % 9) * 5, 0.5 + b % 4, (b % 7) * 5) * CFrame.Angles(0, b % 4, 0)
    elseif k == 9 then
        p.Anchored = not p.Anchored
    elseif k == 10 then
        p.CastShadow = not p.CastShadow
    elseif k == 11 then
        S.homes[2]:PivotTo(CFrame.new(b % 11, 0, b % 13))
    elseif k == 12 then
        local h = p:FindFirstChildOfClass("Highlight")
        if h then h:Destroy() else Instance.new("Highlight", p) end
    elseif k == 13 then
        S.homes[3].Parent = (S.homes[3].Parent == workspace) and S.homes[4] or workspace
    elseif k == 14 then
        p.LocalTransparencyModifier = (b % 3) / 2
    end
end
"#;

/// A small xorshift, so the run is the same every time.
struct Rng(u64);

impl Rng {
    fn next(&mut self) -> u64 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        self.0
    }
}

#[test]
fn random_edits_give_the_frame_of_a_full_rebuild() {
    let mut p: Place = place_in(
        SignalBehavior::Deferred,
        &["Workspace", "ReplicatedStorage", "Lighting"],
    );
    p.run(SETUP);
    p.lua(|lua| render::verify_every(lua, 1));
    let mut rng = Rng(0x9e37_79b9_7f4a_7c15);
    let mut visited_on_edits = 0u64;
    for frame in 0..240 {
        let edits = rng.next() % 4;
        for _ in 0..edits {
            let (k, a, b) = (rng.next() % 15, rng.next() % 1000, rng.next() % 1000);
            p.run(&format!("S.op({k}, {a}, {b})"));
        }
        p.frame(1.0 / 60.0);
        p.physics(1.0 / 60.0);
        // `render` fails the test when the frame differs from a full rebuild.
        let f = p.render(&input());
        if frame > 0 {
            assert!(!f.stats.full);
            visited_on_edits += u64::from(f.stats.parts_visited);
        }
    }
    // Far fewer visits than rebuilding every part each frame.
    assert!(visited_on_edits < 240 * 61 / 2, "{visited_on_edits}");
}

#[test]
fn a_journal_that_overflowed_gives_a_full_pass() {
    let mut p: Place = place_in(
        SignalBehavior::Deferred,
        &["Workspace", "ReplicatedStorage", "Lighting"],
    );
    p.run(SETUP);
    p.lua(|lua| render::verify_every(lua, 1));
    p.frame(1.0 / 60.0);
    assert!(p.render(&input()).stats.full);
    // More writes than the journal keeps, with no build to drain them (a
    // hidden window, a headless replay after one screenshot).
    p.run(
        "for i = 1, 70000 do S.parts[1 + i % 60].Transparency = (i // 60 % 2) * 0.5 end \
         S.parts[1].Color = Color3.new(1, 0, 0)",
    );
    p.frame(1.0 / 60.0);
    let f = p.render(&input());
    assert!(f.stats.full, "an overflowed journal names nothing");
    p.frame(1.0 / 60.0);
    assert!(!p.render(&input()).stats.full);
}

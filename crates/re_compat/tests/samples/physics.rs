//! WP13's steps: physics in a booted sample (after `runtime.boot`). Every step
//! builds its own probe parts in an empty corner of the sample's world, far
//! from what the sample places, and removes them afterwards.
//!
//! Every step first runs the Luau in `setup`, if any, in its side (for
//! example to call the sample's own module that registers its groups).
//!
//! * `physics.groups`: the registered collision groups are exactly `groups`
//!   (in any order), `CollisionGroupsAreCollidable` is false for exactly the
//!   pairs in `apart` (either order) and true for every other ordered pair,
//!   and for every ordered pair a part dropped onto a static part of the
//!   other group rests on it when the pair collides and falls through when it
//!   does not. `side` (default `server`).
//! * `physics.fall`: a part in free fall moves `distance` ± `tolerance` studs
//!   in `seconds` (default 1).
//! * `physics.script`: runs Luau in a side, in `steps` (each `{ "code",
//!   "frames", "realtime" }`: the code, then that many frames, paced at the
//!   wall clock when `realtime`), and compares what the code printed (not
//!   warnings, nor what the sample printed meanwhile) with `expect`, line by
//!   line. For checks that drive a sample's
//!   own modules; the code lives in the kit.
//! * `physics.queries`: `queries` random `Raycast`, `Blockcast`,
//!   `GetPartBoundsInBox` and `GetPartBoundsInRadius` calls over `parts`
//!   random probe parts in the sample's groups (`groups`, `apart` as above),
//!   with Include and Exclude filters and a `CollisionGroup`, each equal to a
//!   brute-force answer.

use std::collections::BTreeSet;

use glam::{Quat, Vec3};
use re_compat::testing::{Harness, Side};
use re_physics::parts::geom::{Placed, obb_overlap, obb_sphere_overlap, shape_cast};
use re_physics::{PartGeom, PartShape, Pose};
use serde_json::Value as J;

use super::Ctx;

pub fn step(name: &str) -> Option<super::Step> {
    Some(match name {
        "physics.groups" => groups,
        "physics.fall" => fall,
        "physics.script" => script,
        "physics.queries" => queries,
        _ => return None,
    })
}

/// The probe corner: outside any sample map, high above everything, and
/// inside the simulation's world limits.
const CORNER: Vec3 = Vec3::new(3000.0, 1500.0, 3000.0);

fn booted(ctx: &mut Ctx) -> Result<&mut Harness, String> {
    super::runtime::booted(ctx).ok_or_else(|| "physics steps need runtime.boot first".into())
}

fn side(args: &J) -> Side {
    match args["side"].as_str() {
        Some("client") => Side::Client,
        _ => Side::Server,
    }
}

fn strings(v: &J) -> Vec<String> {
    v.as_array()
        .map(|a| {
            a.iter()
                .filter_map(|s| s.as_str().map(str::to_string))
                .collect()
        })
        .unwrap_or_default()
}

/// The non-colliding pairs, both orders.
fn apart(args: &J) -> BTreeSet<(String, String)> {
    let mut out = BTreeSet::new();
    for p in args["apart"].as_array().into_iter().flatten() {
        let p = strings(p);
        if let [a, b] = p.as_slice() {
            out.insert((a.clone(), b.clone()));
            out.insert((b.clone(), a.clone()));
        }
    }
    out
}

/// Runs `code` in `side` and returns what it printed, failing on any error.
fn run(h: &mut Harness, side: Side, code: &str) -> Result<Vec<String>, String> {
    let _ = h.take_output(side);
    let d = h.exec(side, "physics-check", code);
    if let Some(d) = d.first() {
        return Err(d.render_human());
    }
    // What the code printed; the sample's own warnings are not its answer.
    Ok(h.take_output(side)
        .into_iter()
        .filter(|l| !l.warn)
        .map(|l| l.text)
        .collect())
}

/// The booted harness, after the step's `setup` Luau ran in its side.
fn prepared<'a>(ctx: &'a mut Ctx, args: &J) -> Result<&'a mut Harness, String> {
    let h = booted(ctx)?;
    if let Some(code) = args["setup"].as_str() {
        run(h, side(args), code).map_err(|e| format!("setup: {e}"))?;
    }
    Ok(h)
}

fn frames(h: &mut Harness, n: u32) {
    // Errors the sample's own scripts raise meanwhile are runtime.boot's
    // business, not these checks'.
    let _ = h.step(n);
}

const CLEAN: &str = "local old = workspace:FindFirstChild('PhysicsCheck') if old then old:Destroy() end local root = Instance.new('Folder') root.Name = 'PhysicsCheck' root.Parent = workspace\n";

fn groups(ctx: &mut Ctx, args: &J) -> Result<(), String> {
    let side = side(args);
    let want: BTreeSet<String> = strings(&args["groups"]).into_iter().collect();
    let apart = apart(args);
    let h = prepared(ctx, args)?;
    let mut problems = Vec::new();
    let got: BTreeSet<String> = run(
        h,
        side,
        "for _, g in game:GetService('PhysicsService'):GetRegisteredCollisionGroups() do print(g.name) end",
    )?
    .into_iter()
    .collect();
    if got != want {
        return Err(format!("registered groups {got:?}, want {want:?}"));
    }
    let names: Vec<&String> = want.iter().collect();
    let mut s = String::from(CLEAN);
    s += "local PS = game:GetService('PhysicsService')\n";
    let mut pairs = Vec::new();
    for (i, a) in names.iter().enumerate() {
        for (j, b) in names.iter().enumerate() {
            let at = CORNER + Vec3::new(i as f32 * 16.0, 0.0, j as f32 * 16.0);
            s += &format!(
                "do print(PS:CollisionGroupsAreCollidable({a:?}, {b:?})) \
                 local floor = Instance.new('Part') floor.Name = 'F{i}_{j}' floor.Anchored = true floor.Size = Vector3.new(10, 1, 10) floor.CFrame = CFrame.new({}, {}, {}) floor.CollisionGroup = {b:?} floor.Parent = root \
                 local drop = Instance.new('Part') drop.Name = 'D{i}_{j}' drop.Size = Vector3.new(2, 2, 2) drop.CFrame = CFrame.new({}, {}, {}) drop.CollisionGroup = {a:?} drop.Parent = root end\n",
                at.x,
                at.y,
                at.z,
                at.x,
                at.y + 2.0,
                at.z,
            );
            pairs.push((i, j, !apart.contains(&((*a).clone(), (*b).clone()))));
        }
    }
    let said = run(h, side, &s)?;
    frames(h, 60);
    let mut q = String::new();
    for (i, j, _) in &pairs {
        q += &format!("print(workspace.PhysicsCheck.D{i}_{j}.Position.Y)\n");
    }
    let ys = run(h, side, &q)?;
    run(h, side, "workspace.PhysicsCheck:Destroy()")?;
    if said.len() != pairs.len() || ys.len() != pairs.len() {
        return Err(format!(
            "{} answers and {} heights for {} pairs",
            said.len(),
            ys.len(),
            pairs.len()
        ));
    }
    for (((i, j, collide), said), y) in pairs.iter().zip(&said).zip(&ys) {
        let (a, b) = (names[*i], names[*j]);
        if said != &collide.to_string() {
            problems.push(format!(
                "{a}-{b}: CollisionGroupsAreCollidable is {said}, want {collide}"
            ));
        }
        let y: f32 = y.parse().map_err(|_| format!("{a}-{b}: height {y:?}"))?;
        let rests = (y - (CORNER.y + 1.5)).abs() < 0.25;
        let fell = y < CORNER.y - 10.0;
        if (*collide && !rests) || (!*collide && !fell) {
            problems.push(format!(
                "{a} dropped on {b} ends at {:.2} studs from the floor top, want it to {}",
                y - (CORNER.y + 0.5),
                if *collide { "rest" } else { "fall through" }
            ));
        }
    }
    if problems.is_empty() {
        Ok(())
    } else {
        Err(format!(
            "{} problem(s):\n{}",
            problems.len(),
            problems.join("\n")
        ))
    }
}

fn fall(ctx: &mut Ctx, args: &J) -> Result<(), String> {
    let side = side(args);
    let want = args["distance"]
        .as_f64()
        .ok_or("physics.fall needs distance")? as f32;
    let tol = args["tolerance"].as_f64().unwrap_or(0.5) as f32;
    let seconds = args["seconds"].as_f64().unwrap_or(1.0);
    let h = prepared(ctx, args)?;
    let start = CORNER - Vec3::new(400.0, 0.0, 400.0);
    run(
        h,
        side,
        &format!(
            "{CLEAN} local p = Instance.new('Part') p.Name = 'Faller' p.Size = Vector3.new(2, 2, 2) p.CFrame = CFrame.new({}, {}, {}) p.Parent = root",
            start.x, start.y, start.z
        ),
    )?;
    frames(h, (seconds * 60.0).round() as u32);
    let y = run(
        h,
        side,
        "print(workspace.PhysicsCheck.Faller.Position.Y) workspace.PhysicsCheck:Destroy()",
    )?;
    let y: f32 = y.first().and_then(|s| s.parse().ok()).ok_or("no height")?;
    let moved = start.y - y;
    if (moved - want).abs() <= tol {
        Ok(())
    } else {
        Err(format!(
            "fell {moved:.3} studs in {seconds} s, want {want} ± {tol}"
        ))
    }
}

fn script(ctx: &mut Ctx, args: &J) -> Result<(), String> {
    let side = side(args);
    let want = strings(&args["expect"]);
    let h = prepared(ctx, args)?;
    let mut got = Vec::new();
    for s in args["steps"].as_array().into_iter().flatten() {
        if let Some(code) = s["code"].as_str() {
            got.extend(run(h, side, code)?);
        }
        let n = s["frames"].as_u64().unwrap_or(0) as u32;
        if s["realtime"].as_bool() == Some(true) {
            // For code that times itself with `os.clock`: frames at the pace
            // of the wall clock.
            let start = re_core::time::Instant::now();
            for i in 0..n {
                frames(h, 1);
                let due = std::time::Duration::from_secs_f64(f64::from(i + 1) / 60.0);
                if let Some(wait) = due.checked_sub(start.elapsed()) {
                    std::thread::sleep(wait);
                }
            }
        } else {
            frames(h, n);
        }
    }
    if got == want {
        Ok(())
    } else {
        Err(format!("printed {got:?}, want {want:?}"))
    }
}

struct Rng(u64);

impl Rng {
    fn next(&mut self) -> f32 {
        self.0 = self
            .0
            .wrapping_mul(6364136223846793005)
            .wrapping_add(1442695040888963407);
        ((self.0 >> 40) as f32) / (1u64 << 24) as f32
    }
    fn range(&mut self, lo: f32, hi: f32) -> f32 {
        lo + (hi - lo) * self.next()
    }
    fn vec(&mut self, lo: f32, hi: f32) -> Vec3 {
        Vec3::new(self.range(lo, hi), self.range(lo, hi), self.range(lo, hi))
    }
    fn pick<'a, T>(&mut self, xs: &'a [T]) -> &'a T {
        &xs[((self.next() * xs.len() as f32) as usize).min(xs.len() - 1)]
    }
}

struct Probe {
    name: String,
    folder: usize,
    geom: PartGeom,
    pose: Pose,
    group: String,
    can_collide: bool,
}

struct Filter {
    lua: String,
    folders: BTreeSet<usize>,
    include: bool,
    group: String,
}

impl Filter {
    fn random(rng: &mut Rng, ty: &str, groups: &[String]) -> Filter {
        let folders: BTreeSet<usize> = (0..4).filter(|_| rng.next() < 0.3).collect();
        let include = rng.next() < 0.3;
        let group = rng.pick(groups).clone();
        let list = folders
            .iter()
            .map(|f| format!("workspace.PhysicsCheck.F{f}"))
            .collect::<Vec<_>>()
            .join(", ");
        Filter {
            lua: format!(
                "local params = {ty}.new() params.FilterDescendantsInstances = {{{list}}} params.FilterType = Enum.RaycastFilterType.{} params.CollisionGroup = {group:?}",
                if include { "Include" } else { "Exclude" }
            ),
            folders,
            include,
            group,
        }
    }

    fn sees(&self, p: &Probe, apart: &BTreeSet<(String, String)>) -> bool {
        !apart.contains(&(self.group.clone(), p.group.clone()))
            && self.folders.contains(&p.folder) == self.include
            && p.can_collide
    }
}

fn queries(ctx: &mut Ctx, args: &J) -> Result<(), String> {
    let side = side(args);
    let groups = strings(&args["groups"]);
    if groups.is_empty() {
        return Err("physics.queries needs groups".into());
    }
    let apart = apart(args);
    let n_parts = args["parts"].as_u64().unwrap_or(400) as usize;
    let n_queries = args["queries"].as_u64().unwrap_or(1000) as usize;
    let mut rng = Rng(args["seed"].as_u64().unwrap_or(7));
    let h = prepared(ctx, args)?;

    // The probe parts, anchored, in four folders.
    let mut parts = Vec::new();
    let mut s = String::from(CLEAN);
    s += "local f = {} for i = 0, 3 do f[i] = Instance.new('Folder') f[i].Name = 'F' .. i f[i].Parent = root end\n";
    for i in 0..n_parts {
        let shape = *rng.pick(&[
            PartShape::Block,
            PartShape::Ball,
            PartShape::Cylinder,
            PartShape::Wedge,
        ]);
        let size = rng.vec(0.5, 6.0);
        let pos = CORNER + rng.vec(-25.0, 25.0);
        let axis = rng.vec(-1.0, 1.0).normalize_or(Vec3::Y);
        let angle = rng.range(0.0, std::f32::consts::TAU);
        let group = rng.pick(&groups).clone();
        let folder = (rng.next() * 4.0) as usize % 4;
        let can_collide = rng.next() < 0.85;
        s += &format!(
            "do local x = Instance.new('Part') x.Name = 'Q{i}' x.Shape = Enum.PartType.{shape:?} x.Size = Vector3.new({}, {}, {}) x.CFrame = CFrame.new({}, {}, {}) * CFrame.fromAxisAngle(Vector3.new({}, {}, {}), {angle}) x.Anchored = true x.CollisionGroup = {group:?} x.CanCollide = {can_collide} x.CanQuery = {can_collide} x.Parent = f[{folder}] end\n",
            size.x, size.y, size.z, pos.x, pos.y, pos.z, axis.x, axis.y, axis.z
        );
        parts.push(Probe {
            name: format!("Q{i}"),
            folder,
            geom: PartGeom::new(shape, size).sanitized(),
            pose: Pose::new(pos, Quat::from_axis_angle(axis, angle)),
            group,
            can_collide,
        });
    }
    run(h, side, &s)?;

    let mut problems = Vec::new();
    let mut found = 0usize;
    let per_chunk = 100;
    let mut done = 0;
    while done < n_queries {
        let mut s = String::new();
        let mut cases = Vec::new();
        for k in 0..per_chunk.min(n_queries - done) {
            let kind = (done + k) % 4;
            let ty = if kind < 2 {
                "RaycastParams"
            } else {
                "OverlapParams"
            };
            let f = Filter::random(&mut rng, ty, &groups);
            let from = CORNER + rng.vec(-35.0, 35.0);
            let target = rng.pick(&parts).pose.pos;
            let dir = (target - from) * 1.5;
            let size = rng.vec(0.5, 4.0);
            let axis = rng.vec(-1.0, 1.0).normalize_or(Vec3::Y);
            let angle = rng.range(0.0, std::f32::consts::TAU);
            let pose = Pose::new(from, Quat::from_axis_angle(axis, angle));
            let r = rng.range(0.5, 12.0);
            let cf = format!(
                "CFrame.new({}, {}, {}) * CFrame.fromAxisAngle(Vector3.new({}, {}, {}), {angle})",
                from.x, from.y, from.z, axis.x, axis.y, axis.z
            );
            let names = "local n = {} for _, x in l do table.insert(n, x.Name) end table.sort(n) print(table.concat(n, ','))";
            s += &match kind {
                0 => format!(
                    "do {} local r = workspace:Raycast(Vector3.new({}, {}, {}), Vector3.new({}, {}, {}), params) print(r and (r.Instance.Name .. ',' .. r.Distance) or '') end\n",
                    f.lua, from.x, from.y, from.z, dir.x, dir.y, dir.z
                ),
                1 => format!(
                    "do {} local r = workspace:Blockcast({cf}, Vector3.new({}, {}, {}), Vector3.new({}, {}, {}), params) print(r and (r.Instance.Name .. ',' .. r.Distance) or '') end\n",
                    f.lua, size.x, size.y, size.z, dir.x, dir.y, dir.z
                ),
                2 => format!(
                    "do {} local l = workspace:GetPartBoundsInBox({cf}, Vector3.new({}, {}, {}) * 4, params) {names} end\n",
                    f.lua, size.x, size.y, size.z
                ),
                _ => format!(
                    "do {} local l = workspace:GetPartBoundsInRadius(Vector3.new({}, {}, {}), {r}, params) {names} end\n",
                    f.lua, from.x, from.y, from.z
                ),
            };
            cases.push((kind, f, from, dir, size, pose, r));
        }
        let out = run(h, side, &s)?;
        if out.len() != cases.len() {
            return Err(format!("{} answers for {} queries", out.len(), cases.len()));
        }
        for (line, (kind, f, from, dir, size, pose, r)) in out.iter().zip(&cases) {
            let seen = parts.iter().filter(|p| f.sees(p, &apart));
            if *kind < 2 {
                // The nearest hit, by brute force.
                let mut best: Option<(f32, &str)> = None;
                for p in seen {
                    let t = if *kind == 0 {
                        let o = p.pose.inv_point(*from);
                        let d = p.pose.rot.inverse() * dir.normalize();
                        p.geom.ray_entry(o, d, dir.length()).map(|(t, _)| t)
                    } else {
                        let block =
                            Placed::new(PartGeom::new(PartShape::Block, *size).sanitized(), pose);
                        shape_cast(&block, &Placed::new(p.geom, &p.pose), dir.as_dvec3(), 1e-4)
                            .map(|(frac, _, _)| (frac * dir.as_dvec3().length()) as f32)
                    };
                    if let Some(t) = t
                        && best.is_none_or(|(b, _)| t < b)
                    {
                        best = Some((t, &p.name));
                    }
                }
                let got = line
                    .split_once(',')
                    .map(|(n, d)| (n, d.parse::<f32>().unwrap_or(f32::NAN)));
                let ok = match (best, got) {
                    (None, None) => true,
                    (Some((t, want)), Some((name, d))) => {
                        (t - d).abs() < 1e-2 && (name == want || (t - d).abs() < 1e-3)
                    }
                    _ => false,
                };
                if !ok {
                    problems.push((*kind, format!("{line:?} vs brute force {best:?}")));
                }
                found += usize::from(best.is_some());
            } else {
                let got: BTreeSet<&str> = line.split(',').filter(|x| !x.is_empty()).collect();
                let want: BTreeSet<&str> = seen
                    .filter(|p| {
                        if *kind == 2 {
                            obb_overlap(&p.pose, p.geom.half(), pose, *size * 2.0)
                        } else {
                            obb_sphere_overlap(&p.pose, p.geom.half(), *from, *r)
                        }
                    })
                    .map(|p| p.name.as_str())
                    .collect();
                found += want.len();
                if got.symmetric_difference(&want).count() > 1 {
                    problems.push((*kind, format!("{got:?} vs brute force {want:?}")));
                }
            }
        }
        done += cases.len();
    }
    run(h, side, "workspace.PhysicsCheck:Destroy()")?;
    // A cast that only grazes an edge, or a box that only touches a face, may
    // round either way: allow a handful, never more.
    let allowed = args["grazing"].as_u64().unwrap_or(5) as usize;
    if found < n_queries / 4 {
        return Err(format!("only {found} hits over {n_queries} queries"));
    }
    if problems.len() <= allowed {
        Ok(())
    } else {
        let kinds = [
            "Raycast",
            "Blockcast",
            "GetPartBoundsInBox",
            "GetPartBoundsInRadius",
        ];
        Err(problems
            .iter()
            .map(|(k, m)| format!("{}: {m}", kinds[*k]))
            .collect::<Vec<_>>()
            .join("\n"))
    }
}

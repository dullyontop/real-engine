//! The synthetic large static map (ADR-0031): a city of anchored parts on a
//! tiled ground, made from a seed so every run builds the same map.
//!
//! The map is laid out in districts of 256 × 256 studs. Each district holds
//! a 12 × 12 grid of ground tiles, a few buildings (models of stacked wall,
//! floor and roof blocks) and props (wedges, corner wedges, cylinders, balls
//! and trusses) scattered on the ground. Districts are added until the part
//! count is reached, so the map grows outward in a square spiral of
//! districts. Every part is anchored; materials vary so the renderer's
//! per-material merging has work to do.
//!
//! [`generate`] returns plain descriptions; [`scene`] turns them into a
//! compat scene file (`examples/bigmap.rs` loads it and measures).

/// One generated part.
#[derive(Clone, Debug, PartialEq)]
pub struct GenPart {
    /// `Part`, `WedgePart`, `CornerWedgePart` or `TrussPart`.
    pub class: &'static str,
    /// `Part.Shape` for `Part`s: `Block`, `Ball` or `Cylinder`.
    pub shape: &'static str,
    /// Its name.
    pub name: String,
    /// The index of its model in [`GenMap::models`], or `None` for a part
    /// directly under its district's folder.
    pub model: Option<usize>,
    /// The index of its district.
    pub district: usize,
    /// Size in studs.
    pub size: [f32; 3],
    /// Position of its centre.
    pub position: [f32; 3],
    /// Rotation about Y, radians.
    pub yaw: f32,
    /// `Enum.Material` item name.
    pub material: &'static str,
}

/// One generated model (a building).
#[derive(Clone, Debug, PartialEq)]
pub struct GenModel {
    /// Its name.
    pub name: String,
    /// The index of its district.
    pub district: usize,
}

/// The generated map.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct GenMap {
    /// One folder per district, in order.
    pub districts: usize,
    /// The buildings.
    pub models: Vec<GenModel>,
    /// The parts, parents before children.
    pub parts: Vec<GenPart>,
}

/// The side of a district, in studs.
pub const DISTRICT: f32 = 256.0;

const TILES: usize = 12;
const TILE: f32 = DISTRICT / TILES as f32;
const BUILDINGS: usize = 6;
const PROPS: usize = 120;

const GROUND: [&str; 4] = ["Grass", "Concrete", "Asphalt", "Sand"];
const WALLS: [&str; 5] = ["Brick", "Concrete", "Wood", "Marble", "SmoothPlastic"];
const PROP: [&str; 4] = ["Metal", "Wood", "Plastic", "Neon"];

/// A small deterministic generator (an LCG; the low bits are not used).
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

    fn index(&mut self, n: usize) -> usize {
        ((self.next() * n as f32) as usize).min(n - 1)
    }
}

/// The centre of district `i`: districts spiral out from the origin.
fn district_center(i: usize) -> [f32; 2] {
    if i == 0 {
        return [0.0, 0.0];
    }
    // Ring r holds 8r districts.
    let mut r = 1usize;
    let mut first = 1usize;
    while i >= first + 8 * r {
        first += 8 * r;
        r += 1;
    }
    let k = (i - first) as i64;
    let r = r as i64;
    let side = 2 * r;
    let (x, z) = match k / side {
        0 => (-r + k, -r),
        1 => (r, -r + (k - side)),
        2 => (r - (k - 2 * side), r),
        _ => (-r, r - (k - 3 * side)),
    };
    [x as f32 * DISTRICT, z as f32 * DISTRICT]
}

/// Builds a map of exactly `parts` anchored parts from `seed`.
pub fn generate(parts: usize, seed: u64) -> GenMap {
    let mut rng = Rng(seed ^ 0x9e37_79b9_7f4a_7c15);
    let mut map = GenMap::default();
    let mut d = 0;
    while map.parts.len() < parts {
        district(&mut map, &mut rng, d, parts);
        d += 1;
    }
    map.districts = d;
    map
}

fn district(map: &mut GenMap, rng: &mut Rng, d: usize, limit: usize) {
    let [cx, cz] = district_center(d);
    let x0 = cx - DISTRICT / 2.0;
    let z0 = cz - DISTRICT / 2.0;
    let full = |m: &GenMap| m.parts.len() >= limit;
    // Ground tiles, with a little height noise.
    let mut heights = [[0.0f32; TILES]; TILES];
    for (i, row) in heights.iter_mut().enumerate() {
        for (j, h) in row.iter_mut().enumerate() {
            if full(map) {
                return;
            }
            *h = (rng.range(0.0, 3.0)).floor();
            let y = *h - 1.0;
            map.parts.push(GenPart {
                class: "Part",
                shape: "Block",
                name: format!("Tile_{i}_{j}"),
                model: None,
                district: d,
                size: [TILE, 2.0 + 2.0 * *h, TILE],
                position: [
                    x0 + (i as f32 + 0.5) * TILE,
                    y,
                    z0 + (j as f32 + 0.5) * TILE,
                ],
                yaw: 0.0,
                material: GROUND[rng.index(GROUND.len())],
            });
        }
    }
    let ground = |x: f32, z: f32| {
        let i = (((x - x0) / TILE) as usize).min(TILES - 1);
        let j = (((z - z0) / TILE) as usize).min(TILES - 1);
        heights[i][j] * 2.0 - 1.0 + 1.0
    };
    // Buildings: stacked storeys of four walls, a floor and windows' frames.
    for b in 0..BUILDINGS {
        if full(map) {
            return;
        }
        let model = map.models.len();
        map.models.push(GenModel {
            name: format!("Building_{d}_{b}"),
            district: d,
        });
        let w = rng.range(16.0, 40.0).floor();
        let l = rng.range(16.0, 40.0).floor();
        let x = cx + rng.range(-DISTRICT * 0.35, DISTRICT * 0.35);
        let z = cz + rng.range(-DISTRICT * 0.35, DISTRICT * 0.35);
        let base = ground(x, z);
        let storeys = 2 + rng.index(8);
        let material = WALLS[rng.index(WALLS.len())];
        let mut n = 0;
        let mut push = |map: &mut GenMap, size: [f32; 3], pos: [f32; 3]| {
            map.parts.push(GenPart {
                class: "Part",
                shape: "Block",
                name: format!("Block_{n}"),
                model: Some(model),
                district: d,
                size,
                position: pos,
                yaw: 0.0,
                material,
            });
            n += 1;
        };
        for s in 0..storeys {
            let y = base + s as f32 * 10.0;
            // A floor slab, then each wall split into panels.
            if full(map) {
                return;
            }
            push(map, [w, 1.0, l], [x, y + 0.5, z]);
            let panels = 4;
            for p in 0..panels {
                let t = (p as f32 + 0.5) / panels as f32;
                for (size, pos) in [
                    (
                        [w / panels as f32, 9.0, 1.0],
                        [x - w / 2.0 + t * w, y + 5.5, z - l / 2.0],
                    ),
                    (
                        [w / panels as f32, 9.0, 1.0],
                        [x - w / 2.0 + t * w, y + 5.5, z + l / 2.0],
                    ),
                    (
                        [1.0, 9.0, l / panels as f32],
                        [x - w / 2.0, y + 5.5, z - l / 2.0 + t * l],
                    ),
                    (
                        [1.0, 9.0, l / panels as f32],
                        [x + w / 2.0, y + 5.5, z - l / 2.0 + t * l],
                    ),
                ] {
                    if full(map) {
                        return;
                    }
                    push(map, size, pos);
                }
            }
        }
        if full(map) {
            return;
        }
        push(
            map,
            [w + 2.0, 1.0, l + 2.0],
            [x, base + storeys as f32 * 10.0 + 0.5, z],
        );
    }
    // Props on the ground.
    for p in 0..PROPS {
        if full(map) {
            return;
        }
        let x = cx + rng.range(-DISTRICT * 0.48, DISTRICT * 0.48);
        let z = cz + rng.range(-DISTRICT * 0.48, DISTRICT * 0.48);
        let (class, shape, size) = match rng.index(6) {
            0 => (
                "WedgePart",
                "Block",
                [4.0, rng.range(1.0, 4.0), rng.range(2.0, 6.0)],
            ),
            1 => ("CornerWedgePart", "Block", [2.0, 2.0, 2.0]),
            2 => {
                let r = rng.range(1.0, 4.0);
                ("Part", "Cylinder", [rng.range(1.0, 8.0), r, r])
            }
            3 => {
                let r = rng.range(1.0, 5.0);
                ("Part", "Ball", [r, r, r])
            }
            4 => (
                "TrussPart",
                "Block",
                [2.0, 2.0 * (1 + rng.index(6)) as f32, 2.0],
            ),
            _ => (
                "Part",
                "Block",
                [
                    rng.range(1.0, 6.0),
                    rng.range(1.0, 6.0),
                    rng.range(1.0, 6.0),
                ],
            ),
        };
        map.parts.push(GenPart {
            class,
            shape,
            name: format!("Prop_{p}"),
            model: None,
            district: d,
            size,
            position: [x, ground(x, z) + size[1] / 2.0, z],
            yaw: rng.range(0.0, std::f32::consts::TAU),
            material: PROP[rng.index(PROP.len())],
        });
    }
}

/// The scene of `map`: the `game` root, its `Workspace`, a `Map` folder with
/// one folder per district, the buildings and the parts.
pub fn scene(
    map: &GenMap,
    classes: &re_compat::ClassRegistry,
) -> Result<re_scene::scene::SceneDoc, re_compat::CompatError> {
    use re_compat::scene::instance_doc;
    use re_reflect::Doc;
    let v3 = |v: [f32; 3]| Doc::List(v.iter().map(|x| Doc::F32(*x)).collect());
    let mut entities = Vec::with_capacity(map.parts.len() + map.models.len() + map.districts + 3);
    entities.push(instance_doc(
        classes,
        re_compat::ROOT_CLASS,
        re_compat::GAME_ID,
        re_compat::GAME_ID,
        None,
    )?);
    entities.push(instance_doc(
        classes,
        "Workspace",
        "Workspace",
        "Workspace",
        Some(re_compat::GAME_ID),
    )?);
    entities.push(instance_doc(
        classes,
        "Folder",
        "Map",
        "Map",
        Some("Workspace"),
    )?);
    for d in 0..map.districts {
        let id = format!("D{d}");
        entities.push(instance_doc(
            classes,
            "Folder",
            &id,
            &format!("District_{d}"),
            Some("Map"),
        )?);
    }
    for (i, m) in map.models.iter().enumerate() {
        let parent = format!("D{}", m.district);
        entities.push(instance_doc(
            classes,
            "Model",
            &format!("M{i}"),
            &m.name,
            Some(&parent),
        )?);
    }
    for (i, p) in map.parts.iter().enumerate() {
        let parent = match p.model {
            Some(m) => format!("M{m}"),
            None => format!("D{}", p.district),
        };
        let mut doc = instance_doc(classes, p.class, &format!("P{i}"), &p.name, Some(&parent))?;
        let c = &mut doc.components;
        if let Some(f) = c.get_mut("Compat_BasePart") {
            f.insert("Size".into(), v3(p.size));
            f.insert("Anchored".into(), Doc::Bool(true));
            f.insert("Material".into(), Doc::Str(p.material.into()));
        }
        if p.class == "Part"
            && let Some(f) = c.get_mut("Compat_Part")
        {
            f.insert("Shape".into(), Doc::Str(p.shape.into()));
        }
        if let Some(f) = c.get_mut("Transform") {
            let (s, co) = (p.yaw * 0.5).sin_cos();
            f.insert("translation".into(), v3(p.position));
            f.insert(
                "rotation".into(),
                Doc::List(vec![
                    Doc::F32(0.0),
                    Doc::F32(s),
                    Doc::F32(0.0),
                    Doc::F32(co),
                ]),
            );
        }
        entities.push(doc);
    }
    Ok(re_scene::scene::SceneDoc {
        schema: None,
        format: re_scene::scene::FORMAT.to_string(),
        version: re_scene::scene::VERSION,
        script_components: Vec::new(),
        entities,
    })
}

#[cfg(test)]
mod physics_bigmap {
    use super::*;

    #[test]
    fn the_map_has_exactly_the_parts_asked_for_and_is_reproducible() {
        let a = generate(5_000, 1);
        assert_eq!(a.parts.len(), 5_000);
        assert_eq!(a, generate(5_000, 1));
        assert_ne!(a.parts, generate(5_000, 2).parts);
        assert!(a.districts > 1);
    }

    #[test]
    fn districts_do_not_overlap() {
        let mut seen = std::collections::HashSet::new();
        for i in 0..200 {
            let [x, z] = district_center(i);
            assert!(
                seen.insert(((x / DISTRICT) as i64, (z / DISTRICT) as i64)),
                "{i}"
            );
        }
    }
}

//! The level-of-detail quadtree: which chunks to show around the focus
//! points, balanced so that neighbours differ by at most one level.

use std::collections::BTreeSet;

use glam::Vec3;

use super::CHUNK_CELLS;

/// One chunk: level `level`, chunk coordinates `(x, z)` on that level's grid.
/// It covers that level's samples `x × 64 ..= x × 64 + 64` along X (and the
/// same along Z).
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct ChunkKey {
    /// The level: 0 is the finest.
    pub level: u8,
    /// Chunk coordinate along X on its level's grid.
    pub x: i32,
    /// Chunk coordinate along Z on its level's grid.
    pub z: i32,
}

impl ChunkKey {
    /// The chunk one level coarser that contains this one.
    pub fn parent(self) -> ChunkKey {
        ChunkKey {
            level: self.level + 1,
            x: self.x.div_euclid(2),
            z: self.z.div_euclid(2),
        }
    }

    /// The four chunks one level finer, in `(x, z)` order: `(0,0)`, `(1,0)`,
    /// `(0,1)`, `(1,1)`. `None` on level 0.
    pub fn children(self) -> Option<[ChunkKey; 4]> {
        let level = self.level.checked_sub(1)?;
        let c = |dx: i32, dz: i32| ChunkKey {
            level,
            x: self.x * 2 + dx,
            z: self.z * 2 + dz,
        };
        Some([c(0, 0), c(1, 0), c(0, 1), c(1, 1)])
    }
}

/// The edges of a chunk whose neighbour is one level coarser, as bits.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub struct Edges(pub u8);

impl Edges {
    /// The edge at the smallest X (the column of samples `x = 0`).
    pub const NEG_X: u8 = 1;
    /// The edge at the largest X.
    pub const POS_X: u8 = 2;
    /// The edge at the smallest Z (the row of samples `z = 0`).
    pub const NEG_Z: u8 = 4;
    /// The edge at the largest Z.
    pub const POS_Z: u8 = 8;

    /// True if the edge `bit` is stitched.
    pub fn has(self, bit: u8) -> bool {
        self.0 & bit != 0
    }
}

/// The layout of a terrain: the spacing of level 0 and the coarsest level.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct TerrainGrid {
    /// Metres between level-0 samples.
    pub base_spacing: f32,
    /// The coarsest level; its chunks are the root tiles.
    pub max_level: u8,
}

impl TerrainGrid {
    /// Metres between samples on `level` (exact: a power-of-two multiple).
    pub fn spacing(&self, level: u8) -> f32 {
        self.base_spacing * (1u32 << level) as f32
    }

    /// The side of a chunk on `level`, in metres.
    pub fn chunk_size(&self, level: u8) -> f32 {
        self.spacing(level) * CHUNK_CELLS as f32
    }

    /// The global sample index of `key`'s sample `(0, 0)` on its level.
    pub fn origin_index(&self, key: ChunkKey) -> [i64; 2] {
        let c = i64::from(CHUNK_CELLS);
        [i64::from(key.x) * c, i64::from(key.z) * c]
    }

    /// The smallest and largest corner of `key` in X and Z, in metres.
    pub fn bounds(&self, key: ChunkKey) -> ([f32; 2], [f32; 2]) {
        let s = self.spacing(key.level);
        let [ox, oz] = self.origin_index(key);
        let c = i64::from(CHUNK_CELLS);
        (
            [ox as f32 * s, oz as f32 * s],
            [(ox + c) as f32 * s, (oz + c) as f32 * s],
        )
    }

    /// Distance in the XZ plane from `p` to `key`'s square (0 inside it).
    pub fn distance(&self, key: ChunkKey, p: Vec3) -> f32 {
        let (lo, hi) = self.bounds(key);
        let dx = (lo[0] - p.x).max(0.0).max(p.x - hi[0]);
        let dz = (lo[1] - p.z).max(0.0).max(p.z - hi[1]);
        (dx * dx + dz * dz).sqrt()
    }

    /// The chunk of `level` containing point `p`.
    pub fn chunk_at(&self, level: u8, p: Vec3) -> ChunkKey {
        let size = self.chunk_size(level);
        ChunkKey {
            level,
            x: floor_i32(p.x / size),
            z: floor_i32(p.z / size),
        }
    }
}

fn floor_i32(v: f32) -> i32 {
    let t = v as i32;
    if (t as f32) > v { t - 1 } else { t }
}

/// The chunks to show, each with the edges it must stitch to a coarser
/// neighbour, sorted by key.
///
/// Root tiles (level `grid.max_level`) within `view` metres of any focus are
/// shown. A chunk splits while a focus is closer than `split × its size`.
/// The result is then balanced: a chunk with a neighbour two or more levels
/// finer along an edge is split too, so every stitched edge faces a chunk
/// exactly one level coarser.
pub fn select(grid: &TerrainGrid, focus: &[Vec3], view: f32, split: f32) -> Vec<(ChunkKey, Edges)> {
    let top = grid.max_level;
    let mut leaves: BTreeSet<ChunkKey> = BTreeSet::new();
    let reach = (view / grid.chunk_size(top)) as i32 + 1;
    let mut roots = BTreeSet::new();
    for f in focus {
        let c = grid.chunk_at(top, *f);
        for x in c.x - reach..=c.x + reach {
            for z in c.z - reach..=c.z + reach {
                let k = ChunkKey { level: top, x, z };
                if grid.distance(k, *f) <= view {
                    roots.insert(k);
                }
            }
        }
    }
    let near = |k: ChunkKey| {
        let limit = split * grid.chunk_size(k.level);
        focus.iter().any(|f| grid.distance(k, *f) < limit)
    };
    let mut stack: Vec<ChunkKey> = roots.into_iter().collect();
    while let Some(k) = stack.pop() {
        match k.children() {
            Some(children) if near(k) => stack.extend(children),
            _ => {
                leaves.insert(k);
            }
        }
    }
    balance(&mut leaves);
    leaves
        .iter()
        .map(|&k| (k, stitched_edges(&leaves, k)))
        .collect()
}

/// The four edge directions: (bit, dx, dz).
const SIDES: [(u8, i32, i32); 4] = [
    (Edges::NEG_X, -1, 0),
    (Edges::POS_X, 1, 0),
    (Edges::NEG_Z, 0, -1),
    (Edges::POS_Z, 0, 1),
];

/// True if a leaf at least two levels finer than `k` touches `k`'s edge
/// towards `(dx, dz)`.
fn finer_neighbour(leaves: &BTreeSet<ChunkKey>, k: ChunkKey, dx: i32, dz: i32) -> bool {
    let n = ChunkKey {
        level: k.level,
        x: k.x + dx,
        z: k.z + dz,
    };
    for m in 0..k.level.saturating_sub(1) {
        let shift = k.level - m;
        let span = 1i32 << shift;
        for i in 0..span {
            // The level-m chunk inside `n` along the shared edge.
            let (x, z) = match (dx, dz) {
                (-1, 0) => ((n.x << shift) + span - 1, (n.z << shift) + i),
                (1, 0) => (n.x << shift, (n.z << shift) + i),
                (0, -1) => ((n.x << shift) + i, (n.z << shift) + span - 1),
                _ => ((n.x << shift) + i, n.z << shift),
            };
            if leaves.contains(&ChunkKey { level: m, x, z }) {
                return true;
            }
        }
    }
    false
}

/// Splits leaves until no leaf has a neighbour two or more levels finer.
fn balance(leaves: &mut BTreeSet<ChunkKey>) {
    loop {
        let split: Vec<ChunkKey> = leaves
            .iter()
            .copied()
            .filter(|&k| {
                k.level >= 2
                    && SIDES
                        .iter()
                        .any(|&(_, dx, dz)| finer_neighbour(leaves, k, dx, dz))
            })
            .collect();
        if split.is_empty() {
            return;
        }
        for k in split {
            leaves.remove(&k);
            leaves.extend(k.children().expect("level >= 2"));
        }
    }
}

/// The edges of leaf `k` whose neighbour is the leaf one level coarser.
fn stitched_edges(leaves: &BTreeSet<ChunkKey>, k: ChunkKey) -> Edges {
    let mut bits = 0;
    for (bit, dx, dz) in SIDES {
        let n = ChunkKey {
            level: k.level,
            x: k.x + dx,
            z: k.z + dz,
        };
        if n.parent() != k.parent() && leaves.contains(&n.parent()) {
            bits |= bit;
        }
    }
    Edges(bits)
}

#[cfg(test)]
mod tests {
    use super::*;

    const GRID: TerrainGrid = TerrainGrid {
        base_spacing: 1.0,
        max_level: 5,
    };

    /// The leaf covering level-0 chunk `(x, z)`, if any.
    fn leaf_at(set: &BTreeSet<ChunkKey>, x: i32, z: i32) -> Option<ChunkKey> {
        (0..=GRID.max_level)
            .map(|l| ChunkKey {
                level: l,
                x: x >> l,
                z: z >> l,
            })
            .find(|k| set.contains(k))
    }

    #[test]
    fn selection_tiles_the_roots_and_is_balanced() {
        let focus = [Vec3::new(130.0, 0.0, -2000.0), Vec3::new(5000.0, 3.0, 40.0)];
        let picked = select(&GRID, &focus, 6000.0, 1.5);
        assert_eq!(picked, select(&GRID, &focus, 6000.0, 1.5), "deterministic");
        let set: BTreeSet<ChunkKey> = picked.iter().map(|p| p.0).collect();
        let edges: std::collections::BTreeMap<ChunkKey, Edges> = picked.iter().copied().collect();

        // Leaves never overlap: no leaf has an ancestor that is also a leaf.
        for k in &set {
            let mut a = *k;
            while a.level < GRID.max_level {
                a = a.parent();
                assert!(!set.contains(&a), "{k:?} under {a:?}");
            }
        }
        // Areas add up to the roots' area.
        let roots: BTreeSet<ChunkKey> = set
            .iter()
            .map(|k| {
                let mut a = *k;
                while a.level < GRID.max_level {
                    a = a.parent();
                }
                a
            })
            .collect();
        let area = |l: u8| 1u64 << (2 * l);
        assert_eq!(
            set.iter().map(|k| area(k.level)).sum::<u64>(),
            roots.len() as u64 * area(GRID.max_level)
        );
        // The focus sits in level-0 chunks, and far roots stay whole.
        for f in focus {
            assert_eq!(
                leaf_at(&set, GRID.chunk_at(0, f).x, GRID.chunk_at(0, f).z)
                    .unwrap()
                    .level,
                0
            );
        }
        assert!(set.iter().any(|k| k.level == GRID.max_level));

        // Neighbours along every level-0 edge differ by at most one level, and
        // the finer side stitches exactly where the other is coarser.
        let c0 = GRID.chunk_at(0, focus[0]);
        for x in c0.x - 200..c0.x + 200 {
            for z in c0.z - 60..c0.z + 60 {
                let (Some(a), Some(b)) = (leaf_at(&set, x, z), leaf_at(&set, x + 1, z)) else {
                    continue;
                };
                if a == b {
                    continue;
                }
                assert!(a.level.abs_diff(b.level) <= 1, "{a:?} {b:?}");
                assert_eq!(
                    edges[&a].has(Edges::POS_X),
                    b.level > a.level,
                    "{a:?} {b:?}"
                );
                assert_eq!(
                    edges[&b].has(Edges::NEG_X),
                    a.level > b.level,
                    "{a:?} {b:?}"
                );
            }
        }
    }

    #[test]
    fn positions_follow_the_exact_rule() {
        let g = TerrainGrid {
            base_spacing: 0.7,
            max_level: 4,
        };
        let k = ChunkKey {
            level: 3,
            x: -2,
            z: 5,
        };
        let (lo, hi) = g.bounds(k);
        assert_eq!(lo[0], (-128i64) as f32 * g.spacing(3));
        assert_eq!(hi[1], (384i64) as f32 * g.spacing(3));
        // A coarse chunk's corner is the same float as its finest descendant's.
        let fine = ChunkKey {
            level: 0,
            x: -16,
            z: 40,
        };
        assert_eq!(g.bounds(fine).0, lo);
        assert_eq!(g.chunk_at(3, Vec3::new(lo[0], 0.0, lo[1])), k);
        assert_eq!(k.children().unwrap()[3].parent(), k);
        assert!(
            ChunkKey {
                level: 0,
                x: 0,
                z: 0
            }
            .children()
            .is_none()
        );
    }
}

//! World configuration: limits, gravity and the layer table.

use glam::Vec3;
use re_types::physics::Layer;

/// Which [`Layer`]s collide with which.
///
/// Every layer is split in two inside Jolt: one object layer for static bodies (in the
/// static broad-phase tree) and one for bodies that move (dynamic and kinematic, in
/// the moving tree). A pair of layers therefore has two switches:
///
/// * moving bodies of layer `a` against moving bodies of layer `b`;
/// * moving bodies of layer `a` against static bodies of layer `b`.
///
/// Static bodies never collide with each other. The table is symmetric in the first
/// switch; the second is directional (debris hits static geometry, but a static body
/// on the debris layer is not hit by moving default-layer bodies unless enabled).
///
/// [`LayerTable::standard`] is the engine's default: [`DEFAULT`](Self::DEFAULT) and
/// every other layer collide with everything, except that moving
/// [`DEBRIS`](Self::DEBRIS) bodies only hit static bodies.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LayerTable {
    count: u16,
    moving: Vec<bool>,
    vs_static: Vec<bool>,
}

impl LayerTable {
    /// The most layers a table can hold.
    pub const MAX_LAYERS: u16 = 32;
    /// Layer 0: the layer of every body unless its `Collider.layer` says otherwise.
    pub const DEFAULT: Layer = Layer(0);
    /// Layer 1 in [`standard`](Self::standard): moving bodies on it collide with static
    /// bodies only (gibs, rubble, particles with physics).
    pub const DEBRIS: Layer = Layer(1);

    /// A table of `count` layers (clamped to `1..=MAX_LAYERS`) in which everything
    /// collides with everything.
    pub fn new(count: u16) -> LayerTable {
        let count = count.clamp(1, Self::MAX_LAYERS);
        let n = usize::from(count) * usize::from(count);
        LayerTable {
            count,
            moving: vec![true; n],
            vs_static: vec![true; n],
        }
    }

    /// The engine's default table: 8 layers, all colliding, except that moving
    /// [`DEBRIS`](Self::DEBRIS) bodies only hit static bodies.
    pub fn standard() -> LayerTable {
        let mut table = LayerTable::new(8);
        for other in 0..table.count {
            table.set_moving_collides(Self::DEBRIS, Layer(other), false);
        }
        table
    }

    /// The number of layers.
    pub fn count(&self) -> u16 {
        self.count
    }

    /// True when `layer` is in the table.
    pub fn contains(&self, layer: Layer) -> bool {
        layer.0 < self.count
    }

    fn index(&self, a: Layer, b: Layer) -> Option<usize> {
        (self.contains(a) && self.contains(b))
            .then(|| usize::from(a.0) * usize::from(self.count) + usize::from(b.0))
    }

    /// Sets both switches for the pair, in both directions. Layers outside the table
    /// are ignored.
    pub fn set_collides(&mut self, a: Layer, b: Layer, collides: bool) -> &mut Self {
        self.set_moving_collides(a, b, collides);
        self.set_static_collides(a, b, collides);
        self.set_static_collides(b, a, collides)
    }

    /// Sets whether moving bodies of `a` and `b` collide (symmetric).
    pub fn set_moving_collides(&mut self, a: Layer, b: Layer, collides: bool) -> &mut Self {
        if let (Some(ab), Some(ba)) = (self.index(a, b), self.index(b, a)) {
            self.moving[ab] = collides;
            self.moving[ba] = collides;
        }
        self
    }

    /// Sets whether moving bodies of `moving` collide with static bodies of `fixed`.
    pub fn set_static_collides(
        &mut self,
        moving: Layer,
        fixed: Layer,
        collides: bool,
    ) -> &mut Self {
        if let Some(i) = self.index(moving, fixed) {
            self.vs_static[i] = collides;
        }
        self
    }

    /// Whether moving bodies of `a` and `b` collide.
    pub fn moving_collides(&self, a: Layer, b: Layer) -> bool {
        self.index(a, b).is_some_and(|i| self.moving[i])
    }

    /// Whether moving bodies of `moving` collide with static bodies of `fixed`.
    pub fn static_collides(&self, moving: Layer, fixed: Layer) -> bool {
        self.index(moving, fixed).is_some_and(|i| self.vs_static[i])
    }
}

impl Default for LayerTable {
    fn default() -> Self {
        LayerTable::standard()
    }
}

/// Everything [`Physics::new`](crate::Physics::new) needs besides the worker pool.
#[derive(Clone, Debug, PartialEq)]
pub struct PhysicsConfig {
    /// Gravity in m/s², applied to dynamic bodies and characters.
    pub gravity: Vec3,
    /// The most bodies the world holds (character inner bodies included).
    pub max_bodies: u32,
    /// The most body pairs the broad phase reports per step.
    pub max_body_pairs: u32,
    /// The most contact constraints per step.
    pub max_contact_constraints: u32,
    /// Which layers collide.
    pub layers: LayerTable,
    /// The upward speed (m/s) a character gets when it jumps from the ground.
    pub character_jump_speed: f32,
    /// How hard a character pushes bodies, as an acceleration (m/s²) times its mass:
    /// a 70 kg character pushes with at most 70 × this newtons.
    pub character_push_accel: f32,
}

impl Default for PhysicsConfig {
    fn default() -> Self {
        PhysicsConfig {
            gravity: Vec3::new(0.0, -9.81, 0.0),
            max_bodies: 65_536,
            max_body_pairs: 65_536,
            max_contact_constraints: 65_536,
            layers: LayerTable::standard(),
            character_jump_speed: 5.0,
            character_push_accel: 10.0,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn standard_table_keeps_debris_off_moving_bodies() {
        let t = LayerTable::standard();
        assert_eq!(t.count(), 8);
        assert!(t.moving_collides(LayerTable::DEFAULT, LayerTable::DEFAULT));
        assert!(!t.moving_collides(LayerTable::DEBRIS, LayerTable::DEFAULT));
        assert!(!t.moving_collides(LayerTable::DEFAULT, LayerTable::DEBRIS));
        assert!(!t.moving_collides(LayerTable::DEBRIS, LayerTable::DEBRIS));
        assert!(t.static_collides(LayerTable::DEBRIS, LayerTable::DEFAULT));
        assert!(t.static_collides(LayerTable::DEFAULT, LayerTable::DEBRIS));
    }

    #[test]
    fn out_of_range_layers_never_collide() {
        let mut t = LayerTable::new(2);
        t.set_collides(Layer(0), Layer(5), true);
        assert!(!t.moving_collides(Layer(0), Layer(5)));
        assert!(!t.contains(Layer(2)));
        assert_eq!(LayerTable::new(0).count(), 1);
        assert_eq!(LayerTable::new(500).count(), LayerTable::MAX_LAYERS);
    }
}

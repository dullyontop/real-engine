//! Where the physics-relevant properties live: the fields of the classes'
//! own components (`Compat_BasePart.Size`, `Compat_JointInstance.C0`, ...),
//! looked up by name once per world (other work packages may add fields to
//! the same components, so indices are never assumed).

use glam::Vec3;
use re_physics::{PartGeom, PartShape, Pose};
use re_reflect::{ComponentKey, Value};
use re_scene::ecs::component::ComponentId;
use re_scene::{Entity, World};

use crate::class::own_component_name;
use crate::error::CompatError;

/// One field of an own component.
#[derive(Clone, Copy, Debug)]
pub struct Field {
    /// The component.
    pub key: ComponentKey,
    /// The field's index.
    pub index: usize,
}

impl Field {
    /// The field `name` of class `class`'s own component, if registered.
    pub fn find(w: &World, class: &str, name: &str) -> Option<Field> {
        let key = w.registry.try_key(&own_component_name(class))?;
        let index = w.registry.schema(key).field_index(name).ok()?;
        Some(Field { key, index })
    }

    /// The field's value on `e`.
    pub fn get(&self, w: &World, e: Entity) -> Option<Value> {
        w.registry.get_field(&w.ecs, e, self.key, self.index)
    }

    /// Writes the field on `e` (change detection sees it).
    pub fn set(&self, w: &mut World, e: Entity, v: Value) -> Result<(), CompatError> {
        Ok(w.registry
            .set_field(&mut w.ecs, e, self.key, self.index, v)?)
    }

    /// The field as a bool (`default` when missing).
    pub fn bool(&self, w: &World, e: Entity, default: bool) -> bool {
        match self.get(w, e) {
            Some(Value::Bool(b)) => b,
            _ => default,
        }
    }

    /// The field as a number.
    pub fn f32(&self, w: &World, e: Entity, default: f32) -> f32 {
        match self.get(w, e) {
            Some(Value::Float(x)) => x as f32,
            Some(Value::Int(i)) => i as f32,
            _ => default,
        }
    }

    /// The field as a vector.
    pub fn vec3(&self, w: &World, e: Entity, default: Vec3) -> Vec3 {
        match self.get(w, e) {
            Some(Value::Vec3(v)) => v,
            _ => default,
        }
    }

    /// The field as an enum item or string.
    pub fn str(&self, w: &World, e: Entity) -> Option<String> {
        match self.get(w, e) {
            Some(Value::Enum(s) | Value::Str(s)) => Some(s),
            _ => None,
        }
    }

    /// The field as an instance reference.
    pub fn entity(&self, w: &World, e: Entity) -> Option<Entity> {
        match self.get(w, e) {
            Some(Value::Entity(x)) => x,
            _ => None,
        }
    }

    /// The field as a `CFrame`.
    pub fn pose(&self, w: &World, e: Entity) -> Pose {
        self.get(w, e)
            .and_then(|v| super::pose::pose_of(&v))
            .unwrap_or(Pose::IDENTITY)
    }

    /// The component's id in the ECS.
    pub fn component(&self, w: &World) -> ComponentId {
        w.registry.component_id(self.key)
    }
}

/// The fields the physics sync reads, found once per world.
#[derive(Clone, Debug)]
pub struct Layout {
    pub size: Field,
    pub anchored: Field,
    pub can_collide: Field,
    pub can_query: Field,
    pub can_touch: Field,
    pub massless: Field,
    pub material: Field,
    pub group: Field,
    pub root_priority: Field,
    pub shape: Option<Field>,
    pub joint: JointFields,
    pub weld_constraint: WeldFields,
    pub gravity: Option<Field>,
    pub fallen_height: Option<Field>,
}

/// `JointInstance` (Weld, Motor6D, ...).
#[derive(Clone, Copy, Debug)]
pub struct JointFields {
    pub part0: Option<Field>,
    pub part1: Option<Field>,
    pub c0: Option<Field>,
    pub c1: Option<Field>,
    pub enabled: Option<Field>,
    pub transform: Option<Field>,
}

/// `WeldConstraint`.
#[derive(Clone, Copy, Debug)]
pub struct WeldFields {
    pub part0: Option<Field>,
    pub part1: Option<Field>,
    pub enabled: Option<Field>,
}

impl Layout {
    /// The layout of `w`, if the part classes are registered.
    pub fn find(w: &World) -> Option<Layout> {
        let b = |n: &str| Field::find(w, "BasePart", n);
        Some(Layout {
            size: b("Size")?,
            anchored: b("Anchored")?,
            can_collide: b("CanCollide")?,
            can_query: b("CanQuery")?,
            can_touch: b("CanTouch")?,
            massless: b("Massless")?,
            material: b("Material")?,
            group: b("CollisionGroup")?,
            root_priority: b("RootPriority")?,
            shape: Field::find(w, "Part", "Shape"),
            joint: JointFields {
                part0: Field::find(w, "JointInstance", "Part0"),
                part1: Field::find(w, "JointInstance", "Part1"),
                c0: Field::find(w, "JointInstance", "C0"),
                c1: Field::find(w, "JointInstance", "C1"),
                enabled: Field::find(w, "JointInstance", "Enabled"),
                transform: Field::find(w, "Motor6D", "Transform"),
            },
            weld_constraint: WeldFields {
                part0: Field::find(w, "WeldConstraint", "Part0"),
                part1: Field::find(w, "WeldConstraint", "Part1"),
                enabled: Field::find(w, "WeldConstraint", "Enabled"),
            },
            gravity: Field::find(w, "Workspace", "Gravity"),
            fallen_height: Field::find(w, "Workspace", "FallenPartsDestroyHeight"),
        })
    }

    /// True if `e` is a part (has `Compat_BasePart`).
    pub fn is_part(&self, w: &World, e: Entity) -> bool {
        w.registry.has(&w.ecs, e, self.size.key)
    }

    /// A part's shape and size.
    pub fn geom(&self, w: &World, e: Entity, class: &str) -> PartGeom {
        let size = self.size.vec3(w, e, Vec3::new(4.0, 1.0, 2.0));
        let shape = match class {
            "WedgePart" => PartShape::Wedge,
            "CornerWedgePart" => PartShape::CornerWedge,
            _ => match self.shape.and_then(|f| f.str(w, e)).as_deref() {
                Some("Ball") => PartShape::Ball,
                Some("Cylinder") => PartShape::Cylinder,
                Some("Wedge") => PartShape::Wedge,
                Some("CornerWedge") => PartShape::CornerWedge,
                _ => PartShape::Block,
            },
        };
        PartGeom::new(shape, size).sanitized()
    }
}

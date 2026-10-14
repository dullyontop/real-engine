//! Proves that bevy's `Component` and `Resource` derives, and
//! `re_reflect::component!` (which derives `Component`), work in a crate that
//! reaches bevy_ecs only through `use re_scene::ecs as bevy_ecs;`.
//!
//! The derives expand to a bare `bevy_ecs::...` path when the deriving crate
//! does not depend on bevy_ecs, so every module that derives needs the alias
//! in scope: here at the crate root and again in [`nested`].

#![warn(missing_docs)]

use bevy_ecs::component::Component;
use bevy_ecs::resource::Resource;
use re_reflect::component;
use re_scene::ecs as bevy_ecs;

/// A component derived at the crate root through the alias.
#[derive(Component, Clone, Copy, Debug, PartialEq)]
pub struct Health(pub f32);

/// A resource derived at the crate root through the alias.
#[derive(Resource, Clone, Copy, Debug, PartialEq, Default)]
pub struct Score(pub u32);

component! {
    /// Stamina, declared with `component!` at the crate root.
    pub struct Stamina {
        /// Current stamina.
        pub current: f32 = 10.0,
    }
}

component! {
    /// Mana, declared with `component!` at the crate root; requires [`Stamina`].
    pub struct Mana requires(Stamina) {
        /// Current mana.
        pub current: f32 = 50.0,
    }
}

/// A submodule: the alias is visible only in the module that declares it, so
/// it is declared again here.
pub mod nested {
    use bevy_ecs::component::Component;
    use bevy_ecs::resource::Resource;
    use re_reflect::component;
    use re_scene::ecs as bevy_ecs;

    /// A component derived in a submodule through the alias.
    #[derive(Component, Clone, Copy, Debug, PartialEq)]
    pub struct Armor(pub u8);

    /// A resource derived in a submodule through the alias.
    #[derive(Resource, Clone, Copy, Debug, PartialEq, Default)]
    pub struct Level(pub u8);

    component! {
        /// A shield, declared with `component!` in a submodule; requires
        /// [`Mana`](super::Mana).
        pub struct Shield requires(super::Mana) {
            /// Damage absorbed per hit.
            pub strength: i32 = 3,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::nested::{Armor, Level, Shield};
    use super::*;

    #[test]
    fn derives_through_the_alias_insert_and_query() {
        let mut world = bevy_ecs::world::World::new();
        world.insert_resource(Score(3));
        world.insert_resource(Level(2));
        let e = world.spawn((Health(10.0), Armor(4))).id();
        let mut q = world.query::<(re_scene::Entity, &Health, &Armor)>();
        let rows: Vec<_> = q.iter(&world).map(|(e, h, a)| (e, *h, *a)).collect();
        assert_eq!(rows, vec![(e, Health(10.0), Armor(4))]);
        assert_eq!(*world.resource::<Score>(), Score(3));
        assert_eq!(*world.resource::<Level>(), Level(2));

        // `component!` types: required components arrive with their defaults,
        // and the registry reads them by name.
        let mut registry = re_reflect::Registry::default();
        registry.register::<Stamina>(&mut world);
        registry.register::<Mana>(&mut world);
        registry.register::<Shield>(&mut world);
        let s = world.spawn(Shield::default()).id();
        let mut q = world.query::<(re_scene::Entity, &Shield, &Mana, &Stamina)>();
        let rows: Vec<_> = q
            .iter(&world)
            .map(|(e, sh, m, st)| (e, sh.strength, m.current, st.current))
            .collect();
        assert_eq!(rows, vec![(s, 3, 50.0, 10.0)]);
        let key = registry.key("Mana").unwrap();
        assert_eq!(
            registry.get_field(&world, s, key, 0),
            Some(re_reflect::Value::Float(50.0))
        );
    }
}

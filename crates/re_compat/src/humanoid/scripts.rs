//! The default player and character scripts, used when a project does not
//! ship its own (a script of the same name in the same container replaces
//! the default):
//!
//! | Container | Name | Class | What it does |
//! |---|---|---|---|
//! | `StarterPlayerScripts` | `PlayerModule` | `ModuleScript` | Keyboard movement into the local character's Humanoid (W A S D or arrows, relative to the camera; Space jumps and leaves seats) and the `Custom` camera (right-drag orbits, the wheel or I and O zoom); `GetControls()` and `GetCameras()` |
//! | `StarterPlayerScripts` | `PlayerScriptsLoader` | `LocalScript` | Requires `PlayerModule` |
//! | `StarterCharacterScripts` | `Animate` | `LocalScript` | Plays the idle, walk, run, jump, fall, climb and sit clips as the Humanoid's state changes |
//! | `StarterCharacterScripts` | `Health` | `Script` | Regains 1 % of `MaxHealth` per second |
//!
//! Whoever starts players and characters (WP12's `LoadCharacter`, WP16's
//! boot) installs them; this module only holds them.

/// One default script.
#[derive(Clone, Copy, Debug)]
pub struct DefaultScript {
    /// The service it sits in (`StarterPlayerScripts` or
    /// `StarterCharacterScripts`).
    pub container: &'static str,
    /// Its instance name.
    pub name: &'static str,
    /// `LocalScript`, `ModuleScript` or `Script`.
    pub class: &'static str,
    /// Its Luau source.
    pub source: &'static str,
}

/// The default player and character scripts.
pub const DEFAULT_SCRIPTS: &[DefaultScript] = &[
    DefaultScript {
        container: "StarterPlayerScripts",
        name: "PlayerModule",
        class: "ModuleScript",
        source: include_str!("scripts/PlayerModule.luau"),
    },
    DefaultScript {
        container: "StarterPlayerScripts",
        name: "PlayerScriptsLoader",
        class: "LocalScript",
        source: include_str!("scripts/PlayerScriptsLoader.luau"),
    },
    DefaultScript {
        container: "StarterCharacterScripts",
        name: "Animate",
        class: "LocalScript",
        source: include_str!("scripts/Animate.luau"),
    },
    DefaultScript {
        container: "StarterCharacterScripts",
        name: "Health",
        class: "Script",
        source: include_str!("scripts/Health.luau"),
    },
];

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_scripts_compile() {
        let lua = mlua::Lua::new();
        for s in DEFAULT_SCRIPTS {
            if let Err(e) = lua.load(s.source).set_name(s.name).into_function() {
                panic!("{}: {e}", s.name);
            }
        }
    }
}

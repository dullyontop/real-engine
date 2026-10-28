//! The selection (ADR-0044's amendment for WP42, §4): an ordered set of
//! entity ids with an active one, the one selected last. The Inspector shows
//! the active entity; the gizmo moves them all.
//!
//! It is editor state: never a command, never in the undo history or the
//! World. The layout saves it (`selection`), and the editor drops the ids
//! whose entities no longer exist at the start of each frame.

use re_scene::World;
use re_scene::hierarchy::{children, parent};
use serde::{Deserialize, Serialize};

/// Selected entities' ids, in the order they were selected, and the active
/// one. The active entity is always one of the ids, and is `None` only when
/// nothing is selected.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, from = "Saved")]
pub struct Selection {
    ids: Vec<String>,
    active: Option<String>,
}

/// A selection as a file holds it, which may break the rules
/// [`Selection`] keeps.
#[derive(Default, Deserialize)]
#[serde(default)]
struct Saved {
    ids: Vec<String>,
    active: Option<String>,
}

impl From<Saved> for Selection {
    fn from(saved: Saved) -> Selection {
        Selection::from_ids(saved.ids, saved.active)
    }
}

impl Selection {
    /// Nothing selected.
    pub fn new() -> Selection {
        Selection::default()
    }

    /// Only `id`, active.
    pub fn one(id: &str) -> Selection {
        Selection {
            ids: vec![id.to_string()],
            active: Some(id.to_string()),
        }
    }

    /// `ids` in order, without repeats, with `active` active when it is one
    /// of them (else the last).
    pub fn from_ids(ids: impl IntoIterator<Item = String>, active: Option<String>) -> Selection {
        let mut out = Selection::new();
        for id in ids {
            if !out.contains(&id) {
                out.ids.push(id);
            }
        }
        out.active = active
            .filter(|a| out.contains(a))
            .or_else(|| out.ids.last().cloned());
        out
    }

    /// The selected ids, in the order they were selected.
    pub fn ids(&self) -> &[String] {
        &self.ids
    }

    /// The active entity's id: the one the Inspector shows.
    pub fn active(&self) -> Option<&str> {
        self.active.as_deref()
    }

    /// How many entities are selected.
    pub fn len(&self) -> usize {
        self.ids.len()
    }

    /// Nothing selected.
    pub fn is_empty(&self) -> bool {
        self.ids.is_empty()
    }

    /// Whether `id` is selected.
    pub fn contains(&self, id: &str) -> bool {
        self.ids.iter().any(|i| i == id)
    }

    /// Selects nothing.
    pub fn clear(&mut self) {
        self.ids.clear();
        self.active = None;
    }

    /// Adds `id` (Shift+click) and makes it active.
    pub fn add(&mut self, id: &str) {
        if !self.contains(id) {
            self.ids.push(id.to_string());
        }
        self.active = Some(id.to_string());
    }

    /// Removes `id`; when it was active, the last remaining one becomes
    /// active.
    pub fn remove(&mut self, id: &str) {
        self.retain(|i| i != id);
    }

    /// Ctrl+click: removes `id` when it is selected, else adds it.
    pub fn toggle(&mut self, id: &str) {
        if self.contains(id) {
            self.remove(id);
        } else {
            self.add(id);
        }
    }

    /// Keeps the ids `keep` accepts; when the active one goes, the last
    /// remaining one becomes active.
    pub fn retain(&mut self, mut keep: impl FnMut(&str) -> bool) {
        self.ids.retain(|i| keep(i));
        if self.active.as_ref().is_none_or(|a| !self.contains(a)) {
            self.active = self.ids.last().cloned();
        }
    }

    /// The selection after a click on `id` in the viewport, with `modifiers`
    /// held: Ctrl (Cmd on macOS) toggles it, Shift adds it, and a plain
    /// click selects it alone.
    pub fn clicked(&self, id: &str, modifiers: egui::Modifiers) -> Selection {
        let mut next = self.clone();
        if modifiers.command {
            next.toggle(id);
        } else if modifiers.shift {
            next.add(id);
        } else {
            next = Selection::one(id);
        }
        next
    }

    /// The selected entities that have no selected ancestor, in selection
    /// order: the ones the gizmo moves and Delete removes (the others move
    /// and go with them). Ids without an entity in `world` are left out.
    pub fn topmost(&self, world: &World) -> Vec<String> {
        self.ids
            .iter()
            .filter(|id| {
                let Ok(mut e) = world.entity(id) else {
                    return false;
                };
                while let Some(p) = parent(&world.ecs, e) {
                    if world.id_of(p).is_some_and(|pid| self.contains(pid)) {
                        return false;
                    }
                    e = p;
                }
                true
            })
            .cloned()
            .collect()
    }

    /// Select parent (Ctrl+Up): each selected entity's parent in its place
    /// (a root stays), the active one's parent active.
    pub fn parents(&self, world: &World) -> Selection {
        let up = |id: &str| -> String {
            world
                .entity(id)
                .ok()
                .and_then(|e| parent(&world.ecs, e))
                .and_then(|p| world.id_of(p))
                .map_or_else(|| id.to_string(), str::to_string)
        };
        Selection::from_ids(
            self.ids.iter().map(|id| up(id)),
            self.active.as_deref().map(up),
        )
    }

    /// Select children (Ctrl+Down): each selected entity's children in its
    /// place (an entity without children stays), the active one's first
    /// child active.
    pub fn children(&self, world: &World) -> Selection {
        let down = |id: &str| -> Vec<String> {
            let kids: Vec<String> = world
                .entity(id)
                .ok()
                .map(|e| {
                    children(&world.ecs, e)
                        .iter()
                        .filter_map(|k| world.id_of(*k).map(str::to_string))
                        .collect()
                })
                .unwrap_or_default();
            if kids.is_empty() {
                vec![id.to_string()]
            } else {
                kids
            }
        };
        let active = self
            .active
            .as_deref()
            .and_then(|a| down(a).into_iter().next());
        Selection::from_ids(self.ids.iter().flat_map(|id| down(id)), active)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ids(s: &Selection) -> Vec<&str> {
        s.ids().iter().map(String::as_str).collect()
    }

    #[test]
    fn adding_removing_and_toggling_keep_an_active_entity() {
        let mut s = Selection::one("a");
        s.add("b");
        s.add("c");
        assert_eq!(ids(&s), ["a", "b", "c"]);
        assert_eq!(s.active(), Some("c"));
        // Adding one already selected makes it active, without a repeat.
        s.add("a");
        assert_eq!((ids(&s), s.active()), (vec!["a", "b", "c"], Some("a")));
        // Removing the active one falls back to the last remaining one.
        s.remove("a");
        assert_eq!((ids(&s), s.active()), (vec!["b", "c"], Some("c")));
        s.toggle("b");
        assert_eq!((ids(&s), s.active()), (vec!["c"], Some("c")));
        s.toggle("d");
        assert_eq!((ids(&s), s.active()), (vec!["c", "d"], Some("d")));
        s.retain(|id| id == "c");
        assert_eq!((ids(&s), s.active()), (vec!["c"], Some("c")));
        s.clear();
        assert!(s.is_empty() && s.active().is_none());
        s.toggle("e");
        assert_eq!(s, Selection::one("e"));
    }

    #[test]
    fn a_click_selects_one_ctrl_toggles_and_shift_adds() {
        use egui::Modifiers;
        let s = Selection::from_ids(["a".to_string(), "b".to_string()], Some("a".into()));
        assert_eq!(s.clicked("c", Modifiers::NONE), Selection::one("c"));
        let next = s.clicked("c", Modifiers::SHIFT);
        assert_eq!(
            (ids(&next), next.active()),
            (vec!["a", "b", "c"], Some("c"))
        );
        // Shift on a selected entity makes it active.
        let next = s.clicked("b", Modifiers::SHIFT);
        assert_eq!((ids(&next), next.active()), (vec!["a", "b"], Some("b")));
        let next = s.clicked("a", Modifiers::COMMAND);
        assert_eq!((ids(&next), next.active()), (vec!["b"], Some("b")));
        // Ctrl with Shift toggles too.
        let next = s.clicked("b", Modifiers::COMMAND | Modifiers::SHIFT);
        assert_eq!((ids(&next), next.active()), (vec!["a"], Some("a")));
    }

    #[test]
    fn a_saved_selection_is_put_right_when_read() {
        let read = |text: &str| serde_json::from_str::<Selection>(text).unwrap();
        let s = read(r#"{"ids": ["a", "b", "a"], "active": "x"}"#);
        assert_eq!((ids(&s), s.active()), (vec!["a", "b"], Some("b")));
        let s = read(r#"{"active": "a"}"#);
        assert!(s.is_empty() && s.active().is_none());
        let s = read(r#"{"ids": ["a", "b"], "active": "a"}"#);
        assert_eq!(s.active(), Some("a"));
        let text = serde_json::to_string(&s).unwrap();
        assert_eq!(read(&text), s);
    }
}

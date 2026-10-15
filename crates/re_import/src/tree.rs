//! The instance tree an input declares, before it meets a project: what both
//! readers ([`loader`](crate::loader), [`rojo`](crate::rojo)) produce and
//! [`apply`](crate::apply) syncs into the scene.

use re_core::Diagnostic;

use crate::report::Unrecognised;

/// How an instance of the input finds its counterpart in the project.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    /// A service: found by its class (its stable id), created only if the
    /// class layer knows the service.
    Service,
    /// A container a path implies: the first child with its name, of any
    /// class; created with [`Node::class`] when there is none.
    Container,
    /// An instance the input declares: the k-th child with its name and
    /// class, where k counts the input's earlier siblings with both.
    Declared,
}

/// One instance.
#[derive(Clone, Debug)]
pub struct Node {
    pub name: String,
    pub class: String,
    pub kind: Kind,
    pub parent: Option<usize>,
    pub children: Vec<usize>,
    /// The script text, for a script.
    pub source: Option<String>,
    /// Where the input declares it (1-based), if it is a text file.
    pub line: Option<u32>,
    /// The input file that declares it, when it is not the input itself.
    pub file: Option<String>,
    /// Tags to add (`CollectionService`).
    pub tags: Vec<String>,
    /// Tags to add only when the import creates the instance.
    pub tags_if_created: Vec<String>,
    /// Property values to set through the class layer, from JSON.
    pub properties: Vec<(String, serde_json::Value)>,
    /// Attributes to set, from JSON.
    pub attributes: Vec<(String, serde_json::Value)>,
    /// Destroyed by a later statement of the input itself.
    pub destroyed: bool,
}

/// Which instances the importer created earlier may be removed when the input
/// no longer declares them.
#[derive(Clone, Debug)]
pub enum StaleScope {
    /// Scripts anywhere (a loader's script table).
    Scripts,
    /// Every child of `parent` named `name`, and their descendants (a
    /// loader's replace-by-name helper).
    Named { parent: usize, name: String },
    /// The descendants of a node (a Rojo `$path`).
    Subtree(usize),
}

/// What a reader produced.
#[derive(Debug, Default)]
pub struct Tree {
    pub nodes: Vec<Node>,
    /// The services, in the order the input first names them.
    pub services: Vec<usize>,
    pub scopes: Vec<StaleScope>,
    /// False when the input asks to keep stale scripts.
    pub remove_stale: bool,
    pub unrecognised: Vec<Unrecognised>,
    pub warnings: Vec<Diagnostic>,
}

impl Tree {
    pub fn new() -> Self {
        Tree {
            remove_stale: true,
            ..Tree::default()
        }
    }

    fn push(&mut self, node: Node) -> usize {
        self.nodes.push(node);
        self.nodes.len() - 1
    }

    /// A new instance, not yet parented.
    pub fn instance(&mut self, class: &str, name: &str, kind: Kind, line: Option<u32>) -> usize {
        self.push(Node {
            name: name.to_string(),
            class: class.to_string(),
            kind,
            parent: None,
            children: Vec::new(),
            source: None,
            line,
            file: None,
            tags: Vec::new(),
            tags_if_created: Vec::new(),
            properties: Vec::new(),
            attributes: Vec::new(),
            destroyed: false,
        })
    }

    /// The service `class`, created on first use.
    pub fn service(&mut self, class: &str, line: Option<u32>) -> usize {
        if let Some(&i) = self
            .services
            .iter()
            .find(|&&i| self.nodes[i].class == class)
        {
            return i;
        }
        let i = self.instance(class, class, Kind::Service, line);
        self.services.push(i);
        i
    }

    /// Sets the parent: the node becomes the parent's last child.
    pub fn set_parent(&mut self, node: usize, parent: Option<usize>) {
        if let Some(old) = self.nodes[node].parent {
            self.nodes[old].children.retain(|&c| c != node);
        }
        self.nodes[node].parent = parent;
        if let Some(p) = parent {
            self.nodes[p].children.push(node);
        }
    }

    /// The first live child of `parent` named `name`.
    pub fn find_child(&self, parent: usize, name: &str) -> Option<usize> {
        self.nodes[parent]
            .children
            .iter()
            .copied()
            .find(|&c| !self.nodes[c].destroyed && self.nodes[c].name == name)
    }

    /// The child of `parent` named `name`, or a new container of `class`.
    pub fn container(
        &mut self,
        parent: usize,
        name: &str,
        class: &str,
        line: Option<u32>,
    ) -> usize {
        if let Some(c) = self.find_child(parent, name) {
            return c;
        }
        let c = self.instance(class, name, Kind::Container, line);
        self.set_parent(c, Some(parent));
        c
    }

    /// Destroys a node and its descendants.
    pub fn destroy(&mut self, node: usize) {
        self.nodes[node].destroyed = true;
        for c in self.nodes[node].children.clone() {
            self.destroy(c);
        }
    }

    /// The live children of a node, in order.
    pub fn live_children(&self, node: usize) -> impl Iterator<Item = usize> + '_ {
        self.nodes[node]
            .children
            .iter()
            .copied()
            .filter(|&c| !self.nodes[c].destroyed)
    }

    /// The names from the service down.
    pub fn path(&self, node: usize) -> Vec<String> {
        let mut out = Vec::new();
        let mut at = Some(node);
        while let Some(i) = at {
            out.push(self.nodes[i].name.clone());
            at = self.nodes[i].parent;
        }
        out.reverse();
        out
    }
}

/// The class a container gets when a path implies it and the project has no
/// child of that name: the classes Roblox puts at those places, else `Folder`.
pub fn container_class(parent_class: &str, name: &str) -> &'static str {
    match (parent_class, name) {
        ("StarterPlayer", "StarterPlayerScripts") => "StarterPlayerScripts",
        ("StarterPlayer", "StarterCharacterScripts") => "StarterCharacterScripts",
        // A character is a model; a rig to fill it comes from the project.
        ("StarterPlayer", "StarterCharacter") => "Model",
        _ => "Folder",
    }
}

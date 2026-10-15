//! Read-only reference material for agents (docs/M2_PLAN.md, WP17): the
//! explain pages, the engine's documents, and the Luau definition files.
//!
//! `re_mcp` serves a [`ResourceSet`] as MCP resources (`resources/list`,
//! `resources/read`). Clients without resources reach the same text through
//! the `resource.list` and `resource.read` commands that [`register_commands`]
//! adds, so nothing is reachable only one way.
//!
//! URIs:
//!
//! ```text
//! real://explain/RE8001          one explain page
//! real://docs/ARCHITECTURE.md    a document under docs/
//! real://luau/engine.d.luau      the engine's Luau definitions
//! ```

use std::sync::Arc;

use re_command::{CommandRegistry, EditTarget, Tier};
use re_core::{DiagInfo, Diagnostic, Location};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::json;

use crate::codes;

include!(concat!(env!("OUT_DIR"), "/docs.rs"));

/// The URI scheme of every resource.
pub const SCHEME: &str = "real://";

/// One resource: a named text.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct Resource {
    /// The URI (`real://explain/RE8001`).
    pub uri: String,
    /// A short name (`RE8001`, `ARCHITECTURE.md`).
    pub name: String,
    /// One line saying what it is.
    pub description: String,
    /// `text/markdown` or `text/x-luau`.
    pub mime_type: String,
    /// The contents.
    #[serde(skip)]
    #[schemars(skip)]
    pub text: Arc<str>,
}

/// Every resource, in URI order.
#[derive(Clone, Debug, Default)]
pub struct ResourceSet {
    items: Vec<Resource>,
}

impl ResourceSet {
    /// An empty set.
    pub fn new() -> ResourceSet {
        ResourceSet::default()
    }

    /// The standard set: the explain pages of `catalog`, every engine
    /// document, and the Luau definition files given as `(file name, text)`
    /// (`engine.d.luau`).
    pub fn standard(catalog: &[&DiagInfo], definitions: &[(&str, String)]) -> ResourceSet {
        let mut set = ResourceSet::new();
        for info in catalog {
            set.add(
                format!("{SCHEME}explain/{}", info.code),
                info.code.to_string(),
                format!("Explain page for {}: {}", info.code, info.title),
                "text/markdown",
                info.page,
            );
        }
        for (path, text) in DOCS {
            set.add(
                format!("{SCHEME}docs/{path}"),
                (*path).to_string(),
                format!("Engine document docs/{path}"),
                "text/markdown",
                text,
            );
        }
        for (name, text) in definitions {
            set.add(
                format!("{SCHEME}luau/{name}"),
                (*name).to_string(),
                format!("Luau type definitions ({name}) for scripts and `luau-lsp`"),
                "text/x-luau",
                text.as_str(),
            );
        }
        set
    }

    /// Adds (or replaces) a resource and keeps the set in URI order.
    pub fn add(
        &mut self,
        uri: String,
        name: String,
        description: String,
        mime_type: &str,
        text: &str,
    ) {
        self.items.retain(|r| r.uri != uri);
        self.items.push(Resource {
            uri,
            name,
            description,
            mime_type: mime_type.to_string(),
            text: Arc::from(text),
        });
        self.items.sort_by(|a, b| a.uri.cmp(&b.uri));
    }

    /// Every resource.
    pub fn list(&self) -> &[Resource] {
        &self.items
    }

    /// The resource at `uri`. An unknown URI is `RE8014`, with the closest
    /// known one as a suggestion.
    pub fn read(&self, uri: &str) -> Result<&Resource, Diagnostic> {
        if let Some(r) = self.items.iter().find(|r| r.uri == uri) {
            return Ok(r);
        }
        let suggestion = re_reflect::suggest(uri, self.items.iter().map(|r| r.uri.as_str()));
        let message = match suggestion {
            Some(s) => format!("no resource {uri:?} (did you mean {s:?}?)"),
            None => format!("no resource {uri:?}"),
        };
        Err(Diagnostic::error(codes::UNKNOWN_RESOURCE, message)
            .at(Location::command("resource.read"))
            .with_hint("list the resources with `resource.list`"))
    }
}

/// Arguments of `resource.list`.
#[derive(Debug, Default, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ListArgs {
    /// Only resources whose URI starts with this (`real://docs/`).
    #[serde(default)]
    pub prefix: Option<String>,
}

/// The result of `resource.list`.
#[derive(Debug, Serialize, JsonSchema)]
pub struct ListOutput {
    /// The matching resources, without their text.
    pub resources: Vec<Resource>,
}

/// Arguments of `resource.read`.
#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ReadArgs {
    /// The resource's URI (`real://explain/RE8001`, `real://docs/ROADMAP.md`,
    /// `real://luau/engine.d.luau`).
    pub uri: String,
}

/// The result of `resource.read`.
#[derive(Debug, Serialize, JsonSchema)]
pub struct ReadOutput {
    /// The URI read.
    pub uri: String,
    /// Its media type.
    pub mime_type: String,
    /// The contents.
    pub text: String,
}

/// Registers `resource.list` and `resource.read` (both read tier) over
/// `set`.
pub fn register_commands<C: EditTarget + 'static>(
    registry: &mut CommandRegistry<C>,
    set: Arc<ResourceSet>,
) {
    let s = Arc::clone(&set);
    registry.register_typed_output::<ListArgs, ListOutput, _>(
        "resource.list",
        "List the reference material agents can read: explain pages, engine documents and Luau definitions (the same items MCP serves as resources).",
        Tier::Read,
        true,
        move |_target, args, _call| {
            let resources: Vec<&Resource> = s
                .list()
                .iter()
                .filter(|r| args.prefix.as_deref().is_none_or(|p| r.uri.starts_with(p)))
                .collect();
            Ok(json!({ "resources": resources }))
        },
    );
    registry.register_typed_output::<ReadArgs, ReadOutput, _>(
        "resource.read",
        "Read one reference item by URI (an explain page, an engine document or a Luau definitions file).",
        Tier::Read,
        true,
        move |_target, args, _call| {
            let r = set.read(&args.uri)?;
            Ok(json!({ "uri": r.uri, "mime_type": r.mime_type, "text": &*r.text }))
        },
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_documents_are_embedded() {
        let names: Vec<&str> = DOCS.iter().map(|(p, _)| *p).collect();
        for want in ["ARCHITECTURE.md", "ROADMAP.md", "M2_PLAN.md"] {
            assert!(names.contains(&want), "{want} missing from {names:?}");
        }
        assert!(DOCS.iter().all(|(_, t)| !t.is_empty()));
    }

    #[test]
    fn unknown_uris_suggest_the_closest() {
        let set = ResourceSet::standard(&[], &[("engine.d.luau", "x".to_string())]);
        let err = set.read("real://luau/engine.d.lua").unwrap_err();
        assert_eq!(err.code, codes::UNKNOWN_RESOURCE);
        assert!(err.message.contains("real://luau/engine.d.luau"), "{err:?}");
        assert_eq!(&*set.read("real://luau/engine.d.luau").unwrap().text, "x");
    }
}

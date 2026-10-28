//! `re_command` tests: the files commands name (ADR-0020's amendment for
//! WP44): `x-real-path` declarations in input schemas, [`PathArg::values`],
//! and [`Call::check_write`] asking the dispatching policy.

use re_command::{
    AllowAll, CommandRegistry, CommandSpec, EditState, PathAccess, PathArg, PolicyCheck, Tier,
    declared_paths, register_builtin_commands,
};
use re_core::Diagnostic;
use schemars::JsonSchema;
use serde::Deserialize;
use serde_json::{Value as Json, json};

fn arg(pointer: &str, access: PathAccess) -> PathArg {
    PathArg {
        pointer: pointer.into(),
        access,
    }
}

#[test]
fn scene_apply_declares_the_documents_set_doc_writes() {
    let mut reg: CommandRegistry<EditState> = CommandRegistry::new();
    register_builtin_commands(&mut reg);
    assert_eq!(
        reg.spec("scene.apply").unwrap().paths,
        vec![arg("/ops/*/doc", PathAccess::Write)]
    );
    // No other built-in command names a file.
    for spec in reg.specs().iter().filter(|s| s.name != "scene.apply") {
        assert!(spec.paths.is_empty(), "{}: {:?}", spec.name, spec.paths);
    }
}

/// An argument type using every shape the walk follows.
#[derive(Deserialize, JsonSchema)]
#[expect(dead_code, reason = "only the schema is used")]
struct Args {
    /// A file the command writes, optional.
    #[serde(default)]
    #[schemars(extend("x-real-path" = "write"))]
    out: Option<String>,
    /// Files it reads.
    #[schemars(extend("x-real-path" = "read"))]
    inputs: Vec<String>,
    /// A nested type, through `$ref`.
    nested: Nested,
    /// Steps of a tagged enum (`oneOf` branches in `$defs`).
    steps: Vec<Step>,
    /// A map whose values are images.
    #[serde(default)]
    shots: std::collections::BTreeMap<String, Shot>,
    /// A mistyped declaration.
    #[serde(default)]
    #[schemars(extend("x-real-path" = "wrtie"))]
    typo: Option<String>,
    /// Not a file.
    name: String,
}

#[derive(Deserialize, JsonSchema)]
#[expect(dead_code, reason = "only the schema is used")]
struct Nested {
    /// An image.
    #[schemars(extend("x-real-path" = "output"))]
    image: String,
}

#[derive(Deserialize, JsonSchema)]
#[serde(tag = "op", rename_all = "snake_case")]
#[expect(dead_code, reason = "only the schema is used")]
enum Step {
    Save {
        /// Written.
        #[schemars(extend("x-real-path" = "write"))]
        file: String,
    },
    Wait {
        /// Frames.
        frames: u32,
    },
}

#[derive(Deserialize, JsonSchema)]
#[expect(dead_code, reason = "only the schema is used")]
struct Shot {
    /// The PNG.
    #[schemars(extend("x-real-path" = "output"))]
    png: String,
}

#[test]
fn typed_arguments_declare_their_paths() {
    let mut reg: CommandRegistry<EditState> = CommandRegistry::new();
    reg.register_typed::<Args, _>("files", "Names files.", Tier::Edit, false, |_t, _a, _c| {
        Ok(Json::Null)
    });
    let mut paths = reg.spec("files").unwrap().paths.clone();
    paths.sort_by(|a, b| a.pointer.cmp(&b.pointer));
    assert_eq!(
        paths,
        vec![
            arg("/inputs", PathAccess::Read),
            arg("/nested/image", PathAccess::Output),
            arg("/out", PathAccess::Write),
            arg("/shots/*/png", PathAccess::Output),
            arg("/steps/*/file", PathAccess::Write),
            // An unknown access fails closed.
            arg("/typo", PathAccess::Write),
        ]
    );
    // `dry_run`, which the registry adds, is not a path.
    assert!(paths.iter().all(|p| p.pointer != "/dry_run"));
}

#[test]
fn raw_specs_take_their_paths_from_the_schema_unless_they_declare_some() {
    let spec = |paths: Vec<PathArg>| CommandSpec {
        name: "tool".into(),
        summary: String::new(),
        tier: Tier::Read,
        input_schema: json!({
            "type": "object",
            "properties": {
                "target": { "type": "string", "x-real-path": "write" },
                "list": {
                    "type": "array",
                    "items": { "$ref": "#/$defs/Item" }
                }
            },
            "$defs": {
                "Item": {
                    "anyOf": [
                        { "type": "object", "properties": { "doc": { "type": "string", "x-real-path": "read" } } },
                        { "type": "object", "properties": { "next": { "$ref": "#/$defs/Item" } } }
                    ]
                }
            }
        }),
        annotations: re_command::CommandAnnotations::for_tier(Tier::Read, true),
        output_schema: None,
        paths,
    };
    let mut reg: CommandRegistry<EditState> = CommandRegistry::new();
    reg.register(
        spec(Vec::new()),
        std::sync::Arc::new(|_t, _a, _c| Ok(Json::Null)),
    );
    let mut paths = reg.spec("tool").unwrap().paths.clone();
    paths.sort_by(|a, b| a.pointer.cmp(&b.pointer));
    // Below the first level of a recursive type, every string is declared.
    assert_eq!(
        paths,
        vec![
            arg("/list/*/doc", PathAccess::Read),
            arg("/list/*/next/**", PathAccess::Read),
            arg("/target", PathAccess::Write),
        ]
    );
    let declared = vec![arg("/other", PathAccess::Output)];
    reg.register(
        spec(declared.clone()),
        std::sync::Arc::new(|_t, _a, _c| Ok(Json::Null)),
    );
    assert_eq!(reg.spec("tool").unwrap().paths, declared);
    assert_eq!(
        declared_paths(&json!({ "properties": { "a": { "x-real-path": "output" } } })),
        vec![arg("/a", PathAccess::Output)]
    );
    // A recursive type that writes somewhere below: every string below its
    // first level is checked as a write, the strictest access it declares.
    let tree = json!({
        "$ref": "#/$defs/Node",
        "$defs": {
            "Node": {
                "type": "object",
                "properties": {
                    "image": { "type": "string", "x-real-path": "output" },
                    "children": { "type": "array", "items": { "$ref": "#/$defs/Node" } },
                    "meta": { "$ref": "#/$defs/Meta" }
                }
            },
            "Meta": {
                "type": "object",
                "properties": { "file": { "type": "string", "x-real-path": "write" } }
            }
        }
    });
    let mut paths = declared_paths(&tree);
    paths.sort_by(|a, b| a.pointer.cmp(&b.pointer));
    assert_eq!(
        paths,
        vec![
            arg("/children/*/**", PathAccess::Write),
            arg("/image", PathAccess::Output),
            arg("/meta/file", PathAccess::Write),
        ]
    );
}

#[test]
fn values_follow_the_pointer() {
    let args = json!({
        "path": "a.png",
        "paths": ["b", 3, "c"],
        "ops": [
            { "op": "set_doc", "doc": "graphs/x.graph.json" },
            { "op": "spawn" },
            { "op": "set_doc", "doc": null },
            { "op": "set_doc", "doc": "graphs/y.graph.json" }
        ],
        "map": { "one": { "png": "1.png" }, "two": { "png": "2.png" } },
        "a/b": { "c~d": "escaped" }
    });
    let values = |pointer: &str| arg(pointer, PathAccess::Write).values(&args);
    assert_eq!(values("/path"), vec!["a.png"]);
    // An array of strings at the pointer: each string.
    assert_eq!(values("/paths"), vec!["b", "c"]);
    assert_eq!(values("/paths/*"), vec!["b", "c"]);
    assert_eq!(values("/paths/2"), vec!["c"]);
    assert_eq!(
        values("/ops/*/doc"),
        vec!["graphs/x.graph.json", "graphs/y.graph.json"]
    );
    assert_eq!(values("/map/*/png"), vec!["1.png", "2.png"]);
    assert_eq!(values("/a~1b/c~0d"), vec!["escaped"]);
    assert_eq!(values("/map/**"), vec!["1.png", "2.png"]);
    assert_eq!(values("/ops/1/**"), vec!["spawn"]);
    assert!(values("/missing").is_empty());
    assert!(values("/path/deeper").is_empty());
    assert!(
        arg("/path", PathAccess::Write)
            .values(&Json::Null)
            .is_empty()
    );
}

#[test]
fn paths_are_serialized_only_when_declared() {
    let mut reg: CommandRegistry<EditState> = CommandRegistry::new();
    register_builtin_commands(&mut reg);
    let plain = serde_json::to_value(reg.spec("entity.spawn").unwrap()).unwrap();
    assert!(plain.get("paths").is_none());
    let back: CommandSpec = serde_json::from_value(plain).unwrap();
    assert!(back.paths.is_empty());
    let apply = serde_json::to_value(reg.spec("scene.apply").unwrap()).unwrap();
    assert_eq!(
        apply["paths"],
        json!([{ "pointer": "/ops/*/doc", "access": "write" }])
    );
    let back: CommandSpec = serde_json::from_value(apply).unwrap();
    assert_eq!(back.paths, vec![arg("/ops/*/doc", PathAccess::Write)]);
}

/// Refuses writes under `secret/`, and records what it was asked.
#[derive(Default)]
struct NoSecrets {
    asked: std::sync::Mutex<Vec<(String, String)>>,
}

impl PolicyCheck for NoSecrets {
    fn check(&self, _spec: &CommandSpec, _args: &Json, _dry_run: bool) -> Result<(), Diagnostic> {
        Ok(())
    }
    fn check_write(&self, command: &str, path: &str) -> Result<(), Diagnostic> {
        self.asked
            .lock()
            .unwrap()
            .push((command.to_string(), path.to_string()));
        if path.starts_with("secret/") {
            return Err(Diagnostic::error(
                re_core::Code::new("RE8002"),
                format!("{command} may not write {path}"),
            ));
        }
        Ok(())
    }
}

#[test]
fn handlers_ask_the_dispatching_policy_before_writing() {
    #[derive(Deserialize, JsonSchema)]
    struct WriteArgs {
        /// A name; the command derives the file from it.
        name: String,
    }
    let mut reg: CommandRegistry<EditState> = CommandRegistry::new();
    reg.register_typed::<WriteArgs, _>(
        "derive",
        "Writes a file it names itself.",
        Tier::Edit,
        false,
        |_t, a, call| {
            call.check_write(&format!("{}/file.txt", a.name))?;
            Ok(json!({ "would_write": !call.dry_run() }))
        },
    );
    let mut s = EditState::new();
    let policy = NoSecrets::default();
    let out = reg.dispatch(
        &mut s,
        "derive",
        json!({ "name": "secret" }),
        false,
        &policy,
    );
    assert!(!out.ok);
    assert_eq!(out.diagnostics[0].code.to_string(), "RE8002");
    let out = reg.dispatch(
        &mut s,
        "derive",
        json!({ "name": "public" }),
        false,
        &policy,
    );
    assert!(out.ok, "{:?}", out.diagnostics);
    assert_eq!(
        *policy.asked.lock().unwrap(),
        vec![
            ("derive".to_string(), "secret/file.txt".to_string()),
            ("derive".to_string(), "public/file.txt".to_string()),
        ]
    );
    // A dry run writes nothing, so nothing is asked.
    let out = reg.dispatch(&mut s, "derive", json!({ "name": "secret" }), true, &policy);
    assert!(out.ok, "{:?}", out.diagnostics);
    assert_eq!(policy.asked.lock().unwrap().len(), 2);
    // A policy that does not say otherwise allows every write.
    let out = reg.dispatch(
        &mut s,
        "derive",
        json!({ "name": "secret" }),
        false,
        &AllowAll,
    );
    assert!(out.ok, "{:?}", out.diagnostics);
}

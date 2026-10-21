//! `real graph eval`: evaluate a ProcGraph file and print its report.
//! `real migrate-graph`: upgrade every graph's nodes to their kinds' current
//! versions (ADR-0043).

use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::sync::Arc;

use re_core::{Diagnostic, Location};
use re_proc::{EvalOptions, Evaluator, NodeRegistry, ProcGraph};
use re_types::graph::GraphDoc;

/// `real graph` subcommands.
#[derive(clap::Subcommand, Debug)]
pub enum GraphCommand {
    /// Evaluate a graph file (`graphs/island.graph.json`) and print what each
    /// node did: cache hits, times, output sizes and diagnostics.
    Eval {
        /// The graph file (relative to the working directory, else to the
        /// project).
        file: PathBuf,
        /// The seed (default: the graph's own).
        #[arg(long)]
        seed: Option<u64>,
        /// Print the JSON report.
        #[arg(long)]
        json: bool,
    },
}

fn invalid(file: &Path, message: String) -> Diagnostic {
    Diagnostic::error(re_proc::codes::INVALID_GRAPH, message)
        .at(Location::file(file.display().to_string()))
}

/// A graph that could not be evaluated: its diagnostics, as JSON with
/// `--json` (so tools always get one document), exit 1.
fn refuse(json: bool, diagnostics: Vec<Diagnostic>) -> ExitCode {
    if json {
        println!(
            "{}",
            serde_json::json!({ "ok": false, "diagnostics": diagnostics })
        );
    } else {
        crate::print_diagnostics(&diagnostics);
    }
    ExitCode::from(1)
}

/// Runs a `real graph` subcommand; prefab paths in the graph are relative to
/// `project`, and so is `file` when the working directory has no such file.
pub fn run(command: GraphCommand, project: &Path) -> ExitCode {
    let GraphCommand::Eval { file, seed, json } = command;
    let file = if file.is_relative() && !file.exists() && project.join(&file).exists() {
        project.join(&file)
    } else {
        file
    };
    let text = match std::fs::read_to_string(&file) {
        Ok(t) => t,
        Err(e) => {
            let d = invalid(&file, format!("cannot read {}: {e}", file.display()));
            return refuse(json, vec![d]);
        }
    };
    let doc = match GraphDoc::from_json(&text) {
        Ok(d) => d,
        Err(e) => {
            let d = invalid(
                &file,
                format!("{} is not a valid graph: {e}", file.display()),
            );
            return refuse(json, vec![d]);
        }
    };
    // The project's Luau kinds too (`scripts/generators/`).
    let limits = re_app::AppConfig::load(project)
        .map(|c| c.proc.limits())
        .unwrap_or_default();
    let m1 = NodeRegistry::m1();
    let declared =
        re_script::generator::declare(&re_core::files::ProjectFiles::project(project), &m1, limits);
    let registry = Arc::new(declared.registry(&m1));
    let graph = match ProcGraph::from_doc(&doc, &registry) {
        Ok(g) => g,
        Err(mut ds) => {
            // A kind whose declaration failed is unknown: say why.
            ds.extend(declared.diagnostics);
            return refuse(json, ds);
        }
    };
    let evaluator = Evaluator::new(registry);
    let evaluation = evaluator.evaluate(
        &graph,
        &EvalOptions {
            seed,
            root: Some(project.to_path_buf()),
            ..Default::default()
        },
    );
    let mut report = evaluation.report.clone();
    let assembled = evaluation
        .fragment()
        .and_then(|f| re_proc::to_scene_fragment(&doc.name, &f, &re_scene::World::new().registry));
    match assembled {
        Ok(fragment) => {
            report.fragment_hash = Some(re_proc::fragment_hash_hex(&fragment));
            report.entities = Some(fragment.len());
        }
        Err(d) if report.ok() => report.diagnostics.push(d),
        Err(_) => {}
    }
    let ok = report.ok();
    if json {
        println!(
            "{}",
            serde_json::to_string_pretty(&report).unwrap_or_default()
        );
    } else {
        for node in &report.nodes {
            println!(
                "{:<16} {:<22} {:>8.2} ms  {:?}",
                node.id, node.kind, node.ms, node.cache
            );
            crate::print_diagnostics(&node.diagnostics);
            for line in &node.logs {
                println!("  log: {line}");
            }
        }
        crate::print_diagnostics(&report.diagnostics);
        println!(
            "graph {}: seed {}, {} node{} ({} computed), {} entities, {:.1} ms: {}",
            report.graph,
            report.seed,
            report.nodes.len(),
            if report.nodes.len() == 1 { "" } else { "s" },
            report.misses,
            report.entities.map_or("no".to_string(), |n| n.to_string()),
            report.ms,
            if ok { "ok" } else { "FAILED" }
        );
    }
    if ok {
        ExitCode::SUCCESS
    } else {
        ExitCode::from(1)
    }
}

/// Arguments of `real migrate-graph`.
#[derive(clap::Args, Debug, Default)]
pub struct MigrateArgs {
    /// Print what would change and write nothing.
    #[arg(long)]
    pub dry_run: bool,
    /// Print the JSON report.
    #[arg(long)]
    pub json: bool,
}

/// Runs `real migrate-graph`: opens the project, reports what upgrading each
/// procedural source's nodes changes (`graph.migration_report`, plus graph
/// files no open scene reads), and unless `--dry-run`, rewrites the graph
/// files. All or nothing: when any graph cannot migrate, nothing is written.
/// Builder scripts are never rewritten; the report names their nodes.
pub fn migrate(args: MigrateArgs, project: &Path) -> ExitCode {
    let app = match re_app::App::open_project(project) {
        Ok(app) => app,
        Err(d) => return refuse(args.json, vec![d]),
    };
    let world = app.world();
    let mut report = re_proc::migrate::migration_report(world, Some(project));
    let m1 = NodeRegistry::m1();
    let registry = world
        .ecs
        .get_resource::<re_proc::GraphStore>()
        .map_or(&m1, |s| s.evaluator().registry());
    let files = re_app::verify::content_files(project, "graphs", ".graph.json");
    re_proc::migrate::add_unopened_files(&mut report, project, registry, &files);
    let written = if args.dry_run || !report.ok {
        Vec::new()
    } else {
        match re_proc::migrate::write_files(&report, project) {
            Ok(w) => w,
            Err(e) => {
                let d = Diagnostic::error(
                    crate::codes::WRITE_FAILED,
                    format!("cannot write a migrated graph: {e}"),
                );
                return refuse(args.json, vec![d]);
            }
        }
    };
    let diags: Vec<Diagnostic> = report.all_diagnostics().cloned().collect();
    if args.json {
        let mut value = serde_json::to_value(&report).unwrap_or_default();
        value["written"] = serde_json::json!(written);
        value["dry_run"] = serde_json::json!(args.dry_run);
        println!(
            "{}",
            serde_json::to_string_pretty(&value).unwrap_or_default()
        );
    } else {
        crate::print_diagnostics(&diags);
        for s in &report.sources {
            let from = s.file.as_deref().or(s.builder.as_deref()).unwrap_or("");
            println!(
                "{} ({from}): {} node{}{}",
                s.source,
                s.nodes.len(),
                if s.nodes.len() == 1 { "" } else { "s" },
                if s.output_changed {
                    ", output changes"
                } else {
                    ", same output"
                }
            );
            for n in &s.nodes {
                println!("  {} {} v{} -> v{}", n.id, n.kind, n.from, n.to);
                for note in &n.notes {
                    println!("    {note}");
                }
                if s.builder.is_some() {
                    println!("    in the builder, write \"{}@{}\"", n.kind, n.to);
                }
            }
            for k in &s.keys_added {
                println!("  + {k}");
            }
            for k in &s.keys_removed {
                println!("  - {k}");
            }
            for o in &s.overrides_orphaned {
                println!("  override {o} no longer applies");
            }
        }
        for f in &report.unopened {
            println!(
                "{}: {} node{}",
                f.path,
                f.nodes.len(),
                if f.nodes.len() == 1 { "" } else { "s" }
            );
        }
        let verb = if args.dry_run {
            "would rewrite"
        } else {
            "rewrote"
        };
        let count = if args.dry_run {
            report.files.len()
        } else {
            written.len()
        };
        println!(
            "migrate-graph: {verb} {count} file{}: {}",
            if count == 1 { "" } else { "s" },
            if report.ok {
                "ok"
            } else {
                "FAILED, nothing written"
            }
        );
    }
    if report.ok {
        ExitCode::SUCCESS
    } else {
        ExitCode::from(1)
    }
}

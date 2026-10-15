//! `real graph eval`: evaluate a ProcGraph file and print its report.

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
    let registry = Arc::new(NodeRegistry::m1());
    let graph = match ProcGraph::from_doc(&doc, &registry) {
        Ok(g) => g,
        Err(ds) => return refuse(json, ds),
    };
    let evaluator = Evaluator::new(registry);
    let evaluation = evaluator.evaluate(
        &graph,
        &EvalOptions {
            seed,
            root: Some(project.to_path_buf()),
            deadline: None,
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

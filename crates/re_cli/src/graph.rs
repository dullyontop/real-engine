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
        /// The graph file.
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

/// Runs a `real graph` subcommand; prefab paths in the graph are relative to
/// `project`.
pub fn run(command: GraphCommand, project: &Path) -> ExitCode {
    let GraphCommand::Eval { file, seed, json } = command;
    let text = match std::fs::read_to_string(&file) {
        Ok(t) => t,
        Err(e) => {
            return crate::fail(invalid(
                &file,
                format!("cannot read {}: {e}", file.display()),
            ));
        }
    };
    let doc = match GraphDoc::from_json(&text) {
        Ok(d) => d,
        Err(e) => {
            return crate::fail(invalid(
                &file,
                format!("{} is not a valid graph: {e}", file.display()),
            ));
        }
    };
    let registry = Arc::new(NodeRegistry::m1());
    let graph = match ProcGraph::from_doc(&doc, &registry) {
        Ok(g) => g,
        Err(ds) => {
            crate::print_diagnostics(&ds);
            return ExitCode::from(1);
        }
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

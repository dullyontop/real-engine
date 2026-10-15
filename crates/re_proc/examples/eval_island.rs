//! Evaluates `examples/island.graph.json`, the M1 island built from the 16 M1
//! node kinds, and writes its `EvalReport`.
//!
//! ```sh
//! cargo run -p re_proc --release --example eval_island -- --seed 1234 --report out/eval.json
//! ```
//!
//! Options: `--seed N` (default: the graph's), `--report PATH` (the report as
//! JSON), `--cache DIR` (a disk cache; none by default), `--threads N`
//! (worker threads; default: cores minus one) and `--twice` (evaluate again
//! to show the memory cache). Prefab paths in the graph are relative to this
//! `examples/` directory, which stands in for a project root.

use std::path::PathBuf;
use std::process::ExitCode;
use std::sync::Arc;

use re_core::TaskPool;
use re_proc::{EvalOptions, Evaluator, NodeRegistry, ProcGraph, fragment_hash_hex};
use re_types::graph::GraphDoc;

struct Args {
    seed: Option<u64>,
    report: Option<PathBuf>,
    cache: Option<PathBuf>,
    threads: usize,
    twice: bool,
}

fn parse_args() -> Result<Args, String> {
    let mut args = Args {
        seed: None,
        report: None,
        cache: None,
        threads: 0,
        twice: false,
    };
    let mut it = std::env::args().skip(1);
    while let Some(a) = it.next() {
        let mut value = |name: &str| it.next().ok_or_else(|| format!("{name} needs a value"));
        match a.as_str() {
            "--seed" => {
                let v = value("--seed")?;
                let seed: i64 = v
                    .parse()
                    .map_err(|_| format!("--seed: `{v}` is not an integer"))?;
                args.seed = Some(seed as u64);
            }
            "--report" => args.report = Some(value("--report")?.into()),
            "--cache" => args.cache = Some(value("--cache")?.into()),
            "--threads" => {
                let v = value("--threads")?;
                args.threads = v
                    .parse()
                    .map_err(|_| format!("--threads: `{v}` is not a number"))?;
            }
            "--twice" => args.twice = true,
            "-h" | "--help" => {
                return Err("usage: eval_island [--seed N] [--report PATH] [--cache DIR] [--threads N] [--twice]".into());
            }
            other => return Err(format!("unknown option `{other}` (see --help)")),
        }
    }
    Ok(args)
}

fn main() -> ExitCode {
    let args = match parse_args() {
        Ok(a) => a,
        Err(e) => {
            eprintln!("eval_island: {e}");
            return ExitCode::from(2);
        }
    };
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("examples");
    let text = include_str!("island.graph.json");
    let doc = match GraphDoc::from_json(text) {
        Ok(d) => d,
        Err(e) => {
            eprintln!("eval_island: {e}");
            return ExitCode::FAILURE;
        }
    };
    let registry = Arc::new(NodeRegistry::m1());
    let graph = match ProcGraph::from_doc(&doc, &registry) {
        Ok(g) => g,
        Err(problems) => {
            eprint!("{}", re_core::diag::render_human_all(&problems));
            return ExitCode::FAILURE;
        }
    };
    let pool = match TaskPool::new(args.threads) {
        Ok(p) => Arc::new(p),
        Err(d) => {
            eprintln!("{}", d.render_human());
            return ExitCode::FAILURE;
        }
    };
    let mut evaluator = Evaluator::new(registry).with_pool(pool);
    if let Some(dir) = &args.cache {
        evaluator = evaluator.with_disk_cache(dir.clone());
    }
    let opts = EvalOptions {
        seed: args.seed,
        root: Some(root),
        deadline: None,
    };
    let world = re_scene::World::new();
    let runs = if args.twice { 2 } else { 1 };
    let mut report = None;
    for _ in 0..runs {
        let evaluation = evaluator.evaluate(&graph, &opts);
        let mut r = evaluation.report.clone();
        match evaluation.scene_fragment(&world.registry) {
            Ok((fragment, assets)) => {
                r.fragment_hash = Some(fragment_hash_hex(&fragment));
                r.entities = Some(fragment.len());
                println!(
                    "eval_island: seed {} on {} threads: {} nodes ({} computed, {} from memory, {} from disk), {} entities, {} derived assets, {:.1} ms, fragment {}",
                    r.seed,
                    r.threads,
                    r.nodes.len(),
                    r.misses,
                    r.memory_hits,
                    r.disk_hits,
                    fragment.len(),
                    assets.len(),
                    r.ms,
                    r.fragment_hash.as_deref().unwrap_or("-"),
                );
            }
            Err(d) => {
                eprintln!("{}", d.render_human());
                return ExitCode::FAILURE;
            }
        }
        report = Some(r);
    }
    if let (Some(path), Some(report)) = (&args.report, &report) {
        if let Some(dir) = path.parent().filter(|d| !d.as_os_str().is_empty())
            && let Err(e) = std::fs::create_dir_all(dir)
        {
            eprintln!("eval_island: cannot create {}: {e}", dir.display());
            return ExitCode::FAILURE;
        }
        let json = serde_json::to_string_pretty(report).expect("an EvalReport serializes");
        if let Err(e) = std::fs::write(path, json + "\n") {
            eprintln!("eval_island: cannot write {}: {e}", path.display());
            return ExitCode::FAILURE;
        }
        println!("eval_island: report written to {}", path.display());
    }
    ExitCode::SUCCESS
}

//! The script debugger on the runtime's contexts (ADR-0045): the scheduler
//! parks the thread a breakpoint stops while the rest of the context runs,
//! the chunks of entry scripts and modules reach the debugger whether they
//! loaded before or after it attached, and a stop ends when its thread is
//! dropped.

mod common;

use common::{Rig, STEP};
use re_script::ContextKind;
use re_script::debug::{Debugger, Event, Resume, StopReason, service};

fn rig(files: &[(&str, &str)]) -> (tempfile::TempDir, Rig) {
    let dir = tempfile::tempdir().unwrap();
    for (path, source) in files {
        let full = dir.path().join(path);
        std::fs::create_dir_all(full.parent().unwrap()).unwrap();
        std::fs::write(full, source).unwrap();
    }
    let rig = Rig::at(dir.path());
    (dir, rig)
}

const LOOP: &str = "local n = 0
while true do
\tn += 1
\tprint(\"loop\", n)
\ttask.wait()
end
";

const TICKER: &str = "local lib = require(\"./lib\")
time.on_frame(function()
\tprint(lib.label())
end)
";

const LIB: &str = "local M = {}
function M.label()
\treturn \"tick\"
end
return M
";

const WORKER: &str = "local frames = 0
local worker = task.spawn(function()
\twhile true do
\t\ttask.wait()
\t\tprint(\"work\")
\tend
end)
time.on_frame(function()
\tframes += 1
\tif frames == 3 then
\t\ttask.cancel(worker)
\t\tprint(\"cancelled\")
\tend
end)
";

const QUITTER: &str = "task.spawn(function()
\ttask.wait()
\ttask.cancel(coroutine.running())
\tprint(\"after\")
\ttask.wait()
\tprint(\"never\")
end)
";

fn frame(rig: &mut Rig) {
    service(rig.rt.context(ContextKind::Game).lua());
    let d = rig.step(STEP);
    assert!(d.is_empty(), "{d:#?}");
}

#[test]
fn a_stopped_script_thread_parks_while_the_context_keeps_running() {
    let (_dir, mut rig) = rig(&[
        ("scripts/loop.luau", LOOP),
        ("scripts/ticker.luau", TICKER),
        ("scripts/lib.luau", LIB),
    ]);
    // Both scripts load before the debugger attaches.
    assert!(rig.run("scripts/loop.luau").is_empty());
    assert!(rig.run("scripts/ticker.luau").is_empty());
    assert_eq!(rig.output(), ["loop\t1"]);
    let debugger = Debugger::new();
    let info = rig
        .rt
        .context(ContextKind::Game)
        .attach_debugger(&debugger)
        .unwrap();
    assert_eq!(info.name, "game");
    let placed = debugger.set_breakpoints("scripts/loop.luau", &[3]);
    assert_eq!(placed.len(), 1);

    // The loop stops at line 3 and parks; the frame callback keeps running.
    frame(&mut rig);
    let stop = debugger.stopped().expect("the loop stopped");
    assert_eq!(stop.context, info.id);
    assert_eq!(stop.top().unwrap().line, Some(3));
    assert_eq!(rig.output(), ["tick"]);
    frame(&mut rig);
    assert_eq!(rig.output(), ["tick"], "the loop stays parked");

    // Continue: it runs on with the host bound and stops again a frame later.
    assert!(debugger.resume(Resume::Continue));
    frame(&mut rig);
    let out = rig.output();
    assert!(out.contains(&"loop\t2".to_string()), "{out:?}");
    debugger.set_breakpoints("scripts/loop.luau", &[]);
    if debugger.stopped().is_some() {
        assert!(debugger.resume(Resume::Continue));
    }
    frame(&mut rig);
    frame(&mut rig);
    assert!(debugger.stopped().is_none());
    assert!(rig.output().iter().any(|l| l.starts_with("loop\t")));
}

#[test]
fn a_stop_in_a_project_run_shows_its_locals() {
    // A development run compiles its scripts at debug level 2 (ADR-0045), so a
    // stop carries the names of the locals in scope.
    let (_dir, mut rig) = rig(&[("scripts/loop.luau", LOOP)]);
    assert!(rig.run("scripts/loop.luau").is_empty());
    let debugger = Debugger::new();
    rig.rt
        .context(ContextKind::Game)
        .attach_debugger(&debugger)
        .unwrap();
    debugger.set_breakpoints("scripts/loop.luau", &[4]);
    frame(&mut rig);
    let stop = debugger.stopped().expect("the loop stopped");
    assert_eq!(stop.top().unwrap().line, Some(4));
    let n = stop.local("n").expect("the local `n` is shown");
    assert_eq!((n.value.as_str(), n.type_name.as_str()), ("2", "number"));
    debugger.set_breakpoints("scripts/loop.luau", &[]);
    assert!(debugger.resume(Resume::Continue));
    frame(&mut rig);
}

#[test]
fn modules_loaded_after_attaching_reach_breakpoints() {
    let (_dir, mut rig) = rig(&[("scripts/ticker.luau", TICKER), ("scripts/lib.luau", LIB)]);
    let debugger = Debugger::new();
    rig.rt
        .context(ContextKind::Game)
        .attach_debugger(&debugger)
        .unwrap();
    debugger.set_breakpoints("scripts/lib.luau", &[3]);
    assert!(rig.run("scripts/ticker.luau").is_empty());
    frame(&mut rig);
    let stop = debugger.stopped().expect("stopped in the module");
    assert_eq!(
        stop.top().unwrap().source.as_deref(),
        Some("scripts/lib.luau")
    );
    assert!(debugger.resume(Resume::Continue));
    debugger.set_breakpoints("scripts/lib.luau", &[]);
    frame(&mut rig);
    assert!(rig.output().contains(&"tick".to_string()));
}

#[test]
fn a_script_stopped_while_parked_is_dropped_with_its_run() {
    let (dir, mut rig) = rig(&[("scripts/loop.luau", LOOP)]);
    let debugger = Debugger::new();
    let info = rig
        .rt
        .context(ContextKind::Game)
        .attach_debugger(&debugger)
        .unwrap();
    assert!(rig.run("scripts/loop.luau").is_empty());
    debugger.set_breakpoints("scripts/loop.luau", &[3]);
    frame(&mut rig);
    assert!(debugger.stopped().is_some());
    rig.output();
    debugger.take_events();

    // A reload stops the parked run, which ends its stop, and starts a new
    // run, which stops at the same breakpoint before its first print.
    std::fs::write(
        dir.path().join("scripts/loop.luau"),
        LOOP.replace("\"loop\"", "\"again\""),
    )
    .unwrap();
    let Rig {
        rt,
        world,
        services,
    } = &mut rig;
    let mut host = re_script::WorldHost { world, services };
    let report = rt.reload(&mut host, "scripts/loop.luau");
    assert!(report.diagnostics.is_empty(), "{:#?}", report.diagnostics);
    assert_eq!(report.restarted, 1);
    assert_eq!(
        stops(&debugger),
        [
            Event::Continued { context: info.id },
            Event::Stopped {
                context: info.id,
                reason: StopReason::Breakpoint
            },
        ]
    );
    let stop = debugger.stopped().expect("the new run stopped");
    assert_eq!(stop.top().unwrap().line, Some(3));
    assert!(rig.output().is_empty(), "it stopped before its first print");

    // Continued, the new run goes on.
    debugger.set_breakpoints("scripts/loop.luau", &[]);
    assert!(debugger.resume(Resume::Continue));
    frame(&mut rig);
    frame(&mut rig);
    let out = rig.output();
    assert_eq!(out.first().map(String::as_str), Some("again\t1"), "{out:?}");
    assert!(out.iter().all(|l| l.starts_with("again\t")), "{out:?}");
}

#[test]
fn a_script_thread_cancelled_while_parked_ends_its_stop() {
    let (_dir, mut rig) = rig(&[("scripts/worker.luau", WORKER)]);
    let debugger = Debugger::new();
    let info = rig
        .rt
        .context(ContextKind::Game)
        .attach_debugger(&debugger)
        .unwrap();
    debugger.set_breakpoints("scripts/worker.luau", &[5]);
    assert!(rig.run("scripts/worker.luau").is_empty());
    frame(&mut rig);
    let stop = debugger.stopped().expect("the worker stopped");
    assert_eq!(stop.top().unwrap().line, Some(5));
    debugger.take_events();

    // The frame callback cancels the parked worker, and its stop ends.
    for _ in 0..10 {
        if rig.output().contains(&"cancelled".to_string()) {
            break;
        }
        assert!(debugger.stopped().is_some(), "the worker stays parked");
        frame(&mut rig);
    }
    assert!(debugger.stopped().is_none());
    assert_eq!(stops(&debugger), [Event::Continued { context: info.id }]);
    // There is nothing left to resume, and the worker never runs again.
    assert!(!debugger.resume(Resume::Continue));
    frame(&mut rig);
    frame(&mut rig);
    assert!(rig.output().is_empty());
}

#[test]
fn a_thread_that_stops_after_cancelling_itself_ends_its_stop() {
    let (_dir, mut rig) = rig(&[("scripts/quitter.luau", QUITTER)]);
    let debugger = Debugger::new();
    let info = rig
        .rt
        .context(ContextKind::Game)
        .attach_debugger(&debugger)
        .unwrap();
    debugger.set_breakpoints("scripts/quitter.luau", &[4]);
    assert!(rig.run("scripts/quitter.luau").is_empty());

    // It stops at line 4 and parks there, which is the yield that ends a
    // cancelled thread: the scheduler drops it, and its stop ends at once.
    frame(&mut rig);
    assert!(debugger.stopped().is_none());
    assert_eq!(
        stops(&debugger),
        [
            Event::Stopped {
                context: info.id,
                reason: StopReason::Breakpoint
            },
            Event::Continued { context: info.id },
        ]
    );
    frame(&mut rig);
    frame(&mut rig);
    assert!(rig.output().is_empty());
}

/// The stop and continue events so far, taken.
fn stops(debugger: &Debugger) -> Vec<Event> {
    debugger
        .take_events()
        .into_iter()
        .filter(|e| matches!(e, Event::Stopped { .. } | Event::Continued { .. }))
        .collect()
}

//! `examples/plaza`, the public Roblox-style example, played through the
//! runtime: the shop, the door and the coins, through its test hooks.

use std::path::{Path, PathBuf};

use re_app::{App, AppConfig};
use serde_json::{Value, json};

fn plaza() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../examples/plaza")
}

fn open() -> App {
    let dir = plaza();
    let config = AppConfig {
        headless: true,
        persist_edits: false,
        ..AppConfig::load(&dir).unwrap()
    };
    App::open(&dir, config).unwrap()
}

fn hook(name: &str, args: Value) -> Value {
    json!({ "invoke": { "path": format!("ServerStorage.TestHooks.{name}"), "args": args } })
}

/// Plays `steps` after 30 frames and returns each step's `results`.
fn play(steps: Value) -> Vec<Value> {
    let mut app = open();
    let steps = serde_json::from_value::<Vec<re_app::play::PlayStep>>(steps).unwrap();
    let (report, results, _) = app.play_copy(30, None, &steps, false).unwrap();
    assert!(report.ok(), "{:?}", report.diagnostics);
    assert!(
        report
            .output
            .iter()
            .any(|l| l.contains("plaza: ready with 12 coins"))
    );
    results.into_iter().map(|r| r["results"].clone()).collect()
}

#[test]
fn the_plaza_places_every_coin() {
    let r = play(json!([hook("CoinParts", json!([]))]));
    assert_eq!(r[0], json!([12]));
}

#[test]
fn the_shop_takes_coins_and_the_door_slides_up() {
    let r = play(json!([
        hook("DoorHeight", json!([])),
        hook("Buy", json!(["Player1", "door"])),
        hook("Give", json!(["Player1", 4])),
        hook("Buy", json!(["Player1", "speed"])),
        hook("Buy", json!(["Player1", "door"])),
        hook("Buy", json!(["Player1", "door"])),
        { "frames": 120 },
        hook("DoorHeight", json!([])),
        hook("Coins", json!(["Player1"])),
        hook("Give", json!(["Player1", 10])),
        hook("Buy", json!(["Player1", "speed"])),
        hook("Buy", json!(["Player1", "speed"])),
    ]));
    assert_eq!(r[0], json!([5]));
    assert_eq!(r[1], json!([false, "not enough coins"]));
    assert_eq!(r[2], json!([4]));
    assert_eq!(r[3], json!([false, "not enough coins"]));
    assert_eq!(r[4], json!([true, 1]));
    assert_eq!(r[5], json!([false, "not enough coins"]));
    let height = r[7][0].as_f64().unwrap();
    assert!((height - 14.0).abs() < 1e-3, "{height}");
    assert_eq!(r[8], json!([1]));
    assert_eq!(r[10], json!([true, 6]));
    assert_eq!(r[11], json!([false, "already bought"]));
}

#[test]
fn a_screenshot_draws_the_parts_and_the_coin_counter() {
    let mut app = open();
    app.run_headless(Some(90), None).unwrap();
    let image = app.screenshot(None).unwrap();
    assert!(image.stats([0; 4]).distinct_colors > 50);
    // The counter's dark panel (top left) over the sky; the green ground
    // below the horizon.
    let [r, g, b, _] = image.pixel(24, 24);
    assert!(r < 140 && g < 140 && b < 160, "panel {r} {g} {b}");
    let [r, g, b, _] = image.pixel(40, 600);
    assert!(g > r && g > b, "ground {r} {g} {b}");
}

#[test]
fn a_restart_draws_the_plaza_again() {
    let mut app = open();
    app.run_headless(Some(30), None).unwrap();
    let first = app.screenshot(None).unwrap();
    app.restart();
    app.run_headless(Some(30), None).unwrap();
    let second = app.screenshot(None).unwrap();
    assert_eq!(first.rgba, second.rgba);
}

#[test]
fn a_play_copy_draws_with_the_lent_renderer_and_gives_it_back() {
    let mut app = open();
    app.run_headless(Some(30), None).unwrap();
    let live = app.screenshot(None).unwrap();
    let (_, _, image) = app.play_copy(30, None, &[], true).unwrap();
    assert!(image.unwrap().stats([0; 4]).distinct_colors > 50);
    assert_eq!(app.screenshot(None).unwrap().rgba, live.rgba);
}

#[test]
fn text_a_play_copy_laid_out_first_still_reaches_the_window() {
    // The copy runs the egui context's first pass and draws nothing, so the
    // font atlas it made must go to the live view.
    let mut app = open();
    app.ensure_graphics().unwrap();
    let steps = serde_json::from_value::<Vec<re_app::play::PlayStep>>(json!([
        hook("Buy", json!(["Player1", "door"])),
        { "frames": 5 },
    ]))
    .unwrap();
    app.play_copy(30, None, &steps, false).unwrap();
    app.run_headless(Some(30), None).unwrap();
    let after_copy = app.screenshot(None).unwrap();

    let mut fresh = open();
    fresh.run_headless(Some(30), None).unwrap();
    assert_eq!(after_copy.rgba, fresh.screenshot(None).unwrap().rgba);
}

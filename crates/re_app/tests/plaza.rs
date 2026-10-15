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
}

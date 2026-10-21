//! `AnimGraph` file round trips and unknown-key refusal.

use re_types::anim::{AnimGraph, CompareOp, Condition, NodeKind, ParamValue};

const WALK: &str = r#"{
  "format": "real-engine/animgraph", "version": 1,
  "params": [
    { "id": "speed", "type": "float", "default": 0.5 },
    { "id": "grounded", "type": "bool", "default": true },
    { "id": "jump", "type": "trigger" }
  ],
  "nodes": [
    { "id": "idle", "kind": "clip", "clip": "assets/hero.glb#idle" },
    { "id": "run", "kind": "clip", "clip": "assets/hero.glb#run", "speed": 1.5, "root_motion": true },
    { "id": "move", "kind": "blend1d", "param": "speed",
      "children": [{ "node": "idle", "threshold": 0.0 }, { "node": "run", "threshold": 4.0 }] },
    { "id": "aim", "kind": "blend2d", "x": "speed", "y": "speed", "sync": false,
      "children": [{ "node": "idle", "point": [0.0, 0.0] }] },
    { "id": "lean", "kind": "additive", "base": "move", "additive": "idle", "weight_param": "speed" }
  ],
  "layers": [
    { "id": "base", "entry": "ground",
      "states": [{ "id": "ground", "node": "move" }, { "id": "air", "node": "idle" }],
      "transitions": [
        { "id": "up", "from": "any", "to": "air", "duration": 0.2,
          "conditions": [{ "test": "trigger", "param": "jump" }] },
        { "id": "down", "from": "air", "to": "ground", "duration": 0.1, "exit_time": 0.9,
          "interruptible": true,
          "conditions": [{ "test": "bool", "param": "grounded", "value": true },
                         { "test": "compare", "param": "speed", "op": ">=", "value": 0.1 }] }
      ] },
    { "id": "upper", "blend": "additive", "weight": 0.5, "mask": "arms", "entry": "a",
      "states": [{ "id": "a", "node": "lean" }] }
  ],
  "masks": [{ "id": "arms", "joints": [{ "joint": "spine", "subtree": true }, { "joint": "head", "weight": 0.0 }] }]
}"#;

#[test]
fn a_graph_round_trips() {
    let g: AnimGraph = serde_json::from_str(WALK).unwrap();
    assert_eq!(g.params[0].value, ParamValue::Float { default: 0.5 });
    assert_eq!(g.params[2].value, ParamValue::Trigger);
    assert!(matches!(
        &g.node("run").unwrap().kind,
        NodeKind::Clip { speed, looping: true, root_motion: true, .. } if *speed == 1.5
    ));
    assert!(matches!(&g.node("aim").unwrap().kind, NodeKind::Blend2d { sync: false, .. }));
    let down = &g.layers[0].transitions[1];
    assert_eq!(down.exit_time, Some(0.9));
    assert!(down.interruptible);
    assert_eq!(
        down.conditions[1],
        Condition::Compare { param: "speed".into(), op: CompareOp::Ge, value: 0.1 }
    );
    assert!(g.mask("arms").unwrap().joints[0].subtree);

    let text = serde_json::to_string(&g).unwrap();
    assert!(!text.contains("looping"), "defaults are not written: {text}");
    let back: AnimGraph = serde_json::from_str(&text).unwrap();
    assert_eq!(back, g);
}

#[test]
fn unknown_keys_are_refused() {
    for (from, to) in [
        (r#""format""#, r#""extra": 1, "format""#),
        (r#""id": "idle", "kind""#, r#""id": "idle", "colour": 1, "kind""#),
        (r#""threshold": 4.0"#, r#""threshold": 4.0, "w": 1"#),
        (r#""id": "up", "from""#, r#""id": "up", "blend": 1, "from""#),
        (r#""test": "trigger", "param": "jump""#, r#""test": "trigger", "param": "jump", "value": 1"#),
        (r#""id": "jump", "type": "trigger""#, r#""id": "jump", "type": "trigger", "default": 1"#),
        (r#""kind": "clip", "clip": "assets/hero.glb#idle""#, r#""kind": "clip", "clip": "assets/hero.glb#idle", "param": "x""#),
    ] {
        assert_eq!(WALK.matches(from).count(), 1, "{from}");
        let bad = WALK.replace(from, to);
        assert!(serde_json::from_str::<AnimGraph>(&bad).is_err(), "accepted {to}");
    }
}

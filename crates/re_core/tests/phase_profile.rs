//! `Phase` order and the default (empty) `profile_scope!`.

use re_core::Phase;

#[test]
fn phases_follow_adr_0009() {
    let names: Vec<&str> = Phase::ALL.iter().map(|p| p.as_str()).collect();
    assert_eq!(
        names,
        [
            "Input",
            "PreRender",
            "PreAnimation",
            "PreSimulation",
            "Physics",
            "PostSimulation",
            "LateFrame",
            "Sync",
            "Render",
            "EndFrame"
        ]
    );
    assert!(Phase::ALL.windows(2).all(|w| w[0] < w[1]));
    let fixed: Vec<Phase> = Phase::ALL
        .into_iter()
        .filter(|p| p.is_fixed_step())
        .collect();
    assert_eq!(fixed, Phase::FIXED);
    assert_eq!(
        serde_json::to_string(&Phase::PostSimulation).unwrap(),
        r#""PostSimulation""#
    );
}

fn profiled_work(x: u64) -> u64 {
    re_core::profile_scope!("profiled_work");
    x * 2
}

#[test]
fn profile_scope_compiles_and_costs_nothing_by_default() {
    re_core::profile::enable();
    assert_eq!(profiled_work(21), 42);
    re_core::profile::finish_frame();
    assert_eq!(
        re_core::profile::is_compiled_in(),
        cfg!(any(feature = "profile-puffin", feature = "profile-tracy"))
    );
}

//! The spike's twelfth scenario, alone in its process so other tests' allocations do
//! not disturb the RSS readings.

mod common;

use common::*;
use re_physics::PhysicsConfig;

/// Creating, stepping and dropping worlds repeatedly does not grow memory (Linux RSS).
#[test]
fn create_drop_cycles() {
    let rss_kib = || -> u64 {
        std::fs::read_to_string("/proc/self/statm")
            .ok()
            .and_then(|s| s.split_whitespace().nth(1)?.parse::<u64>().ok())
            .map_or(0, |pages| pages * 4)
    };
    let pool = pool(2);
    let mut first = 0;
    for i in 0..60 {
        let mut p = re_physics::Physics::new(PhysicsConfig::default(), pool).unwrap();
        apply(&mut p, pile(200));
        steps(&mut p, 10);
        drop(p);
        if i == 9 {
            first = rss_kib();
        }
    }
    let last = rss_kib();
    assert!(
        last <= first + 8 * 1024,
        "RSS grew from {first} KiB to {last} KiB"
    );
}

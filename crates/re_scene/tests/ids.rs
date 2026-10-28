//! Engine-made ids in play (ADR-0036 amendment): the session stream, its
//! state, how `World::new_id` and `World::fork` use it, and that it stays out
//! of the state hash.

use indexmap::IndexMap;
use re_scene::{EntityDoc, IdGen, World};

fn ids(g: &mut IdGen, n: usize) -> Vec<String> {
    (0..n).map(|_| g.next_id()).collect()
}

fn entity(id: &str) -> EntityDoc {
    EntityDoc {
        id: id.to_string(),
        name: String::new(),
        note: String::new(),
        parent: None,
        components: IndexMap::new(),
        overrides: Vec::new(),
    }
}

#[test]
fn a_session_seed_gives_one_sequence() {
    let a = ids(&mut IdGen::session(7), 8);
    assert_eq!(a, ids(&mut IdGen::session(7), 8));
    assert_ne!(a, ids(&mut IdGen::session(8), 8));
    assert_ne!(
        ids(&mut IdGen::session(-1), 8),
        ids(&mut IdGen::session(1), 8)
    );
    // The salt keeps a session apart from a stream seeded with the same
    // number (compat's streams and `math.random` start from other salts).
    assert_ne!(a, ids(&mut IdGen::seeded(7), 8));
}

#[test]
fn session_ids_are_the_same_on_every_platform() {
    // SplitMix64 is integer arithmetic; these are the first ids of two
    // seeds, worked out independently of this crate.
    assert_eq!(
        ids(&mut IdGen::session(0), 3),
        ["epk7tzt4qkc", "envsr0kjret", "e06xqrgm84k"]
    );
    assert_eq!(
        ids(&mut IdGen::session(42), 3),
        ["ezffqaargzq", "ecn51havmd9", "ej2rtfhfdak"]
    );
}

#[test]
fn the_state_continues_the_stream() {
    let mut g = IdGen::session(42);
    let _ = ids(&mut g, 3);
    let mut resumed = IdGen::seeded(g.state());
    assert_eq!(ids(&mut resumed, 5), ids(&mut g, 5));
}

#[test]
fn new_id_draws_from_the_world_generator_and_skips_taken_ids() {
    let mut expected = IdGen::session(5);
    let first = ids(&mut expected, 3);
    let mut w = World::new();
    w.set_id_gen(IdGen::session(5));
    // An entity already holds the stream's first id.
    w.spawn_docs(&[entity(&first[0])], None).unwrap();
    assert_eq!(w.new_id(), first[1]);
    assert_eq!(w.new_id(), first[2]);
    assert_eq!(w.id_gen().state(), expected.state());
}

#[test]
fn a_fork_continues_its_original_sequence() {
    let mut w = World::new();
    w.set_id_gen(IdGen::session(9));
    let _ = w.new_id();
    let mut fork = w.fork();
    assert_eq!(fork.id_gen().state(), w.id_gen().state());
    assert_eq!(fork.new_id(), w.new_id());
}

#[test]
fn two_runs_that_spawn_without_ids_hash_the_same() {
    let run = |seed: i64| {
        let mut w = World::new();
        w.set_id_gen(IdGen::session(seed));
        for _ in 0..10 {
            let id = w.new_id();
            w.spawn_docs(&[entity(&id)], None).unwrap();
        }
        w.state_hash()
    };
    assert_eq!(run(3), run(3));
    assert_ne!(run(3), run(4));
}

#[test]
fn the_generator_is_not_hashed() {
    let mut a = World::new();
    let mut b = World::new();
    a.set_id_gen(IdGen::session(1));
    b.set_id_gen(IdGen::session(2));
    assert_eq!(a.state_hash(), b.state_hash());
}

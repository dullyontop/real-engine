//! `Events<T>`: double buffering and per-reader cursors.

use re_core::{EventCursor, Events};

#[test]
fn events_live_for_two_updates() {
    let mut events = Events::new();
    events.send(1);
    events.update();
    assert_eq!(events.len(), 1, "still readable in the next frame");
    events.update();
    assert!(events.is_empty(), "gone after the second update");
}

#[test]
fn a_reader_before_the_sender_sees_each_event_once() {
    let mut events = Events::new();
    let mut early = EventCursor::default(); // runs before the sender each frame
    let mut late = EventCursor::default(); // runs after the sender each frame
    let mut seen_early = Vec::new();
    let mut seen_late = Vec::new();
    for frame in 0..5 {
        seen_early.extend(early.read(&events).copied());
        events.send(frame * 10);
        events.send(frame * 10 + 1);
        seen_late.extend(late.read(&events).copied());
        events.update();
    }
    seen_early.extend(early.read(&events).copied());
    let all: Vec<i32> = (0..5).flat_map(|f| [f * 10, f * 10 + 1]).collect();
    assert_eq!(seen_early, all);
    assert_eq!(seen_late, all);
}

#[test]
fn a_slow_reader_skips_dropped_events() {
    let mut events = Events::new();
    let mut reader = EventCursor::default();
    events.send("a");
    events.update();
    events.send("b");
    events.update();
    events.send("c");
    assert_eq!(reader.unread(&events), 2);
    assert_eq!(
        reader.read(&events).copied().collect::<Vec<_>>(),
        ["b", "c"]
    );
    assert_eq!(reader.unread(&events), 0);
}

#[test]
fn cursor_at_end_ignores_the_past() {
    let mut events = Events::new();
    events.send_batch([1, 2, 3]);
    let mut reader = events.cursor_at_end();
    events.send(4);
    assert_eq!(reader.read(&events).copied().collect::<Vec<_>>(), [4]);
}

#[test]
fn drain_and_iteration() {
    let mut events = Events::new();
    let mut reader = EventCursor::default();
    events.send(1);
    events.update();
    events.send(2);
    assert_eq!(events.iter().copied().collect::<Vec<_>>(), [1, 2]);
    assert_eq!(events.iter_current().copied().collect::<Vec<_>>(), [2]);
    assert_eq!(events.drain().collect::<Vec<_>>(), [1, 2]);
    assert!(events.is_empty());
    assert_eq!(reader.read(&events).count(), 0);
    events.send(3);
    assert_eq!(reader.read(&events).copied().collect::<Vec<_>>(), [3]);
    events.clear();
    assert!(events.is_empty());
}

#[test]
fn events_are_send_for_send_payloads() {
    fn assert_send<T: Send>() {}
    assert_send::<Events<String>>();
}

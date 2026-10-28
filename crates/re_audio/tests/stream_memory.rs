//! ADR-0066 §8: a 3-minute Ogg clip streams through the headless mixer in
//! fixed memory, on the browser's path (the pump in `Audio::advance`, the file
//! read from memory) and on the desktop's (the stream thread, the file read
//! from disk). `tests/ogg.rs` says how `data/sparse180.ogg` was made.
//!
//! The test has a binary of its own because the counting allocator counts
//! everything the process allocates.

use std::alloc::{GlobalAlloc, Layout, System};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::{Duration, Instant};

use glam::Vec3;
use re_audio::{Audio, AudioCmd, CaptureSettings};
use re_core::files::{Form, LayerKind, MemLayer, ProjectFiles, ProjectPath};
use re_types::EntityId;

/// Counts the bytes live, and the most that were live at once.
struct Counting;

static LIVE: AtomicUsize = AtomicUsize::new(0);
static PEAK: AtomicUsize = AtomicUsize::new(0);

fn grew(by: usize) {
    let live = LIVE.fetch_add(by, Ordering::Relaxed) + by;
    PEAK.fetch_max(live, Ordering::Relaxed);
}

// SAFETY: every call goes to the system allocator unchanged; the counters
// only add up the sizes it was asked for.
unsafe impl GlobalAlloc for Counting {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        // SAFETY: as the caller promised for `layout`.
        let p = unsafe { System.alloc(layout) };
        if !p.is_null() {
            grew(layout.size());
        }
        p
    }

    unsafe fn alloc_zeroed(&self, layout: Layout) -> *mut u8 {
        // SAFETY: as the caller promised for `layout`.
        let p = unsafe { System.alloc_zeroed(layout) };
        if !p.is_null() {
            grew(layout.size());
        }
        p
    }

    unsafe fn dealloc(&self, p: *mut u8, layout: Layout) {
        // SAFETY: `p` came from this allocator with `layout`.
        unsafe { System.dealloc(p, layout) };
        LIVE.fetch_sub(layout.size(), Ordering::Relaxed);
    }

    unsafe fn realloc(&self, p: *mut u8, layout: Layout, new_size: usize) -> *mut u8 {
        // SAFETY: `p` came from this allocator with `layout`, as the caller
        // promised.
        let q = unsafe { System.realloc(p, layout, new_size) };
        if !q.is_null() {
            if new_size >= layout.size() {
                grew(new_size - layout.size());
            } else {
                LIVE.fetch_sub(layout.size() - new_size, Ordering::Relaxed);
            }
        }
        q
    }
}

#[global_allocator]
static COUNTING: Counting = Counting;

/// What playing the clip may allocate at once. Decoding it whole takes
/// 180 s × 22,050 frames × 8 bytes, about 30 MiB.
const LIMIT: usize = 2 << 20;

const CLIP: &str = "sparse180.ogg";

/// A 60 Hz game frame.
const FRAME: Duration = Duration::from_nanos(1_000_000_000 / 60);

/// Plays the clip to its end, a game frame at a time; the most bytes live at
/// once beyond those live before it started, and the stream's underruns.
/// A frame here takes far less than a 60th of a second, so the stream thread
/// can fall behind by more than the clip's length (as on macOS in CI): after a
/// frame that underran, the thread gets a millisecond, and the clip has a
/// minute in all.
fn play(a: &mut Audio) -> (usize, u64) {
    let before = LIVE.load(Ordering::Relaxed);
    PEAK.store(before, Ordering::Relaxed);
    let problems = a.apply(&[AudioCmd::StartSource {
        entity: EntityId(1),
        clip: CLIP.into(),
        volume: 1.0,
        looping: false,
        spatial: false,
        position: Vec3::ZERO,
    }]);
    assert!(problems.is_empty(), "{problems:?}");
    let deadline = Instant::now() + Duration::from_secs(60);
    let mut underruns = 0;
    loop {
        a.advance(FRAME);
        let problems = a.apply(&[]);
        assert!(problems.is_empty(), "{problems:?}");
        if !a.is_source_playing(EntityId(1)) {
            return (
                PEAK.load(Ordering::Relaxed).saturating_sub(before),
                a.stream_underruns(),
            );
        }
        assert!(Instant::now() < deadline, "the clip did not end");
        let now = a.stream_underruns();
        if now > underruns {
            underruns = now;
            std::thread::sleep(Duration::from_millis(1));
        }
    }
}

#[test]
fn a_long_clip_streams_in_fixed_memory() {
    let bytes = std::fs::read(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/tests/data/sparse180.ogg"
    ))
    .unwrap();

    // The browser: no stream thread, and the file in memory.
    let mut browser = Audio::headless_with(CaptureSettings {
        sample_rate: 22_050,
        stream_thread: false,
    });
    let mut mem = MemLayer::new(LayerKind::Memory);
    mem.insert(ProjectPath::new(CLIP).unwrap(), bytes, Form::Source);
    browser.set_files(ProjectFiles::for_tests(mem));
    let (peak, underruns) = play(&mut browser);
    println!("browser: {peak} bytes at most, {underruns} underruns");
    assert!(peak < LIMIT, "{peak} bytes");
    assert_eq!(underruns, 0);
    drop(browser);

    // The desktop: the stream thread, and the file on disk.
    let mut desktop = Audio::headless_with(CaptureSettings {
        sample_rate: 22_050,
        stream_thread: true,
    });
    desktop.set_asset_root(concat!(env!("CARGO_MANIFEST_DIR"), "/tests/data"));
    let (peak, underruns) = play(&mut desktop);
    // Rendering runs faster than real time here, so the thread can fall
    // behind; that is silence, not memory.
    println!("desktop: {peak} bytes at most, {underruns} underruns");
    assert!(peak < LIMIT, "{peak} bytes");
}

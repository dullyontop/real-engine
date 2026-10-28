//! Streamed sounds (ADR-0066 §2–§4): an Ogg clip plays through a ring of
//! frames, which a [`Pump`] fills and a [`StreamSound`] plays from.
//!
//! - The pump opens the clip on its first round. A clip of
//!   [`STREAM_SECONDS`] or less is decoded whole: the play goes on from those
//!   frames, and they go back to the game side ([`Event::Classified`]) so later
//!   plays are static sounds. A longer clip is decoded a packet at a time.
//! - The sound is a kira [`Sound`] with its own volume, rate, pause, resume
//!   and stop fades, as kira's static sound has, and resamples with kira's
//!   4-point interpolation, so the clip's rate, the playback rate and doppler
//!   are one step. It never allocates, and it stays silent until the ring holds
//!   a tenth of a second or the clip has ended.
//! - A seek goes to the pump, which seeks its reader and raises a flush
//!   number; the sound fades out for 10 ms, empties the ring and acknowledges
//!   it, and only then does the pump push the new frames.
//! - [`Streams`] runs the pumps: on one stream thread on the desktop, or in
//!   [`Audio::advance`](crate::Audio::advance) in the browser and headless
//!   runs, so headless output does not depend on thread timing.

use std::io::BufReader;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU8, AtomicU32, AtomicU64, Ordering};

use kira::command::{CommandReader, CommandWriter, ValueChangeCommand, command_writer_and_reader};
use kira::info::Info;
use kira::sound::static_sound::{StaticSoundData, StaticSoundSettings};
use kira::sound::{PlaybackState, Sound, SoundData};
use kira::{Decibels, Frame, Parameter, PlaybackRate, Tween, Value, interpolate_frame};
use re_core::files::{FileReader, ProjectFiles};
use re_core::{Diagnostic, Location};
use rtrb::{Consumer, Producer, RingBuffer};

use crate::codes;
use crate::mixer::read_error;
use crate::vorbis::{self, VorbisReader};

/// An Ogg clip longer than this many seconds streams; a shorter one is decoded
/// whole at its first play (ADR-0066 §2).
pub(crate) const STREAM_SECONDS: u64 = 10;

/// The most streams that play at once.
pub(crate) const MAX_STREAMS: usize = 32;

/// True when a clip of this length streams rather than being decoded whole.
pub(crate) fn streams(info: &vorbis::Info) -> bool {
    info.frames > STREAM_SECONDS * u64::from(info.sample_rate)
}

/// Frames decoded whole, as kira's static sound data.
pub(crate) fn static_data(sample_rate: u32, frames: Arc<[Frame]>) -> StaticSoundData {
    StaticSoundData {
        sample_rate,
        frames,
        settings: StaticSoundSettings::default(),
        slice: None,
    }
}

/// The ring's size in frames: half a second at 48 kHz with a stream thread,
/// a second when the pumps run once a game frame.
const RING_THREAD: usize = 24_000;
const RING_INLINE: usize = 48_000;

/// How long a seek's fade out (and back in) lasts, in seconds.
const FLUSH_FADE: f64 = 0.01;

/// The pieces a stream reads its file in, in bytes.
const READ_PIECE: usize = 32 << 10;

/// How long the stream thread waits between rounds at most; a new stream or a
/// seek wakes it early.
#[cfg(not(target_arch = "wasm32"))]
const PARK: std::time::Duration = std::time::Duration::from_millis(10);

/// What the pump and the sound both read.
#[derive(Debug, Default)]
struct Shared {
    /// The sound's [`PlaybackState`].
    state: AtomicU8,
    /// The clip's sample rate once the pump opened it; 0 before.
    rate: AtomicU32,
    /// The pump has pushed the clip's last frame, and no seek came since.
    ended: AtomicBool,
    /// The stream failed: the sound stops.
    failed: AtomicBool,
    /// Seeks asked for: how many, and the last one's time in seconds (bits).
    seeks: AtomicU64,
    seek_to: AtomicU64,
    /// The last seek the pump carried out, and the last one the sound
    /// emptied the ring for.
    flush: AtomicU64,
    acked: AtomicU64,
    /// Frames the sound needed while the ring was empty before the end.
    underruns: AtomicU64,
}

fn state_bits(state: PlaybackState) -> u8 {
    state as u8
}

fn state_from(bits: u8) -> PlaybackState {
    match bits {
        0 => PlaybackState::Playing,
        1 => PlaybackState::Pausing,
        2 => PlaybackState::Paused,
        3 => PlaybackState::WaitingToResume,
        4 => PlaybackState::Resuming,
        5 => PlaybackState::Stopping,
        _ => PlaybackState::Stopped,
    }
}

/// What a pump tells the game side.
pub(crate) enum Event {
    /// The pump opened `clip`, `seconds` long: decoded whole when it is short
    /// (`Some`), or to stream. `epoch` is the clip cache's epoch when the play
    /// started, so a clip changed since is not cached from the old file.
    Classified {
        clip: String,
        epoch: u64,
        seconds: f64,
        data: Option<Box<StaticSoundData>>,
    },
    /// A stream could not open or failed while it played.
    Problem(Diagnostic),
}

/// Wakes the stream thread, when there is one.
#[derive(Clone, Debug, Default)]
pub(crate) struct Wake(Option<std::thread::Thread>);

impl Wake {
    fn wake(&self) {
        if let Some(t) = &self.0 {
            t.unpark();
        }
    }
}

/// Starts a stream of `clip`: the pump, for [`Streams::start`], and the sound
/// data, for kira. `volume` is the starting volume.
pub(crate) fn open(
    clip: &str,
    files: ProjectFiles,
    looping: bool,
    volume: Decibels,
    streams: &mut Streams,
    epoch: u64,
    panic: bool,
) -> (Pump, StreamData) {
    let (producer, consumer) = RingBuffer::new(streams.ring_frames());
    let shared = Arc::new(Shared::default());
    let (volume_writer, volume_reader) = command_writer_and_reader();
    let (rate_writer, rate_reader) = command_writer_and_reader();
    let (pause_writer, pause_reader) = command_writer_and_reader();
    let (resume_writer, resume_reader) = command_writer_and_reader();
    let (stop_writer, stop_reader) = command_writer_and_reader();
    let pump = Pump {
        clip: clip.to_owned(),
        epoch,
        files: Some(files),
        source: Source::Closed,
        producer,
        carry: Vec::new(),
        carry_at: 0,
        looping,
        position: 0,
        shared: shared.clone(),
        seeks_done: 0,
        panic,
    };
    let sound = Box::new(StreamSound {
        consumer,
        shared: shared.clone(),
        commands: Readers {
            volume: volume_reader,
            rate: rate_reader,
            pause: pause_reader,
            resume: resume_reader,
            stop: stop_reader,
        },
        volume: Parameter::new(Value::Fixed(volume), Decibels::IDENTITY),
        rate: Parameter::new(Value::Fixed(PlaybackRate(1.0)), PlaybackRate(1.0)),
        fade: Parameter::new(Value::Fixed(Decibels::IDENTITY), Decibels::IDENTITY),
        state: PlaybackState::Playing,
        history: [Frame::ZERO; 4],
        fraction: 0.0,
        waiting: true,
        flushing: false,
        acked: 0,
        duck: Duck {
            gain: 1.0,
            target: 1.0,
        },
        tail: 0,
    });
    let handle = StreamHandle {
        shared,
        volume: volume_writer,
        rate: rate_writer,
        pause: pause_writer,
        resume: resume_writer,
        stop: stop_writer,
        wake: streams.wake(),
    };
    (pump, StreamData { sound, handle })
}

// ---------------------------------------------------------------------------
// The pump
// ---------------------------------------------------------------------------

/// Where a pump's frames come from.
enum Source {
    /// Not opened yet.
    Closed,
    /// A long clip, decoded a packet at a time.
    Reader(Box<VorbisReader<BufReader<FileReader>>>),
    /// A short clip decoded whole, and the next frame to push.
    Whole { frames: Arc<[Frame]>, at: usize },
    /// Failed: nothing more.
    Done,
}

/// The decoding half of a stream.
pub(crate) struct Pump {
    clip: String,
    epoch: u64,
    /// The project's files, until the clip is opened.
    files: Option<ProjectFiles>,
    source: Source,
    producer: Producer<Frame>,
    /// Decoded frames not yet in the ring.
    carry: Vec<Frame>,
    carry_at: usize,
    looping: bool,
    /// The frame of the clip after the last one decoded, for error messages.
    position: u64,
    shared: Arc<Shared>,
    /// The last seek carried out.
    seeks_done: u64,
    /// Panics at its first decode step: tests of the panic path.
    panic: bool,
}

impl Pump {
    /// One round: opens the clip the first time, carries out a seek, and fills
    /// the ring. `false` once the pump is no longer needed, because its sound
    /// is gone or the stream failed.
    pub(crate) fn round(&mut self, events: &mut Vec<Event>) -> bool {
        if self.producer.is_abandoned() || matches!(self.source, Source::Done) {
            return false;
        }
        match self.step(events) {
            Ok(()) => true,
            Err(d) => {
                self.fail(d, events);
                false
            }
        }
    }

    /// Stops the stream: its sound stops too.
    fn fail(&mut self, d: Diagnostic, events: &mut Vec<Event>) {
        self.shared.failed.store(true, Ordering::Release);
        self.source = Source::Done;
        events.push(Event::Problem(d));
    }

    fn step(&mut self, events: &mut Vec<Event>) -> Result<(), Diagnostic> {
        if matches!(self.source, Source::Closed) {
            self.open(events)?;
        }
        self.seek()?;
        // After a seek, the sound empties the ring before new frames go in.
        if self.shared.acked.load(Ordering::Acquire) < self.seeks_done {
            return Ok(());
        }
        self.fill()
    }

    fn open(&mut self, events: &mut Vec<Event>) -> Result<(), Diagnostic> {
        let Some(files) = self.files.take() else {
            return Err(self.failed("it has no project files to read"));
        };
        let file = files
            .open(&self.clip)
            .map_err(|e| read_error(&self.clip, e))?;
        let mut reader = VorbisReader::open(BufReader::with_capacity(READ_PIECE, file))
            .map_err(|e| decode_error(&self.clip, e))?;
        let info = reader.info();
        self.shared.rate.store(info.sample_rate, Ordering::Release);
        let data = if streams(&info) {
            self.carry.reserve(reader.long_block());
            self.source = Source::Reader(Box::new(reader));
            None
        } else {
            // Short: decode it whole, for this play and the next ones.
            let frames: Arc<[Frame]> = reader
                .read_all()
                .map_err(|e| decode_error(&self.clip, e))?
                .into();
            self.source = Source::Whole {
                frames: frames.clone(),
                at: 0,
            };
            Some(Box::new(static_data(info.sample_rate, frames)))
        };
        events.push(Event::Classified {
            clip: self.clip.clone(),
            epoch: self.epoch,
            seconds: info.seconds(),
            data,
        });
        Ok(())
    }

    /// Carries out the last seek asked for, if it is new.
    fn seek(&mut self) -> Result<(), Diagnostic> {
        let asked = self.shared.seeks.load(Ordering::Acquire);
        if asked == self.seeks_done {
            return Ok(());
        }
        let seconds = f64::from_bits(self.shared.seek_to.load(Ordering::Acquire));
        let rate = f64::from(self.shared.rate.load(Ordering::Acquire));
        let total = match &self.source {
            Source::Reader(r) => r.info().frames,
            Source::Whole { frames, .. } => frames.len() as u64,
            Source::Closed | Source::Done => 0,
        };
        // `as` saturates: a time far past the end is the end, or wraps
        // around a looping clip.
        let mut frame = (seconds * rate) as u64;
        if self.looping && total > 0 {
            frame %= total;
        } else {
            frame = frame.min(total);
        }
        self.carry.clear();
        self.carry_at = 0;
        let sought = match &mut self.source {
            Source::Reader(r) => r.seek(frame, &mut self.carry),
            Source::Whole { frames, at } => {
                *at = usize::try_from(frame)
                    .unwrap_or(usize::MAX)
                    .min(frames.len());
                Ok(())
            }
            Source::Closed | Source::Done => Ok(()),
        };
        sought.map_err(|e| self.stream_error(&e))?;
        self.position = frame + self.carry.len() as u64;
        self.shared.ended.store(false, Ordering::Release);
        self.seeks_done = asked;
        self.shared.flush.store(asked, Ordering::Release);
        Ok(())
    }

    /// Pushes frames until the ring is full or the clip has ended.
    fn fill(&mut self) -> Result<(), Diagnostic> {
        // A looping clip that gives no frame from its start ends instead.
        let mut empty_pass = false;
        loop {
            let free = self.producer.slots();
            if free == 0 {
                return Ok(());
            }
            if self.carry_at < self.carry.len() {
                let n = free.min(self.carry.len() - self.carry_at);
                push(
                    &mut self.producer,
                    &self.carry[self.carry_at..self.carry_at + n],
                );
                self.carry_at += n;
                continue;
            }
            if self.shared.ended.load(Ordering::Acquire) {
                return Ok(());
            }
            match &mut self.source {
                Source::Whole { frames, at } => {
                    if *at < frames.len() {
                        let n = free.min(frames.len() - *at);
                        push(&mut self.producer, &frames[*at..*at + n]);
                        *at += n;
                    } else if self.looping && !frames.is_empty() {
                        *at = 0;
                    } else {
                        self.shared.ended.store(true, Ordering::Release);
                    }
                }
                Source::Reader(r) => {
                    assert!(!self.panic, "a test made this stream's decoder panic");
                    self.carry.clear();
                    self.carry_at = 0;
                    let more = r.read(&mut self.carry);
                    match more {
                        Ok(true) => {
                            self.position += self.carry.len() as u64;
                            empty_pass = false;
                        }
                        Ok(false) if self.looping && !empty_pass => {
                            let rewound = r.rewind();
                            rewound.map_err(|e| self.stream_error(&e))?;
                            self.position = 0;
                            empty_pass = true;
                        }
                        Ok(false) => self.shared.ended.store(true, Ordering::Release),
                        Err(e) => return Err(self.stream_error(&e)),
                    }
                }
                Source::Closed | Source::Done => return Ok(()),
            }
        }
    }

    fn failed(&self, why: &str) -> Diagnostic {
        Diagnostic::error(
            codes::STREAM_FAILED,
            format!("audio stream `{}` failed: {why}", self.clip),
        )
        .at(Location::file(self.clip.clone()))
    }

    /// RE7410 for an error while the stream plays, with where it was.
    fn stream_error(&self, e: &vorbis::Error) -> Diagnostic {
        let rate = self.shared.rate.load(Ordering::Acquire).max(1);
        let at = self.position as f64 / f64::from(rate);
        self.failed(&format!("{e} (at {at:.1} s)"))
            .with_hint("re-encode the file, for example with `oggenc`, and check that it plays")
    }

    /// RE7410 for a decoder that panicked.
    #[cfg(not(target_arch = "wasm32"))]
    fn panicked(&self, payload: &(dyn std::any::Any + Send)) -> Diagnostic {
        let why = payload
            .downcast_ref::<&str>()
            .map(|s| (*s).to_string())
            .or_else(|| payload.downcast_ref::<String>().cloned())
            .unwrap_or_default();
        self.failed(&format!("its decoder panicked: {why}"))
            .with_hint("the file is damaged in a way the decoder does not expect: re-encode it")
    }
}

/// Pushes all of `frames`, which fit.
fn push(producer: &mut Producer<Frame>, frames: &[Frame]) {
    if let Ok(chunk) = producer.write_chunk_uninit(frames.len()) {
        chunk.fill_from_iter(frames.iter().copied());
    }
}

/// The diagnostic for a clip whose file does not open as Ogg Vorbis: RE7409
/// when the engine does not play its kind, RE7402 when it is damaged.
pub(crate) fn decode_error(clip: &str, e: vorbis::Error) -> Diagnostic {
    match e {
        vorbis::Error::Unsupported(why) => Diagnostic::error(
            codes::UNSUPPORTED_AUDIO,
            format!("audio clip `{clip}` is not a file the engine plays: {why}"),
        )
        .with_hint(
            "the engine plays WAV and Ogg Vorbis, mono or stereo, one stream per file: \
             convert it, for example `ffmpeg -i in.flac -ac 2 -c:a libvorbis out.ogg`",
        ),
        e => Diagnostic::error(
            codes::CLIP_DECODE_FAILED,
            format!("cannot decode audio clip `{clip}`: {e}"),
        )
        .with_hint("re-encode the file, for example with `oggenc`, and check that it plays"),
    }
    .at(Location::file(clip))
}

/// Decodes every packet of the Ogg Vorbis file `clip`, whose bytes are
/// `bytes`, once, keeping no frames: `real build` refuses a file damaged past
/// its headers before a game ships it (ADR-0066 §6). Its length in seconds.
///
/// # Errors
///
/// [`codes::UNSUPPORTED_AUDIO`] for a file the engine does not play, and
/// [`codes::CLIP_DECODE_FAILED`] for a damaged one, including one whose
/// decoder panics.
pub fn check_ogg(clip: &str, bytes: &[u8]) -> Result<f64, Diagnostic> {
    let decode = || -> Result<f64, vorbis::Error> {
        let mut reader = VorbisReader::open(std::io::Cursor::new(bytes))?;
        let mut frames = Vec::with_capacity(reader.long_block());
        while reader.read(&mut frames)? {
            frames.clear();
        }
        Ok(reader.info().seconds())
    };
    #[cfg(not(target_arch = "wasm32"))]
    let decoded = std::panic::catch_unwind(decode).unwrap_or_else(|_| {
        Err(vorbis::Error::Damaged(
            "its decoder panicked on a damaged packet".into(),
        ))
    });
    #[cfg(target_arch = "wasm32")]
    let decoded = decode();
    decoded.map_err(|e| decode_error(clip, e))
}

// ---------------------------------------------------------------------------
// The sound
// ---------------------------------------------------------------------------

/// The command readers of a [`StreamSound`].
struct Readers {
    volume: CommandReader<ValueChangeCommand<Decibels>>,
    rate: CommandReader<ValueChangeCommand<PlaybackRate>>,
    pause: CommandReader<Tween>,
    resume: CommandReader<Tween>,
    stop: CommandReader<Tween>,
}

/// A short fade of a stream's own, for seeks: out before the ring empties,
/// back in after.
struct Duck {
    gain: f64,
    target: f64,
}

impl Duck {
    /// The gain for the next output frame, `dt` seconds on.
    fn next(&mut self, dt: f64) -> f64 {
        let step = dt / FLUSH_FADE;
        self.gain = if self.gain < self.target {
            (self.gain + step).min(self.target)
        } else {
            (self.gain - step).max(self.target)
        };
        self.gain
    }
}

/// The playing half of a stream: a kira sound fed by the ring.
pub(crate) struct StreamSound {
    consumer: Consumer<Frame>,
    shared: Arc<Shared>,
    commands: Readers,
    volume: Parameter<Decibels>,
    rate: Parameter<PlaybackRate>,
    /// The pause, resume and stop fades.
    fade: Parameter<Decibels>,
    state: PlaybackState,
    /// The last four frames of the clip, and where between the second and
    /// third the output is.
    history: [Frame; 4],
    fraction: f64,
    /// Silent until the ring holds a tenth of a second or the clip ended: at
    /// the start, and after a seek.
    waiting: bool,
    /// Fading out for a seek, before the ring empties.
    flushing: bool,
    /// The last flush this sound carried out.
    acked: u64,
    duck: Duck,
    /// Frames played since the ring ran out at the end of the clip: the
    /// history plays out over three.
    tail: u8,
}

impl StreamSound {
    fn set_state(&mut self, state: PlaybackState) {
        self.state = state;
        self.shared
            .state
            .store(state_bits(state), Ordering::Release);
    }

    fn read_commands(&mut self) {
        self.volume.read_command(&mut self.commands.volume);
        self.rate.read_command(&mut self.commands.rate);
        let stopped = self.state == PlaybackState::Stopped;
        if let Some(tween) = self.commands.pause.read()
            && !stopped
        {
            self.set_state(PlaybackState::Pausing);
            self.fade.set(Value::Fixed(Decibels::SILENCE), tween);
        }
        if let Some(tween) = self.commands.resume.read()
            && !stopped
        {
            self.set_state(PlaybackState::Resuming);
            self.fade.set(Value::Fixed(Decibels::IDENTITY), tween);
        }
        if let Some(tween) = self.commands.stop.read()
            && !stopped
        {
            self.set_state(PlaybackState::Stopping);
            self.fade.set(Value::Fixed(Decibels::SILENCE), tween);
        }
    }

    /// True once the ring holds enough to start: a tenth of a second (at most
    /// half the ring), or the rest of a clip that has ended.
    fn ready(&self, rate: u32) -> bool {
        if rate == 0 {
            return false;
        }
        let want = (rate as usize / 10).min(self.consumer.buffer().capacity() / 2);
        self.consumer.slots() >= want || self.shared.ended.load(Ordering::Acquire)
    }

    /// Moves on by one frame of the clip.
    fn advance(&mut self) {
        self.history.rotate_left(1);
        self.history[3] = match self.consumer.pop() {
            Ok(frame) => frame,
            Err(_) => {
                if self.flushing || self.waiting {
                    // A seek is under way: no underrun.
                } else if self.shared.ended.load(Ordering::Acquire) {
                    self.tail = self.tail.saturating_add(1);
                } else {
                    self.shared.underruns.fetch_add(1, Ordering::Relaxed);
                }
                Frame::ZERO
            }
        };
    }

    /// Empties the ring for the seek the pump carried out, and waits for the
    /// new frames.
    fn flush(&mut self, flush: u64) {
        let n = self.consumer.slots();
        if let Ok(chunk) = self.consumer.read_chunk(n) {
            chunk.commit_all();
        }
        self.acked = flush;
        self.shared.acked.store(flush, Ordering::Release);
        self.flushing = false;
        self.waiting = true;
        self.tail = 0;
    }
}

impl Sound for StreamSound {
    fn on_start_processing(&mut self) {
        self.read_commands();
    }

    fn process(&mut self, out: &mut [Frame], dt: f64, info: &Info) {
        let span = dt * out.len() as f64;
        self.volume.update(span, info);
        self.rate.update(span, info);
        if self.fade.update(span, info) {
            match self.state {
                PlaybackState::Pausing => self.set_state(PlaybackState::Paused),
                PlaybackState::Resuming => self.set_state(PlaybackState::Playing),
                PlaybackState::Stopping => self.set_state(PlaybackState::Stopped),
                _ => {}
            }
        }
        if self.shared.failed.load(Ordering::Acquire) {
            self.set_state(PlaybackState::Stopped);
        }
        if matches!(self.state, PlaybackState::Paused | PlaybackState::Stopped) {
            out.fill(Frame::ZERO);
            return;
        }
        let flush = self.shared.flush.load(Ordering::Acquire);
        if flush != self.acked {
            if self.waiting {
                // Nothing plays yet: empty the ring at once.
                self.flush(flush);
            } else if !self.flushing {
                self.flushing = true;
                self.duck.target = 0.0;
            } else if self.duck.gain <= 0.0 {
                self.flush(flush);
            }
        }
        let rate = self.shared.rate.load(Ordering::Acquire);
        if self.waiting {
            if !self.ready(rate) {
                out.fill(Frame::ZERO);
                return;
            }
            self.waiting = false;
            self.history = [Frame::ZERO; 4];
            self.fraction = 0.0;
            for _ in 0..3 {
                self.advance();
            }
            // A seek's frames fade in; a stream's start does not.
            self.duck.target = 1.0;
        }
        let n = out.len() as f64;
        let step = f64::from(rate) * dt;
        for (i, frame) in out.iter_mut().enumerate() {
            let t = (i + 1) as f64 / n;
            let gain = self.volume.interpolated_value(t).as_amplitude()
                * self.fade.interpolated_value(t).as_amplitude()
                * self.duck.next(dt) as f32;
            let [a, b, c, d] = self.history;
            *frame = interpolate_frame(a, b, c, d, self.fraction as f32) * gain;
            self.fraction += step * self.rate.interpolated_value(t).0.abs();
            while self.fraction >= 1.0 {
                self.fraction -= 1.0;
                self.advance();
            }
        }
        if self.tail >= 3 && !self.flushing {
            self.set_state(PlaybackState::Stopped);
        }
    }

    fn finished(&self) -> bool {
        self.state == PlaybackState::Stopped
    }
}

/// A stream's sound and handle, for kira's `play`.
pub(crate) struct StreamData {
    sound: Box<StreamSound>,
    handle: StreamHandle,
}

impl SoundData for StreamData {
    type Error = std::convert::Infallible;
    type Handle = StreamHandle;

    fn into_sound(self) -> Result<(Box<dyn Sound>, StreamHandle), Self::Error> {
        Ok((self.sound, self.handle))
    }
}

/// Controls a playing stream, as kira's handle controls a static sound.
pub(crate) struct StreamHandle {
    shared: Arc<Shared>,
    volume: CommandWriter<ValueChangeCommand<Decibels>>,
    rate: CommandWriter<ValueChangeCommand<PlaybackRate>>,
    pause: CommandWriter<Tween>,
    resume: CommandWriter<Tween>,
    stop: CommandWriter<Tween>,
    wake: Wake,
}

impl StreamHandle {
    /// The sound's state; a stream that failed is stopped at once.
    pub(crate) fn state(&self) -> PlaybackState {
        if self.shared.failed.load(Ordering::Acquire) {
            return PlaybackState::Stopped;
        }
        state_from(self.shared.state.load(Ordering::Acquire))
    }

    pub(crate) fn set_volume(&mut self, volume: Decibels, tween: Tween) {
        self.volume.write(ValueChangeCommand {
            target: Value::Fixed(volume),
            tween,
        });
    }

    pub(crate) fn set_playback_rate(&mut self, rate: f64, tween: Tween) {
        self.rate.write(ValueChangeCommand {
            target: Value::Fixed(PlaybackRate(rate)),
            tween,
        });
    }

    pub(crate) fn pause(&mut self, tween: Tween) {
        self.pause.write(tween);
    }

    pub(crate) fn resume(&mut self, tween: Tween) {
        self.resume.write(tween);
    }

    pub(crate) fn stop(&mut self, tween: Tween) {
        self.stop.write(tween);
    }

    /// Jumps to `seconds` from the start of the clip: past the end, the end
    /// (a looping clip wraps around).
    pub(crate) fn seek_to(&mut self, seconds: f64) {
        self.shared
            .seek_to
            .store(seconds.max(0.0).to_bits(), Ordering::Release);
        self.shared.seeks.fetch_add(1, Ordering::AcqRel);
        self.wake.wake();
    }

    /// Frames this stream needed while its ring was empty.
    pub(crate) fn underruns(&self) -> u64 {
        self.shared.underruns.load(Ordering::Relaxed)
    }
}

// ---------------------------------------------------------------------------
// Who pumps
// ---------------------------------------------------------------------------

/// Runs one round of `pump`. On native targets a panic in it stops only that
/// stream, with RE7410.
fn guarded_round(pump: &mut Pump, events: &mut Vec<Event>) -> bool {
    #[cfg(not(target_arch = "wasm32"))]
    {
        match std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| pump.round(events))) {
            Ok(alive) => alive,
            Err(payload) => {
                let d = pump.panicked(payload.as_ref());
                pump.fail(d, events);
                false
            }
        }
    }
    #[cfg(target_arch = "wasm32")]
    {
        pump.round(events)
    }
}

/// The stream thread: its inbox of new pumps, its outbox of events, and the
/// flag that stops it.
#[cfg(not(target_arch = "wasm32"))]
struct StreamThread {
    new: std::sync::mpsc::Sender<Pump>,
    events: std::sync::mpsc::Receiver<Event>,
    stop: Arc<AtomicBool>,
    handle: Option<std::thread::JoinHandle<()>>,
}

#[cfg(not(target_arch = "wasm32"))]
impl StreamThread {
    fn spawn() -> std::io::Result<StreamThread> {
        let (new, inbox) = std::sync::mpsc::channel::<Pump>();
        let (outbox, events) = std::sync::mpsc::channel();
        let stop = Arc::new(AtomicBool::new(false));
        let stopped = stop.clone();
        let handle = std::thread::Builder::new()
            .name("re_audio streams".into())
            .spawn(move || {
                let mut pumps: Vec<Pump> = Vec::new();
                let mut out = Vec::new();
                while !stopped.load(Ordering::Acquire) {
                    pumps.extend(inbox.try_iter());
                    pumps.retain_mut(|p| guarded_round(p, &mut out));
                    for e in out.drain(..) {
                        // The game side is gone only when the thread stops.
                        let _ = outbox.send(e);
                    }
                    std::thread::park_timeout(PARK);
                }
            })?;
        Ok(StreamThread {
            new,
            events,
            stop,
            handle: Some(handle),
        })
    }
}

#[cfg(not(target_arch = "wasm32"))]
impl Drop for StreamThread {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Release);
        if let Some(handle) = self.handle.take() {
            handle.thread().unpark();
            let _ = handle.join();
        }
    }
}

/// Runs the pumps: on a stream thread, started with the first stream and
/// joined when this drops, or inline, in [`Streams::pump`].
pub(crate) struct Streams {
    threaded: bool,
    #[cfg(not(target_arch = "wasm32"))]
    thread: Option<StreamThread>,
    /// The inline pumps, and their events.
    pumps: Vec<Pump>,
    events: Vec<Event>,
}

impl Streams {
    /// Pumps on a stream thread when `threaded` (never in the browser).
    pub(crate) fn new(threaded: bool) -> Streams {
        Streams {
            threaded: threaded && cfg!(not(target_arch = "wasm32")),
            #[cfg(not(target_arch = "wasm32"))]
            thread: None,
            pumps: Vec::new(),
            events: Vec::new(),
        }
    }

    fn ring_frames(&self) -> usize {
        if self.threaded {
            RING_THREAD
        } else {
            RING_INLINE
        }
    }

    /// What wakes the pumps early: the stream thread, started now if it is
    /// not running yet. Without a thread, nothing.
    fn wake(&mut self) -> Wake {
        #[cfg(not(target_arch = "wasm32"))]
        if self.threaded {
            if self.thread.is_none() {
                match StreamThread::spawn() {
                    Ok(t) => self.thread = Some(t),
                    Err(e) => {
                        tracing::warn!(error = %e, "no stream thread; streams are decoded once a frame");
                        self.threaded = false;
                    }
                }
            }
            if let Some(handle) = self.thread.as_ref().and_then(|t| t.handle.as_ref()) {
                return Wake(Some(handle.thread().clone()));
            }
        }
        Wake::default()
    }

    /// Hands a new pump over. Inline, it opens the clip and fills the ring
    /// now, so the sound has its first frames at once.
    pub(crate) fn start(&mut self, mut pump: Pump) {
        #[cfg(not(target_arch = "wasm32"))]
        if let Some(thread) = &self.thread {
            match thread.new.send(pump) {
                Ok(()) => {
                    if let Some(handle) = &thread.handle {
                        handle.thread().unpark();
                    }
                    return;
                }
                // The thread is gone: pump here instead.
                Err(std::sync::mpsc::SendError(back)) => pump = back,
            }
        }
        if guarded_round(&mut pump, &mut self.events) {
            self.pumps.push(pump);
        }
    }

    /// One round of every inline pump (the stream thread runs its own).
    pub(crate) fn pump(&mut self) {
        let events = &mut self.events;
        self.pumps.retain_mut(|p| guarded_round(p, events));
    }

    /// What the pumps have told the game side since the last call.
    pub(crate) fn take_events(&mut self) -> Vec<Event> {
        #[cfg(not(target_arch = "wasm32"))]
        if let Some(thread) = &self.thread {
            self.events.extend(thread.events.try_iter());
        }
        std::mem::take(&mut self.events)
    }
}

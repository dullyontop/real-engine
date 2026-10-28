//! Ogg Vorbis with exact sample positions (ADR-0066 §1): `ogg` 0.9 reads pages
//! and packets, and lewton, without its own `ogg` feature, decodes packets.
//!
//! lewton returns each packet's samples up to the start of the right window
//! slope rather than to the window's centre, so after a long block followed by
//! a short one its packet boundaries run up to (long - short) / 4 samples
//! ahead of the granule positions, while the samples themselves are the same.
//! The reader tracks where each packet ends by the standard (its granule
//! position) and places every chunk lewton returns by its window offsets, so a
//! seek lands on the exact sample.

use std::fmt;
use std::io::{Read, Seek, SeekFrom};

use kira::Frame;
use lewton::audio::{PreviousWindowRight, get_decoded_sample_count, read_audio_packet_generic};
use lewton::header::{
    HeaderReadError, IdentHeader, SetupHeader, read_header_comment, read_header_ident,
    read_header_setup,
};
use ogg::OggReadError;
use ogg::reading::PacketReader;

/// How far from the end [`VorbisReader::open`] looks for the last page.
const TAIL: u64 = 64 << 10;

/// Why a file cannot be read.
#[derive(Debug)]
pub(crate) enum Error {
    /// Not a file this reader plays: not Ogg, not Vorbis, more than two
    /// channels, or several streams (RE7409).
    Unsupported(String),
    /// A damaged or unreadable file (RE7402 when it opens, RE7410 while it
    /// plays).
    Damaged(String),
    /// A seek landed in the last page before any page end placed the stream:
    /// seek again from earlier.
    Unanchored,
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Error::Unsupported(why) | Error::Damaged(why) => f.write_str(why),
            Error::Unanchored => f.write_str("a seek found no page to place it"),
        }
    }
}

impl From<OggReadError> for Error {
    fn from(e: OggReadError) -> Self {
        Error::Damaged(format!("the Ogg pages are damaged: {e}"))
    }
}

impl From<lewton::audio::AudioReadError> for Error {
    fn from(e: lewton::audio::AudioReadError) -> Self {
        Error::Damaged(format!("an audio packet is damaged: {e}"))
    }
}

impl From<std::io::Error> for Error {
    fn from(e: std::io::Error) -> Self {
        Error::Damaged(format!("it cannot be read: {e}"))
    }
}

fn several() -> Error {
    Error::Unsupported("it holds several chained or multiplexed streams".into())
}

fn header(e: HeaderReadError) -> Error {
    match e {
        HeaderReadError::NotVorbisHeader | HeaderReadError::UnsupportedVorbisVersion => {
            Error::Unsupported("the Ogg stream is not Vorbis".into())
        }
        e => Error::Damaged(format!("a Vorbis header is damaged: {e}")),
    }
}

/// The length and format a file's headers give, without decoding audio.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Info {
    /// 1 or 2.
    pub channels: u16,
    /// Frames per second, above 0.
    pub sample_rate: u32,
    /// Frames in the stream: the last page's granule position.
    pub frames: u64,
}

impl Info {
    /// The length in seconds.
    pub(crate) fn seconds(&self) -> f64 {
        self.frames as f64 / f64::from(self.sample_rate.max(1))
    }
}

/// The granule position and the serial number of the last page, from the
/// file's last [`TAIL`] bytes.
fn last_page<R: Read + Seek>(rdr: &mut R) -> Result<(u64, u32), Error> {
    let len = rdr.seek(SeekFrom::End(0))?;
    let from = len.saturating_sub(TAIL);
    rdr.seek(SeekFrom::Start(from))?;
    let mut tail = Vec::new();
    rdr.take(len - from).read_to_end(&mut tail)?;
    rdr.seek(SeekFrom::Start(0))?;
    let at = tail
        .windows(4)
        .rposition(|w| w == b"OggS")
        .filter(|&i| i + 27 <= tail.len())
        .ok_or_else(|| Error::Damaged("no Ogg page in its last 64 KiB".into()))?;
    let mut granule = [0u8; 8];
    granule.copy_from_slice(&tail[at + 6..at + 14]);
    let mut serial = [0u8; 4];
    serial.copy_from_slice(&tail[at + 14..at + 18]);
    Ok((u64::from_le_bytes(granule), u32::from_le_bytes(serial)))
}

/// Decoded samples, planar, and where they start relative to the anchor.
struct Chunk {
    rel_start: i64,
    samples: Vec<Vec<f32>>,
}

impl Chunk {
    fn len(&self) -> usize {
        self.samples[0].len()
    }

    /// Appends the frames from `skip` on.
    fn frames_into(&self, skip: usize, out: &mut Vec<Frame>) {
        match &self.samples[..] {
            [mono] => out.extend(mono[skip..].iter().map(|&s| Frame::from_mono(s))),
            [left, right, ..] => out.extend(
                left[skip..]
                    .iter()
                    .zip(&right[skip..])
                    .map(|(&l, &r)| Frame::new(l, r)),
            ),
            [] => {}
        }
    }
}

/// An Ogg Vorbis stream read packet by packet.
pub(crate) struct VorbisReader<R: Read + Seek> {
    pr: PacketReader<R>,
    ident: IdentHeader,
    setup: SetupHeader,
    serial: u32,
    pwr: PreviousWindowRight,
    /// The short and long block sizes.
    n0: i64,
    n1: i64,
    prev_n: Option<i64>,
    /// The standard end of the previous packet, relative to the anchor.
    rel_std: i64,
    /// Absolute position = relative + offset, once a page end placed it.
    offset: Option<i64>,
    /// The first audio page's byte offset.
    first_audio: u64,
    /// Frames in the stream.
    total: u64,
}

impl<R: Read + Seek> VorbisReader<R> {
    /// Reads the headers and the last page.
    pub(crate) fn open(mut rdr: R) -> Result<Self, Error> {
        let mut magic = [0u8; 4];
        let is_ogg = rdr.read_exact(&mut magic).is_ok() && &magic == b"OggS";
        if !is_ogg {
            return Err(Error::Unsupported("it is not an Ogg file".into()));
        }
        let (total, last_serial) = last_page(&mut rdr)?;
        let mut pr = PacketReader::new(rdr);
        let p = pr.read_packet_expected()?;
        if !p.first_in_stream() {
            return Err(Error::Damaged(
                "its first packet does not start a stream".into(),
            ));
        }
        let serial = p.stream_serial();
        let ident = read_header_ident(&p.data).map_err(header)?;
        if ident.audio_sample_rate == 0 {
            return Err(Error::Damaged("its sample rate is 0".into()));
        }
        if !(1..=2).contains(&ident.audio_channels) {
            return Err(Error::Unsupported(format!(
                "it has {} channels; only mono and stereo play",
                ident.audio_channels
            )));
        }
        let p = pr.read_packet_expected()?;
        if p.first_in_stream() || p.stream_serial() != serial {
            return Err(several());
        }
        read_header_comment(&p.data).map_err(header)?;
        let p = pr.read_packet_expected()?;
        if p.stream_serial() != serial {
            return Err(several());
        }
        let blocks = (ident.blocksize_0, ident.blocksize_1);
        let setup = read_header_setup(&p.data, ident.audio_channels, blocks).map_err(header)?;
        if last_serial != serial {
            return Err(several());
        }
        // The setup header ends its page, so this is the first audio page.
        let first_audio = pr.seek_bytes(SeekFrom::Current(0))?;
        Ok(VorbisReader {
            pr,
            n0: 1 << ident.blocksize_0,
            n1: 1 << ident.blocksize_1,
            ident,
            setup,
            serial,
            pwr: PreviousWindowRight::new(),
            prev_n: None,
            rel_std: 0,
            offset: Some(0),
            first_audio,
            total,
        })
    }

    /// The format and length.
    pub(crate) fn info(&self) -> Info {
        Info {
            channels: u16::from(self.ident.audio_channels),
            sample_rate: self.ident.audio_sample_rate,
            frames: self.total,
        }
    }

    /// The most frames one packet gives.
    pub(crate) fn long_block(&self) -> usize {
        self.n1 as usize
    }

    fn reset(&mut self) {
        self.pwr = PreviousWindowRight::new();
        self.prev_n = None;
        self.rel_std = 0;
    }

    /// Back to the first frame.
    pub(crate) fn rewind(&mut self) -> Result<(), Error> {
        self.pr.seek_bytes(SeekFrom::Start(self.first_audio))?;
        self.reset();
        self.offset = Some(0);
        Ok(())
    }

    /// Appends the next packet's frames to `out`; `false` at the end of the
    /// stream.
    pub(crate) fn read(&mut self, out: &mut Vec<Frame>) -> Result<bool, Error> {
        let Some(c) = self.next_chunk()? else {
            return Ok(false);
        };
        c.frames_into(0, out);
        Ok(true)
    }

    /// Every frame from here to the end.
    pub(crate) fn read_all(&mut self) -> Result<Vec<Frame>, Error> {
        let mut frames = Vec::new();
        while self.read(&mut frames)? {}
        Ok(frames)
    }

    /// Moves to `frame` (the end, past it) and appends the frames from there
    /// to the end of their packet to `out`: a page seek two long blocks early,
    /// so the packet that primes the decoder ends before the target, then
    /// decoding forward until a page end places the stream.
    pub(crate) fn seek(&mut self, frame: u64, out: &mut Vec<Frame>) -> Result<(), Error> {
        let frame = frame.min(self.total);
        let mut margin = 2 * self.n1 as u64;
        loop {
            match self.try_seek(frame, margin, out) {
                // From earlier: at most from the start, which is always placed.
                Err(Error::Unanchored) => margin = margin.saturating_mul(4),
                other => return other,
            }
        }
    }

    fn try_seek(&mut self, frame: u64, margin: u64, out: &mut Vec<Frame>) -> Result<(), Error> {
        let goal = frame.saturating_sub(margin);
        if goal < 2 * self.n1 as u64 {
            self.rewind()?;
        } else {
            self.pr.seek_absgp(Some(self.serial), goal)?;
            self.reset();
            self.offset = None;
        }
        let target = i64::try_from(frame).unwrap_or(i64::MAX);
        let mut pending = Vec::new();
        while let Some(c) = self.next_chunk()? {
            pending.push(c);
            let Some(o) = self.offset else {
                continue;
            };
            for c in pending.drain(..) {
                let start = c.rel_start.saturating_add(o);
                let end = start.saturating_add(c.len() as i64);
                if end > target {
                    let skip = usize::try_from(target.saturating_sub(start)).unwrap_or(0);
                    c.frames_into(skip, out);
                }
            }
            if !out.is_empty() {
                return Ok(());
            }
        }
        // The target is the end.
        Ok(())
    }

    /// The next packet's samples, or `None` at the end of the stream.
    fn next_chunk(&mut self) -> Result<Option<Chunk>, Error> {
        loop {
            let Some(p) = self.pr.read_packet()? else {
                return Ok(None);
            };
            if p.first_in_stream() {
                // Another stream starts, even under the same serial number.
                return Err(several());
            }
            if p.stream_serial() != self.serial {
                continue;
            }
            if p.data.first().is_none_or(|b| b & 1 == 1) {
                // An empty packet, or a header.
                continue;
            }
            let would = get_decoded_sample_count(&self.ident, &self.setup, &p.data)? as i64;
            let n = if would == self.n0 / 2 {
                self.n0
            } else {
                self.n1
            };
            let mut samples: Vec<Vec<f32>> =
                read_audio_packet_generic(&self.ident, &self.setup, &p.data, &mut self.pwr)?;
            match self.prev_n {
                None => self.rel_std = 0,
                Some(prev) => self.rel_std += prev / 4 + n / 4,
            }
            let lws = if n == self.n1 && self.prev_n == Some(self.n0) {
                (self.n1 - self.n0) / 4
            } else {
                0
            };
            self.prev_n = Some(n);
            let rel_start = self.rel_std - n / 2 + lws;
            let granule = p.absgp_page();
            if p.last_in_stream() {
                // The last page's granule is where the stream ends, which may
                // cut the last packet short; it places nothing.
                let Some(o) = self.offset else {
                    return Err(Error::Unanchored);
                };
                let start = rel_start.saturating_add(o);
                let keep = usize::try_from((granule as i64).saturating_sub(start)).unwrap_or(0);
                for ch in &mut samples {
                    ch.truncate(keep);
                }
            } else if p.last_in_page() && granule != u64::MAX {
                self.offset = Some((granule as i64).saturating_sub(self.rel_std));
            }
            if samples.first().is_none_or(Vec::is_empty) {
                continue;
            }
            return Ok(Some(Chunk { rel_start, samples }));
        }
    }
}

#[cfg(test)]
mod tests {
    use std::io::Cursor;

    use super::*;

    // The files `tests/ogg.rs` describes.
    const SPARSE: &[u8] = include_bytes!("../tests/data/sparse180.ogg");
    const STEREO: &[u8] = include_bytes!("../tests/data/stereo1.ogg");
    const THREE: &[u8] = include_bytes!("../tests/data/three.ogg");

    fn reader(bytes: &[u8]) -> VorbisReader<Cursor<&[u8]>> {
        match VorbisReader::open(Cursor::new(bytes)) {
            Ok(r) => r,
            Err(e) => panic!("{e}"),
        }
    }

    fn refusal(bytes: Vec<u8>) -> Error {
        match VorbisReader::open(Cursor::new(&bytes[..])) {
            Ok(_) => panic!("opened"),
            Err(e) => e,
        }
    }

    /// The same frames, bit for bit.
    fn same(a: &[Frame], b: &[Frame]) -> bool {
        a.len() == b.len()
            && a.iter().zip(b).all(|(x, y)| {
                x.left.to_bits() == y.left.to_bits() && x.right.to_bits() == y.right.to_bits()
            })
    }

    #[test]
    fn the_headers_give_the_format_and_length() {
        let info = reader(SPARSE).info();
        assert_eq!(
            info,
            Info {
                channels: 1,
                sample_rate: 22_050,
                frames: 180 * 22_050
            }
        );
        assert_eq!(info.seconds(), 180.0);
        assert_eq!(
            reader(STEREO).info(),
            Info {
                channels: 2,
                sample_rate: 44_100,
                frames: 44_100
            }
        );
    }

    #[test]
    fn a_whole_decode_gives_every_frame() {
        let frames = reader(SPARSE).read_all().unwrap();
        assert_eq!(frames.len(), 180 * 22_050);
        // A tone at 0 s and silence between the tones.
        let peak = |from: usize, to: usize| {
            frames[from..to]
                .iter()
                .map(|f| f.left.abs())
                .fold(0.0f32, f32::max)
        };
        assert!(peak(0, 5_000) > 0.2, "{}", peak(0, 5_000));
        assert!(peak(11_025, 33_075) < 0.02, "{}", peak(11_025, 33_075));
        assert!(frames.iter().all(|f| f.left == f.right));

        let stereo = reader(STEREO).read_all().unwrap();
        assert_eq!(stereo.len(), 44_100);
        let rms = |pick: fn(&Frame) -> f32| {
            (stereo.iter().map(|f| pick(f).powi(2)).sum::<f32>() / stereo.len() as f32).sqrt()
        };
        // 0.5 and 0.25 sines: RMS 0.354 and 0.177.
        assert!(
            (rms(|f| f.left) - 0.354).abs() < 0.02,
            "{}",
            rms(|f| f.left)
        );
        assert!(
            (rms(|f| f.right) - 0.177).abs() < 0.02,
            "{}",
            rms(|f| f.right)
        );
    }

    #[test]
    fn a_seek_lands_on_the_exact_frame() {
        let whole = reader(SPARSE).read_all().unwrap();
        let total = whole.len() as u64;
        let mut r = reader(SPARSE);
        let mut targets = vec![
            0,
            1,
            255,
            256,
            4_095,
            4_096,
            22_050,
            total / 2,
            total - 4_096,
            total - 1,
            total,
            total + 1,
            u64::MAX,
        ];
        let mut x = 0x2545_f491_4f6c_dd1d_u64;
        for _ in 0..40 {
            x ^= x << 13;
            x ^= x >> 7;
            x ^= x << 17;
            targets.push(x % total);
        }
        for target in targets {
            let mut out = Vec::new();
            r.seek(target, &mut out).unwrap();
            while out.len() < 5_000 && r.read(&mut out).unwrap() {}
            let at = target.min(total) as usize;
            let want = &whole[at..(at + out.len()).min(whole.len())];
            assert!(same(&out, want), "seek to {target}");
            // Read on to the end from there: the frames still match.
            if target == total / 2 {
                while r.read(&mut out).unwrap() {}
                assert!(
                    same(&out, &whole[at..]),
                    "to the end after a seek to {target}"
                );
            }
        }
    }

    #[test]
    fn a_rewind_repeats_the_first_pass() {
        let mut r = reader(SPARSE);
        let first = r.read_all().unwrap();
        r.rewind().unwrap();
        let second = r.read_all().unwrap();
        assert!(same(&first, &second));
        let mut out = Vec::new();
        r.seek(1_000, &mut out).unwrap();
        r.rewind().unwrap();
        assert!(same(&r.read_all().unwrap(), &first));
    }

    #[test]
    fn files_it_does_not_play_are_refused() {
        let unsupported = |bytes: Vec<u8>, why: &str| match refusal(bytes) {
            Error::Unsupported(w) => assert!(w.contains(why), "{w}"),
            e => panic!("{e}"),
        };
        unsupported(b"RIFF\0\0\0\0WAVEfmt ".to_vec(), "not an Ogg file");
        unsupported(THREE.to_vec(), "3 channels");
        unsupported([STEREO, THREE].concat(), "chained");
    }

    #[test]
    fn damaged_files_are_damaged() {
        let damaged = |bytes: Vec<u8>| match refusal(bytes) {
            Error::Damaged(_) => {}
            e => panic!("{e}"),
        };
        damaged(SPARSE[..200].to_vec());
        damaged(b"OggS".to_vec());
        let mut header = SPARSE.to_vec();
        header[40] ^= 0xff;
        damaged(header);
        // Two streams under one serial number break the format: refused
        // where the second starts.
        let twice = [STEREO, STEREO].concat();
        match reader(&twice).read_all() {
            Err(Error::Damaged(_)) => {}
            Err(e) => panic!("{e}"),
            Ok(frames) => panic!("{} frames", frames.len()),
        }
    }
}

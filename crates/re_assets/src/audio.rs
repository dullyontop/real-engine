//! What an audio file's headers say (ADR-0066 §6): its format, channels, rate
//! and length, for the editor's asset browser and for `real check` and
//! `real build`, which refuse a file the engine does not play. Nothing is
//! decoded, so `re_assets` takes no decoder: `re_audio` plays the files.

use re_core::{Diagnostic, Location};

use crate::codes;

/// An audio file's container and codec.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub enum AudioFormat {
    /// A RIFF WAVE file of PCM or float samples.
    Wav,
    /// An Ogg file holding one Vorbis stream.
    OggVorbis,
}

/// What an audio file's headers say.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct AudioInfo {
    /// The container and codec.
    pub format: AudioFormat,
    /// 1 (mono) or 2 (stereo).
    pub channels: u16,
    /// Frames per second, above 0.
    pub sample_rate: u32,
    /// The length in frames (one sample per channel).
    pub frames: u64,
}

impl AudioInfo {
    /// The length in seconds.
    pub fn seconds(&self) -> f64 {
        self.frames as f64 / f64::from(self.sample_rate.max(1))
    }
}

/// True for a file the engine plays as audio: `.wav` or `.ogg`, in any case.
pub fn is_audio(path: &str) -> bool {
    audio_extension(path).is_some()
}

/// The extension of an audio file, lowercase.
fn audio_extension(path: &str) -> Option<&'static str> {
    let (_, ext) = path.rsplit_once('.')?;
    ["wav", "ogg"]
        .into_iter()
        .find(|e| ext.eq_ignore_ascii_case(e))
}

/// Reads the headers of the audio file `path`, whose bytes are `bytes`: a WAV
/// file's `fmt ` and `data` chunks, or an Ogg file's first page and its last.
///
/// # Errors
///
/// [`codes::UNSUPPORTED_AUDIO`] for a file the engine does not play: not
/// `.wav` or `.ogg`, a WAV file it cannot read or whose samples are not PCM or
/// float, an Ogg stream that is not Vorbis, several streams in one Ogg file,
/// or more than two channels.
pub fn audio_info(path: &str, bytes: &[u8]) -> Result<AudioInfo, Diagnostic> {
    let refused = |why: String, hint: &str| {
        Diagnostic::error(
            codes::UNSUPPORTED_AUDIO,
            format!("`{path}` is not an audio file the engine plays: {why}"),
        )
        .at(Location::file(path))
        .with_hint(hint)
    };
    let (info, hint) = match audio_extension(path) {
        Some("wav") => (wav(bytes), WAV_HINT),
        Some(_) => (ogg(bytes), OGG_HINT),
        None => (Err("it is not a `.wav` or `.ogg` file".into()), OGG_HINT),
    };
    let info = info.map_err(|why| refused(why, hint))?;
    if !(1..=2).contains(&info.channels) {
        return Err(refused(
            format!(
                "it has {} channels; only mono and stereo play",
                info.channels
            ),
            "mix it down to stereo, for example `ffmpeg -i in.ogg -ac 2 -c:a libvorbis out.ogg`",
        ));
    }
    Ok(info)
}

const WAV_HINT: &str = "save it as 8-, 16-, 24- or 32-bit PCM or 32- or 64-bit float WAV, \
                        or as Ogg Vorbis";
const OGG_HINT: &str = "the engine plays WAV and Ogg Vorbis, mono or stereo, one stream per \
                        file: convert it, for example `ffmpeg -i in.flac -ac 2 -c:a libvorbis \
                        out.ogg`";

fn u16_at(b: &[u8], i: usize) -> u16 {
    u16::from_le_bytes([b[i], b[i + 1]])
}

fn u32_at(b: &[u8], i: usize) -> u32 {
    u32::from_le_bytes([b[i], b[i + 1], b[i + 2], b[i + 3]])
}

/// A WAV file's format and length, by the rules of `re_audio`'s reader.
fn wav(bytes: &[u8]) -> Result<AudioInfo, String> {
    if bytes.len() < 12 || &bytes[0..4] != b"RIFF" || &bytes[8..12] != b"WAVE" {
        return Err("it has no RIFF WAVE header".into());
    }
    let mut format = None;
    let mut data = None;
    let mut rest = &bytes[12..];
    while rest.len() >= 8 {
        let len = u32_at(rest, 4) as usize;
        let end = 8usize.saturating_add(len);
        // A data length past the end of the file plays what is there.
        let body = &rest[8..end.min(rest.len())];
        match &rest[0..4] {
            b"fmt " => format = Some(body),
            b"data" => data = Some(body.len()),
            _ => {}
        }
        // Chunks are padded to an even length.
        let next = end.saturating_add(len & 1);
        if next >= rest.len() {
            break;
        }
        rest = &rest[next..];
    }
    let fmt = format.ok_or("it has no `fmt ` chunk")?;
    let data = data.ok_or("it has no `data` chunk")?;
    if fmt.len() < 16 {
        return Err("its `fmt ` chunk is too short".into());
    }
    let mut tag = u16_at(fmt, 0);
    // WAVE_FORMAT_EXTENSIBLE: the format is the sub-format GUID's first two bytes.
    if tag == 0xfffe && fmt.len() >= 26 {
        tag = u16_at(fmt, 24);
    }
    let channels = u16_at(fmt, 2);
    let sample_rate = u32_at(fmt, 4);
    let bits = u16_at(fmt, 14);
    match (tag, bits) {
        (1, 8 | 16 | 24 | 32) | (3, 32 | 64) => {}
        (1 | 3, _) => return Err(format!("its {bits}-bit samples are not supported")),
        _ => {
            return Err(format!("its sample format {tag:#06x} is not PCM or float"));
        }
    }
    if sample_rate == 0 {
        return Err("its sample rate is 0".into());
    }
    let frame_bytes = usize::from(bits / 8) * usize::from(channels.max(1));
    Ok(AudioInfo {
        format: AudioFormat::Wav,
        channels,
        sample_rate,
        frames: (data / frame_bytes) as u64,
    })
}

/// An Ogg page's header: the fields this module reads.
struct Page {
    /// The first page of a stream.
    first: bool,
    granule: u64,
    serial: u32,
    /// The bytes of the header, with its segment table.
    header_len: usize,
    /// The bytes of the packets that follow.
    body_len: usize,
}

/// The page at the start of `b`, if it has a whole header.
fn page(b: &[u8]) -> Option<Page> {
    if b.len() < 27 || &b[0..4] != b"OggS" {
        return None;
    }
    let segments = usize::from(b[26]);
    let table = b.get(27..27 + segments)?;
    Some(Page {
        first: b[5] & 2 != 0,
        granule: u64::from_le_bytes(b[6..14].try_into().ok()?),
        serial: u32_at(b, 14),
        header_len: 27 + segments,
        body_len: table.iter().map(|&s| usize::from(s)).sum(),
    })
}

/// The codec an Ogg stream's first packet names.
fn codec(packet: &[u8]) -> &'static str {
    let starts = |magic: &[u8]| packet.starts_with(magic);
    if starts(b"\x01vorbis") {
        "Vorbis"
    } else if starts(b"OpusHead") {
        "Opus"
    } else if starts(b"\x7fFLAC") {
        "FLAC"
    } else if starts(b"Speex   ") {
        "Speex"
    } else if starts(b"\x80theora") {
        "Theora"
    } else {
        "an unknown codec"
    }
}

/// An Ogg Vorbis file's format, from its first page, and its length, from
/// the last page's granule position.
fn ogg(bytes: &[u8]) -> Result<AudioInfo, String> {
    let first = page(bytes).ok_or("it is not an Ogg file")?;
    if !first.first {
        return Err("its first page does not start a stream".into());
    }
    let packet = bytes
        .get(first.header_len..first.header_len + first.body_len)
        .ok_or("its first page is cut short")?;
    match codec(packet) {
        "Vorbis" => {}
        other => return Err(format!("its stream is {other}, not Vorbis")),
    }
    // The identification header: version, channels, rate.
    if packet.len() < 30 {
        return Err("its Vorbis identification header is cut short".into());
    }
    if u32_at(packet, 7) != 0 {
        return Err("its Vorbis version is not 0".into());
    }
    let channels = u16::from(packet[11]);
    let sample_rate = u32_at(packet, 12);
    if sample_rate == 0 {
        return Err("its sample rate is 0".into());
    }
    // Streams are multiplexed when another starts on the second page.
    let second = page(&bytes[first.header_len + first.body_len..]);
    if second.is_some_and(|p| p.first) {
        return Err("it holds several multiplexed streams".into());
    }
    // The last page: where the stream ends, and whose it is.
    let from = bytes.len().saturating_sub(64 << 10);
    let last = bytes[from..]
        .windows(4)
        .rposition(|w| w == b"OggS")
        .and_then(|at| page(&bytes[from + at..]))
        .ok_or("it has no Ogg page in its last 64 KiB")?;
    if last.serial != first.serial {
        return Err("it holds several chained streams".into());
    }
    if last.granule == u64::MAX {
        return Err("its last page has no granule position".into());
    }
    Ok(AudioInfo {
        format: AudioFormat::OggVorbis,
        channels,
        sample_rate,
        frames: last.granule,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A WAV file's bytes: `bits`-bit samples of `tag`, `frames` silent frames.
    fn wave(tag: u16, channels: u16, rate: u32, bits: u16, frames: usize) -> Vec<u8> {
        let data = frames * usize::from(channels) * usize::from(bits / 8);
        let mut b = Vec::new();
        b.extend_from_slice(b"RIFF");
        b.extend_from_slice(&(36 + data as u32).to_le_bytes());
        b.extend_from_slice(b"WAVEfmt ");
        b.extend_from_slice(&16u32.to_le_bytes());
        b.extend_from_slice(&tag.to_le_bytes());
        b.extend_from_slice(&channels.to_le_bytes());
        b.extend_from_slice(&rate.to_le_bytes());
        let align = channels * bits / 8;
        b.extend_from_slice(&(rate * u32::from(align)).to_le_bytes());
        b.extend_from_slice(&align.to_le_bytes());
        b.extend_from_slice(&bits.to_le_bytes());
        b.extend_from_slice(b"data");
        b.extend_from_slice(&(data as u32).to_le_bytes());
        b.resize(b.len() + data, 0);
        b
    }

    fn refusal(path: &str, bytes: &[u8]) -> String {
        let d = audio_info(path, bytes).unwrap_err();
        assert_eq!(d.code, codes::UNSUPPORTED_AUDIO);
        d.message
    }

    #[test]
    fn wav_headers_give_the_format_and_length() {
        assert_eq!(
            audio_info("a.wav", &wave(1, 2, 44_100, 16, 4_410)).unwrap(),
            AudioInfo {
                format: AudioFormat::Wav,
                channels: 2,
                sample_rate: 44_100,
                frames: 4_410
            }
        );
        let info = audio_info("A.WAV", &wave(3, 1, 48_000, 32, 24_000)).unwrap();
        assert_eq!((info.frames, info.seconds()), (24_000, 0.5));

        assert!(refusal("a.wav", b"RIFF").contains("RIFF WAVE"));
        assert!(refusal("a.wav", &wave(2, 1, 8_000, 4, 10)).contains("not PCM or float"));
        assert!(refusal("a.wav", &wave(1, 1, 8_000, 12, 10)).contains("12-bit"));
        assert!(refusal("a.wav", &wave(1, 6, 48_000, 16, 10)).contains("6 channels"));
        assert!(refusal("a.wav", &wave(1, 1, 0, 16, 10)).contains("rate is 0"));
        assert!(refusal("a.mp3", b"ID3").contains("`.wav` or `.ogg`"));
    }

    // The Ogg files are `re_audio`'s test files; `re_audio/tests/ogg.rs` says
    // how they were made.
    const SPARSE: &[u8] = include_bytes!("../../re_audio/tests/data/sparse180.ogg");
    const STEREO: &[u8] = include_bytes!("../../re_audio/tests/data/stereo1.ogg");
    const THREE: &[u8] = include_bytes!("../../re_audio/tests/data/three.ogg");

    #[test]
    fn ogg_headers_give_the_format_and_length() {
        assert_eq!(
            audio_info("music/a.ogg", SPARSE).unwrap(),
            AudioInfo {
                format: AudioFormat::OggVorbis,
                channels: 1,
                sample_rate: 22_050,
                frames: 180 * 22_050
            }
        );
        let info = audio_info("b.OGG", STEREO).unwrap();
        assert_eq!((info.channels, info.seconds()), (2, 1.0));
        assert!(is_audio("b.OGG") && is_audio("x/y.wav") && !is_audio("song.mp3"));
    }

    #[test]
    fn ogg_files_it_does_not_play_are_refused() {
        assert!(refusal("a.ogg", THREE).contains("3 channels"));
        assert!(refusal("a.ogg", &[STEREO, THREE].concat()).contains("chained"));
        assert!(refusal("a.ogg", b"RIFF\0\0\0\0WAVE").contains("not an Ogg file"));
        assert!(refusal("a.ogg", &SPARSE[..40]).contains("cut short"));
        // Another codec, named.
        let mut opus = STEREO.to_vec();
        let at = opus.windows(7).position(|w| w == b"\x01vorbis").unwrap();
        opus[at..at + 8].copy_from_slice(b"OpusHead");
        assert!(refusal("a.ogg", &opus).contains("Opus, not Vorbis"));
        // Two streams that start together.
        let first = page(STEREO).unwrap();
        let mut muxed = STEREO[..first.header_len + first.body_len].to_vec();
        muxed.extend_from_slice(&THREE[..page(THREE).map(|p| p.header_len + p.body_len).unwrap()]);
        muxed.extend_from_slice(&STEREO[first.header_len + first.body_len..]);
        assert!(refusal("a.ogg", &muxed).contains("multiplexed"));
    }
}

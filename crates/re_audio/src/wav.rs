//! A small WAV reader: RIFF `WAVE` with PCM (8, 16, 24 or 32 bit) or 32/64-bit
//! float samples, mono or stereo.
//!
//! The engine builds kira without any decoder feature (its `wav` feature would
//! enable symphonia's RIFF container but not its PCM codec, so it could not
//! decode a WAV file anyway). Decoding WAV here keeps the runtime free of the
//! MPL-licensed symphonia crates (ADR-0019).

use std::sync::Arc;

use kira::Frame;
use kira::sound::static_sound::{StaticSoundData, StaticSoundSettings};

/// Why a WAV file could not be decoded.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct WavError(pub String);

impl std::fmt::Display for WavError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

fn err<T>(msg: impl Into<String>) -> Result<T, WavError> {
    Err(WavError(msg.into()))
}

#[derive(Clone, Copy)]
enum SampleKind {
    Int,
    Float,
}

struct Format {
    kind: SampleKind,
    channels: u16,
    sample_rate: u32,
    bits: u16,
}

/// Decodes a WAV file's bytes into kira sound data.
pub(crate) fn decode(bytes: &[u8]) -> Result<StaticSoundData, WavError> {
    if bytes.len() < 12 || &bytes[0..4] != b"RIFF" || &bytes[8..12] != b"WAVE" {
        return err("not a WAV file (no RIFF/WAVE header)");
    }
    let mut format = None;
    let mut data = None;
    let mut rest = &bytes[12..];
    while rest.len() >= 8 {
        let id = &rest[0..4];
        let len = u32::from_le_bytes([rest[4], rest[5], rest[6], rest[7]]) as usize;
        let body_end = 8 + len;
        // Some writers give a data length past the end of the file; read what is there.
        let body = &rest[8..body_end.min(rest.len())];
        match id {
            b"fmt " => format = Some(parse_format(body)?),
            b"data" => data = Some(body),
            _ => {}
        }
        // Chunks are padded to an even length.
        let next = body_end + (len & 1);
        if next >= rest.len() {
            break;
        }
        rest = &rest[next..];
    }
    let Some(format) = format else {
        return err("no `fmt ` chunk");
    };
    let Some(data) = data else {
        return err("no `data` chunk");
    };
    let width = usize::from(format.bits / 8);
    let frame_bytes = width * usize::from(format.channels);
    let sample = |s: &[u8]| -> f32 {
        match (format.kind, format.bits) {
            (SampleKind::Int, 8) => (f32::from(s[0]) - 128.0) / 128.0,
            (SampleKind::Int, 16) => f32::from(i16::from_le_bytes([s[0], s[1]])) / 32_768.0,
            (SampleKind::Int, 24) => {
                (i32::from_le_bytes([0, s[0], s[1], s[2]]) >> 8) as f32 / 8_388_608.0
            }
            (SampleKind::Int, _) => {
                i32::from_le_bytes([s[0], s[1], s[2], s[3]]) as f32 / 2_147_483_648.0
            }
            (SampleKind::Float, 32) => f32::from_le_bytes([s[0], s[1], s[2], s[3]]),
            (SampleKind::Float, _) => {
                f64::from_le_bytes([s[0], s[1], s[2], s[3], s[4], s[5], s[6], s[7]]) as f32
            }
        }
    };
    let frames: Arc<[Frame]> = data
        .chunks_exact(frame_bytes)
        .map(|f| {
            if format.channels == 1 {
                Frame::from_mono(sample(f))
            } else {
                Frame::new(sample(&f[..width]), sample(&f[width..]))
            }
        })
        .collect();
    Ok(StaticSoundData {
        sample_rate: format.sample_rate,
        frames,
        settings: StaticSoundSettings::default(),
        slice: None,
    })
}

fn parse_format(body: &[u8]) -> Result<Format, WavError> {
    if body.len() < 16 {
        return err("the `fmt ` chunk is too short");
    }
    let u16_at = |i: usize| u16::from_le_bytes([body[i], body[i + 1]]);
    let mut tag = u16_at(0);
    // WAVE_FORMAT_EXTENSIBLE: the real format is the first two bytes of the sub-format GUID.
    if tag == 0xFFFE && body.len() >= 26 {
        tag = u16_at(24);
    }
    let channels = u16_at(2);
    let sample_rate = u32::from_le_bytes([body[4], body[5], body[6], body[7]]);
    let bits = u16_at(14);
    let kind = match (tag, bits) {
        (1, 8 | 16 | 24 | 32) => SampleKind::Int,
        (3, 32 | 64) => SampleKind::Float,
        (1 | 3, _) => return err(format!("{bits}-bit samples are not supported")),
        _ => {
            return err(format!(
                "sample format {tag:#06x} is not supported (only PCM and float)"
            ));
        }
    };
    if !(1..=2).contains(&channels) {
        return err(format!(
            "{channels} channels are not supported (only mono and stereo)"
        ));
    }
    if sample_rate == 0 {
        return err("the sample rate is 0");
    }
    Ok(Format {
        kind,
        channels,
        sample_rate,
        bits,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn wav(tag: u16, channels: u16, bits: u16, data: &[u8]) -> Vec<u8> {
        let mut out = Vec::new();
        out.extend_from_slice(b"RIFF");
        out.extend_from_slice(&(4 + 24 + 8 + data.len() as u32).to_le_bytes());
        out.extend_from_slice(b"WAVE");
        out.extend_from_slice(b"LIST\x04\x00\x00\x00INFO"); // an ignored chunk
        out.extend_from_slice(b"fmt \x10\x00\x00\x00");
        out.extend_from_slice(&tag.to_le_bytes());
        out.extend_from_slice(&channels.to_le_bytes());
        out.extend_from_slice(&8000u32.to_le_bytes());
        out.extend_from_slice(&(8000 * u32::from(channels * bits / 8)).to_le_bytes());
        out.extend_from_slice(&(channels * bits / 8).to_le_bytes());
        out.extend_from_slice(&bits.to_le_bytes());
        out.extend_from_slice(b"data");
        out.extend_from_slice(&(data.len() as u32).to_le_bytes());
        out.extend_from_slice(data);
        out
    }

    #[test]
    fn decodes_each_sample_format() {
        let d = decode(&wav(1, 1, 16, &[0x00, 0x40, 0x00, 0xC0])).unwrap();
        assert_eq!(d.sample_rate, 8000);
        assert_eq!(d.frames.len(), 2);
        assert_eq!(d.frames[0], Frame::from_mono(0.5));
        assert_eq!(d.frames[1], Frame::from_mono(-0.5));

        let d = decode(&wav(1, 2, 8, &[255, 0])).unwrap();
        assert_eq!(d.frames.len(), 1);
        assert!((d.frames[0].left - 127.0 / 128.0).abs() < 1e-6 && d.frames[0].right == -1.0);

        let d = decode(&wav(1, 1, 24, &[0, 0, 0x40])).unwrap();
        assert_eq!(d.frames[0], Frame::from_mono(0.5));

        let d = decode(&wav(3, 1, 32, &0.25f32.to_le_bytes())).unwrap();
        assert_eq!(d.frames[0], Frame::from_mono(0.25));
    }

    #[test]
    fn rejects_what_it_cannot_play() {
        assert!(decode(b"RIFF\0\0\0\0WAVX").is_err());
        assert!(
            decode(&wav(2, 1, 4, &[0, 0]))
                .unwrap_err()
                .0
                .contains("0x0002")
        );
        assert!(
            decode(&wav(1, 6, 16, &[0; 12]))
                .unwrap_err()
                .0
                .contains("6 channels")
        );
        assert!(decode(&wav(1, 1, 12, &[0; 2])).is_err());
        let mut no_data = wav(1, 1, 16, &[]);
        no_data.truncate(no_data.len() - 8);
        assert!(decode(&no_data).unwrap_err().0.contains("data"));
    }
}

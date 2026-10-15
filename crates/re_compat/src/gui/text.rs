//! Text of `TextLabel`, `TextButton` and `TextBox`: rich-text runs, sizing
//! (`TextScaled`), wrapping and truncation, laid out with egui's fonts.

use std::sync::Arc;

use egui::epaint::text::{ByteIndex, LayoutJob, LayoutSection, TextFormat, TextWrapping};
use egui::{Align, FontId, Galley};
use glam::Vec2;

use super::fonts;
use super::props::Rgba;

/// What a text element asks for.
#[derive(Clone, Debug)]
pub(crate) struct TextStyle {
    pub text: String,
    pub family: String,
    pub weight: String,
    pub italic: bool,
    pub size: f32,
    pub color: Rgba,
    pub scaled: bool,
    pub wrapped: bool,
    pub truncate: bool,
    pub rich: bool,
    pub line_height: f32,
    pub x_align: f32,
}

/// One run of text with its own weight and colour.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct Run {
    pub text: String,
    pub bold: bool,
    pub color: Option<[f32; 3]>,
}

fn hex_color(s: &str) -> Option<[f32; 3]> {
    let h = s.strip_prefix('#')?;
    if h.len() != 6 {
        return None;
    }
    let v = u32::from_str_radix(h, 16).ok()?;
    Some([
        ((v >> 16) & 255) as f32 / 255.0,
        ((v >> 8) & 255) as f32 / 255.0,
        (v & 255) as f32 / 255.0,
    ])
}

fn rgb_color(s: &str) -> Option<[f32; 3]> {
    let inner = s.strip_prefix("rgb(")?.strip_suffix(')')?;
    let parts: Vec<f32> = inner
        .split(',')
        .map(|p| p.trim().parse::<f32>().ok())
        .collect::<Option<_>>()?;
    match parts[..] {
        [r, g, b] => Some([r / 255.0, g / 255.0, b / 255.0]),
        _ => None,
    }
}

/// The colour of a `<font color=...>` tag.
fn font_tag_color(tag: &str) -> Option<[f32; 3]> {
    let at = tag.find("color")?;
    let rest = tag[at + 5..].trim_start().strip_prefix('=')?.trim_start();
    let q = rest.chars().next()?;
    if q != '"' && q != '\'' {
        return None;
    }
    let body = &rest[1..];
    let end = body.find(q)?;
    let v = body[..end].trim();
    hex_color(v).or_else(|| rgb_color(&v.replace(' ', "")))
}

fn unescape(s: &str) -> String {
    s.replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&quot;", "\"")
        .replace("&apos;", "'")
        .replace("&amp;", "&")
}

/// Splits rich text into runs: `<b>`, `<font color=...>` and the escapes
/// `&lt; &gt; &quot; &apos; &amp;`. Other tags stay as text.
pub(crate) fn runs(text: &str, rich: bool) -> Vec<Run> {
    if !rich {
        return vec![Run {
            text: text.to_string(),
            bold: false,
            color: None,
        }];
    }
    let mut out: Vec<Run> = Vec::new();
    let mut bold = 0usize;
    let mut colors: Vec<[f32; 3]> = Vec::new();
    let mut buf = String::new();
    let mut rest = text;
    let flush = |buf: &mut String, out: &mut Vec<Run>, bold: usize, colors: &[[f32; 3]]| {
        if !buf.is_empty() {
            out.push(Run {
                text: unescape(buf),
                bold: bold > 0,
                color: colors.last().copied(),
            });
            buf.clear();
        }
    };
    while let Some(lt) = rest.find('<') {
        buf.push_str(&rest[..lt]);
        let after = &rest[lt..];
        let Some(gt) = after.find('>') else {
            buf.push_str(after);
            rest = "";
            break;
        };
        let tag = &after[1..gt];
        let lower = tag.trim().to_ascii_lowercase();
        let known = if lower == "b" {
            flush(&mut buf, &mut out, bold, &colors);
            bold += 1;
            true
        } else if lower == "/b" {
            flush(&mut buf, &mut out, bold, &colors);
            bold = bold.saturating_sub(1);
            true
        } else if lower.starts_with("font ") || lower == "font" {
            flush(&mut buf, &mut out, bold, &colors);
            let c = font_tag_color(tag)
                .or_else(|| colors.last().copied())
                .unwrap_or([f32::NAN; 3]);
            colors.push(c);
            true
        } else if lower == "/font" {
            flush(&mut buf, &mut out, bold, &colors);
            colors.pop();
            true
        } else {
            false
        };
        if !known {
            buf.push_str(&after[..=gt]);
        }
        rest = &after[gt + 1..];
    }
    buf.push_str(rest);
    flush(&mut buf, &mut out, bold, &colors);
    // A <font> without a colour keeps the outer one (NaN marks "none").
    for r in &mut out {
        if r.color.is_some_and(|c| c[0].is_nan()) {
            r.color = None;
        }
    }
    out
}

fn job(style: &TextStyle, size: f32, wrap_width: Option<f32>, max_rows: usize) -> LayoutJob {
    let mut job = LayoutJob::default();
    for run in runs(&style.text, style.rich) {
        let weight = if run.bold {
            "Bold"
        } else {
            style.weight.as_str()
        };
        let color = match run.color {
            Some(c) => Rgba([c[0], c[1], c[2], style.color.0[3]]),
            None => style.color,
        };
        let start = job.text.len();
        job.text.push_str(&run.text);
        job.sections.push(LayoutSection {
            leading_space: 0.0,
            byte_range: ByteIndex(start)..ByteIndex(job.text.len()),
            format: TextFormat {
                font_id: FontId::new(size.max(1.0), fonts::egui_family(&style.family, weight)),
                color: color.egui(),
                italics: style.italic,
                line_height: (style.line_height != 1.0)
                    .then(|| size * 1.2 * style.line_height.max(0.0)),
                ..Default::default()
            },
        });
    }
    job.wrap = TextWrapping {
        max_width: wrap_width.unwrap_or(f32::INFINITY),
        max_rows,
        break_anywhere: max_rows == 1,
        overflow_character: Some('…'),
    };
    job.halign = if style.x_align < 0.25 {
        Align::LEFT
    } else if style.x_align > 0.75 {
        Align::RIGHT
    } else {
        Align::Center
    };
    job.round_output_to_gui = false;
    job
}

/// Laid-out text.
pub(crate) struct Laid {
    pub galley: Arc<Galley>,
    /// False when the text did not fit the box.
    pub fits: bool,
}

/// Lays out `style` for a box of `bounds` pixels (`None` on an axis that
/// grows with the text, for `AutomaticSize`).
pub(crate) fn lay_out(ctx: &egui::Context, style: &TextStyle, bounds: [Option<f32>; 2]) -> Laid {
    let layout = |size: f32, wrap: Option<f32>, rows: usize| {
        ctx.fonts_mut(|f| f.layout_job(job(style, size, wrap, rows)))
    };
    let fits = |g: &Galley| {
        let s = g.rect.size();
        bounds[0].is_none_or(|w| s.x <= w + 0.5) && bounds[1].is_none_or(|h| s.y <= h + 0.5)
    };
    if style.scaled {
        // The largest whole size up to 100 that fits, wrapping at the width.
        let (mut lo, mut hi) = (1u32, 100u32);
        let mut best = layout(1.0, bounds[0], usize::MAX);
        while lo <= hi {
            let mid = (lo + hi) / 2;
            let g = layout(mid as f32, bounds[0], usize::MAX);
            if fits(&g) {
                best = g;
                lo = mid + 1;
            } else {
                hi = mid - 1;
            }
        }
        return Laid {
            fits: fits(&best),
            galley: best,
        };
    }
    let size = style.size;
    let wrap = if style.wrapped { bounds[0] } else { None };
    let mut g = layout(size, wrap, usize::MAX);
    let mut ok = fits(&g);
    if !ok && style.truncate {
        let row = g.rows.first().map_or(size, |r| r.height()).max(1.0);
        let rows = match bounds[1] {
            Some(h) if style.wrapped => ((h / row).floor() as usize).max(1),
            _ => 1,
        };
        g = layout(size, bounds[0].or(wrap), rows);
        ok = fits(&g);
    }
    Laid {
        galley: g,
        fits: ok,
    }
}

/// The size of laid-out text in pixels.
pub(crate) fn bounds(l: &Laid) -> Vec2 {
    let s = l.galley.rect.size();
    Vec2::new(s.x, s.y)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rich_text_runs() {
        let r = runs(
            "a <b>b</b> <font color=\"#ff0000\">c<b>d</b></font> &lt;e&gt; <i>f</i>",
            true,
        );
        let t: Vec<(&str, bool, Option<[f32; 3]>)> = r
            .iter()
            .map(|r| (r.text.as_str(), r.bold, r.color))
            .collect();
        assert_eq!(
            t,
            vec![
                ("a ", false, None),
                ("b", true, None),
                (" ", false, None),
                ("c", false, Some([1.0, 0.0, 0.0])),
                ("d", true, Some([1.0, 0.0, 0.0])),
                (" <e> <i>f</i>", false, None),
            ]
        );
        assert_eq!(
            runs("<font color='rgb(0,255,0)'>g</font>", true)[0].color,
            Some([0.0, 1.0, 0.0])
        );
    }
}

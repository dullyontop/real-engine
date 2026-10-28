//! `field.combine` and `field.remap`.

use std::sync::Arc;

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use re_core::Diagnostic;

use super::{
    InputValue, NodeCtx, NodeDef, NodeRegistry, NodeSpec, Params, PortDef, positive, within,
};
use crate::codes;
use crate::value::{Field, Output, PortType};

/// How [`CombineParams`] combines two fields.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Deserialize, Serialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum CombineOp {
    /// `a + b`.
    #[default]
    Add,
    /// `a - b`.
    Sub,
    /// `a * b` (masking).
    Mul,
    /// The smaller of `a` and `b`.
    Min,
    /// The larger of `a` and `b`.
    Max,
    /// `a + (b - a) * t`.
    Lerp,
}

/// Parameters of `field.combine`.
#[derive(Debug, Deserialize, Serialize, JsonSchema)]
#[serde(deny_unknown_fields, default)]
pub struct CombineParams {
    /// `add`, `sub`, `mul`, `min`, `max` or `lerp`.
    pub op: CombineOp,
    /// The blend factor of `lerp` (0 gives `a`, 1 gives `b`).
    pub t: f32,
}

impl Default for CombineParams {
    fn default() -> Self {
        CombineParams {
            op: CombineOp::Add,
            t: 0.5,
        }
    }
}

impl Params for CombineParams {
    fn validate(&self) -> Result<(), String> {
        within("t", self.t, -16.0, 16.0)
    }
}

fn combine(ctx: &NodeCtx, p: &CombineParams) -> Result<Output, String> {
    let a = ctx.need_field("a")?;
    let b = ctx.need_field("b")?;
    // The result has `a`'s grid; `b` is resampled onto it when needed.
    let bv = b.resampled(a.n());
    let values =
        a.0.samples
            .iter()
            .zip(&bv)
            .map(|(&x, &y)| match p.op {
                CombineOp::Add => x + y,
                CombineOp::Sub => x - y,
                CombineOp::Mul => x * y,
                CombineOp::Min => x.min(y),
                CombineOp::Max => x.max(y),
                CombineOp::Lerp => x + (y - x) * p.t,
            })
            .collect();
    Ok(Output::Field(Arc::new(a.like(values)?)))
}

/// `RE4030` when `a` and `b` cover different areas (a different size or
/// position): `b` is then stretched over `a` by normalised position, not
/// matched by world position. Fields over one area at different spacings
/// resample correctly. A `terrain.island_mask` field is made to be
/// stretched over the field it masks, so it is not reported.
fn check_extents(inputs: &[InputValue]) -> Vec<Diagnostic> {
    let field = |port: &str| {
        inputs.iter().find_map(|i| match i.value {
            Output::Field(f) if i.port == port && i.from_kind != "terrain.island_mask" => {
                Some(f.clone())
            }
            _ => None,
        })
    };
    let (Some(a), Some(b)) = (field("a"), field("b")) else {
        return Vec::new();
    };
    let extent = |f: &Field| {
        let h = &f.0;
        (h.offset.x, h.offset.z, (h.n - 1) as f32 * h.spacing)
    };
    let ((ax, az, a_side), (bx, bz, b_side)) = (extent(&a), extent(&b));
    let tolerance = 1e-4 * a_side.max(b_side).max(1.0);
    if (ax - bx).abs() <= tolerance
        && (az - bz).abs() <= tolerance
        && (a_side - b_side).abs() <= tolerance
    {
        return Vec::new();
    }
    vec![
        Diagnostic::warning(
            codes::COMBINE_EXTENTS,
            format!(
                "`a` covers {a_side} m from ({ax}, {az}) and `b` {b_side} m from ({bx}, {bz}): `b` is stretched over `a` by normalised position, not matched by world position"
            ),
        )
        .with_hint("give both fields the same extent: the same size × cell, and the same centre"),
    ]
}

/// How [`RemapParams`] maps values.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Deserialize, Serialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum RemapMode {
    /// Clamp to `[min, max]`.
    #[default]
    Clamp,
    /// Map `[in_min, in_max]` linearly onto `[out_min, out_max]` (not clamped).
    Linear,
    /// A piecewise-linear curve through `points` (`[[in, out], …]`, sorted by
    /// `in`; flat beyond the ends).
    Curve,
    /// Steps `step` metres high; `smooth` is the fraction of each step that ramps.
    Terrace,
}

/// Parameters of `field.remap`.
#[derive(Debug, Deserialize, Serialize, JsonSchema)]
#[serde(deny_unknown_fields, default)]
pub struct RemapParams {
    /// `clamp`, `linear`, `curve` or `terrace`.
    pub mode: RemapMode,
    /// `clamp`: the lower bound.
    pub min: f32,
    /// `clamp`: the upper bound.
    pub max: f32,
    /// `linear`: the input value mapped to `out_min`.
    pub in_min: f32,
    /// `linear`: the input value mapped to `out_max`.
    pub in_max: f32,
    /// `linear`: the output for `in_min`.
    pub out_min: f32,
    /// `linear`: the output for `in_max`.
    pub out_max: f32,
    /// `curve`: at least two `[in, out]` points with increasing `in`.
    pub points: Vec<[f32; 2]>,
    /// `terrace`: the height of each step.
    pub step: f32,
    /// `terrace`: 0 for sharp steps, up to 1 for a smooth ramp.
    pub smooth: f32,
}

impl Default for RemapParams {
    fn default() -> Self {
        RemapParams {
            mode: RemapMode::Clamp,
            min: 0.0,
            max: 1.0,
            in_min: 0.0,
            in_max: 1.0,
            out_min: 0.0,
            out_max: 1.0,
            points: vec![[0.0, 0.0], [1.0, 1.0]],
            step: 1.0,
            smooth: 0.0,
        }
    }
}

impl Params for RemapParams {
    fn validate(&self) -> Result<(), String> {
        let finite = [
            self.min,
            self.max,
            self.in_min,
            self.in_max,
            self.out_min,
            self.out_max,
        ];
        if let Some(v) = finite.iter().find(|v| !v.is_finite()) {
            return Err(format!("limits must be finite, got {v}"));
        }
        match self.mode {
            RemapMode::Clamp if self.min > self.max => Err(format!(
                "`min` ({}) is above `max` ({})",
                self.min, self.max
            )),
            RemapMode::Linear if self.in_min == self.in_max => {
                Err("`in_min` and `in_max` must differ".into())
            }
            RemapMode::Curve => {
                if self.points.len() < 2 {
                    return Err("`points` needs at least two [in, out] points".into());
                }
                for (i, w) in self.points.windows(2).enumerate() {
                    let increasing =
                        w[0][0].is_finite() && w[1][0].is_finite() && w[0][0] < w[1][0];
                    if !increasing || !w[1][1].is_finite() || !w[0][1].is_finite() {
                        return Err(format!(
                            "`points` must be finite with increasing inputs (points {i} and {})",
                            i + 1
                        ));
                    }
                }
                Ok(())
            }
            RemapMode::Terrace => {
                positive("step", self.step)?;
                within("smooth", self.smooth, 0.0, 1.0)
            }
            _ => Ok(()),
        }
    }
}

fn remap_value(p: &RemapParams, v: f32) -> f32 {
    match p.mode {
        RemapMode::Clamp => v.clamp(p.min, p.max),
        RemapMode::Linear => {
            p.out_min + (v - p.in_min) / (p.in_max - p.in_min) * (p.out_max - p.out_min)
        }
        RemapMode::Curve => {
            let pts = &p.points;
            if v <= pts[0][0] {
                return pts[0][1];
            }
            for w in pts.windows(2) {
                if v <= w[1][0] {
                    let t = (v - w[0][0]) / (w[1][0] - w[0][0]);
                    return w[0][1] + (w[1][1] - w[0][1]) * t;
                }
            }
            pts[pts.len() - 1][1]
        }
        RemapMode::Terrace => {
            let x = v / p.step;
            let k = x.floor();
            let f = x - k;
            let ramp = if p.smooth > 0.0 {
                ((f - (1.0 - p.smooth)) / p.smooth).clamp(0.0, 1.0)
            } else {
                0.0
            };
            (k + ramp) * p.step
        }
    }
}

fn remap(ctx: &NodeCtx, p: &RemapParams) -> Result<Output, String> {
    let f = ctx.need_field("field")?;
    let values = f.0.samples.iter().map(|&v| remap_value(p, v)).collect();
    Ok(Output::Field(Arc::new(f.like(values)?)))
}

const FIELD: &[PortType] = &[PortType::Field];

pub(super) fn register(r: &mut NodeRegistry) {
    r.register(NodeDef::new(
        NodeSpec {
            kind: "field.combine",
            version: 1,
            oldest: 1,
            migrations: &[],
            streaming: super::Streaming::Pointwise,
            doc: "Combines two fields value by value (add, sub, mul, min, max or lerp). The result has `a`'s size; `b` is resampled to it.",
            inputs: &[
                PortDef {
                    name: "a",
                    accepts: FIELD,
                    required: true,
                    doc: "The first field; sets the result's size and spacing.",
                },
                PortDef {
                    name: "b",
                    accepts: FIELD,
                    required: true,
                    doc: "The second field.",
                },
            ],
            variadic: None,
            output: PortType::Field,
            uses_seed: false,
            file_params: &[],
        },
        combine,
    )
    .with_input_check(check_extents));
    r.register(NodeDef::new(
        NodeSpec {
            kind: "field.remap",
            version: 1,
            oldest: 1,
            migrations: &[],
            streaming: super::Streaming::Pointwise,
            doc: "Maps every value of a field: clamp, linear, a piecewise-linear curve, or terraces.",
            inputs: &[PortDef {
                name: "field",
                accepts: FIELD,
                required: true,
                doc: "The field to remap.",
            }],
            variadic: None,
            output: PortType::Field,
            uses_seed: false,
            file_params: &[],
        },
        remap,
    ));
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn remap_modes() {
        let mut p = RemapParams::default();
        assert_eq!(remap_value(&p, 2.0), 1.0);
        p.mode = RemapMode::Curve;
        p.points = vec![[0.0, 0.0], [1.0, 10.0], [2.0, 10.0]];
        assert_eq!(remap_value(&p, 0.5), 5.0);
        assert_eq!(remap_value(&p, 5.0), 10.0);
        assert_eq!(remap_value(&p, -1.0), 0.0);
        p.mode = RemapMode::Terrace;
        p.step = 2.0;
        assert_eq!(remap_value(&p, 3.9), 2.0);
        p.smooth = 0.5;
        assert_eq!(remap_value(&p, 3.5), 3.0);
        p.mode = RemapMode::Linear;
        p.in_max = 2.0;
        p.out_max = 4.0;
        assert_eq!(remap_value(&p, 1.0), 2.0);
    }
}

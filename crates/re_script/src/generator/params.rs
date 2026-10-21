//! The parameters of a Luau node: the `proc.param` schema DSL read into
//! [`ParamSpec`]s, checked and defaulted in Rust (no VM), and written as the
//! kind's JSON Schema for `describe`.

use serde_json::{Map, Value as Json, json};

/// The type of one parameter.
#[derive(Clone, Debug, PartialEq)]
pub(crate) enum ParamKind {
    /// `proc.param.number{default, min, max}`.
    Number { min: Option<f64>, max: Option<f64> },
    /// `proc.param.integer{default, min, max}`.
    Integer { min: Option<i64>, max: Option<i64> },
    /// `proc.param.bool{default}`.
    Bool,
    /// `proc.param.string{default}`.
    Str,
    /// `proc.param.enum{values, default}`.
    Enum(Vec<String>),
    /// `proc.param.vec3{default}`: `[x, y, z]` in JSON, a `vector` in Luau.
    Vec3,
}

/// One declared parameter.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct ParamSpec {
    pub name: String,
    pub kind: ParamKind,
    /// `None`: the parameter must be given.
    pub default: Option<Json>,
    pub doc: String,
}

/// The parameters of a kind, sorted by name.
#[derive(Clone, Debug, Default, PartialEq)]
pub(crate) struct Params(pub Vec<ParamSpec>);

impl ParamSpec {
    /// Checks one value; `Err` says why, without the parameter's name.
    pub(crate) fn check(&self, v: &Json) -> Result<(), String> {
        let number = |v: &Json| v.as_f64().filter(|x| x.is_finite());
        match &self.kind {
            ParamKind::Number { min, max } => {
                let x = number(v).ok_or_else(|| format!("expected a number, found {v}"))?;
                if min.is_some_and(|m| x < m) || max.is_some_and(|m| x > m) {
                    return Err(format!("{x} is outside {}", range(min, max)));
                }
            }
            ParamKind::Integer { min, max } => {
                let x = v
                    .as_i64()
                    .or_else(|| {
                        v.as_f64()
                            .filter(|f| f.fract() == 0.0 && f.abs() <= 9.007_199_254_740_992e15)
                            .map(|f| f as i64)
                    })
                    .ok_or_else(|| format!("expected an integer, found {v}"))?;
                if min.is_some_and(|m| x < m) || max.is_some_and(|m| x > m) {
                    return Err(format!("{x} is outside {}", range(min, max)));
                }
            }
            ParamKind::Bool => {
                v.as_bool()
                    .ok_or_else(|| format!("expected true or false, found {v}"))?;
            }
            ParamKind::Str => {
                v.as_str()
                    .ok_or_else(|| format!("expected a string, found {v}"))?;
            }
            ParamKind::Enum(values) => {
                let s = v
                    .as_str()
                    .ok_or_else(|| format!("expected a string, found {v}"))?;
                if !values.iter().any(|x| x == s) {
                    return Err(format!(
                        "`{s}` is not one of {}",
                        values
                            .iter()
                            .map(|x| format!("`{x}`"))
                            .collect::<Vec<_>>()
                            .join(", ")
                    ));
                }
            }
            ParamKind::Vec3 => {
                let ok = v
                    .as_array()
                    .is_some_and(|a| a.len() == 3 && a.iter().all(|x| number(x).is_some()));
                if !ok {
                    return Err(format!("expected [x, y, z] (three numbers), found {v}"));
                }
            }
        }
        Ok(())
    }

    fn schema(&self) -> Json {
        let mut s = match &self.kind {
            ParamKind::Number { min, max } => {
                let mut s = json!({ "type": "number" });
                if let Some(m) = min {
                    s["minimum"] = json!(m);
                }
                if let Some(m) = max {
                    s["maximum"] = json!(m);
                }
                s
            }
            ParamKind::Integer { min, max } => {
                let mut s = json!({ "type": "integer" });
                if let Some(m) = min {
                    s["minimum"] = json!(m);
                }
                if let Some(m) = max {
                    s["maximum"] = json!(m);
                }
                s
            }
            ParamKind::Bool => json!({ "type": "boolean" }),
            ParamKind::Str => json!({ "type": "string" }),
            ParamKind::Enum(values) => json!({ "type": "string", "enum": values }),
            ParamKind::Vec3 => json!({
                "type": "array",
                "items": { "type": "number" },
                "minItems": 3,
                "maxItems": 3
            }),
        };
        if let Some(d) = &self.default {
            s["default"] = d.clone();
        }
        if !self.doc.is_empty() {
            s["description"] = json!(self.doc);
        }
        s
    }
}

fn range<T: std::fmt::Display>(min: &Option<T>, max: &Option<T>) -> String {
    match (min, max) {
        (Some(a), Some(b)) => format!("[{a}, {b}]"),
        (Some(a), None) => format!("[{a}, ∞)"),
        (None, Some(b)) => format!("(-∞, {b}]"),
        (None, None) => "any value".into(),
    }
}

impl Params {
    /// Checks `params` (a JSON object, or null for none): no unknown names,
    /// every parameter without a default given, every value of its type and
    /// in its range. `Err` is the message of `RE4005`.
    pub(crate) fn check(&self, params: &Json) -> Result<(), String> {
        let empty = Map::new();
        let map = match params {
            Json::Null => &empty,
            Json::Object(m) => m,
            other => return Err(format!("parameters must be an object, found {other}")),
        };
        for name in map.keys() {
            if !self.0.iter().any(|p| &p.name == name) {
                let known: Vec<String> = self.0.iter().map(|p| format!("`{}`", p.name)).collect();
                return Err(if known.is_empty() {
                    format!("unknown parameter `{name}`: this kind has no parameters")
                } else {
                    format!(
                        "unknown parameter `{name}` (parameters: {})",
                        known.join(", ")
                    )
                });
            }
        }
        for p in &self.0 {
            match map.get(&p.name) {
                Some(v) => p.check(v).map_err(|m| format!("`{}`: {m}", p.name))?,
                None if p.default.is_none() => {
                    return Err(format!("`{}` is required (it has no default)", p.name));
                }
                None => {}
            }
        }
        Ok(())
    }

    /// `params` with every missing parameter at its default (after
    /// [`Params::check`] passed).
    pub(crate) fn resolve(&self, params: &Json) -> Map<String, Json> {
        let mut out = Map::new();
        for p in &self.0 {
            let v = params
                .get(&p.name)
                .cloned()
                .or_else(|| p.default.clone())
                .unwrap_or(Json::Null);
            out.insert(p.name.clone(), v);
        }
        out
    }

    /// The JSON Schema of the parameters object.
    pub(crate) fn schema(&self) -> Json {
        let properties: Map<String, Json> = self
            .0
            .iter()
            .map(|p| (p.name.clone(), p.schema()))
            .collect();
        let required: Vec<&str> = self
            .0
            .iter()
            .filter(|p| p.default.is_none())
            .map(|p| p.name.as_str())
            .collect();
        let mut s = json!({
            "$schema": "https://json-schema.org/draft/2020-12/schema",
            "type": "object",
            "additionalProperties": false,
            "properties": properties,
        });
        if !required.is_empty() {
            s["required"] = json!(required);
        }
        s
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn params() -> Params {
        Params(vec![
            ParamSpec {
                name: "count".into(),
                kind: ParamKind::Integer {
                    min: Some(0),
                    max: Some(10),
                },
                default: Some(json!(3)),
                doc: "How many.".into(),
            },
            ParamSpec {
                name: "mode".into(),
                kind: ParamKind::Enum(vec!["a".into(), "b".into()]),
                default: None,
                doc: String::new(),
            },
            ParamSpec {
                name: "offset".into(),
                kind: ParamKind::Vec3,
                default: Some(json!([0, 0, 0])),
                doc: String::new(),
            },
        ])
    }

    #[test]
    fn params_are_checked_and_defaulted() {
        let p = params();
        assert!(p.check(&json!({ "mode": "a" })).is_ok());
        assert!(p.check(&json!({ "mode": "a", "count": 4.0 })).is_ok());
        let err = |v: Json| p.check(&v).unwrap_err();
        assert!(err(json!({})).contains("`mode` is required"));
        assert!(err(json!({ "mode": "c" })).contains("not one of"));
        assert!(err(json!({ "mode": "a", "count": 11 })).contains("outside [0, 10]"));
        assert!(err(json!({ "mode": "a", "count": 1.5 })).contains("integer"));
        assert!(err(json!({ "mode": "a", "offset": [1, 2] })).contains("three numbers"));
        assert!(err(json!({ "mode": "a", "nope": 1 })).contains("unknown parameter `nope`"));
        assert!(err(json!([1])).contains("must be an object"));
        let r = p.resolve(&json!({ "mode": "b" }));
        assert_eq!(
            Json::Object(r),
            json!({ "count": 3, "mode": "b", "offset": [0, 0, 0] })
        );
    }

    #[test]
    fn the_schema_lists_properties_and_required_names() {
        let s = params().schema();
        assert_eq!(s["additionalProperties"], json!(false));
        assert_eq!(s["required"], json!(["mode"]));
        assert_eq!(s["properties"]["count"]["maximum"], json!(10));
        assert_eq!(s["properties"]["count"]["description"], json!("How many."));
        assert_eq!(s["properties"]["mode"]["enum"], json!(["a", "b"]));
    }
}

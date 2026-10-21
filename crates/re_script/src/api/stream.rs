//! `stream`: world streaming as scripts see it (ADR-0042). The cells loaded
//! and unloaded at each streaming tick fire `stream.on_unloaded` and then
//! `stream.on_loaded` in `PreSimulation`, before `time.on_step`, each list in
//! key order. `stream.load` and `stream.release` exist in the `tools` context
//! only. A host without streaming ([`ScriptHost::stream`] is `None`) has no
//! cells and fires nothing.
//!
//! [`ScriptHost::stream`]: crate::ScriptHost::stream

use mlua::{Function, Lua, MultiValue, Result, Table, Value};
use re_scene::StreamCell;

use super::Lib;
use super::math::V3;
use crate::codes;
use crate::host::with_host;
use crate::scheduler::{self, Signal};

const CELL: &str = "{ key: string, coord: { number }, min: vector, max: vector, sources: { string }, version: string }";

/// `cell` as the table scripts get: read-only.
fn cell_table(lua: &Lua, cell: &StreamCell) -> Result<Table> {
    let t = lua.create_table()?;
    t.raw_set("key", cell.key.as_str())?;
    t.raw_set("coord", lua.create_sequence_from(cell.coord)?)?;
    t.raw_set("min", V3(cell.min))?;
    t.raw_set("max", V3(cell.max))?;
    t.raw_set(
        "sources",
        lua.create_sequence_from(cell.sources.iter().map(String::as_str))?,
    )?;
    t.raw_set("version", cell.version.as_str())?;
    t.set_readonly(true);
    Ok(t)
}

/// The `stream` library; `tools` adds `load` and `release`.
pub(crate) fn lib(lua: &Lua, lib: &mut Lib, tools: bool) -> Result<()> {
    lib.func(
        lua,
        "on_loaded",
        &format!("(f: (cell: {CELL}) -> ()) -> Connection"),
        "Calls `f` for each cell that loads, before `time.on_step`, in key order.",
        |lua, f: Function| scheduler::connect(lua, Signal::StreamLoaded, f),
    )?;
    lib.func(
        lua,
        "on_unloaded",
        &format!("(f: (cell: {CELL}) -> ()) -> Connection"),
        "Calls `f` for each cell that unloads, before the loads of the same step, in key order.",
        |lua, f: Function| scheduler::connect(lua, Signal::StreamUnloaded, f),
    )?;
    lib.func(
        lua,
        "cells",
        &format!("() -> {{ {CELL} }}"),
        "The loaded cells, in key order.",
        |lua, ()| {
            let cells = with_host(lua, |h| {
                Ok(match h.stream() {
                    Some(s) => s.cells(h.world()),
                    None => Vec::new(),
                })
            })?;
            let out = lua.create_table()?;
            for (i, c) in cells.iter().enumerate() {
                out.raw_set(i + 1, cell_table(lua, c)?)?;
            }
            Ok(out)
        },
    )?;
    lib.func(
        lua,
        "is_loaded",
        "(key: string) -> boolean",
        "True if cell `key` (`c/<x>_<z>`) is loaded.",
        |lua, key: String| {
            with_host(lua, |h| {
                Ok(match h.stream() {
                    Some(s) => s.is_loaded(h.world(), &key),
                    None => false,
                })
            })
        },
    )?;
    if tools {
        lib.func(
            lua,
            "load",
            "(key: string) -> ()",
            "Tools only: keeps cell `key` loaded whatever the focuses, until `stream.release`; it loads at the streaming delay.",
            |lua, key: String| pin(lua, &key, true),
        )?;
        lib.func(
            lua,
            "release",
            "(key: string) -> ()",
            "Tools only: lets a cell kept by `stream.load` unload when no focus wants it.",
            |lua, key: String| pin(lua, &key, false),
        )?;
    }
    Ok(())
}

fn pin(lua: &Lua, key: &str, on: bool) -> Result<()> {
    if re_scene::parse_cell_key(key).is_none() {
        return Err(super::error(
            codes::BAD_ARGUMENT,
            format!("{key:?} is not a cell key (c/<x>_<z>)"),
        ));
    }
    with_host(lua, |h| match h.stream() {
        Some(s) => {
            s.pin(h.world(), key, on);
            Ok(())
        }
        None => Err(super::error(
            codes::SERVICE_UNAVAILABLE,
            "world streaming is not running here",
        )),
    })
}

/// Fires `stream.on_unloaded` and then `stream.on_loaded` for the cells of
/// one streaming tick.
pub(crate) fn fire(lua: &Lua, unloaded: &[StreamCell], loaded: &[StreamCell]) -> Result<()> {
    for (signal, cells) in [
        (Signal::StreamUnloaded, unloaded),
        (Signal::StreamLoaded, loaded),
    ] {
        for c in cells {
            let t = Value::Table(cell_table(lua, c)?);
            scheduler::fire(lua, |s| *s == signal, MultiValue::from_iter([t]))?;
        }
    }
    Ok(())
}

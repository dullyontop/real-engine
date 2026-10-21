//! Reading a stopped thread's stack and values without running Luau
//! (ADR-0045): raw stack and table access only, so `__tostring`, `__index`
//! and `__iter` are never called while paused.

use std::collections::HashMap;
use std::ffi::CStr;
use std::os::raw::{c_char, c_int};

use mlua::ffi;

use super::{Scope, StackFrame, Variable};

/// Tables are expanded this many levels below a frame's locals.
const MAX_DEPTH: usize = 3;
/// At most this many entries of one table are read.
const MAX_ENTRIES: usize = 100;
/// Longer strings are cut to this many bytes.
const MAX_STRING: usize = 200;

pub(super) fn cstr(p: *const c_char) -> Option<String> {
    // SAFETY: Luau hands out NUL-terminated strings that live at least as long
    // as the call that returned them; null is checked first.
    (!p.is_null()).then(|| unsafe { CStr::from_ptr(p) }.to_string_lossy().into_owned())
}

/// The variables of one stop: each list is one scope or one table's entries,
/// referred to by its index.
#[derive(Default)]
pub(super) struct Arena {
    pub lists: Vec<Vec<Variable>>,
    /// Tables already read, by address: a table shown twice (or a cycle) is
    /// read once.
    tables: HashMap<usize, usize>,
    /// Chunk names that are not `@path` (a compat script's `=Service.Name`),
    /// with the project-relative path each was registered under.
    pub aliases: HashMap<String, String>,
}

impl Arena {
    fn source(&self, chunk: &str) -> Option<String> {
        source_of(&self.aliases, chunk)
    }

    fn push(&mut self, list: Vec<Variable>) -> usize {
        self.lists.push(list);
        self.lists.len() - 1
    }
}

/// The source of a chunk name: `@scripts/a.luau` → `scripts/a.luau`; `None`
/// for a chunk loaded from a string (`=...` or source text).
pub(super) fn source_path(source: &str) -> Option<String> {
    source.strip_prefix('@').map(str::to_string)
}

/// The path a chunk named `chunk` was registered under, else [`source_path`].
pub(super) fn source_of(aliases: &HashMap<String, String>, chunk: &str) -> Option<String> {
    aliases.get(chunk).cloned().or_else(|| source_path(chunk))
}

/// Reads the stack of `l`, innermost frame first, with every frame's locals
/// and upvalues.
///
/// # Safety
/// `l` is running a debug callback or is suspended, and nothing else uses it.
pub(super) unsafe fn read_stack(l: *mut ffi::lua_State, arena: &mut Arena) -> Vec<StackFrame> {
    let mut frames = Vec::new();
    // SAFETY: the caller guarantees `l` is a valid state we may push to; every
    // push below is popped before the next frame, and `lua_checkstack` makes
    // room first.
    unsafe {
        let top = ffi::lua_gettop(l);
        let depth = ffi::lua_stackdepth(l);
        for level in 0..depth {
            let mut ar: ffi::lua_Debug = std::mem::zeroed();
            if ffi::lua_getinfo(l, level, c"slnf".as_ptr(), &mut ar) == 0 {
                break;
            }
            // "f" pushed the function.
            let is_c = !ar.what.is_null() && *ar.what == b'C' as c_char;
            let mut scopes = Vec::new();
            if !is_c && ffi::lua_checkstack(l, 8) != 0 {
                let mut locals = Vec::new();
                let mut n = 1;
                loop {
                    let name = ffi::lua_getlocal(l, level, n);
                    if name.is_null() {
                        break;
                    }
                    let name = cstr(name).unwrap_or_default();
                    // Luau's internal locals (for-loop state) start with '('.
                    if !name.starts_with('(') {
                        locals.push(variable(l, name, -1, arena, 0));
                    }
                    ffi::lua_pop(l, 1);
                    n += 1;
                }
                scopes.push(Scope {
                    name: "Locals".into(),
                    variables: arena.push(locals),
                });
                let mut upvalues = Vec::new();
                let mut n = 1;
                loop {
                    let name = ffi::lua_getupvalue(l, -1, n);
                    if name.is_null() {
                        break;
                    }
                    let name = cstr(name)
                        .filter(|s| !s.is_empty())
                        .unwrap_or_else(|| format!("upvalue {n}"));
                    upvalues.push(variable(l, name, -1, arena, 0));
                    ffi::lua_pop(l, 1);
                    n += 1;
                }
                if !upvalues.is_empty() {
                    scopes.push(Scope {
                        name: "Upvalues".into(),
                        variables: arena.push(upvalues),
                    });
                }
            }
            ffi::lua_settop(l, top);
            let source = cstr(ar.source).unwrap_or_default();
            frames.push(StackFrame {
                name: cstr(ar.name).unwrap_or_else(|| {
                    if level == depth - 1 {
                        "main chunk".into()
                    } else {
                        "?".into()
                    }
                }),
                source: if is_c { None } else { arena.source(&source) },
                line: if is_c {
                    None
                } else {
                    u32::try_from(ar.currentline).ok()
                },
                scopes,
            });
        }
        ffi::lua_settop(l, top);
    }
    frames
}

/// The variable `name` for the value at `idx`, expanding tables into `arena`.
///
/// # Safety
/// `idx` is a valid index of `l`, which has room for 4 more slots.
unsafe fn variable(
    l: *mut ffi::lua_State,
    name: String,
    idx: c_int,
    arena: &mut Arena,
    depth: usize,
) -> Variable {
    // SAFETY: as the caller promises; see `describe` and `expand`.
    unsafe {
        let idx = ffi::lua_absindex(l, idx);
        let (value, type_name) = describe(l, idx);
        let children = if ffi::lua_type(l, idx) == ffi::LUA_TTABLE && depth < MAX_DEPTH {
            Some(expand(l, idx, arena, depth + 1))
        } else {
            None
        };
        Variable {
            name,
            value,
            type_name,
            children,
        }
    }
}

/// Reads the entries of the table at `idx` (raw, at most [`MAX_ENTRIES`]) into
/// a new list and returns its index.
///
/// # Safety
/// `idx` is an absolute index of a table in `l`.
unsafe fn expand(l: *mut ffi::lua_State, idx: c_int, arena: &mut Arena, depth: usize) -> usize {
    // SAFETY: `lua_rawiter` reads the table without metamethods and pushes a
    // key and a value, which are popped before the next step.
    unsafe {
        let addr = ffi::lua_topointer(l, idx) as usize;
        if let Some(&list) = arena.tables.get(&addr) {
            return list;
        }
        let list = arena.push(Vec::new());
        arena.tables.insert(addr, list);
        let mut entries = Vec::new();
        if ffi::lua_checkstack(l, 6) == 0 {
            return list;
        }
        let mut iter = 0;
        let mut more = false;
        loop {
            iter = ffi::lua_rawiter(l, idx, iter);
            if iter < 0 {
                break;
            }
            if entries.len() == MAX_ENTRIES {
                more = true;
                ffi::lua_pop(l, 2);
                break;
            }
            let key = key_name(l, -2);
            entries.push(variable(l, key, -1, arena, depth));
            ffi::lua_pop(l, 2);
        }
        if more {
            entries.push(Variable {
                name: "…".into(),
                value: format!("more than {MAX_ENTRIES} entries"),
                type_name: String::new(),
                children: None,
            });
        }
        arena.lists[list] = entries;
        list
    }
}

/// A table key as a debugger shows it: `name` for an identifier string,
/// `[1]` or `["a b"]` otherwise.
unsafe fn key_name(l: *mut ffi::lua_State, idx: c_int) -> String {
    // SAFETY: `idx` is valid; `lua_tolstring` is only called on a string, so
    // it never converts (and so never changes) the key in place.
    unsafe {
        if ffi::lua_type(l, idx) == ffi::LUA_TSTRING {
            let s = string(l, idx);
            let ident = !s.is_empty()
                && s.chars().all(|c| c.is_ascii_alphanumeric() || c == '_')
                && !s.starts_with(|c: char| c.is_ascii_digit());
            return if ident { s } else { format!("[{s:?}]") };
        }
        format!("[{}]", describe(l, idx).0)
    }
}

unsafe fn string(l: *mut ffi::lua_State, idx: c_int) -> String {
    // SAFETY: the value at `idx` is a string; Luau returns its bytes and length.
    unsafe {
        let mut len = 0usize;
        let p = ffi::lua_tolstring(l, idx, &mut len);
        if p.is_null() {
            return String::new();
        }
        let bytes = std::slice::from_raw_parts(p.cast::<u8>(), len);
        String::from_utf8_lossy(bytes).into_owned()
    }
}

/// The value at `idx` and its type, as text, without running Luau code.
///
/// # Safety
/// `idx` is a valid index of `l`, which has room for 2 more slots.
pub(super) unsafe fn describe(l: *mut ffi::lua_State, idx: c_int) -> (String, String) {
    // SAFETY: every read is typed by `lua_type` first; the metatable lookup
    // is raw and popped again.
    unsafe {
        let t = ffi::lua_type(l, idx);
        let type_name = cstr(ffi::lua_typename(l, t)).unwrap_or_default();
        let value = match t {
            ffi::LUA_TNIL => "nil".into(),
            ffi::LUA_TBOOLEAN => (ffi::lua_toboolean(l, idx) != 0).to_string(),
            ffi::LUA_TNUMBER => number(ffi::lua_tonumber(l, idx)),
            ffi::LUA_TSTRING => {
                let mut s = string(l, idx);
                if s.len() > MAX_STRING {
                    let mut end = MAX_STRING;
                    while !s.is_char_boundary(end) {
                        end -= 1;
                    }
                    s.truncate(end);
                    format!("{s:?}…")
                } else {
                    format!("{s:?}")
                }
            }
            ffi::LUA_TVECTOR => {
                let v = ffi::lua_tovector(l, idx);
                format!(
                    "vector({}, {}, {})",
                    number(*v as f64),
                    number(*v.add(1) as f64),
                    number(*v.add(2) as f64)
                )
            }
            ffi::LUA_TTABLE => format!("table: {:p}", ffi::lua_topointer(l, idx)),
            ffi::LUA_TFUNCTION => {
                let mut ar: ffi::lua_Debug = std::mem::zeroed();
                ffi::lua_pushvalue(l, idx);
                // A negative level reads the function on top of the stack.
                let named = ffi::lua_getinfo(l, -1, c"sn".as_ptr(), &mut ar) != 0;
                ffi::lua_pop(l, 1);
                let name = if named { cstr(ar.name) } else { None };
                let at = if named && ar.linedefined > 0 {
                    cstr(ar.source)
                        .and_then(|s| source_path(&s))
                        .map(|s| format!(" ({s}:{})", ar.linedefined))
                } else {
                    None
                };
                format!(
                    "function {}{}",
                    name.unwrap_or_default(),
                    at.unwrap_or_default()
                )
            }
            ffi::LUA_TUSERDATA | ffi::LUA_TLIGHTUSERDATA => match userdata_type(l, idx) {
                Some(name) => {
                    return (name.clone(), name);
                }
                None => format!("userdata: {:p}", ffi::lua_topointer(l, idx)),
            },
            _ => format!("{type_name}: {:p}", ffi::lua_topointer(l, idx)),
        };
        (value, type_name)
    }
}

/// The `__type` (Luau's `typeof`) or `__name` of a userdata's metatable, read raw.
unsafe fn userdata_type(l: *mut ffi::lua_State, idx: c_int) -> Option<String> {
    // SAFETY: the metatable and the field are pushed and popped again.
    unsafe {
        if ffi::lua_getmetatable(l, idx) == 0 {
            return None;
        }
        let mut name = None;
        for field in [c"__type", c"__name"] {
            if ffi::lua_rawgetfield(l, -1, field.as_ptr()) == ffi::LUA_TSTRING {
                name = Some(string(l, -1));
            }
            ffi::lua_pop(l, 1);
            if name.is_some() {
                break;
            }
        }
        ffi::lua_pop(l, 1);
        name
    }
}

fn number(n: f64) -> String {
    if n.fract() == 0.0 && n.abs() < 1e15 {
        format!("{}", n as i64)
    } else {
        format!("{n}")
    }
}

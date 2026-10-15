//! Script file names: one path element per instance `Name`, kept as written
//! (dots included) except for the characters a file system cannot hold.

/// A file name for an instance name. Characters no common file system allows
/// in a name (`/ \ : * ? " < > |`, control characters) and `%` itself become
/// `%XX`; `.` and `..`, an empty name, a trailing dot or space and the names
/// Windows reserves (`CON`, `NUL`, `COM1`, ...) are escaped the same way. Every other character, dots included, is kept, so
/// `Combat.Animator` stays one file name.
pub fn file_name(name: &str) -> String {
    if name.is_empty() {
        return "%".to_string();
    }
    let mut out = String::with_capacity(name.len());
    for c in name.chars() {
        match c {
            '/' | '\\' | ':' | '*' | '?' | '"' | '<' | '>' | '|' | '%' => {
                out.push_str(&format!("%{:02X}", c as u32))
            }
            c if (c as u32) < 0x20 || c as u32 == 0x7f => {
                out.push_str(&format!("%{:02X}", c as u32))
            }
            c => out.push(c),
        }
    }
    // Windows reserves these names whatever follows the first dot.
    let base = out.split('.').next().unwrap_or("").to_ascii_uppercase();
    let reserved = matches!(base.as_str(), "CON" | "PRN" | "AUX" | "NUL")
        || (base.len() == 4
            && (base.starts_with("COM") || base.starts_with("LPT"))
            && base.as_bytes()[3].is_ascii_digit());
    if reserved {
        let first = out.remove(0);
        out.insert_str(0, &format!("%{:02X}", first as u32));
    }
    if out == "." || out == ".." {
        return out.replace('.', "%2E");
    }
    if let Some(last) = out.chars().last()
        && (last == '.' || last == ' ')
    {
        out.pop();
        out.push_str(if last == '.' { "%2E" } else { "%20" });
    }
    out
}

/// The file suffix for a script class (Rojo's convention, so the files read
/// the same in either tool).
pub fn script_suffix(class: &str) -> &'static str {
    match class {
        "Script" => ".server.luau",
        "LocalScript" => ".client.luau",
        _ => ".luau",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn keeps_dots_and_escapes_what_files_cannot_hold() {
        assert_eq!(
            file_name("ReplicatedStorage.Combat.Animator"),
            "ReplicatedStorage.Combat.Animator"
        );
        assert_eq!(file_name("PartA_2026-10-09"), "PartA_2026-10-09");
        assert_eq!(file_name("a/b"), "a%2Fb");
        assert_eq!(file_name("100%"), "100%25");
        assert_eq!(file_name(".."), "%2E%2E");
        assert_eq!(file_name("end."), "end%2E");
        assert_eq!(file_name(""), "%");
        assert_eq!(file_name("tab\there"), "tab%09here");
        assert_eq!(file_name("héllo ↑"), "héllo ↑");
        assert_eq!(file_name("CON"), "%43ON");
        assert_eq!(file_name("nul.cfg"), "%6Eul.cfg");
        assert_eq!(file_name("COM1"), "%43OM1");
        assert_eq!(file_name("Console"), "Console");
    }
}

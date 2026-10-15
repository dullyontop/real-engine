//! Path globs for the policy's scopes: `**` matches any number of whole path
//! segments (including none), `*` any run of characters inside one segment,
//! `?` one character. Paths and patterns use `/` and are relative to the
//! project root.

/// True if `path` matches `pattern`.
pub fn matches(pattern: &str, path: &str) -> bool {
    let pattern: Vec<&str> = pattern.split('/').filter(|s| !s.is_empty()).collect();
    let path: Vec<&str> = path.split('/').filter(|s| !s.is_empty()).collect();
    match_segments(&pattern, &path)
}

fn match_segments(pattern: &[&str], path: &[&str]) -> bool {
    match pattern.split_first() {
        None => path.is_empty(),
        Some((&"**", rest)) => (0..=path.len()).any(|skip| match_segments(rest, &path[skip..])),
        Some((first, rest)) => match path.split_first() {
            Some((segment, path_rest)) => {
                match_segment(first.as_bytes(), segment.as_bytes())
                    && match_segments(rest, path_rest)
            }
            None => false,
        },
    }
}

fn match_segment(pattern: &[u8], text: &[u8]) -> bool {
    match pattern.split_first() {
        None => text.is_empty(),
        Some((b'*', rest)) => (0..=text.len()).any(|skip| match_segment(rest, &text[skip..])),
        Some((b'?', rest)) => !text.is_empty() && match_segment(rest, &text[1..]),
        Some((c, rest)) => text.first() == Some(c) && match_segment(rest, &text[1..]),
    }
}

#[cfg(test)]
mod tests {
    use super::matches;

    #[test]
    fn globs() {
        assert!(matches("scripts/**", "scripts/game/player.luau"));
        assert!(matches("scripts/**", "scripts/a.luau"));
        assert!(!matches("scripts/**", "scenes/a.scene.json"));
        assert!(!matches("scripts/**", "scriptsx/a.luau"));
        assert!(matches("**/*.luau", "a.luau"));
        assert!(matches("**/*.luau", "x/y/a.luau"));
        assert!(!matches("**/*.luau", "x/y/a.lua"));
        assert!(matches("assets/*.png", "assets/rock.png"));
        assert!(!matches("assets/*.png", "assets/sub/rock.png"));
        assert!(matches("a?c", "abc"));
        assert!(!matches("a?c", "ac"));
        assert!(matches("**", "anything/at/all"));
    }
}

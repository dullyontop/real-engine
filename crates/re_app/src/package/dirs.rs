//! A packaged game's per-user folders (ADR-0037): the writable folders of a
//! run that has no project directory.

use std::path::{Path, PathBuf};

use re_core::Diagnostic;
use re_core::files::{WritableDir, WritableDirs};

use crate::codes;

/// Where the per-user folders go, from the environment.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct UserRoots {
    /// The folder that holds the game's cache folder.
    pub cache: PathBuf,
    /// The folder that holds the game's data, crash and out folders.
    pub data: PathBuf,
}

/// The roots for this platform, from `var` (the environment, or a test's
/// stand-in): `$XDG_CACHE_HOME` and `$XDG_DATA_HOME` (by default `~/.cache` and
/// `~/.local/share`) on Linux and other Unix systems, `~/Library/Caches` and
/// `~/Library/Application Support` on macOS, `%LOCALAPPDATA%` and `%APPDATA%`
/// on Windows.
///
/// # Errors
///
/// What is missing from the environment.
pub fn user_roots(var: &dyn Fn(&str) -> Option<String>) -> Result<UserRoots, String> {
    // Only absolute values count (the XDG spec ignores relative ones).
    let abs = |name: &str| {
        var(name)
            .filter(|v| !v.is_empty())
            .map(PathBuf::from)
            .filter(|p| p.is_absolute())
    };
    if cfg!(windows) {
        let local = abs("LOCALAPPDATA").ok_or("LOCALAPPDATA is not set")?;
        let roaming = abs("APPDATA").ok_or("APPDATA is not set")?;
        return Ok(UserRoots {
            cache: local,
            data: roaming,
        });
    }
    let home = || abs("HOME").ok_or("HOME is not set");
    if cfg!(target_os = "macos") {
        let library = home()?.join("Library");
        return Ok(UserRoots {
            cache: library.join("Caches"),
            data: library.join("Application Support"),
        });
    }
    let cache = match abs("XDG_CACHE_HOME") {
        Some(p) => p,
        None => home()?.join(".cache"),
    };
    let data = match abs("XDG_DATA_HOME") {
        Some(p) => p,
        None => home()?.join(".local").join("share"),
    };
    Ok(UserRoots { cache, data })
}

/// The writable folders of a packaged game with `identifier`: the cache in
/// `<cache root>/<identifier>`, the rest in `<data root>/<identifier>/{data,
/// crash,out}`. With `data_dir` (`--data-dir`) all four are in `data_dir/{cache,
/// data,crash,out}` instead.
///
/// # Errors
///
/// `RE0605` when the environment names no folder.
pub fn user_dirs(identifier: &str, data_dir: Option<&Path>) -> Result<WritableDirs, Diagnostic> {
    let (cache, base) = match data_dir {
        Some(dir) => (dir.join("cache"), dir.to_path_buf()),
        None => {
            let roots = user_roots(&|name| std::env::var(name).ok()).map_err(|why| {
                Diagnostic::error(
                    codes::NO_USER_FOLDER,
                    format!("no folder for the game's user data: {why}"),
                )
                .with_hint("pass `--data-dir DIR` to keep the game's data in DIR")
            })?;
            (roots.cache.join(identifier), roots.data.join(identifier))
        }
    };
    Ok(WritableDirs {
        cache: WritableDir::dir(cache),
        data: WritableDir::dir(base.join("data")),
        crash: WritableDir::dir(base.join("crash")),
        out: WritableDir::dir(base.join("out")),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn env(pairs: &'static [(&'static str, &'static str)]) -> impl Fn(&str) -> Option<String> {
        move |name| {
            pairs
                .iter()
                .find(|(k, _)| *k == name)
                .map(|(_, v)| (*v).to_owned())
        }
    }

    #[cfg(all(unix, not(target_os = "macos")))]
    #[test]
    fn linux_follows_xdg_with_the_standard_defaults() {
        let r = user_roots(&env(&[("HOME", "/home/a")])).unwrap();
        assert_eq!(r.cache, Path::new("/home/a/.cache"));
        assert_eq!(r.data, Path::new("/home/a/.local/share"));
        let r = user_roots(&env(&[
            ("HOME", "/home/a"),
            ("XDG_CACHE_HOME", "/c"),
            ("XDG_DATA_HOME", "relative/ignored"),
        ]))
        .unwrap();
        assert_eq!(r.cache, Path::new("/c"));
        assert_eq!(r.data, Path::new("/home/a/.local/share"));
        assert!(user_roots(&env(&[])).is_err());
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn macos_uses_the_library_folders() {
        let r = user_roots(&env(&[("HOME", "/Users/a")])).unwrap();
        assert_eq!(r.cache, Path::new("/Users/a/Library/Caches"));
        assert_eq!(r.data, Path::new("/Users/a/Library/Application Support"));
    }

    #[cfg(windows)]
    #[test]
    fn windows_uses_local_and_roaming_app_data() {
        let r = user_roots(&env(&[
            ("LOCALAPPDATA", "C:\\Users\\a\\AppData\\Local"),
            ("APPDATA", "C:\\Users\\a\\AppData\\Roaming"),
        ]))
        .unwrap();
        assert_eq!(r.cache, Path::new("C:\\Users\\a\\AppData\\Local"));
        assert_eq!(r.data, Path::new("C:\\Users\\a\\AppData\\Roaming"));
    }

    #[test]
    fn a_data_dir_holds_all_four_folders() {
        let d = user_dirs("com.example.x", Some(Path::new("/tmp/game-data"))).unwrap();
        assert_eq!(d.cache.path(), Some(Path::new("/tmp/game-data/cache")));
        assert_eq!(d.data.path(), Some(Path::new("/tmp/game-data/data")));
        assert_eq!(d.crash.path(), Some(Path::new("/tmp/game-data/crash")));
        assert_eq!(d.out.path(), Some(Path::new("/tmp/game-data/out")));
    }
}

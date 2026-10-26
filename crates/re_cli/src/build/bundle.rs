//! The platform layouts of a game folder (ADR-0037): where the executable,
//! the archive and the notices go for each OS, and the macOS app's
//! `Info.plist` and `.icns` icon.

use std::collections::BTreeMap;
use std::path::Path;
use std::process::Command;

use re_app::AppConfig;
use re_app::package::ARCHIVE_NAME;
use re_core::Diagnostic;
use re_types::render::TextureData;

use super::notices::NOTICES_FILE;
use crate::codes;

/// Where a game's files go in its folder, as paths relative to `--out`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Layout {
    /// `linux`, `macos` or `windows`.
    pub os: &'static str,
    /// The executable.
    pub exe: String,
    /// The archive.
    pub archive: String,
    /// The third-party notices.
    pub notices: String,
    /// The macOS app's folder.
    pub app: Option<String>,
}

/// The game's display name: `[package] display_name`, else `[project] name`.
pub fn display_name(config: &AppConfig) -> String {
    config
        .package
        .as_ref()
        .and_then(|p| p.display_name.clone())
        .unwrap_or_else(|| config.project.name.clone())
}

/// The layout of `config`'s game for `os`.
pub fn layout(config: &AppConfig, os: &'static str) -> Layout {
    let name = super::executable_name(&config.project.name);
    let plain = |exe: String| Layout {
        os,
        exe,
        archive: ARCHIVE_NAME.to_owned(),
        notices: NOTICES_FILE.to_owned(),
        app: None,
    };
    match os {
        "windows" => plain(format!("{name}.exe")),
        "macos" => {
            let app = format!("{}.app", app_name(&display_name(config), &name));
            Layout {
                os,
                exe: format!("{app}/Contents/MacOS/{name}"),
                archive: format!("{app}/Contents/Resources/{ARCHIVE_NAME}"),
                notices: format!("{app}/Contents/Resources/{NOTICES_FILE}"),
                app: Some(app),
            }
        }
        _ => plain(name),
    }
}

/// The app folder's name: the display name without the characters a file
/// name cannot hold on some OS, and without `..`, which the build's marker
/// file refuses.
fn app_name(display: &str, fallback: &str) -> String {
    let kept: String = display
        .chars()
        .filter(|c| {
            !c.is_control() && !matches!(c, '/' | '\\' | ':' | '*' | '?' | '"' | '<' | '>' | '|')
        })
        .collect();
    let mut name = kept.trim().trim_matches('.').trim().to_owned();
    while name.contains("..") {
        name = name.replace("..", ".");
    }
    if name.is_empty() {
        fallback.to_owned()
    } else {
        name
    }
}

/// The oldest macOS a runtime for `triple` runs on: 11 on Apple silicon (the
/// first release for it), 10.15 on Intel.
pub fn minimum_macos(triple: &str) -> &'static str {
    if triple.starts_with("aarch64") {
        "11.0"
    } else {
        "10.15"
    }
}

/// A version as macOS wants it in `CFBundleShortVersionString`: up to three
/// whole numbers (`1.2.0-beta` gives `1.2.0`).
fn apple_version(version: &str) -> String {
    let numbers: Vec<&str> = version
        .split(|c: char| !c.is_ascii_digit() && c != '.')
        .next()
        .unwrap_or("")
        .split('.')
        .filter(|n| !n.is_empty())
        .take(3)
        .collect();
    if numbers.is_empty() {
        "0".to_owned()
    } else {
        numbers.join(".")
    }
}

/// What the macOS app's `Info.plist` says.
pub struct AppInfo<'a> {
    /// The executable's file name in `Contents/MacOS/`.
    pub executable: &'a str,
    /// `[package] identifier`.
    pub identifier: &'a str,
    /// The display name.
    pub name: &'a str,
    /// `[package] version`.
    pub version: &'a str,
    /// The `.icns` file's name in `Contents/Resources/`.
    pub icon: Option<&'a str>,
    /// `LSMinimumSystemVersion`.
    pub minimum_system: &'a str,
}

fn xml_escaped(text: &str) -> String {
    text.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
}

/// The app's `Info.plist`, as XML text.
pub fn info_plist(info: &AppInfo<'_>) -> String {
    let version = apple_version(info.version);
    let mut strings = vec![
        ("CFBundleDisplayName", info.name),
        ("CFBundleExecutable", info.executable),
    ];
    if let Some(icon) = info.icon {
        strings.push(("CFBundleIconFile", icon));
    }
    strings.extend([
        ("CFBundleIdentifier", info.identifier),
        ("CFBundleInfoDictionaryVersion", "6.0"),
        ("CFBundleName", info.name),
        ("CFBundlePackageType", "APPL"),
        ("CFBundleShortVersionString", &version),
        ("CFBundleVersion", &version),
        ("LSMinimumSystemVersion", info.minimum_system),
    ]);
    let mut out = String::from(
        "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n<!DOCTYPE plist PUBLIC \"-//Apple//DTD PLIST 1.0//EN\" \"http://www.apple.com/DTDs/PropertyList-1.0.dtd\">\n<plist version=\"1.0\">\n<dict>\n",
    );
    for (key, value) in strings {
        out.push_str(&format!(
            "\t<key>{key}</key>\n\t<string>{}</string>\n",
            xml_escaped(value)
        ));
    }
    out.push_str("\t<key>NSHighResolutionCapable</key>\n\t<true/>\n</dict>\n</plist>\n");
    out
}

/// The `.icns` entry types of PNG icons and their sizes in pixels; the @2x
/// types share a size with the next one up.
const ICNS_TYPES: &[(&[u8; 4], u32)] = &[
    (b"icp4", 16),
    (b"ic11", 32),
    (b"icp5", 32),
    (b"ic12", 64),
    (b"ic07", 128),
    (b"ic13", 256),
    (b"ic08", 256),
    (b"ic14", 512),
    (b"ic09", 512),
    (b"ic10", 1024),
];

/// An `.icns` file with a PNG entry for each size up to the icon's own,
/// resized from `icon` (square, RGBA8).
///
/// # Errors
///
/// The pixels do not match the size, or a PNG cannot be encoded.
pub fn icns(icon: &TextureData) -> Result<Vec<u8>, String> {
    use image::ImageEncoder;

    let source = image::RgbaImage::from_raw(icon.width, icon.height, icon.pixels.clone())
        .ok_or("the icon's pixels do not match its size")?;
    let mut pngs: BTreeMap<u32, Vec<u8>> = BTreeMap::new();
    let mut body = Vec::new();
    for (kind, size) in ICNS_TYPES {
        if *size > icon.width.min(icon.height) {
            continue;
        }
        if !pngs.contains_key(size) {
            let resized = if *size == icon.width {
                source.clone()
            } else {
                image::imageops::resize(
                    &source,
                    *size,
                    *size,
                    image::imageops::FilterType::Lanczos3,
                )
            };
            let mut png = Vec::new();
            image::codecs::png::PngEncoder::new(&mut png)
                .write_image(&resized, *size, *size, image::ExtendedColorType::Rgba8)
                .map_err(|e| format!("cannot encode the {size} px icon: {e}"))?;
            pngs.insert(*size, png);
        }
        let png = &pngs[size];
        body.extend_from_slice(*kind);
        body.extend_from_slice(&entry_len(png.len())?.to_be_bytes());
        body.extend_from_slice(png);
    }
    let mut out = Vec::with_capacity(body.len() + 8);
    out.extend_from_slice(b"icns");
    out.extend_from_slice(&entry_len(body.len())?.to_be_bytes());
    out.extend(body);
    Ok(out)
}

/// An `.icns` length field: the data and the 8-byte header before it.
fn entry_len(data: usize) -> Result<u32, String> {
    u32::try_from(data + 8).map_err(|_| "the icon is too large for an .icns file".to_owned())
}

/// Seals the app ad hoc (`codesign --force --sign -`), as the Mac that built
/// it needs to run it.
///
/// # Errors
///
/// `RE0955` when codesign does not run or fails.
pub fn seal(app: &Path) -> Result<(), Diagnostic> {
    let failed = |why: String| {
        Diagnostic::error(
            codes::BUNDLE_FAILED,
            format!("cannot seal {}: {why}", app.display()),
        )
    };
    let output = Command::new("codesign")
        .args(["--force", "--sign", "-"])
        .arg(app)
        .output()
        .map_err(|e| failed(format!("cannot run codesign: {e}")))?;
    if !output.status.success() {
        return Err(failed(
            String::from_utf8_lossy(&output.stderr).trim().to_owned(),
        ));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn config(toml: &str) -> AppConfig {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("real.toml"), toml).unwrap();
        AppConfig::load(dir.path()).unwrap()
    }

    #[test]
    fn each_os_has_its_layout() {
        let c = config(
            "[project]\nname = \"My Island\"\n\n[package]\ndisplay_name = \"My: Island..2\"\n",
        );
        assert_eq!(layout(&c, "linux").exe, "MyIsland");
        assert_eq!(layout(&c, "linux").archive, "game.rpak");
        assert_eq!(layout(&c, "windows").exe, "MyIsland.exe");
        let mac = layout(&c, "macos");
        assert_eq!(mac.app.as_deref(), Some("My Island.2.app"));
        assert_eq!(mac.exe, "My Island.2.app/Contents/MacOS/MyIsland");
        assert_eq!(mac.archive, "My Island.2.app/Contents/Resources/game.rpak");
        assert_eq!(
            mac.notices,
            "My Island.2.app/Contents/Resources/THIRD_PARTY_NOTICES.txt"
        );
        assert_eq!(app_name("../..", "game"), "game");
    }

    #[test]
    fn versions_become_apple_versions() {
        assert_eq!(apple_version("1.2.3"), "1.2.3");
        assert_eq!(apple_version("1.2.0-beta.1"), "1.2.0");
        assert_eq!(apple_version("2"), "2");
        assert_eq!(apple_version("1.2.3.4"), "1.2.3");
        assert_eq!(apple_version("beta"), "0");
    }

    #[test]
    fn the_plist_names_the_app() {
        let plist = info_plist(&AppInfo {
            executable: "island",
            identifier: "com.example.island",
            name: "Rock & Roll",
            version: "1.0.0",
            icon: Some("island.icns"),
            minimum_system: minimum_macos("aarch64-apple-darwin"),
        });
        for line in [
            "<key>CFBundleExecutable</key>\n\t<string>island</string>",
            "<key>CFBundleIdentifier</key>\n\t<string>com.example.island</string>",
            "<key>CFBundleName</key>\n\t<string>Rock &amp; Roll</string>",
            "<key>CFBundlePackageType</key>\n\t<string>APPL</string>",
            "<key>CFBundleIconFile</key>\n\t<string>island.icns</string>",
            "<key>LSMinimumSystemVersion</key>\n\t<string>11.0</string>",
            "<key>NSHighResolutionCapable</key>\n\t<true/>",
        ] {
            assert!(plist.contains(line), "{line}\n{plist}");
        }
    }

    #[test]
    fn the_icns_holds_a_png_per_size() {
        let size = 64;
        let mut pixels = Vec::new();
        for y in 0..size {
            for x in 0..size {
                pixels.extend([(x * 4) as u8, (y * 4) as u8, 128, 255]);
            }
        }
        let icon = TextureData {
            width: size,
            height: size,
            pixels,
            ..TextureData::default()
        };
        let file = icns(&icon).unwrap();
        assert_eq!(&file[..4], b"icns");
        assert_eq!(
            u32::from_be_bytes(file[4..8].try_into().unwrap()) as usize,
            file.len()
        );
        let mut at = 8;
        let mut kinds = Vec::new();
        while at < file.len() {
            let kind = std::str::from_utf8(&file[at..at + 4]).unwrap().to_owned();
            let len = u32::from_be_bytes(file[at + 4..at + 8].try_into().unwrap()) as usize;
            let png = &file[at + 8..at + len];
            let decoded = image::load_from_memory(png).unwrap();
            let want = ICNS_TYPES
                .iter()
                .find(|(k, _)| k == &kind.as_bytes())
                .unwrap()
                .1;
            assert_eq!((decoded.width(), decoded.height()), (want, want), "{kind}");
            kinds.push(kind);
            at += len;
        }
        assert_eq!(kinds, ["icp4", "ic11", "icp5", "ic12"]);
    }
}

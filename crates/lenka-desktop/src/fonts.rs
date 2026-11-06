// SPDX-License-Identifier: GPL-3.0-only
// Copyright (C) 2026 Igor Pie and lenka contributors

//! Loads system fonts at run time. The binary holds no font, so its license stays simple.
//!
//! A font file is untrusted input. lenka reads only a regular file with a known extension, limits
//! its size, checks its signature, and tests it in a separate font store before the app uses it.

use std::fmt;
use std::io::Read as _;
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::path::{Path, PathBuf};
use std::process::Command;

use egui::epaint::text::{Fonts, TextOptions};
use egui::{FontData, FontDefinitions, FontFamily, FontId};

/// The largest font file that lenka reads.
pub(crate) const MAX_FONT_BYTES: u64 = 32 * 1024 * 1024;

const PROPORTIONAL_NAME: &str = "lenka-proportional";
const MONOSPACE_NAME: &str = "lenka-monospace";

/// The characters that the UI itself uses. Task text can use others.
const UI_GLYPHS: &str = " !\"#$%&'()*+,-./0123456789:;<=>?@ABCDEFGHIJKLMNOPQRSTUVWXYZ[\\]^_`\
                         abcdefghijklmnopqrstuvwxyz{|}~\u{2026}";

/// The operating systems with a known list of font paths.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Platform {
    Linux,
    MacOs,
    Windows,
    Other,
}

impl Platform {
    /// Returns the platform of this build.
    fn current() -> Self {
        if cfg!(target_os = "macos") {
            Self::MacOs
        } else if cfg!(target_os = "windows") {
            Self::Windows
        } else if cfg!(target_os = "linux") {
            Self::Linux
        } else {
            Self::Other
        }
    }
}

/// The font files to try, in order.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct Candidates {
    pub proportional: Vec<PathBuf>,
    pub monospace: Vec<PathBuf>,
}

/// Returns the fixed font paths of a platform.
pub(crate) fn font_paths(platform: Platform) -> Candidates {
    let paths = |list: &[&str]| list.iter().map(PathBuf::from).collect::<Vec<_>>();
    match platform {
        Platform::Linux => Candidates {
            proportional: paths(&[
                "/usr/share/fonts/truetype/noto/NotoSans-Regular.ttf",
                "/usr/share/fonts/noto/NotoSans-Regular.ttf",
                "/usr/share/fonts/google-noto/NotoSans-Regular.ttf",
                "/usr/share/fonts/truetype/dejavu/DejaVuSans.ttf",
                "/usr/share/fonts/TTF/DejaVuSans.ttf",
                "/usr/share/fonts/dejavu-sans-fonts/DejaVuSans.ttf",
                "/usr/share/fonts/truetype/liberation/LiberationSans-Regular.ttf",
                "/usr/share/fonts/liberation/LiberationSans-Regular.ttf",
                "/usr/share/fonts/liberation-sans/LiberationSans-Regular.ttf",
            ]),
            monospace: paths(&[
                "/usr/share/fonts/truetype/noto/NotoSansMono-Regular.ttf",
                "/usr/share/fonts/noto/NotoSansMono-Regular.ttf",
                "/usr/share/fonts/google-noto/NotoSansMono-Regular.ttf",
                "/usr/share/fonts/truetype/dejavu/DejaVuSansMono.ttf",
                "/usr/share/fonts/TTF/DejaVuSansMono.ttf",
                "/usr/share/fonts/dejavu-sans-mono-fonts/DejaVuSansMono.ttf",
                "/usr/share/fonts/truetype/liberation/LiberationMono-Regular.ttf",
                "/usr/share/fonts/liberation/LiberationMono-Regular.ttf",
                "/usr/share/fonts/liberation-mono/LiberationMono-Regular.ttf",
            ]),
        },
        Platform::MacOs => Candidates {
            proportional: paths(&[
                "/System/Library/Fonts/Supplemental/Arial.ttf",
                "/System/Library/Fonts/Helvetica.ttc",
            ]),
            monospace: paths(&["/System/Library/Fonts/Menlo.ttc"]),
        },
        Platform::Windows => Candidates {
            proportional: paths(&[r"C:\Windows\Fonts\segoeui.ttf"]),
            monospace: paths(&[r"C:\Windows\Fonts\consola.ttf"]),
        },
        Platform::Other => Candidates::default(),
    }
}

/// Asks fontconfig for a font. The command has fixed arguments and no shell.
fn fc_match(pattern: &str) -> Option<PathBuf> {
    let output = Command::new("fc-match")
        .args(["-f", "%{file}", pattern])
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    let text = String::from_utf8(output.stdout).ok()?;
    let text = text.trim();
    (!text.is_empty()).then(|| PathBuf::from(text))
}

/// A reason to refuse a font file.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum FontRefusal {
    Extension,
    LargeFontFamily,
    NotAFile,
    TooLarge,
    Unreadable,
    Signature,
}

/// Tells if a path can hold a font that lenka loads. This reads no file.
pub(crate) fn check_path(path: &Path) -> Result<(), FontRefusal> {
    let extension = path
        .extension()
        .and_then(|text| text.to_str())
        .map(str::to_ascii_lowercase);
    if !matches!(extension.as_deref(), Some("ttf" | "otf" | "ttc")) {
        return Err(FontRefusal::Extension);
    }
    // A CJK font can use more than 20 MiB of memory for the life of the process.
    let name = path
        .file_name()
        .and_then(|text| text.to_str())
        .unwrap_or_default()
        .to_ascii_uppercase();
    if name.contains("CJK") {
        return Err(FontRefusal::LargeFontFamily);
    }
    Ok(())
}

/// Tells if the first bytes are the signature of a font file.
fn has_font_signature(bytes: &[u8]) -> bool {
    matches!(
        bytes.get(..4),
        Some([0, 1, 0, 0] | b"OTTO" | b"true" | b"ttcf")
    )
}

/// Reads a font file, with a limit on its size.
pub(crate) fn read_font(path: &Path, limit: u64) -> Result<Vec<u8>, FontRefusal> {
    check_path(path)?;
    let metadata = std::fs::metadata(path).map_err(|_err| FontRefusal::Unreadable)?;
    if !metadata.is_file() {
        return Err(FontRefusal::NotAFile);
    }
    if metadata.len() > limit {
        return Err(FontRefusal::TooLarge);
    }
    let file = std::fs::File::open(path).map_err(|_err| FontRefusal::Unreadable)?;
    let mut bytes = Vec::new();
    // The file can grow after the size check, so the read has a limit too.
    file.take(limit + 1)
        .read_to_end(&mut bytes)
        .map_err(|_err| FontRefusal::Unreadable)?;
    if u64::try_from(bytes.len()).map_or(true, |length| length > limit) {
        return Err(FontRefusal::TooLarge);
    }
    if !has_font_signature(&bytes) {
        return Err(FontRefusal::Signature);
    }
    Ok(bytes)
}

fn single_font(bytes: Vec<u8>) -> FontDefinitions {
    let mut definitions = FontDefinitions::empty();
    definitions.font_data.insert(
        PROPORTIONAL_NAME.to_owned(),
        FontData::from_owned(bytes).into(),
    );
    definitions
        .families
        .insert(FontFamily::Proportional, vec![PROPORTIONAL_NAME.to_owned()]);
    definitions
}

/// Tells if the font parses and has all characters of a list.
///
/// The font store is separate and temporary, and a panic in the font code counts as a bad font.
fn has_glyphs(bytes: &[u8], glyphs: &str) -> bool {
    let definitions = single_font(bytes.to_vec());
    catch_unwind(AssertUnwindSafe(|| {
        let mut fonts = Fonts::new(TextOptions::default(), definitions);
        fonts.has_glyphs(&FontId::proportional(14.0), glyphs)
    }))
    .unwrap_or(false)
}

/// Reads the first font of a list that is safe to use and has the characters.
fn first_usable(paths: &[PathBuf], glyphs: &str) -> Option<Vec<u8>> {
    paths.iter().find_map(|path| {
        let bytes = read_font(path, MAX_FONT_BYTES).ok()?;
        has_glyphs(&bytes, glyphs).then_some(bytes)
    })
}

/// Builds the font list. Each family falls back to the other font.
pub(crate) fn definitions(proportional: Vec<u8>, monospace: Option<Vec<u8>>) -> FontDefinitions {
    let mut definitions = single_font(proportional);
    let mut names = vec![PROPORTIONAL_NAME.to_owned()];
    if let Some(bytes) = monospace {
        definitions.font_data.insert(
            MONOSPACE_NAME.to_owned(),
            FontData::from_owned(bytes).into(),
        );
        definitions.families.insert(
            FontFamily::Proportional,
            vec![PROPORTIONAL_NAME.to_owned(), MONOSPACE_NAME.to_owned()],
        );
        names.insert(0, MONOSPACE_NAME.to_owned());
    }
    definitions.families.insert(FontFamily::Monospace, names);
    definitions
}

/// The error when no font can show the UI.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct NoFontError;

impl fmt::Display for NoFontError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(
            "lenka found no usable system font, so the desktop app cannot show text. \
             Install a font such as Noto Sans, and start the app again.",
        )
    }
}

impl std::error::Error for NoFontError {}

/// Loads fonts from a list of paths.
pub(crate) fn load_from(candidates: &Candidates) -> Result<FontDefinitions, NoFontError> {
    let proportional = first_usable(&candidates.proportional, UI_GLYPHS).ok_or(NoFontError)?;
    let monospace = first_usable(&candidates.monospace, "0Aa");
    Ok(definitions(proportional, monospace))
}

/// Loads the system fonts of this machine.
pub(crate) fn load_system() -> Result<FontDefinitions, NoFontError> {
    let platform = Platform::current();
    let mut candidates = font_paths(platform);
    if platform == Platform::Linux || platform == Platform::Other {
        // The choice of fontconfig comes first, and the fixed paths are the fallback.
        if let Some(path) = fc_match("monospace") {
            candidates.monospace.insert(0, path);
        }
        if let Some(path) = fc_match("sans") {
            candidates.proportional.insert(0, path);
        }
    }
    load_from(&candidates)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn each_platform_has_its_fixed_paths() {
        let linux = font_paths(Platform::Linux);
        for name in [
            "NotoSans-Regular",
            "DejaVuSans.ttf",
            "LiberationSans-Regular",
        ] {
            assert!(
                linux
                    .proportional
                    .iter()
                    .any(|path| path.to_string_lossy().contains(name)),
                "{name}"
            );
        }
        for name in ["NotoSansMono", "DejaVuSansMono", "LiberationMono"] {
            assert!(
                linux
                    .monospace
                    .iter()
                    .any(|path| path.to_string_lossy().contains(name)),
                "{name}"
            );
        }
        let mac = font_paths(Platform::MacOs);
        assert_eq!(
            mac.proportional[0],
            Path::new("/System/Library/Fonts/Supplemental/Arial.ttf")
        );
        assert_eq!(
            mac.proportional[1],
            Path::new("/System/Library/Fonts/Helvetica.ttc")
        );
        assert_eq!(
            mac.monospace[0],
            Path::new("/System/Library/Fonts/Menlo.ttc")
        );
        let windows = font_paths(Platform::Windows);
        assert_eq!(
            windows.proportional[0],
            Path::new(r"C:\Windows\Fonts\segoeui.ttf")
        );
        assert_eq!(
            windows.monospace[0],
            Path::new(r"C:\Windows\Fonts\consola.ttf")
        );
        assert_eq!(font_paths(Platform::Other), Candidates::default());
    }

    #[test]
    fn only_font_extensions_and_small_families_pass() {
        for good in ["/a/b.ttf", "/a/b.OTF", "/a/b.ttc"] {
            assert_eq!(check_path(Path::new(good)), Ok(()), "{good}");
        }
        for bad in ["/a/b.pfb", "/a/b.txt", "/a/b", "/a/ttf"] {
            assert_eq!(
                check_path(Path::new(bad)),
                Err(FontRefusal::Extension),
                "{bad}"
            );
        }
        assert_eq!(
            check_path(Path::new("/a/NotoSansCJK-Regular.ttc")),
            Err(FontRefusal::LargeFontFamily)
        );
    }

    #[test]
    fn a_read_refuses_a_large_file_a_directory_and_a_bad_signature() {
        let temp = tempfile::tempdir().expect("a temporary directory");
        let large = temp.path().join("large.ttf");
        let file = std::fs::File::create(&large).expect("a file");
        file.set_len(MAX_FONT_BYTES + 1).expect("a length");
        assert_eq!(
            read_font(&large, MAX_FONT_BYTES),
            Err(FontRefusal::TooLarge)
        );

        let text = temp.path().join("text.ttf");
        std::fs::write(&text, "<html>not a font</html>").expect("a file");
        assert_eq!(
            read_font(&text, MAX_FONT_BYTES),
            Err(FontRefusal::Signature)
        );

        let directory = temp.path().join("directory.ttf");
        std::fs::create_dir(&directory).expect("a directory");
        assert_eq!(
            read_font(&directory, MAX_FONT_BYTES),
            Err(FontRefusal::NotAFile)
        );

        assert_eq!(
            read_font(&temp.path().join("missing.ttf"), MAX_FONT_BYTES),
            Err(FontRefusal::Unreadable)
        );
    }

    #[test]
    fn a_damaged_font_with_a_good_signature_does_not_load() {
        let temp = tempfile::tempdir().expect("a temporary directory");
        let path = temp.path().join("damaged.ttf");
        let mut bytes = vec![0, 1, 0, 0];
        bytes.extend(std::iter::repeat_n(0xAB, 4_096));
        std::fs::write(&path, bytes).expect("a file");
        let candidates = Candidates {
            proportional: vec![path],
            monospace: Vec::new(),
        };
        assert_eq!(load_from(&candidates).err(), Some(NoFontError));
    }

    #[test]
    fn no_proportional_font_is_a_clear_error() {
        let Err(error) = load_from(&Candidates::default()) else {
            panic!("an empty list of fonts must give an error");
        };
        assert!(error.to_string().contains("Noto Sans"), "{error}");
    }

    #[test]
    fn a_missing_monospace_font_uses_the_proportional_font_for_both() {
        let result = definitions(vec![0, 1, 0, 0], None);
        assert_eq!(
            result.families[&FontFamily::Monospace],
            vec![PROPORTIONAL_NAME.to_owned()]
        );
        assert_eq!(
            result.families[&FontFamily::Proportional],
            vec![PROPORTIONAL_NAME.to_owned()]
        );
        let both = definitions(vec![0, 1, 0, 0], Some(vec![0, 1, 0, 0]));
        assert_eq!(both.families[&FontFamily::Monospace][0], MONOSPACE_NAME);
        assert_eq!(
            both.families[&FontFamily::Proportional][0],
            PROPORTIONAL_NAME
        );
    }

    #[test]
    fn a_system_font_loads_if_this_machine_has_one() {
        match load_system() {
            Ok(fonts) => assert!(fonts.font_data.contains_key(PROPORTIONAL_NAME)),
            Err(error) => {
                println!("This machine has no usable font, so this test checks nothing: {error}")
            }
        }
    }
}

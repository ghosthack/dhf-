//! Text rasterisation for the game's `com.bicore.Font` class.
//!
//! On Android the Java layer drew each character with the system font into an 8-bit alpha
//! bitmap and handed it to the native code. The host does the same with a pure-Rust
//! rasteriser and font files chosen by the user; the guest never sees a font file.

use std::path::{Path, PathBuf};

pub struct Text {
    faces: Vec<fontdue::Font>,
}

/// What `Font.getStringData` passes to the native `setStringData`.
pub struct StringData {
    pub measured: i32,
    pub bounds_height: i32,
    pub advance: i32,
    pub width: u32,
    pub height: u32,
    pub alpha: Vec<u8>,
}

/// Fonts tried when none is given, covering Latin and CJK on common systems.
const DEFAULT_FONTS: &[&str] = &[
    "/usr/share/fonts/truetype/dejavu/DejaVuSans.ttf",
    "/usr/share/fonts/TTF/DejaVuSans.ttf",
    "/usr/share/fonts/truetype/liberation/LiberationSans-Regular.ttf",
    "/usr/share/fonts/opentype/noto/NotoSansCJK-Regular.ttc",
    "/usr/share/fonts/noto-cjk/NotoSansCJK-Regular.ttc",
    "/usr/share/fonts/truetype/wqy/wqy-microhei.ttc",
    "/usr/share/fonts/truetype/droid/DroidSansFallbackFull.ttf",
    "/System/Library/Fonts/Supplemental/Arial.ttf",
    "/System/Library/Fonts/AppleSDGothicNeo.ttc",
    "/System/Library/Fonts/PingFang.ttc",
    "C:\\Windows\\Fonts\\arial.ttf",
    "C:\\Windows\\Fonts\\malgun.ttf",
    "C:\\Windows\\Fonts\\msyh.ttc",
];

impl Text {
    /// Load the given fonts, or the system defaults if none are given. Earlier fonts win;
    /// later ones fill in characters the earlier ones lack.
    pub fn load(paths: &[PathBuf]) -> Text {
        let defaults: Vec<PathBuf> = DEFAULT_FONTS.iter().map(PathBuf::from).collect();
        let (list, explicit) = if paths.is_empty() { (&defaults[..], false) } else { (paths, true) };
        let mut faces = Vec::new();
        for p in list {
            match Self::load_one(p) {
                Ok(f) => faces.push(f),
                Err(e) if explicit => eprintln!("[host] warning: font {}: {e}", p.display()),
                Err(_) => {}
            }
        }
        if faces.is_empty() {
            eprintln!("[host] warning: no usable font found; text will be blank (use --font PATH)");
        }
        Text { faces }
    }

    fn load_one(p: &Path) -> Result<fontdue::Font, String> {
        let data = std::fs::read(p).map_err(|e| e.to_string())?;
        fontdue::Font::from_bytes(data, fontdue::FontSettings::default()).map_err(|e| e.to_string())
    }

    fn face_for(&self, c: char) -> Option<&fontdue::Font> {
        self.faces.iter().find(|f| f.lookup_glyph_index(c) != 0).or(self.faces.first())
    }

    /// (ascent, descent, leading) in pixels, all positive, from the primary font.
    pub fn line_metrics(&self, px: f32) -> (f32, f32, f32) {
        match self.faces.first().and_then(|f| f.horizontal_line_metrics(px)) {
            Some(l) => (l.ascent.abs(), l.descent.abs(), l.line_gap.max(0.0)),
            None => (px * 0.93, px * 0.24, 0.0),
        }
    }

    pub fn line_height(&self, px: f32) -> u32 {
        let (a, d, _) = self.line_metrics(px);
        (a + d).ceil() as u32
    }

    /// Mirror of `Font.getStringData`: the bitmap is sized from the *first* character's
    /// bounds (plus 5 pixels), one line high, with the text drawn from x = 0.
    pub fn string_data(&self, s: &str, px: f32) -> Option<StringData> {
        let first = s.chars().next()?;
        let (ascent, _, _) = self.line_metrics(px);
        let height = self.line_height(px).clamp(1, 512);
        let baseline = ascent.round() as i32;
        let (fm, _) = self.face_for(first).map(|f| f.rasterize(first, px))?;
        let width = if fm.width == 0 { 1 } else { (fm.width as u32 + 5).min(2048) };
        let mut alpha = vec![0u8; (width * height) as usize];
        let (mut pen, mut measured, mut advance0) = (0.0f32, 0.0f32, None);
        // The game draws one character at a time; bound the work for anything longer.
        for c in s.chars().take(256) {
            let Some(face) = self.face_for(c) else { continue };
            let (gm, cov) = face.rasterize(c, px);
            let (x0, y0) = (pen.round() as i32 + gm.xmin, baseline - gm.ymin - gm.height as i32);
            for gy in 0..gm.height as i32 {
                for gx in 0..gm.width as i32 {
                    let (x, y) = (x0 + gx, y0 + gy);
                    if x >= 0 && y >= 0 && (x as u32) < width && (y as u32) < height {
                        let d = &mut alpha[(y as u32 * width + x as u32) as usize];
                        *d = (*d).max(cov[(gy * gm.width as i32 + gx) as usize]);
                    }
                }
            }
            advance0.get_or_insert(gm.advance_width);
            pen += gm.advance_width;
            measured += gm.advance_width;
        }
        Some(StringData {
            measured: measured as i32,
            bounds_height: fm.height as i32,
            advance: advance0.unwrap_or(0.0).ceil() as i32,
            width,
            height,
            alpha,
        })
    }
}

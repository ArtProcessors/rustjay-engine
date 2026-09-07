//! Font atlases: one texture holding every glyph, and where each character
//! sits in it.
//!
//! Two kinds, one shape. A TTF/OTF is rasterised glyph by glyph and packed;
//! an image someone drew is cut into a grid. Either way the rest of the
//! pipeline sees the same thing — a texture plus a [`Cell`] per character —
//! which is what lets a hand-drawn atlas be a font.
//!
//! Everything outside the atlas is measured in **em units**: one em is the
//! size the glyphs were rasterised at, so a layout is resolution-independent
//! and the source scales it at draw time.

use ab_glyph::{Font, FontVec, PxScale, ScaleFont};
use std::collections::HashMap;
use std::path::Path;

/// Size glyphs are rasterised at. Text is scaled from here, so this is the
/// ceiling on how large it can get before softening.
const EM: f32 = 128.0;
/// Atlas texture width. Glyphs shelf-pack into rows of this width.
const ATLAS_W: u32 = 2048;
/// The characters a font atlas covers before anything the text itself needs.
const ASCII: &str = " !\"#$%&'()*+,-./0123456789:;<=>?@ABCDEFGHIJKLMNOPQRSTUVWXYZ\
[\\]^_`abcdefghijklmnopqrstuvwxyz{|}~";

/// Where one character lives in the atlas, and how it sits on the line.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Cell {
    /// `[u0, v0, u1, v1]` in the atlas texture.
    pub uv: [f32; 4],
    /// Quad size in em units.
    pub size: [f32; 2],
    /// Offset from the pen position to the quad's top-left, in em units. The
    /// y is measured from the baseline, negative upward.
    pub bearing: [f32; 2],
    /// How far the pen moves after this character, in em units.
    pub advance: f32,
}

/// A texture of glyphs, and the map into it.
pub struct Atlas {
    pub pixels: Vec<u8>,
    pub width: u32,
    pub height: u32,
    /// True when the pixels carry colour of their own — a drawn atlas — rather
    /// than coverage to be tinted.
    pub colour: bool,
    pub cells: HashMap<char, Cell>,
    /// Whether a drawn atlas found its `.txt` sidecar. False means the
    /// characters were guessed, which is the first thing to check when the
    /// letters come out wrong.
    pub sidecar: bool,
    /// Top of the line box to the baseline, in em units.
    pub ascent: f32,
    /// Baseline to baseline, in em units.
    pub line_height: f32,
}

impl Atlas {
    /// Whether every character in `text` has a cell. A miss means the atlas
    /// has to be rebuilt to cover it.
    pub fn covers(&self, text: &str) -> bool {
        text.chars().all(|c| c == '\n' || self.cells.contains_key(&c))
    }

    /// Rasterise a font into a packed atlas: the printable ASCII range plus
    /// whatever else `text` uses, so an accent or a symbol pasted into a layer
    /// is covered without carrying every glyph in the file.
    pub fn from_font(font: &FontVec, text: &str) -> Self {
        let scaled = font.as_scaled(PxScale::from(EM));
        let mut chars: Vec<char> = ASCII.chars().collect();
        for c in text.chars() {
            if c != '\n' && !chars.contains(&c) {
                chars.push(c);
            }
        }

        // Shelf-pack: place left to right, drop to a new shelf when the row is
        // full. Glyphs are all much the same height, so the waste is small and
        // a real packer would not earn its keep.
        struct Slot {
            ch: char,
            outline: ab_glyph::OutlinedGlyph,
            x: u32,
            y: u32,
            w: u32,
            h: u32,
        }
        let mut slots: Vec<Slot> = Vec::new();
        let (mut pen_x, mut shelf_y, mut shelf_h) = (0_u32, 0_u32, 0_u32);
        for ch in &chars {
            let id = scaled.glyph_id(*ch);
            let glyph = id.with_scale_and_position(EM, ab_glyph::point(0.0, 0.0));
            let Some(outline) = font.outline_glyph(glyph) else {
                continue;
            };
            let b = outline.px_bounds();
            let (w, h) = (b.width().ceil() as u32 + 1, b.height().ceil() as u32 + 1);
            if pen_x + w > ATLAS_W {
                pen_x = 0;
                shelf_y += shelf_h + 1;
                shelf_h = 0;
            }
            slots.push(Slot { ch: *ch, outline, x: pen_x, y: shelf_y, w, h });
            pen_x += w;
            shelf_h = shelf_h.max(h);
        }
        let height = (shelf_y + shelf_h + 1).max(1);

        let mut pixels = vec![0u8; (ATLAS_W * height) as usize];
        let mut cells = HashMap::new();
        for slot in &slots {
            let b = slot.outline.px_bounds();
            slot.outline.draw(|gx, gy, coverage| {
                let px = slot.x + gx;
                let py = slot.y + gy;
                if px >= ATLAS_W || py >= height {
                    return;
                }
                let i = (py * ATLAS_W + px) as usize;
                pixels[i] = pixels[i].max((coverage * 255.0) as u8);
            });
            cells.insert(
                slot.ch,
                Cell {
                    uv: [
                        slot.x as f32 / ATLAS_W as f32,
                        slot.y as f32 / height as f32,
                        (slot.x + slot.w) as f32 / ATLAS_W as f32,
                        (slot.y + slot.h) as f32 / height as f32,
                    ],
                    size: [slot.w as f32 / EM, slot.h as f32 / EM],
                    bearing: [b.min.x / EM, b.min.y / EM],
                    advance: scaled.h_advance(scaled.glyph_id(slot.ch)) / EM,
                },
            );
        }
        // A space has no outline, so it never reaches the packer — but it very
        // much has an advance, and without a cell it would swallow the gap.
        for ch in [' ', '\t'] {
            let advance = scaled.h_advance(scaled.glyph_id(ch)) / EM;
            if advance > 0.0 {
                cells.insert(
                    ch,
                    Cell { uv: [0.0; 4], size: [0.0; 2], bearing: [0.0; 2], advance },
                );
            }
        }

        Self {
            pixels,
            width: ATLAS_W,
            height,
            colour: false,
            sidecar: false,
            cells,
            ascent: scaled.ascent() / EM,
            line_height: (scaled.ascent() - scaled.descent() + scaled.line_gap()) / EM,
        }
    }

    /// Cut a drawn atlas into a grid.
    ///
    /// The characters come from a sidecar text file next to the image, one
    /// line per row of the grid — so the file *is* the mapping, and its shape
    /// gives the rows and columns without a single parameter:
    ///
    /// ```text
    /// ABCDEFGH
    /// IJKLMNOP
    /// ```
    ///
    /// Without a sidecar the grid is assumed to be printable ASCII in 16
    /// columns, which is the common convention for a drawn font.
    pub fn from_image(path: &Path) -> Option<Self> {
        let image = image::open(path).ok()?.to_rgba8();
        let (width, height) = image.dimensions();
        let sidecar = path.with_extension("txt").is_file();
        let rows = charset_rows(path);
        let cols = rows.iter().map(|r| r.chars().count()).max().unwrap_or(1).max(1) as u32;
        let row_count = rows.len().max(1) as u32;
        let (cw, ch) = (width / cols, height / row_count);
        // Each cell is one em tall, so a wide cell is a wide glyph.
        let aspect = cw as f32 / ch.max(1) as f32;

        let mut cells = HashMap::new();
        for (row, line) in rows.iter().enumerate() {
            for (col, c) in line.chars().enumerate() {
                if col as u32 >= cols {
                    break;
                }
                let (x, y) = (col as u32 * cw, row as u32 * ch);
                cells.insert(
                    c,
                    Cell {
                        uv: [
                            x as f32 / width as f32,
                            y as f32 / height as f32,
                            (x + cw) as f32 / width as f32,
                            (y + ch) as f32 / height as f32,
                        ],
                        size: [aspect, 1.0],
                        bearing: [0.0, -1.0],
                        advance: aspect,
                    },
                );
            }
        }
        // A drawn atlas rarely has a space cell worth drawing.
        cells.entry(' ').or_insert(Cell {
            uv: [0.0; 4],
            size: [0.0; 2],
            bearing: [0.0; 2],
            advance: aspect,
        });

        Some(Self {
            pixels: image.into_raw(),
            width,
            height,
            colour: true,
            sidecar,
            cells,
            ascent: 1.0,
            line_height: 1.15,
        })
    }
}

/// The rows of characters a drawn atlas holds, from `<image>.txt` if it is
/// there. Blank lines are skipped so a sidecar can be laid out readably.
fn charset_rows(image: &Path) -> Vec<String> {
    let sidecar = image.with_extension("txt");
    if let Ok(text) = std::fs::read_to_string(&sidecar) {
        let rows: Vec<String> = text
            .lines()
            .map(|l| l.trim_end_matches(['\r', '\n']).to_string())
            .filter(|l| !l.is_empty())
            .collect();
        if !rows.is_empty() {
            return rows;
        }
    }
    ASCII
        .chars()
        .collect::<Vec<_>>()
        .chunks(16)
        .map(|c| c.iter().collect())
        .collect()
}

/// What the layout depends on. A change here rebuilds the quads.
#[derive(Clone, PartialEq, Debug)]
pub struct Layout {
    pub text: String,
    pub tracking: f32,
    pub line_height: f32,
    pub align: usize,
}

impl Default for Layout {
    fn default() -> Self {
        Self { text: "TEXT".to_string(), tracking: 0.0, line_height: 1.2, align: 1 }
    }
}

/// One glyph, ready to draw: where it sits in the text block and where its
/// picture is in the atlas.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Quad {
    /// `[x, y, w, h]` in em units, from the block's top-left.
    pub rect: [f32; 4],
    /// `[u0, v0, u1, v1]` in the atlas.
    pub uv: [f32; 4],
    /// Ordinal of this glyph in the string, which is what staggers per-glyph
    /// animation.
    pub index: f32,
}

/// Lay the string out into quads, and report the block's size in em units.
///
/// Newlines break lines; everything else is one run. No shaping, so this is
/// Latin-shaped text — Arabic or Devanagari would need a shaper.
pub fn layout(atlas: &Atlas, l: &Layout) -> (Vec<Quad>, [f32; 2]) {
    let step = atlas.line_height * l.line_height;
    let mut lines: Vec<(Vec<Quad>, f32)> = Vec::new();
    let mut index = 0.0;

    // A trailing newline is how a text box ends, not a blank line to make room
    // for — left in, it shifts the whole string up by half a line.
    for (row, line) in l.text.trim_end_matches('\n').split('\n').enumerate() {
        let mut quads = Vec::new();
        let mut pen = 0.0_f32;
        let baseline = atlas.ascent + row as f32 * step;
        for c in line.chars() {
            let Some(cell) = atlas.cells.get(&c) else {
                continue;
            };
            if cell.size[0] > 0.0 && cell.size[1] > 0.0 {
                quads.push(Quad {
                    rect: [
                        pen + cell.bearing[0],
                        baseline + cell.bearing[1],
                        cell.size[0],
                        cell.size[1],
                    ],
                    uv: cell.uv,
                    index,
                });
            }
            pen += cell.advance + l.tracking;
            index += 1.0;
        }
        lines.push((quads, (pen - l.tracking).max(0.0)));
    }

    let block_w = lines.iter().map(|(_, w)| *w).fold(0.0_f32, f32::max);
    let block_h = atlas.line_height + (lines.len().saturating_sub(1)) as f32 * step;
    let mut out = Vec::new();
    for (quads, width) in lines {
        let shift = match l.align {
            0 => 0.0,
            2 => block_w - width,
            _ => (block_w - width) / 2.0,
        };
        out.extend(quads.into_iter().map(|mut q| {
            q.rect[0] += shift;
            q
        }));
    }
    // A floor with room in it: the vertex stage divides by the block height,
    // and an epsilon would divide by a millionth.
    (out, [block_w.max(0.01), block_h.max(0.01)])
}

#[cfg(test)]
mod tests {
    use super::*;

    fn a_font() -> Option<FontVec> {
        let path = crate::sources::text_source::default_font()?;
        FontVec::try_from_vec(std::fs::read(path).ok()?).ok()
    }

    /// The sidecar *is* the mapping: its lines are the grid's rows, and their
    /// length is its columns. Nothing else describes a drawn atlas.
    #[test]
    fn a_drawn_atlas_takes_its_characters_from_the_sidecar() {
        let dir = std::env::temp_dir().join(format!(
            "kovvboj_atlas_{}_{:?}",
            std::process::id(),
            std::thread::current().id()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let png = dir.join("font.png");
        // Four columns, two rows, of square cells.
        image::RgbaImage::from_pixel(32, 16, image::Rgba([255, 0, 0, 255]))
            .save(&png)
            .unwrap();
        std::fs::write(dir.join("font.txt"), "ABCD\nEFGH\n").unwrap();

        let atlas = Atlas::from_image(&png).expect("atlas loads");
        assert!(atlas.sidecar);
        assert!(atlas.colour, "a drawn atlas carries its own colour");
        assert_eq!(atlas.cells[&'A'].uv, [0.0, 0.0, 0.25, 0.5]);
        assert_eq!(atlas.cells[&'F'].uv, [0.25, 0.5, 0.5, 1.0]);
        assert_eq!(atlas.cells[&'A'].advance, 1.0, "square cells advance one em");
        assert!(atlas.covers("FADE"), "every character in the sidecar maps");

        // Without the sidecar the grid is a guess, and it says so.
        std::fs::remove_file(dir.join("font.txt")).unwrap();
        assert!(!Atlas::from_image(&png).unwrap().sidecar);

        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn a_font_atlas_covers_the_printable_range() {
        let Some(font) = a_font() else { return };
        let atlas = Atlas::from_font(&font, "");
        assert!(atlas.covers(ASCII), "every printable character needs a cell");
        assert!(atlas.width > 0 && atlas.height > 0);
        assert_eq!(atlas.pixels.len(), (atlas.width * atlas.height) as usize);
    }

    /// A character outside the built-in range is added when the text uses it,
    /// rather than silently vanishing.
    #[test]
    fn the_text_extends_the_atlas() {
        let Some(font) = a_font() else { return };
        assert!(!Atlas::from_font(&font, "").covers("é"));
        assert!(Atlas::from_font(&font, "café").covers("café"));
    }

    #[test]
    fn a_space_advances_even_though_it_draws_nothing() {
        let Some(font) = a_font() else { return };
        let atlas = Atlas::from_font(&font, "");
        let space = atlas.cells[&' '];
        assert!(space.advance > 0.0);
        assert_eq!(space.size, [0.0, 0.0], "nothing to draw");

        let (quads, size) = layout(&atlas, &Layout { text: "A A".into(), ..Default::default() });
        assert_eq!(quads.len(), 2, "the space is not a quad");
        let (one, _) = layout(&atlas, &Layout { text: "AA".into(), ..Default::default() });
        assert!(size[0] > one[1].rect[0], "the gap is wider without it");
    }

    #[test]
    fn every_line_adds_height_and_alignment_shifts_the_short_one() {
        let Some(font) = a_font() else { return };
        let atlas = Atlas::from_font(&font, "");
        let one = layout(&atlas, &Layout { text: "AAAA".into(), ..Default::default() });
        let two = layout(&atlas, &Layout { text: "AAAA\nA".into(), ..Default::default() });
        assert!(two.1[1] > one.1[1], "a second line is taller");

        let centred = layout(&atlas, &Layout { text: "AAAA\nA".into(), align: 1, ..Default::default() });
        let left = layout(&atlas, &Layout { text: "AAAA\nA".into(), align: 0, ..Default::default() });
        assert!(centred.0.last().unwrap().rect[0] > left.0.last().unwrap().rect[0]);
    }

    /// The glyph ordinal is what staggers per-glyph animation, so it counts
    /// through the whole string — unbroken across lines, or the stagger would
    /// jump at every line break.
    /// Glyphs are laid out in em units, so their size is the font's business
    /// and the leading is the block's. Sizing used to come off the block's
    /// height, which made a second line shrink the letters.
    #[test]
    fn leading_moves_the_lines_without_resizing_the_glyphs() {
        let Some(font) = a_font() else { return };
        let atlas = Atlas::from_font(&font, "");
        let tight = layout(&atlas, &Layout { text: "A\nB".into(), line_height: 1.0, ..Default::default() });
        let loose = layout(&atlas, &Layout { text: "A\nB".into(), line_height: 2.0, ..Default::default() });
        assert_eq!(tight.0[0].rect[2..], loose.0[0].rect[2..], "same glyph size");
        assert!(loose.0[1].rect[1] > tight.0[1].rect[1], "the second line drops further");
        assert!(loose.1[1] > tight.1[1], "and the block is taller for it");
    }

    /// The newline that ends a text box is not a blank line to make room for.
    #[test]
    fn a_trailing_newline_is_not_a_line() {
        let Some(font) = a_font() else { return };
        let atlas = Atlas::from_font(&font, "");
        let plain = layout(&atlas, &Layout { text: "DJ".into(), ..Default::default() });
        let ended = layout(&atlas, &Layout { text: "DJ\n".into(), ..Default::default() });
        assert_eq!(plain.1, ended.1);
        // A blank line in the middle still is one.
        let gapped = layout(&atlas, &Layout { text: "DJ\n\nAC".into(), ..Default::default() });
        assert!(gapped.1[1] > plain.1[1]);
    }

    #[test]
    fn the_glyph_index_runs_through_the_whole_string() {
        let Some(font) = a_font() else { return };
        let atlas = Atlas::from_font(&font, "");
        let (quads, _) = layout(&atlas, &Layout { text: "AB\nCD".into(), ..Default::default() });
        let indices: Vec<f32> = quads.iter().map(|q| q.index).collect();
        assert_eq!(indices, vec![0.0, 1.0, 2.0, 3.0]);
    }
}

//! CoreText でグリフを濃さの表にする。フォントに無い文字は
//! macOS のフォールバックに任せ、別のフォントで描く。

use core_foundation::base::{CFRange, TCFType};
use core_foundation::string::{CFString, CFStringRef};
use core_graphics::base::kCGImageAlphaNone;
use core_graphics::color_space::CGColorSpace;
use core_graphics::context::CGContext;
use core_graphics::geometry::{CGPoint, CGRect, CGSize};
use core_text::font::{self as ct_font, CTFont, CTFontRef};
use core_text::font_descriptor::kCTFontOrientationDefault;

#[link(name = "CoreText", kind = "framework")]
unsafe extern "C" {
    fn CTFontCreateForString(current: CTFontRef, string: CFStringRef, range: CFRange) -> CTFontRef;
}

/// 1 文字分の濃さの表。`coverage` は上の行から順に並ぶ。
/// `left` と `bottom` はペン位置 (ベースライン) から見た表の左下の位置で、上向きが正。
#[derive(Clone, Debug, Default)]
pub struct Glyph {
    pub left: i32,
    pub bottom: i32,
    pub width: usize,
    pub height: usize,
    pub advance: f32,
    pub coverage: Vec<u8>,
}

pub struct Face {
    font: CTFont,
}

impl Face {
    pub fn new(name: &str, px: f32) -> Self {
        let font = ct_font::new_from_name(name, px as f64)
            .unwrap_or_else(|()| panic!("フォント読込失敗 {name}"));
        Self { font }
    }

    pub fn ascent(&self) -> f32 {
        self.font.ascent() as f32
    }

    pub fn line_height(&self) -> f32 {
        (self.font.ascent() + self.font.descent() + self.font.leading()) as f32
    }

    /// `c` を描くフォント (無ければフォールバック先)、そのグリフ番号、送り幅。
    fn lookup(&self, c: char) -> (CTFont, u16, f32) {
        let mut units = [0u16; 2];
        let units = c.encode_utf16(&mut units);
        let string = CFString::new(c.encode_utf8(&mut [0; 4]));
        let font = unsafe {
            CTFont::wrap_under_create_rule(CTFontCreateForString(
                self.font.as_concrete_TypeRef(),
                string.as_concrete_TypeRef(),
                CFRange::init(0, units.len() as isize),
            ))
        };
        let mut glyphs = [0u16; 2];
        unsafe { font.get_glyphs_for_characters(units.as_ptr(), glyphs.as_mut_ptr(), units.len() as isize) };
        let glyph = glyphs[0];
        let mut advance = CGSize::new(0.0, 0.0);
        unsafe { font.get_advances_for_glyphs(kCTFontOrientationDefault, &glyph, &mut advance, 1) };
        (font, glyph, advance.width as f32)
    }

    pub fn advance(&self, c: char) -> f32 {
        self.lookup(c).2
    }

    pub fn rasterize(&self, c: char) -> Glyph {
        let (font, glyph, advance) = self.lookup(c);

        let bounds = font.get_bounding_rects_for_glyphs(kCTFontOrientationDefault, &[glyph]);
        let left = bounds.origin.x.floor();
        let bottom = bounds.origin.y.floor();
        let width = ((bounds.origin.x + bounds.size.width).ceil() - left).max(0.0) as usize;
        let height = ((bounds.origin.y + bounds.size.height).ceil() - bottom).max(0.0) as usize;
        if width == 0 || height == 0 {
            return Glyph { advance, ..Glyph::default() };
        }

        let mut context = CGContext::create_bitmap_context(
            None, width, height, 8, width, &CGColorSpace::create_device_gray(), kCGImageAlphaNone,
        );
        context.set_gray_fill_color(0.0, 1.0);
        context.fill_rect(CGRect::new(&CGPoint::new(0.0, 0.0), &CGSize::new(width as f64, height as f64)));
        context.set_gray_fill_color(1.0, 1.0);
        context.set_allows_font_smoothing(false);
        context.set_should_antialias(true);
        font.draw_glyphs(&[glyph], &[CGPoint::new(-left, -bottom)], context.clone());

        let stride = context.bytes_per_row();
        let data = context.data();
        let coverage = (0..height).flat_map(|row| data[row * stride..row * stride + width].to_vec()).collect();
        Glyph { left: left as i32, bottom: bottom as i32, width, height, advance, coverage }
    }
}

/// ブロック要素 (U+2580–U+259F) の塗る範囲。座標はマスを 8 等分した目盛りで、左上が原点。
enum Block {
    Rects(&'static [(u8, u8, u8, u8)]),
    /// 網点の濃さ (4 点中いくつ塗るか)。二値化しても消えないよう、濃淡ではなく点で表す。
    Shade(u8),
}

const UPPER: (u8, u8, u8, u8) = (0, 0, 8, 4);
const LOWER: (u8, u8, u8, u8) = (0, 4, 8, 8);
const UL: (u8, u8, u8, u8) = (0, 0, 4, 4);
const UR: (u8, u8, u8, u8) = (4, 0, 8, 4);
const LL: (u8, u8, u8, u8) = (0, 4, 4, 8);
const LR: (u8, u8, u8, u8) = (4, 4, 8, 8);

fn block(c: char) -> Option<Block> {
    use Block::*;
    Some(match c {
        '▀' => Rects(&[UPPER]),
        '▁' => Rects(&[(0, 7, 8, 8)]),
        '▂' => Rects(&[(0, 6, 8, 8)]),
        '▃' => Rects(&[(0, 5, 8, 8)]),
        '▄' => Rects(&[LOWER]),
        '▅' => Rects(&[(0, 3, 8, 8)]),
        '▆' => Rects(&[(0, 2, 8, 8)]),
        '▇' => Rects(&[(0, 1, 8, 8)]),
        '█' => Rects(&[(0, 0, 8, 8)]),
        '▉' => Rects(&[(0, 0, 7, 8)]),
        '▊' => Rects(&[(0, 0, 6, 8)]),
        '▋' => Rects(&[(0, 0, 5, 8)]),
        '▌' => Rects(&[(0, 0, 4, 8)]),
        '▍' => Rects(&[(0, 0, 3, 8)]),
        '▎' => Rects(&[(0, 0, 2, 8)]),
        '▏' => Rects(&[(0, 0, 1, 8)]),
        '▐' => Rects(&[(4, 0, 8, 8)]),
        '░' => Shade(1),
        '▒' => Shade(2),
        '▓' => Shade(3),
        '▔' => Rects(&[(0, 0, 8, 1)]),
        '▕' => Rects(&[(7, 0, 8, 8)]),
        '▖' => Rects(&[LL]),
        '▗' => Rects(&[LR]),
        '▘' => Rects(&[UL]),
        '▙' => Rects(&[UL, LL, LR]),
        '▚' => Rects(&[UL, LR]),
        '▛' => Rects(&[UL, UR, LL]),
        '▜' => Rects(&[UL, UR, LR]),
        '▝' => Rects(&[UR]),
        '▞' => Rects(&[UR, LL]),
        '▟' => Rects(&[UR, LL, LR]),
        _ => return None,
    })
}

/// マスの左上の画素位置 (`x`, `y`) で変わる、網点の模様のずれ。網点以外は常に (0, 0)。
/// マスの幅や高さが奇数でも隣のマスと模様がつながるよう、網点は画面全体の位置で打つ。
pub fn pattern_phase(c: char, x: i32, y: i32) -> (usize, usize) {
    match block(c) {
        Some(Block::Shade(_)) => ((x & 1) as usize, (y & 1) as usize),
        _ => (0, 0),
    }
}

/// ブロック要素をマスいっぱいに描く。フォントの字形はマスの高さに合わず、
/// 縦に並べたときにすき間やずれが出るため。`ascent` はマス上端からベースラインまで。
/// `phase` は [`pattern_phase`] の値。
pub fn block_element(c: char, cell_w: usize, cell_h: usize, ascent: i32, phase: (usize, usize)) -> Option<Glyph> {
    let mut coverage = vec![0u8; cell_w * cell_h];
    match block(c)? {
        Block::Rects(rects) => {
            let at = |eighths: u8, size: usize| (size * eighths as usize + 4) / 8;
            for &(x0, y0, x1, y1) in rects {
                for row in at(y0, cell_h)..at(y1, cell_h) {
                    coverage[row * cell_w + at(x0, cell_w)..row * cell_w + at(x1, cell_w)].fill(255);
                }
            }
        }
        Block::Shade(level) => {
            for row in 0..cell_h {
                for col in 0..cell_w {
                    let (col, row, i) = (col + phase.0, row + phase.1, row * cell_w + col);
                    let dot = match level {
                        1 => row % 2 == 0 && col % 2 == 0,
                        2 => (row + col) % 2 == 0,
                        _ => !(row % 2 == 1 && col % 2 == 1),
                    };
                    coverage[i] = if dot { 255 } else { 0 };
                }
            }
        }
    }
    Some(Glyph {
        left: 0,
        bottom: ascent - cell_h as i32,
        width: cell_w,
        height: cell_h,
        advance: cell_w as f32,
        coverage,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn block_elements_fill_the_cell_so_rows_join_without_gaps() {
        let (w, h) = (14, 33);
        let full = block_element('█', w, h, 24, (0, 0)).unwrap();
        assert!(full.coverage.iter().all(|&a| a == 255));
        assert_eq!(full.bottom + full.height as i32, 24, "マスの上端から描く");

        let quadrant = block_element('▛', w, h, 24, (0, 0)).unwrap();
        let ink = |row: usize, col: usize| quadrant.coverage[row * w + col];
        assert_eq!((ink(0, 0), ink(0, w - 1), ink(h - 1, 0)), (255, 255, 255));
        assert_eq!(ink(h - 1, w - 1), 0);

        assert!(('\u{2580}'..='\u{259f}').all(|c| block_element(c, w, h, 24, (0, 0)).is_some()));
        assert!(block_element('─', w, h, 24, (0, 0)).is_none());
    }

    #[test]
    fn shades_on_odd_sized_cells_join_without_seams() {
        let (w, h) = (7, 17);
        let patterns: [(char, fn(usize, usize) -> bool); 3] = [
            ('░', |x, y| x % 2 == 0 && y % 2 == 0),
            ('▒', |x, y| (x + y) % 2 == 0),
            ('▓', |x, y| !(x % 2 == 1 && y % 2 == 1)),
        ];
        for (c, expected) in patterns {
            for (cell_x, cell_y) in [(0, 0), (w, 0), (0, h), (w, h)] {
                let phase = pattern_phase(c, cell_x as i32, cell_y as i32);
                let glyph = block_element(c, w, h, 12, phase).unwrap();
                for row in 0..h {
                    for col in 0..w {
                        let (x, y) = (cell_x + col, cell_y + row);
                        assert_eq!(glyph.coverage[row * w + col] == 255, expected(x, y), "{c} の ({x}, {y}) が画面全体の模様とずれる");
                    }
                }
            }
        }
        assert_eq!(pattern_phase('█', 7, 17), (0, 0));
    }

    fn ink(glyph: &Glyph, row: usize, col: usize) -> u8 {
        glyph.coverage[row * glyph.width + col]
    }

    #[test]
    fn latin1_characters_are_not_read_through_the_mac_roman_table() {
        let face = Face::new("Monaco", 24.0);
        let dot = face.rasterize('·');
        assert!(dot.height * 3 < face.rasterize('M').height, "· は ∑ ではなく小さな点");
    }

    #[test]
    fn symbols_missing_from_monaco_come_from_another_font() {
        let face = Face::new("Monaco", 24.0);
        let quadrant = face.rasterize('▛');
        let (w, h) = (quadrant.width, quadrant.height);
        assert_eq!(ink(&quadrant, 1, 1), 255, "左上は塗られる");
        assert_eq!(ink(&quadrant, h - 2, w - 2), 0, "右下は空く");
        for c in ['❯', '⏵', '✻'] {
            assert!(face.rasterize(c).coverage.iter().any(|&a| a > 0), "{c} が描かれる");
        }
    }

    #[test]
    fn blank_glyphs_keep_their_advance() {
        let space = Face::new("Monaco", 24.0).rasterize(' ');
        assert!(space.coverage.is_empty());
        assert!(space.advance > 0.0);
    }
}

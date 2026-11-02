//! CPU レンダラ。Platinum 風ウィンドウクロームと端末グリッドを
//! softbuffer のピクセルバッファへ直接描画する。

use std::collections::HashMap;
use std::hash::Hash;

use alacritty_terminal::term::cell::Flags;
use alacritty_terminal::event::EventListener;
use alacritty_terminal::grid::Dimensions;
use alacritty_terminal::term::color::Colors;
use alacritty_terminal::term::{self, Term};
use alacritty_terminal::vte::ansi::{Color, CursorShape, NamedColor, Rgb};

use crate::font::{self, Face, Glyph};
use crate::ime::Preedit;
use crate::panes::{SplitDir, SplitterHit};
use crate::theme::{self, Scheme};

// 無いグリフ (日本語・記号など) は CoreText が別のフォントから探す
const MONACO: &str = "Monaco";
const GENEVA: &str = "Geneva";

const FONT_PT: f32 = 12.0; // 端末本文 (論理px)
const TITLE_PT: f32 = 12.0; // タイトルバー (論理px)

#[derive(Copy, Clone, Debug, Default)]
pub struct Rect {
    pub x: i32,
    pub y: i32,
    pub w: i32,
    pub h: i32,
}

impl Rect {
    pub fn new(x: i32, y: i32, w: i32, h: i32) -> Self {
        Self { x, y, w, h }
    }

    pub fn inset(&self, e: i32) -> Self {
        Self::new(self.x + e, self.y + e, self.w - 2 * e, self.h - 2 * e)
    }

    pub fn contains(&self, px: f64, py: f64) -> bool {
        px >= self.x as f64
            && px < (self.x + self.w) as f64
            && py >= self.y as f64
            && py < (self.y + self.h) as f64
    }

    pub fn right(&self) -> i32 {
        self.x + self.w
    }

    pub fn bottom(&self) -> i32 {
        self.y + self.h
    }

    pub fn center(&self) -> (i32, i32) {
        (self.x + self.w / 2, self.y + self.h / 2)
    }
}

/// 設定ウィンドウで押せる行。
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum SettingsHit {
    Scheme(usize),
    BitmapText,
}

#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum ScrollbarPart {
    Up,
    Down,
    PageUp,
    PageDown,
    Thumb,
}

/// 描画と操作が共用するスクロールバー。offset は最下部を 0 とする。
#[derive(Copy, Clone, Debug)]
pub struct ScrollbarLayout {
    pub bounds: Rect,
    pub up: Rect,
    pub down: Rect,
    pub track: Rect,
    pub thumb: Rect,
    pub history_size: usize,
}

impl ScrollbarLayout {
    pub fn compute(pane: Rect, scale: f64, history_size: usize, screen_lines: usize, offset: usize) -> Self {
        let s = |v: f32| px(v, scale);
        let edge = s(1.0);
        let width = s(16.0).min((pane.w - 2 * edge).max(0));
        let bounds = Rect {
            x: pane.right() - edge - width,
            y: pane.y + edge,
            w: width,
            h: (pane.h - 2 * edge).max(0),
        };
        let arrow_h = s(16.0).min(bounds.h / 2);
        let up = Rect { h: arrow_h, ..bounds };
        let down = Rect { y: bounds.bottom() - arrow_h, h: arrow_h, ..bounds };
        let track = Rect { y: up.bottom(), h: (bounds.h - 2 * arrow_h).max(0), ..bounds };
        let total = history_size.saturating_add(screen_lines).max(1);
        let fraction = screen_lines as f64 / total as f64;
        let thumb_h = ((track.h as f64 * fraction).round() as i32)
            .max(s(18.0)).min(track.h);
        let travel = track.h - thumb_h;
        let fraction = if history_size == 0 { 1.0 } else {
            1.0 - offset.min(history_size) as f64 / history_size as f64
        };
        let thumb = Rect { y: track.y + (travel as f64 * fraction).round() as i32, h: thumb_h, ..track };
        Self { bounds, up, down, track, thumb, history_size }
    }

    pub fn hit_test(&self, x: f64, y: f64) -> Option<ScrollbarPart> {
        if !self.bounds.contains(x, y) {
            None
        } else if self.up.contains(x, y) {
            Some(ScrollbarPart::Up)
        } else if self.down.contains(x, y) {
            Some(ScrollbarPart::Down)
        } else if self.thumb.contains(x, y) {
            Some(ScrollbarPart::Thumb)
        } else if y < self.thumb.y as f64 {
            Some(ScrollbarPart::PageUp)
        } else {
            Some(ScrollbarPart::PageDown)
        }
    }

    pub fn offset_for_thumb_top(&self, y: f64) -> usize {
        let travel = self.track.h - self.thumb.h;
        if travel <= 0 || self.history_size == 0 {
            return 0;
        }
        let position = ((y - self.track.y as f64) / travel as f64).clamp(0.0, 1.0);
        ((1.0 - position) * self.history_size as f64).round() as usize
    }
}

/// 論理 px を物理 px に直す。0 にはしない。
fn px(v: f32, scale: f64) -> i32 {
    ((v as f64 * scale).round() as i32).max(1)
}

/// ウィンドウ内の各パーツの配置 (物理ピクセル)。描画とヒットテストで共用する。
#[derive(Copy, Clone, Debug, Default)]
pub struct Layout {
    pub titlebar: Rect,
    pub close_box: Rect,
    pub zoom_box: Rect,
    pub collapse_box: Rect,
    pub content: Rect,
    pub grow_box: Rect,
}

impl Layout {
    pub fn compute(width: u32, height: u32, scale: f64) -> Self {
        let s = |v: f32| px(v, scale);
        let (w, h) = (width as i32, height as i32);

        let f = s(1.0) + s(1.0); // 輪郭線 + 外側ベベル
        let tb_h = s(20.0);
        let titlebar = Rect { x: f, y: f, w: w - 2 * f, h: tb_h };

        let box_sz = s(13.0);
        let box_y = f + (tb_h - box_sz) / 2;
        let close_box = Rect { x: f + s(6.0), y: box_y, w: box_sz, h: box_sz };
        let collapse_box = Rect { x: w - f - s(6.0) - box_sz, y: box_y, w: box_sz, h: box_sz };
        let zoom_box = Rect { x: collapse_box.x - s(4.0) - box_sz, y: box_y, w: box_sz, h: box_sz };

        let top = f + tb_h + s(1.0); // タイトルバー + 区切り線の下端

        let pad = s(4.0);
        let inset = f + pad + s(1.0); // フレーム + 余白 + 内側ベベル
        let top = top + pad + s(1.0);
        // リサイズボックス用の下枠を確保し、スクロールバーの下矢印と重ねない。
        let content = Rect {
            x: inset, y: top, w: (w - 2 * inset).max(0),
            h: (h - top - inset - s(16.0)).max(0),
        };

        let gs = s(15.0);
        let grow_box = Rect { x: w - f - gs - s(2.0), y: h - f - gs - s(2.0), w: gs, h: gs };

        Self { titlebar, close_box, zoom_box, collapse_box, content, grow_box }
    }
}

/// 字体と、その字体でラスタライズ済みのグリフ。
struct GlyphCache<K> {
    face: Face,
    glyphs: HashMap<K, Glyph>,
}

impl<K: Hash + Eq> GlyphCache<K> {
    fn new(face: Face) -> Self {
        Self { face, glyphs: HashMap::new() }
    }

    /// `key` のグリフを返す。無ければ `make` で作って覚える。
    fn get(&mut self, key: K, make: impl FnOnce(&Face) -> Glyph) -> &Glyph {
        let face = &self.face;
        self.glyphs.entry(key).or_insert_with(|| make(face))
    }
}

pub struct Renderer {
    /// 端末本文。キーは文字と網点の位相。
    body: GlyphCache<(char, (usize, usize))>,
    title: GlyphCache<char>,
    pub cell_w: usize,
    pub cell_h: usize,
    ascent: i32,
    pub scale: f64,
    /// True のときグリフを二値化し、ビットマップ風の輪郭で描く。
    pub bitmap_text: bool,
}

impl Renderer {
    pub fn new(scale: f64) -> Self {
        let face = Face::new(MONACO, (FONT_PT as f64 * scale) as f32);
        let title_face = Face::new(GENEVA, (TITLE_PT as f64 * scale) as f32);

        let cell_w = (face.advance('M').round() as usize).max(1);
        let cell_h = (face.line_height().ceil() as usize).max(1);
        let ascent = face.ascent().round() as i32;

        Self {
            body: GlyphCache::new(face),
            title: GlyphCache::new(title_face),
            cell_w,
            cell_h,
            ascent,
            scale,
            bitmap_text: false,
        }
    }

    fn px(&self, v: f32) -> i32 {
        px(v, self.scale)
    }

    /// ウィンドウクローム (フレーム・タイトルバー) を描画する。
    /// ペインとスプリッターは呼び出し側がこの後に個別に描く。
    pub fn draw_chrome(
        &mut self,
        cv: &mut Canvas,
        layout: &Layout,
        title: &str,
        focused: bool,
    ) {
        let e = self.px(1.0);

        cv.fill(cv.bounds(), theme::FRAME);
        // 外周の輪郭線
        cv.stroke(cv.bounds(), e, theme::OUTLINE);
        // 外側ベベル (左上ハイライト・右下シャドウ)
        cv.bevel(cv.bounds().inset(e), e, theme::BEVEL_LIGHT, theme::BEVEL_DARK);

        self.draw_titlebar(cv, layout, title, focused);
    }

    /// ペイン1枚 (窪んだベベル + 白地 + 端末グリッド) を描画する。
    /// `term` はロック済みの端末状態。`active` はウィンドウとペインの
    /// 両方にフォーカスがあるときのみ true。
    pub fn draw_pane(
        &mut self,
        cv: &mut Canvas,
        rect: Rect,
        term: &Term<impl EventListener>,
        active: bool,
        scheme: &Scheme,
    ) {
        let e = self.px(1.0);

        // 窪みベベル: 左上が影、右下がハイライト
        cv.bevel(rect, e, theme::SUNKEN_DARK, theme::SUNKEN_LIGHT);
        cv.fill(rect.inset(e), scheme.bg);

        self.draw_grid(cv, self.grid_rect(rect), term, active, scheme);
        self.draw_scrollbar(cv, &self.scrollbar_layout(rect, term));
    }

    /// ペイン矩形から実際に文字グリッドを置く領域 (ベベル+余白の内側) を求める。
    pub fn grid_rect(&self, pane: Rect) -> Rect {
        let inset = self.px(1.0) + self.px(3.0);
        Rect {
            x: pane.x + inset,
            y: pane.y + inset,
            w: (pane.w - 2 * inset - self.px(16.0)).max(0),
            h: (pane.h - 2 * inset).max(0),
        }
    }

    pub fn scrollbar_layout(&self, pane: Rect, term: &Term<impl EventListener>) -> ScrollbarLayout {
        let grid = term.grid();
        ScrollbarLayout::compute(pane, self.scale, grid.history_size(), grid.screen_lines(), grid.display_offset())
    }

    fn draw_scrollbar(&self, cv: &mut Canvas, bar: &ScrollbarLayout) {
        let e = self.px(1.0);
        let track = bar.track;
        cv.fill(track, theme::FRAME);
        // グレーの網掛けトラック。
        for y in (track.y..track.bottom()).step_by(e as usize) {
            for x in (track.x..track.right()).step_by(e as usize) {
                if ((x - track.x) / e + (y - track.y) / e) % 2 == 0 {
                    cv.fill_clipped(Rect::new(x, y, e, e), theme::STRIPE_LIGHT, track);
                }
            }
        }
        cv.stroke(track, e, theme::BEVEL_DARK);
        let button = |cv: &mut Canvas, r: Rect| {
            if r.w <= 0 || r.h <= 0 { return; }
            cv.fill(r, theme::BOX_FILL);
            cv.stroke(r, e, theme::BOX_BORDER);
            cv.bevel(r.inset(e), e, theme::BEVEL_LIGHT, theme::BEVEL_DARK);
        };
        button(cv, bar.up);
        button(cv, bar.down);
        let color = if bar.history_size > 0 { theme::BOX_BORDER } else { theme::BEVEL_DARK };
        for (r, up) in [(bar.up, true), (bar.down, false)] {
            let (cx, cy) = r.center();
            for i in 0..4 {
                let y = if up { cy - self.px(2.0) + i * e } else { cy + self.px(1.0) - i * e };
                cv.fill_clipped(Rect::new(cx - i * e, y, (2 * i + 1) * e, e), color, r);
            }
        }
        if bar.history_size > 0 {
            button(cv, bar.thumb);
            let (cx, cy) = bar.thumb.center();
            for i in -1..=1 {
                cv.fill_clipped(Rect::new(cx - self.px(4.0), cy + i * self.px(3.0), self.px(8.0), e), theme::BEVEL_DARK, bar.thumb);
                cv.fill_clipped(Rect::new(cx - self.px(4.0), cy + i * self.px(3.0) + e, self.px(8.0), e), theme::BEVEL_LIGHT, bar.thumb);
            }
        }
    }

    /// ベルの鳴ったペインだけを短時間反転する。終了時は通常の再描画で戻す。
    pub fn draw_visual_bell(&self, cv: &mut Canvas, rect: Rect) {
        cv.invert(rect);
    }

    /// 未確定文字を PTY の内容に重ね、選択中の文節とキャレットを示す。
    /// 戻り値はネイティブ IME 候補ウィンドウを置く範囲 (物理 px)。
    pub fn draw_preedit(
        &mut self,
        cv: &mut Canvas,
        pane: Rect,
        term: &Term<impl EventListener>,
        preedit: &Preedit,
        scheme: &Scheme,
    ) -> Option<Rect> {
        if preedit.text.is_empty() { return None; }
        let grid = self.grid_rect(pane);
        let (cw, ch) = (self.cell_w as i32, self.cell_h as i32);
        let columns = (grid.w / cw) as usize;
        let rows = (grid.h / ch) as usize;
        if columns == 0 || rows == 0 { return None; }
        let content = term.renderable_content();
        let cursor = term::point_to_viewport(content.display_offset, content.cursor.point)?;
        if cursor.line >= rows { return None; }
        let composition = preedit.layout(columns, cursor.column.0.min(columns - 1));
        let y = grid.y + cursor.line as i32 * ch;
        let x = grid.x + composition.start_column as i32 * cw;
        let width = composition.width as i32 * cw;
        let edge = self.px(1.0);
        cv.fill_clipped(Rect::new(x, y, width, ch), scheme.bg, grid);
        cv.fill_clipped(Rect::new(x, y + ch - edge, width, edge), scheme.fg, grid);
        if let Some((start, end)) = composition.selection {
            let x = grid.x + start as i32 * cw;
            let width = (end - start) as i32 * cw;
            cv.fill_clipped(Rect::new(x, y, width, ch), scheme.fg, grid);
            cv.fill_clipped(Rect::new(x, y + ch - edge, width, edge), scheme.bg, grid);
        }
        for glyph in &composition.glyphs {
            let selected = composition.selection.is_some_and(|(start, end)| glyph.column >= start && glyph.column < end);
            let color = if selected { scheme.bg } else { scheme.fg };
            self.draw_cell_glyph(cv, glyph.character, grid.x + glyph.column as i32 * cw, y, color, grid);
        }
        if let Some(column) = composition.cursor_column {
            let selected = composition.selection.is_some_and(|(start, end)| column >= start && column < end);
            let color = if selected { scheme.bg } else { scheme.cursor };
            cv.fill_clipped(Rect::new(grid.x + column as i32 * cw, y, edge, ch), color, grid);
        }
        Some(Rect { x: grid.x + composition.candidate_column as i32 * cw, y, w: composition.candidate_width as i32 * cw, h: ch })
    }

    /// ペイン間のスプリッターバー。タイトルバーと同じリブ (縞) の意匠。
    pub fn draw_splitter(&self, cv: &mut Canvas, sh: &SplitterHit) {
        let e = self.px(1.0);
        let r = sh.rect;

        cv.fill(r, theme::FRAME);
        let (cx, cy) = r.center();
        match sh.dir {
            SplitDir::Vertical => {
                // 縦バー: 左にハイライト、右に影
                cv.fill(Rect::new(r.x, r.y, e, r.h), theme::BEVEL_LIGHT);
                cv.fill(Rect::new(r.right() - e, r.y, e, r.h), theme::BEVEL_DARK);
                // 中央のリブ3本 (横線)
                for i in -1..=1 {
                    let y = cy + i * self.px(4.0);
                    cv.fill(Rect::new(r.x + self.px(2.0), y, r.w - self.px(4.0), e), theme::STRIPE);
                    cv.fill(Rect::new(r.x + self.px(2.0), y + e, r.w - self.px(4.0), e), theme::STRIPE_LIGHT);
                }
            }
            SplitDir::Horizontal => {
                cv.fill(Rect::new(r.x, r.y, r.w, e), theme::BEVEL_LIGHT);
                cv.fill(Rect::new(r.x, r.bottom() - e, r.w, e), theme::BEVEL_DARK);
                for i in -1..=1 {
                    let x = cx + i * self.px(4.0);
                    cv.fill(Rect::new(x, r.y + self.px(2.0), e, r.h - self.px(4.0)), theme::STRIPE);
                    cv.fill(Rect::new(x + e, r.y + self.px(2.0), e, r.h - self.px(4.0)), theme::STRIPE_LIGHT);
                }
            }
        }
    }

    fn draw_titlebar(
        &mut self,
        cv: &mut Canvas,
        layout: &Layout,
        title: &str,
        focused: bool,
    ) {
        let tb = layout.titlebar;

        // タイトルバー下端の区切り線
        cv.fill(Rect::new(tb.x, tb.bottom(), tb.w, self.px(1.0)), theme::SEPARATOR);

        // タイトル文字列 (中央揃え)
        let title_w = self.measure_title(title);
        let title_x = tb.x + (tb.w - title_w) / 2;
        let baseline = tb.y + tb.h / 2
            + (self.title.face.ascent() / 2.0).round() as i32
            - self.px(1.0);
        let color = if focused { theme::TITLE_TEXT } else { theme::TITLE_TEXT_INACTIVE };

        if focused {
            // 横縞 (アクティブ時のみ)。ボックスとタイトルの周囲は空ける。
            let gap = self.px(6.0);
            let segs = [
                (tb.x + self.px(4.0), layout.close_box.x - self.px(4.0)),
                (layout.close_box.right() + self.px(4.0), title_x - gap),
                (title_x + title_w + gap, layout.zoom_box.x - self.px(4.0)),
                (layout.collapse_box.right() + self.px(4.0), tb.right() - self.px(4.0)),
            ];
            let mut y = tb.y + self.px(4.0);
            while y + self.px(1.0) < tb.bottom() - self.px(3.0) {
                for &(x0, x1) in &segs {
                    if x1 > x0 {
                        cv.fill(Rect::new(x0, y, x1 - x0, self.px(1.0)), theme::STRIPE);
                        cv.fill(Rect::new(x0, y + self.px(1.0), x1 - x0, self.px(1.0)), theme::STRIPE_LIGHT);
                    }
                }
                y += self.px(2.0);
            }

            self.draw_control_box(cv, layout.close_box, BoxKind::Close);
            self.draw_control_box(cv, layout.zoom_box, BoxKind::Zoom);
            self.draw_control_box(cv, layout.collapse_box, BoxKind::Collapse);
        }

        self.draw_title_text(cv, title, title_x, baseline, color);
    }

    fn draw_control_box(&self, cv: &mut Canvas, r: Rect, kind: BoxKind) {
        let e = self.px(1.0);
        cv.fill(r, theme::BOX_FILL);
        cv.stroke(r, e, theme::BOX_BORDER);
        // 立体感: 内側の左上に影、右下にハイライト (押し込める見た目)
        cv.bevel(r.inset(e), e, theme::BEVEL_DARK, theme::BEVEL_LIGHT);

        match kind {
            BoxKind::Close => {}
            BoxKind::Zoom => {
                // 左上寄りの小さい四角
                cv.stroke(Rect::new(r.x + 2 * e, r.y + 2 * e, r.w * 2 / 3, r.h * 2 / 3), e, theme::BOX_BORDER);
            }
            BoxKind::Collapse => {
                // 中央の横線 (ウィンドウシェード)
                cv.fill(Rect::new(r.x + 2 * e, r.y + r.h / 2, r.w - 4 * e, e), theme::BOX_BORDER);
            }
        }
    }

    /// ペインドラッグ中のドロップ先プレビュー枠。
    pub fn draw_drop_outline(&self, cv: &mut Canvas, r: Rect) {
        let t = ((2.0 * self.scale).round() as i32).max(2);
        cv.stroke(r, t, theme::DROP_HINT);
    }

    /// 設定ウィンドウの中身 (カラースキーマのラジオボタン一覧) を描画し、
    /// 各行のヒットテスト用矩形を返す。クロームは draw_chrome で描いておくこと。
    pub fn draw_settings(
        &mut self,
        cv: &mut Canvas,
        layout: &Layout,
        selected: usize,
    ) -> Vec<(SettingsHit, Rect)> {
        let c = layout.content;

        let ascent = self.title.face.ascent().round() as i32;

        self.draw_title_text(cv, "カラースキーマ:", c.x, c.y + ascent, theme::TITLE_TEXT);

        let row_h = self.px(24.0);
        let mut hits = Vec::with_capacity(theme::SCHEMES.len());
        for (i, scheme) in theme::SCHEMES.iter().enumerate() {
            let row = Rect { x: c.x, y: c.y + self.px(24.0) + i as i32 * row_h, w: c.w, h: row_h };
            let cy = row.y + row.h / 2;

            // ラジオボタン
            let r_out = self.px(6.0);
            cv.circle(row.x + self.px(8.0), cy, r_out, theme::BOX_BORDER);
            cv.circle(row.x + self.px(8.0), cy, r_out - self.px(1.0), 0xFFFFFF);
            if i == selected {
                cv.circle(row.x + self.px(8.0), cy, self.px(3.0), 0x000000);
            }

            self.draw_title_text(
                cv,
                scheme.name,
                row.x + self.px(22.0),
                cy + ascent / 2 - self.px(1.0),
                theme::TITLE_TEXT,
            );

            // 右端に配色見本 (背景・文字・赤・緑・青)
            let sw_w = self.px(13.0);
            let sw_h = self.px(11.0);
            let gap = self.px(3.0);
            let samples = [scheme.bg, scheme.fg, scheme.ansi[1], scheme.ansi[2], scheme.ansi[4]];
            let mut x = row.right() - self.px(4.0) - (sw_w + gap) * samples.len() as i32;
            for color in samples {
                cv.fill(Rect::new(x, cy - sw_h / 2, sw_w, sw_h), color);
                cv.stroke(Rect::new(x, cy - sw_h / 2, sw_w, sw_h), self.px(1.0), theme::BOX_BORDER);
                x += sw_w + gap;
            }

            hits.push((SettingsHit::Scheme(i), row));
        }
        let row = Rect {
            x: c.x, y: c.y + self.px(32.0) + theme::SCHEMES.len() as i32 * row_h,
            w: c.w, h: row_h,
        };
        let cy = row.y + row.h / 2;
        let checkbox = Rect { x: row.x + self.px(2.0), y: cy - self.px(6.0), w: self.px(12.0), h: self.px(12.0) };
        cv.fill(checkbox, theme::BEVEL_LIGHT);
        cv.stroke(checkbox, self.px(1.0), theme::BOX_BORDER);
        if self.bitmap_text {
            // ピクセルで描くチェックマーク。
            for (dx, dy) in [(2.0, 5.0), (3.0, 6.0), (4.0, 7.0), (5.0, 6.0), (6.0, 5.0), (7.0, 4.0), (8.0, 3.0)] {
                cv.fill(Rect::new(checkbox.x + self.px(dx), checkbox.y + self.px(dy), self.px(1.0), self.px(2.0)), theme::BOX_BORDER);
            }
        }
        self.draw_title_text(cv, "ビットマップ風の文字", row.x + self.px(22.0), cy + ascent / 2 - self.px(1.0), theme::TITLE_TEXT);
        hits.push((SettingsHit::BitmapText, row));
        hits
    }

    pub fn draw_grow_box(&self, cv: &mut Canvas, r: Rect) {
        let e = self.px(1.0);
        cv.fill(r, theme::BOX_FILL);
        cv.stroke(r, e, theme::BOX_BORDER);
        // 重なった2つの四角のアイコン
        cv.stroke(Rect::new(r.x + 3 * e, r.y + 3 * e, r.w / 2, r.h / 2), e, theme::BOX_BORDER);
        cv.stroke(Rect::new(r.x + r.w / 3, r.y + r.h / 3, r.w / 2, r.h / 2), e, theme::BOX_BORDER);
    }

    fn draw_grid(
        &mut self,
        cv: &mut Canvas,
        clip: Rect,
        term: &Term<impl EventListener>,
        active: bool,
        scheme: &Scheme,
    ) {
        let content = term.renderable_content();
        let display_offset = content.display_offset;
        let colors = content.colors;
        let (cw, ch) = (self.cell_w as i32, self.cell_h as i32);
        let cols = (clip.w / cw).max(1);
        let rows = (clip.h / ch).max(1);

        let cursor = content.cursor;
        let cursor_vp = term::point_to_viewport(display_offset, cursor.point);

        for indexed in content.display_iter {
            let Some(p) = term::point_to_viewport(display_offset, indexed.point) else { continue };
            let (row, col) = (p.line as i32, p.column.0 as i32);
            if row >= rows || col >= cols {
                continue;
            }
            let cell = &indexed.cell;
            if cell.flags.contains(Flags::WIDE_CHAR_SPACER) {
                continue;
            }

            let mut fg = resolve_color(colors, cell.fg, scheme);
            let mut bg = resolve_color(colors, cell.bg, scheme);
            if cell.flags.contains(Flags::INVERSE) {
                std::mem::swap(&mut fg, &mut bg);
            }
            if cell.flags.contains(Flags::DIM) {
                fg = dim(fg);
            }

            if content.selection.as_ref().is_some_and(|selection|
                selection.contains_cell(&indexed, cursor.point, CursorShape::Hidden)) {
                fg = scheme.bg;
                bg = scheme.fg;
            }
            let x = clip.x + col * cw;
            let y = clip.y + row * ch;
            if bg != scheme.bg {
                let bw = if cell.flags.contains(Flags::WIDE_CHAR) { cw * 2 } else { cw };
                cv.fill_clipped(Rect::new(x, y, bw, ch), bg, clip);
            }
            if cell.c != ' ' {
                self.draw_cell_glyph(cv, cell.c, x, y, fg, clip);
            }
            if cell.flags.contains(Flags::UNDERLINE) {
                cv.fill_clipped(Rect::new(x, y + self.ascent + 1, cw, 1), fg, clip);
            }
        }

        // ─── カーソル ───
        if cursor.shape == CursorShape::Hidden {
            return;
        }
        let Some(p) = cursor_vp else { return };
        let (row, col) = (p.line as i32, p.column.0 as i32);
        if row >= rows || col >= cols {
            return;
        }
        let x = clip.x + col * cw;
        let y = clip.y + row * ch;
        let cursor_color = resolve_named(colors, NamedColor::Cursor, scheme.cursor);

        if !active {
            cv.stroke(Rect::new(x, y, cw, ch), self.px(1.0), cursor_color);
            return;
        }
        match cursor.shape {
            CursorShape::Beam => {
                cv.fill_clipped(Rect::new(x, y, self.px(2.0), ch), cursor_color, clip);
            }
            CursorShape::Underline => {
                cv.fill_clipped(Rect::new(x, y + ch - self.px(2.0), cw, self.px(2.0)), cursor_color, clip);
            }
            _ => {
                // ブロックカーソル: セルを反転
                cv.fill_clipped(Rect::new(x, y, cw, ch), cursor_color, clip);
                let c = term.grid()[cursor.point].c;
                if c != ' ' {
                    self.draw_cell_glyph(cv, c, x, y, scheme.bg, clip);
                }
            }
        }
    }

    fn draw_cell_glyph(
        &mut self,
        cv: &mut Canvas,
        c: char,
        cell_x: i32,
        cell_y: i32,
        color: u32,
        clip: Rect,
    ) {
        let phase = font::pattern_phase(c, cell_x, cell_y);
        let glyph = self.body.get((c, phase), |face| {
            font::block_element(c, self.cell_w, self.cell_h, self.ascent, phase)
                .unwrap_or_else(|| face.rasterize(c))
        });
        let gx = cell_x + glyph.left;
        let gy = cell_y + self.ascent - glyph.bottom - glyph.height as i32;
        cv.blend_glyph(gx, gy, glyph, color, clip, self.bitmap_text);
    }

    fn measure_title(&mut self, text: &str) -> i32 {
        let mut total = 0.0f32;
        for c in text.chars() {
            total += self.title.get(c, |face| face.rasterize(c)).advance;
        }
        total.round() as i32
    }

    fn draw_title_text(
        &mut self,
        cv: &mut Canvas,
        text: &str,
        x: i32,
        baseline: i32,
        color: u32,
    ) {
        let clip = cv.bounds();
        let mut pen = x as f32;
        for c in text.chars() {
            let glyph = self.title.get(c, |face| face.rasterize(c));
            let gx = pen.round() as i32 + glyph.left;
            let gy = baseline - glyph.bottom - glyph.height as i32;
            cv.blend_glyph(gx, gy, glyph, color, clip, self.bitmap_text);
            pen += glyph.advance;
        }
    }
}

enum BoxKind {
    Close,
    Zoom,
    Collapse,
}

// ─── 色解決 ───

fn pack(rgb: Rgb) -> u32 {
    ((rgb.r as u32) << 16) | ((rgb.g as u32) << 8) | rgb.b as u32
}

fn resolve_color(colors: &Colors, color: Color, scheme: &Scheme) -> u32 {
    match color {
        Color::Spec(rgb) => pack(rgb),
        Color::Indexed(i) if i < 16 => colors[i as usize].map(pack).unwrap_or(scheme.ansi[i as usize]),
        Color::Indexed(i) => colors[i as usize].map(pack).unwrap_or_else(|| theme::indexed256(i)),
        Color::Named(named) => {
            let fallback = named_fallback(named, scheme);
            colors[named as usize].map(pack).unwrap_or(fallback)
        }
    }
}

fn resolve_named(colors: &Colors, named: NamedColor, fallback: u32) -> u32 {
    colors[named as usize].map(pack).unwrap_or(fallback)
}

fn named_fallback(named: NamedColor, scheme: &Scheme) -> u32 {
    use NamedColor::*;
    match named {
        Foreground | BrightForeground => scheme.fg,
        Background => scheme.bg,
        Cursor => scheme.cursor,
        DimForeground => dim(scheme.fg),
        DimBlack | DimRed | DimGreen | DimYellow | DimBlue | DimMagenta | DimCyan | DimWhite => {
            dim(scheme.ansi[named as usize - DimBlack as usize])
        }
        _ => scheme.ansi[named as usize],
    }
}

fn dim(color: u32) -> u32 {
    let r = ((color >> 16) & 0xFF) * 2 / 3;
    let g = ((color >> 8) & 0xFF) * 2 / 3;
    let b = (color & 0xFF) * 2 / 3;
    (r << 16) | (g << 8) | b
}

// ─── 低レベル描画 ───

/// ピクセルバッファとその寸法。矩形はバッファからはみ出してよく、はみ出した分は捨てる。
pub struct Canvas<'a> {
    buf: &'a mut [u32],
    w: i32,
    h: i32,
}

impl<'a> Canvas<'a> {
    pub fn new(buf: &'a mut [u32], width: u32, height: u32) -> Self {
        Self { buf, w: width as i32, h: height as i32 }
    }

    fn bounds(&self) -> Rect {
        Rect::new(0, 0, self.w, self.h)
    }

    fn fill(&mut self, r: Rect, color: u32) {
        let x0 = r.x.clamp(0, self.w);
        let y0 = r.y.clamp(0, self.h);
        let x1 = r.right().clamp(0, self.w);
        let y1 = r.bottom().clamp(0, self.h);
        if x1 <= x0 || y1 <= y0 { return; }
        for row in y0..y1 {
            let start = (row * self.w + x0) as usize;
            let end = (row * self.w + x1) as usize;
            self.buf[start..end].fill(color);
        }
    }

    fn fill_clipped(&mut self, r: Rect, color: u32, clip: Rect) {
        let x0 = r.x.max(clip.x);
        let y0 = r.y.max(clip.y);
        let x1 = r.right().min(clip.right());
        let y1 = r.bottom().min(clip.bottom());
        if x1 > x0 && y1 > y0 {
            self.fill(Rect::new(x0, y0, x1 - x0, y1 - y0), color);
        }
    }

    fn invert(&mut self, r: Rect) {
        let x0 = r.x.clamp(0, self.w);
        let x1 = r.right().clamp(0, self.w);
        let y0 = r.y.clamp(0, self.h);
        let y1 = r.bottom().clamp(0, self.h);
        for y in y0..y1 {
            for x in x0..x1 {
                self.buf[(y * self.w + x) as usize] ^= 0x00FF_FFFF;
            }
        }
    }

    /// 幅 `e` の縁を描く。上と左が `top_left`、下と右が `bottom_right`。
    fn bevel(&mut self, r: Rect, e: i32, top_left: u32, bottom_right: u32) {
        self.fill(Rect::new(r.x, r.y, r.w, e), top_left);
        self.fill(Rect::new(r.x, r.y, e, r.h), top_left);
        self.fill(Rect::new(r.x, r.bottom() - e, r.w, e), bottom_right);
        self.fill(Rect::new(r.right() - e, r.y, e, r.h), bottom_right);
    }

    fn circle(&mut self, cx: i32, cy: i32, r: i32, color: u32) {
        for dy in -r..=r {
            for dx in -r..=r {
                if dx * dx + dy * dy > r * r {
                    continue;
                }
                let (px, py) = (cx + dx, cy + dy);
                if px >= 0 && px < self.w && py >= 0 && py < self.h {
                    self.buf[(py * self.w + px) as usize] = color;
                }
            }
        }
    }

    fn stroke(&mut self, r: Rect, t: i32, color: u32) {
        self.fill(Rect::new(r.x, r.y, r.w, t), color);
        self.fill(Rect::new(r.x, r.y + r.h - t, r.w, t), color);
        self.fill(Rect::new(r.x, r.y, t, r.h), color);
        self.fill(Rect::new(r.x + r.w - t, r.y, t, r.h), color);
    }

    /// グリフの左上を (gx, gy) に置いて合成する。`clip` とバッファの外は描かない。
    fn blend_glyph(&mut self, gx: i32, gy: i32, glyph: &Glyph, color: u32, clip: Rect, bitmap_text: bool) {
        let (x_min, x_max) = (clip.x.max(0), clip.right().min(self.w));
        let (y_min, y_max) = (clip.y.max(0), clip.bottom().min(self.h));
        let (fr, fg_, fb) = ((color >> 16) & 0xFF, (color >> 8) & 0xFF, color & 0xFF);
        for row in 0..glyph.height as i32 {
            let py = gy + row;
            if py < y_min || py >= y_max {
                continue;
            }
            for col in 0..glyph.width as i32 {
                let px = gx + col;
                if px < x_min || px >= x_max {
                    continue;
                }
                let coverage = glyph.coverage[(row * glyph.width as i32 + col) as usize] as u32;
                let a = if bitmap_text { if coverage >= 128 { 255 } else { 0 } } else { coverage };
                if a == 0 {
                    continue;
                }
                let idx = (py * self.w + px) as usize;
                if a == 255 {
                    self.buf[idx] = color;
                    continue;
                }
                let dst = self.buf[idx];
                let (dr, dg, db) = ((dst >> 16) & 0xFF, (dst >> 8) & 0xFF, dst & 0xFF);
                let r = (fr * a + dr * (255 - a)) / 255;
                let g = (fg_ * a + dg * (255 - a)) / 255;
                let b = (fb * a + db * (255 - a)) / 255;
                self.buf[idx] = (r << 16) | (g << 8) | b;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pane() -> Rect {
        Rect { x: 10, y: 20, w: 640, h: 480 }
    }

    #[test]
    fn thumb_tracks_scrollback_with_live_output_at_the_bottom() {
        let oldest = ScrollbarLayout::compute(pane(), 1.0, 976, 24, 976);
        let live = ScrollbarLayout::compute(pane(), 1.0, 976, 24, 0);
        let middle = ScrollbarLayout::compute(pane(), 1.0, 976, 24, 488);
        assert_eq!(oldest.thumb.y, oldest.track.y);
        assert_eq!(live.thumb.bottom(), live.track.bottom());
        assert_eq!(middle.thumb.y - middle.track.y, (middle.track.h - middle.thumb.h) / 2);
        assert_eq!(live.offset_for_thumb_top(oldest.thumb.y as f64), 976);
        assert_eq!(oldest.offset_for_thumb_top(live.thumb.y as f64), 0);
        assert_eq!(middle.offset_for_thumb_top(middle.thumb.y as f64), 488);
        assert_eq!(middle.offset_for_thumb_top(-1000.0), 976);
        assert_eq!(middle.offset_for_thumb_top(10000.0), 0);
    }

    #[test]
    fn thumb_size_is_proportional_and_empty_history_is_inert() {
        let half = ScrollbarLayout::compute(pane(), 2.0, 100, 100, 0);
        assert!((half.thumb.h * 2 - half.track.h).abs() <= 1);
        let empty = ScrollbarLayout::compute(pane(), 2.0, 0, 100, 0);
        assert_eq!(empty.thumb.h, empty.track.h);
        assert_eq!(empty.offset_for_thumb_top(-1000.0), 0);
        assert_eq!(empty.offset_for_thumb_top(10000.0), 0);
    }

    #[test]
    fn hit_testing_uses_the_same_arrow_thumb_and_page_geometry() {
        let bar = ScrollbarLayout::compute(pane(), 1.0, 500, 24, 250);
        let x = bar.bounds.center().0 as f64;
        assert_eq!(bar.hit_test(x, bar.up.center().1 as f64), Some(ScrollbarPart::Up));
        assert_eq!(bar.hit_test(x, bar.down.center().1 as f64), Some(ScrollbarPart::Down));
        assert_eq!(bar.hit_test(x, bar.thumb.center().1 as f64), Some(ScrollbarPart::Thumb));
        assert_eq!(bar.hit_test(x, bar.thumb.y as f64 - 1.0), Some(ScrollbarPart::PageUp));
        assert_eq!(bar.hit_test(x, bar.thumb.bottom() as f64), Some(ScrollbarPart::PageDown));
        assert_eq!(bar.hit_test(bar.bounds.x as f64 - 1.0, bar.thumb.y as f64), None);
    }

    #[test]
    fn tiny_panes_never_create_negative_track_or_thumb_sizes() {
        for width in 0..40 {
            for height in 0..40 {
                let bar = ScrollbarLayout::compute(Rect { w: width, h: height, ..pane() }, 2.0, 100, 24, 10);
                assert!(bar.bounds.w >= 0 && bar.bounds.h >= 0);
                assert!(bar.track.h >= 0 && bar.thumb.h >= 0);
                assert!(bar.thumb.y >= bar.track.y && bar.thumb.bottom() <= bar.track.bottom());
            }
        }
    }

    #[test]
    fn bitmap_text_uses_only_foreground_and_background_pixels() {
        let mut crisp = [0x0000FF; 3];
        let mut smooth = crisp;
        let glyph = Glyph { width: 3, height: 1, coverage: vec![127, 128, 255], ..Glyph::default() };
        let clip = Rect::new(0, 0, 3, 1);
        Canvas::new(&mut crisp, 3, 1).blend_glyph(0, 0, &glyph, 0x00FF00, clip, true);
        Canvas::new(&mut smooth, 3, 1).blend_glyph(0, 0, &glyph, 0x00FF00, clip, false);
        assert_eq!(crisp, [0x0000FF, 0x00FF00, 0x00FF00]);
        assert_ne!(smooth[0], crisp[0]);
        assert_ne!(smooth[1], crisp[1]);
        assert_eq!(smooth[2], crisp[2]);
    }
}

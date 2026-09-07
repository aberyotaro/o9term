//! CPU レンダラ。Platinum 風ウィンドウクロームと端末グリッドを
//! softbuffer のピクセルバッファへ直接描画する。

use std::collections::HashMap;

use alacritty_terminal::term::cell::Flags;
use alacritty_terminal::term::color::Colors;
use alacritty_terminal::term::{self, Term};
use alacritty_terminal::vte::ansi::{Color, CursorShape, NamedColor, Rgb};
use fontdue::{Font, FontSettings, Metrics};

use crate::tabs::{SplitDir, SplitterHit};
use crate::term::EventProxy;
use crate::theme::{self, Scheme};

const MONACO: &str = "/System/Library/Fonts/Monaco.ttf";
const GENEVA: &str = "/System/Library/Fonts/Geneva.ttf";
// Monaco/Geneva に無いグリフ (日本語など) のフォールバック
const HIRAGINO: &str = "/System/Library/Fonts/ヒラギノ角ゴシック W3.ttc";

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

/// ウィンドウ内の各パーツの配置 (物理ピクセル)。描画とヒットテストで共用する。
#[derive(Copy, Clone, Debug, Default)]
pub struct Layout {
    pub titlebar: Rect,
    pub close_box: Rect,
    pub zoom_box: Rect,
    pub collapse_box: Rect,
    /// タブが2枚以上のときだけ存在する
    pub tab_bar: Option<Rect>,
    pub content: Rect,
    pub grow_box: Rect,
}

impl Layout {
    pub fn compute(width: u32, height: u32, scale: f64, show_tabs: bool) -> Self {
        let s = |v: f32| ((v as f64 * scale).round() as i32).max(1);
        let (w, h) = (width as i32, height as i32);

        let f = s(1.0) + s(1.0); // 輪郭線 + 外側ベベル
        let tb_h = s(20.0);
        let titlebar = Rect { x: f, y: f, w: w - 2 * f, h: tb_h };

        let box_sz = s(13.0);
        let box_y = f + (tb_h - box_sz) / 2;
        let close_box = Rect { x: f + s(6.0), y: box_y, w: box_sz, h: box_sz };
        let collapse_box = Rect { x: w - f - s(6.0) - box_sz, y: box_y, w: box_sz, h: box_sz };
        let zoom_box = Rect { x: collapse_box.x - s(4.0) - box_sz, y: box_y, w: box_sz, h: box_sz };

        let mut top = f + tb_h + s(1.0); // タイトルバー + 区切り線の下端
        let tab_bar = if show_tabs {
            let bar = Rect { x: f, y: top, w: w - 2 * f, h: s(24.0) };
            top = bar.bottom();
            Some(bar)
        } else {
            None
        };

        let pad = s(4.0);
        let inset = f + pad + s(1.0); // フレーム + 余白 + 内側ベベル
        let top = top + pad + s(1.0);
        let content =
            Rect { x: inset, y: top, w: (w - 2 * inset).max(0), h: (h - top - inset).max(0) };

        let gs = s(15.0);
        let grow_box = Rect { x: w - f - gs - s(2.0), y: h - f - gs - s(2.0), w: gs, h: gs };

        Self { titlebar, close_box, zoom_box, collapse_box, tab_bar, content, grow_box }
    }
}

/// タブバー内の各タブと「+」ボタンの配置。描画とヒットテストで共用する。
pub fn tab_layout(bar: Rect, n: usize, scale: f64) -> (Vec<Rect>, Rect) {
    let s = |v: f32| ((v as f64 * scale).round() as i32).max(1);
    let margin = s(6.0);
    let gap = s(3.0);
    let plus_w = s(17.0);
    let n = n.max(1);
    let avail = bar.w - 2 * margin - plus_w - gap * n as i32;
    let tw = (avail / n as i32).clamp(s(40.0), s(160.0));

    let mut rects = Vec::with_capacity(n);
    let mut x = bar.x + margin;
    for _ in 0..n {
        rects.push(Rect { x, y: bar.y + s(4.0), w: tw, h: bar.h - s(4.0) });
        x += tw + gap;
    }
    let plus = Rect { x, y: bar.y + (bar.h - plus_w) / 2, w: plus_w, h: plus_w };
    (rects, plus)
}

pub struct Renderer {
    font: Font,
    title_font: Font,
    jp_font: Option<Font>,
    font_px: f32,
    title_px: f32,
    pub cell_w: usize,
    pub cell_h: usize,
    ascent: i32,
    pub scale: f64,
    glyphs: HashMap<char, (Metrics, Vec<u8>)>,
    title_glyphs: HashMap<char, (Metrics, Vec<u8>)>,
}

impl Renderer {
    pub fn new(scale: f64) -> Self {
        let load = |path: &str| -> Font {
            let data = std::fs::read(path).unwrap_or_else(|e| panic!("フォント読込失敗 {path}: {e}"));
            Font::from_bytes(data, FontSettings::default())
                .unwrap_or_else(|e| panic!("フォント解析失敗 {path}: {e}"))
        };
        let font = load(MONACO);
        let title_font = load(GENEVA);
        let jp_font = std::fs::read(HIRAGINO)
            .ok()
            .and_then(|data| Font::from_bytes(data, FontSettings::default()).ok());

        let font_px = (FONT_PT as f64 * scale) as f32;
        let title_px = (TITLE_PT as f64 * scale) as f32;

        let line = font.horizontal_line_metrics(font_px).expect("Monacoの行メトリクス取得失敗");
        let cell_w = (font.metrics('M', font_px).advance_width.round() as usize).max(1);
        let cell_h = (line.new_line_size.ceil() as usize).max(1);
        let ascent = line.ascent.round() as i32;

        Self {
            font,
            title_font,
            jp_font,
            font_px,
            title_px,
            cell_w,
            cell_h,
            ascent,
            scale,
            glyphs: HashMap::new(),
            title_glyphs: HashMap::new(),
        }
    }

    /// ウィンドウクローム (フレーム・タイトルバー・タブバー) を描画する。
    /// ペインとスプリッターは呼び出し側がこの後に個別に描く。
    pub fn draw_chrome(
        &mut self,
        buf: &mut [u32],
        width: u32,
        height: u32,
        layout: &Layout,
        title: &str,
        tabs: &[(String, bool)],
        focused: bool,
    ) {
        let (w, h) = (width as i32, height as i32);
        let s = |v: f32| ((v as f64 * self.scale).round() as i32).max(1);

        buf.fill(theme::FRAME);
        // 外周の輪郭線
        stroke_rect(buf, w, h, 0, 0, w, h, s(1.0), theme::OUTLINE);
        // 外側ベベル (左上ハイライト・右下シャドウ)
        let e = s(1.0);
        fill_rect(buf, w, h, e, e, w - 2 * e, e, theme::BEVEL_LIGHT);
        fill_rect(buf, w, h, e, e, e, h - 2 * e, theme::BEVEL_LIGHT);
        fill_rect(buf, w, h, e, h - 2 * e, w - 2 * e, e, theme::BEVEL_DARK);
        fill_rect(buf, w, h, w - 2 * e, e, e, h - 2 * e, theme::BEVEL_DARK);

        self.draw_titlebar(buf, w, h, layout, title, focused);

        if let Some(bar) = layout.tab_bar {
            self.draw_tab_bar(buf, w, h, bar, tabs, focused);
        }
    }

    /// 角形の Platinum 風タブバー。アクティブタブは下辺の境界線を消して
    /// コンテンツ側と地続きに見せる。
    fn draw_tab_bar(
        &mut self,
        buf: &mut [u32],
        w: i32,
        h: i32,
        bar: Rect,
        tabs: &[(String, bool)],
        focused: bool,
    ) {
        let scale = self.scale;
        let s = move |v: f32| ((v as f64 * scale).round() as i32).max(1);
        let e = s(1.0);

        fill_rect(buf, w, h, bar.x, bar.y, bar.w, bar.h, theme::TAB_BAR_BG);
        let (rects, plus) = tab_layout(bar, tabs.len(), self.scale);

        // 非アクティブタブ (境界線の下に沈む)
        for (rect, (label, active)) in rects.iter().zip(tabs) {
            if *active {
                continue;
            }
            let r = *rect;
            fill_rect(buf, w, h, r.x, r.y, r.w, r.h, theme::TAB_INACTIVE);
            stroke_rect(buf, w, h, r.x, r.y, r.w, r.h, e, theme::TAB_BORDER);
            self.draw_tab_label(buf, w, h, label, r, theme::TITLE_TEXT_INACTIVE);
        }

        // タブバー下端の境界線
        fill_rect(buf, w, h, bar.x, bar.bottom() - e, bar.w, e, theme::SEPARATOR);

        // アクティブタブ (少し背が高く、下辺の線を上書きして消す)
        for (rect, (label, active)) in rects.iter().zip(tabs) {
            if !*active {
                continue;
            }
            let r = Rect { x: rect.x, y: bar.y + s(2.0), w: rect.w, h: bar.h - s(2.0) };
            fill_rect(buf, w, h, r.x, r.y, r.w, r.h, theme::FRAME);
            // 左・上・右のみ線を引く (下は開けて地続きに)
            fill_rect(buf, w, h, r.x, r.y, e, r.h, theme::TAB_BORDER);
            fill_rect(buf, w, h, r.x, r.y, r.w, e, theme::TAB_BORDER);
            fill_rect(buf, w, h, r.right() - e, r.y, e, r.h, theme::TAB_BORDER);
            // 内側ハイライト
            fill_rect(buf, w, h, r.x + e, r.y + e, r.w - 2 * e, e, theme::BEVEL_LIGHT);
            fill_rect(buf, w, h, r.x + e, r.y + e, e, r.h - e, theme::BEVEL_LIGHT);
            let color = if focused { theme::TITLE_TEXT } else { theme::TITLE_TEXT_INACTIVE };
            self.draw_tab_label(buf, w, h, label, r, color);
        }

        // 「+」ボタン (新規タブ)
        fill_rect(buf, w, h, plus.x, plus.y, plus.w, plus.h, theme::BOX_FILL);
        stroke_rect(buf, w, h, plus.x, plus.y, plus.w, plus.h, e, theme::TAB_BORDER);
        let (cx, cy) = plus.center();
        let arm = s(4.0);
        fill_rect(buf, w, h, cx - arm, cy, arm * 2 + e, e, theme::BOX_BORDER);
        fill_rect(buf, w, h, cx, cy - arm, e, arm * 2 + e, theme::BOX_BORDER);
    }

    fn draw_tab_label(&mut self, buf: &mut [u32], w: i32, h: i32, label: &str, r: Rect, color: u32) {
        let scale = self.scale;
        let s = move |v: f32| ((v as f64 * scale).round() as i32).max(1);
        let label_w = self.measure_title(label);
        let x = if label_w < r.w - 2 * s(4.0) { r.x + (r.w - label_w) / 2 } else { r.x + s(4.0) };
        let ascent = self
            .title_font
            .horizontal_line_metrics(self.title_px)
            .map(|m| m.ascent)
            .unwrap_or(8.0);
        let baseline = r.y + r.h / 2 + (ascent / 2.0).round() as i32 - s(1.0);
        let clip = Rect { x: r.x + s(2.0), y: r.y, w: r.w - s(4.0), h: r.h };
        self.draw_title_text(buf, w, h, label, x, baseline, color, clip);
    }

    /// ペイン1枚 (窪んだベベル + 白地 + 端末グリッド) を描画する。
    /// `term` はロック済みの端末状態。`active` はウィンドウとペインの
    /// 両方にフォーカスがあるときのみ true。
    pub fn draw_pane(
        &mut self,
        buf: &mut [u32],
        width: u32,
        height: u32,
        rect: Rect,
        term: &Term<EventProxy>,
        active: bool,
        scheme: &Scheme,
    ) {
        let (w, h) = (width as i32, height as i32);
        let e = ((self.scale).round() as i32).max(1);

        // 窪みベベル: 左上が影、右下がハイライト
        fill_rect(buf, w, h, rect.x, rect.y, rect.w, e, theme::SUNKEN_DARK);
        fill_rect(buf, w, h, rect.x, rect.y, e, rect.h, theme::SUNKEN_DARK);
        fill_rect(buf, w, h, rect.x, rect.bottom() - e, rect.w, e, theme::SUNKEN_LIGHT);
        fill_rect(buf, w, h, rect.right() - e, rect.y, e, rect.h, theme::SUNKEN_LIGHT);
        fill_rect(buf, w, h, rect.x + e, rect.y + e, rect.w - 2 * e, rect.h - 2 * e, scheme.bg);

        self.draw_grid(buf, w, h, self.grid_rect(rect), term, active, scheme);
    }

    /// ペイン矩形から実際に文字グリッドを置く領域 (ベベル+余白の内側) を求める。
    pub fn grid_rect(&self, pane: Rect) -> Rect {
        let s = |v: f32| ((v as f64 * self.scale).round() as i32).max(1);
        let inset = s(1.0) + s(3.0);
        Rect {
            x: pane.x + inset,
            y: pane.y + inset,
            w: (pane.w - 2 * inset).max(0),
            h: (pane.h - 2 * inset).max(0),
        }
    }

    /// ペイン間のスプリッターバー。タイトルバーと同じリブ (縞) の意匠。
    pub fn draw_splitter(&self, buf: &mut [u32], width: u32, height: u32, sh: &SplitterHit) {
        let (w, h) = (width as i32, height as i32);
        let s = |v: f32| ((v as f64 * self.scale).round() as i32).max(1);
        let e = s(1.0);
        let r = sh.rect;

        fill_rect(buf, w, h, r.x, r.y, r.w, r.h, theme::FRAME);
        let (cx, cy) = r.center();
        match sh.dir {
            SplitDir::Vertical => {
                // 縦バー: 左にハイライト、右に影
                fill_rect(buf, w, h, r.x, r.y, e, r.h, theme::BEVEL_LIGHT);
                fill_rect(buf, w, h, r.right() - e, r.y, e, r.h, theme::BEVEL_DARK);
                // 中央のリブ3本 (横線)
                for i in -1..=1 {
                    let y = cy + i * s(4.0);
                    fill_rect(buf, w, h, r.x + s(2.0), y, r.w - s(4.0), e, theme::STRIPE);
                    fill_rect(buf, w, h, r.x + s(2.0), y + e, r.w - s(4.0), e, theme::STRIPE_LIGHT);
                }
            }
            SplitDir::Horizontal => {
                fill_rect(buf, w, h, r.x, r.y, r.w, e, theme::BEVEL_LIGHT);
                fill_rect(buf, w, h, r.x, r.bottom() - e, r.w, e, theme::BEVEL_DARK);
                for i in -1..=1 {
                    let x = cx + i * s(4.0);
                    fill_rect(buf, w, h, x, r.y + s(2.0), e, r.h - s(4.0), theme::STRIPE);
                    fill_rect(buf, w, h, x + e, r.y + s(2.0), e, r.h - s(4.0), theme::STRIPE_LIGHT);
                }
            }
        }
    }

    fn draw_titlebar(
        &mut self,
        buf: &mut [u32],
        w: i32,
        h: i32,
        layout: &Layout,
        title: &str,
        focused: bool,
    ) {
        let scale = self.scale;
        let s = move |v: f32| ((v as f64 * scale).round() as i32).max(1);
        let tb = layout.titlebar;

        // タイトルバー下端の区切り線
        fill_rect(buf, w, h, tb.x, tb.bottom(), tb.w, s(1.0), theme::SEPARATOR);

        // タイトル文字列 (中央揃え)
        let title_w = self.measure_title(title);
        let title_x = tb.x + (tb.w - title_w) / 2;
        let baseline = tb.y + tb.h / 2
            + (self.title_font.horizontal_line_metrics(self.title_px).map(|m| m.ascent).unwrap_or(8.0)
                / 2.0)
                .round() as i32
            - s(1.0);
        let color = if focused { theme::TITLE_TEXT } else { theme::TITLE_TEXT_INACTIVE };

        if focused {
            // 横縞 (アクティブ時のみ)。ボックスとタイトルの周囲は空ける。
            let gap = s(6.0);
            let segs = [
                (tb.x + s(4.0), layout.close_box.x - s(4.0)),
                (layout.close_box.right() + s(4.0), title_x - gap),
                (title_x + title_w + gap, layout.zoom_box.x - s(4.0)),
                (layout.collapse_box.right() + s(4.0), tb.right() - s(4.0)),
            ];
            let mut y = tb.y + s(4.0);
            while y + s(1.0) < tb.bottom() - s(3.0) {
                for &(x0, x1) in &segs {
                    if x1 > x0 {
                        fill_rect(buf, w, h, x0, y, x1 - x0, s(1.0), theme::STRIPE);
                        fill_rect(buf, w, h, x0, y + s(1.0), x1 - x0, s(1.0), theme::STRIPE_LIGHT);
                    }
                }
                y += s(2.0);
            }

            self.draw_control_box(buf, w, h, layout.close_box, BoxKind::Close);
            self.draw_control_box(buf, w, h, layout.zoom_box, BoxKind::Zoom);
            self.draw_control_box(buf, w, h, layout.collapse_box, BoxKind::Collapse);
        }

        let clip = Rect { x: 0, y: 0, w, h };
        self.draw_title_text(buf, w, h, title, title_x, baseline, color, clip);
    }

    fn draw_control_box(&self, buf: &mut [u32], w: i32, h: i32, r: Rect, kind: BoxKind) {
        let s = |v: f32| ((v as f64 * self.scale).round() as i32).max(1);
        let e = s(1.0);
        fill_rect(buf, w, h, r.x, r.y, r.w, r.h, theme::BOX_FILL);
        stroke_rect(buf, w, h, r.x, r.y, r.w, r.h, e, theme::BOX_BORDER);
        // 立体感: 内側の左上に影、右下にハイライト (押し込める見た目)
        fill_rect(buf, w, h, r.x + e, r.y + e, r.w - 2 * e, e, theme::BEVEL_DARK);
        fill_rect(buf, w, h, r.x + e, r.y + e, e, r.h - 2 * e, theme::BEVEL_DARK);
        fill_rect(buf, w, h, r.x + e, r.bottom() - 2 * e, r.w - 2 * e, e, theme::BEVEL_LIGHT);
        fill_rect(buf, w, h, r.right() - 2 * e, r.y + e, e, r.h - 2 * e, theme::BEVEL_LIGHT);

        match kind {
            BoxKind::Close => {}
            BoxKind::Zoom => {
                // 左上寄りの小さい四角
                stroke_rect(buf, w, h, r.x + 2 * e, r.y + 2 * e, r.w * 2 / 3, r.h * 2 / 3, e, theme::BOX_BORDER);
            }
            BoxKind::Collapse => {
                // 中央の横線 (ウィンドウシェード)
                fill_rect(buf, w, h, r.x + 2 * e, r.y + r.h / 2, r.w - 4 * e, e, theme::BOX_BORDER);
            }
        }
    }

    /// タブドラッグ中のドロップ先プレビュー枠。
    pub fn draw_drop_outline(&self, buf: &mut [u32], width: u32, height: u32, r: Rect) {
        let (w, h) = (width as i32, height as i32);
        let t = ((2.0 * self.scale).round() as i32).max(2);
        stroke_rect(buf, w, h, r.x, r.y, r.w, r.h, t, theme::DROP_HINT);
    }

    /// タブ並べ替え時の挿入位置バー。
    pub fn draw_drop_bar(&self, buf: &mut [u32], width: u32, height: u32, r: Rect) {
        let (w, h) = (width as i32, height as i32);
        fill_rect(buf, w, h, r.x, r.y, r.w, r.h, theme::DROP_HINT);
    }

    /// 設定ウィンドウの中身 (カラースキーマのラジオボタン一覧) を描画し、
    /// 各行のヒットテスト用矩形を返す。クロームは draw_chrome で描いておくこと。
    pub fn draw_settings(
        &mut self,
        buf: &mut [u32],
        width: u32,
        height: u32,
        layout: &Layout,
        selected: usize,
    ) -> Vec<Rect> {
        let (w, h) = (width as i32, height as i32);
        let scale = self.scale;
        let s = move |v: f32| ((v as f64 * scale).round() as i32).max(1);
        let c = layout.content;
        let clip = Rect { x: 0, y: 0, w, h };

        let ascent = self
            .title_font
            .horizontal_line_metrics(self.title_px)
            .map(|m| m.ascent)
            .unwrap_or(8.0)
            .round() as i32;

        self.draw_title_text(buf, w, h, "カラースキーマ:", c.x, c.y + ascent, theme::TITLE_TEXT, clip);

        let row_h = s(24.0);
        let mut hits = Vec::with_capacity(theme::SCHEMES.len());
        for (i, scheme) in theme::SCHEMES.iter().enumerate() {
            let row = Rect { x: c.x, y: c.y + s(24.0) + i as i32 * row_h, w: c.w, h: row_h };
            let cy = row.y + row.h / 2;

            // ラジオボタン
            let r_out = s(6.0);
            fill_circle(buf, w, h, row.x + s(8.0), cy, r_out, theme::BOX_BORDER);
            fill_circle(buf, w, h, row.x + s(8.0), cy, r_out - s(1.0), 0xFFFFFF);
            if i == selected {
                fill_circle(buf, w, h, row.x + s(8.0), cy, s(3.0), 0x000000);
            }

            self.draw_title_text(
                buf,
                w,
                h,
                scheme.name,
                row.x + s(22.0),
                cy + ascent / 2 - s(1.0),
                theme::TITLE_TEXT,
                clip,
            );

            // 右端に配色見本 (背景・文字・赤・緑・青)
            let sw_w = s(13.0);
            let sw_h = s(11.0);
            let gap = s(3.0);
            let samples = [scheme.bg, scheme.fg, scheme.ansi[1], scheme.ansi[2], scheme.ansi[4]];
            let mut x = row.right() - s(4.0) - (sw_w + gap) * samples.len() as i32;
            for color in samples {
                fill_rect(buf, w, h, x, cy - sw_h / 2, sw_w, sw_h, color);
                stroke_rect(buf, w, h, x, cy - sw_h / 2, sw_w, sw_h, s(1.0), theme::BOX_BORDER);
                x += sw_w + gap;
            }

            hits.push(row);
        }
        hits
    }

    pub fn draw_grow_box(&self, buf: &mut [u32], width: u32, height: u32, r: Rect) {
        let (w, h) = (width as i32, height as i32);
        let s = |v: f32| ((v as f64 * self.scale).round() as i32).max(1);
        let e = s(1.0);
        fill_rect(buf, w, h, r.x, r.y, r.w, r.h, theme::BOX_FILL);
        stroke_rect(buf, w, h, r.x, r.y, r.w, r.h, e, theme::BOX_BORDER);
        // 重なった2つの四角のアイコン
        stroke_rect(buf, w, h, r.x + 3 * e, r.y + 3 * e, r.w / 2, r.h / 2, e, theme::BOX_BORDER);
        stroke_rect(buf, w, h, r.x + r.w / 3, r.y + r.h / 3, r.w / 2, r.h / 2, e, theme::BOX_BORDER);
    }

    fn draw_grid(
        &mut self,
        buf: &mut [u32],
        w: i32,
        h: i32,
        clip: Rect,
        term: &Term<EventProxy>,
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

            let mut fg = resolve_color(colors, cell.fg, true, scheme);
            let mut bg = resolve_color(colors, cell.bg, false, scheme);
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
                fill_rect_clipped(buf, w, h, x, y, bw, ch, bg, &clip);
            }
            if cell.c != ' ' {
                self.draw_cell_glyph(buf, w, h, cell.c, x, y, fg, &clip);
            }
            if cell.flags.contains(Flags::UNDERLINE) {
                fill_rect_clipped(buf, w, h, x, y + self.ascent + 1, cw, 1, fg, &clip);
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
        let s = |v: f32| ((v as f64 * self.scale).round() as i32).max(1);

        if !active {
            stroke_rect(buf, w, h, x, y, cw, ch, s(1.0), cursor_color);
            return;
        }
        match cursor.shape {
            CursorShape::Beam => {
                fill_rect_clipped(buf, w, h, x, y, s(2.0), ch, cursor_color, &clip);
            }
            CursorShape::Underline => {
                fill_rect_clipped(buf, w, h, x, y + ch - s(2.0), cw, s(2.0), cursor_color, &clip);
            }
            _ => {
                // ブロックカーソル: セルを反転
                fill_rect_clipped(buf, w, h, x, y, cw, ch, cursor_color, &clip);
                let c = term.grid()[cursor.point].c;
                if c != ' ' {
                    self.draw_cell_glyph(buf, w, h, c, x, y, scheme.bg, &clip);
                }
            }
        }
    }

    fn draw_cell_glyph(
        &mut self,
        buf: &mut [u32],
        w: i32,
        h: i32,
        c: char,
        cell_x: i32,
        cell_y: i32,
        color: u32,
        clip: &Rect,
    ) {
        let px = self.font_px;
        let font = if self.font.lookup_glyph_index(c) != 0 {
            &self.font
        } else {
            self.jp_font.as_ref().unwrap_or(&self.font)
        };
        let (metrics, bitmap) = self.glyphs.entry(c).or_insert_with(|| font.rasterize(c, px));
        let gx = cell_x + metrics.xmin;
        let gy = cell_y + self.ascent - metrics.ymin - metrics.height as i32;
        blend_glyph(buf, w, h, gx, gy, metrics, bitmap, color, clip);
    }

    fn measure_title(&mut self, text: &str) -> i32 {
        let px = self.title_px;
        let mut total = 0.0f32;
        for c in text.chars() {
            let font = if self.title_font.lookup_glyph_index(c) != 0 {
                &self.title_font
            } else {
                self.jp_font.as_ref().unwrap_or(&self.title_font)
            };
            let (m, _) = self.title_glyphs.entry(c).or_insert_with(|| font.rasterize(c, px));
            total += m.advance_width;
        }
        total.round() as i32
    }

    fn draw_title_text(
        &mut self,
        buf: &mut [u32],
        w: i32,
        h: i32,
        text: &str,
        x: i32,
        baseline: i32,
        color: u32,
        clip: Rect,
    ) {
        let px = self.title_px;
        let mut pen = x as f32;
        for c in text.chars() {
            let font = if self.title_font.lookup_glyph_index(c) != 0 {
                &self.title_font
            } else {
                self.jp_font.as_ref().unwrap_or(&self.title_font)
            };
            let (metrics, bitmap) =
                self.title_glyphs.entry(c).or_insert_with(|| font.rasterize(c, px));
            let gx = pen.round() as i32 + metrics.xmin;
            let gy = baseline - metrics.ymin - metrics.height as i32;
            blend_glyph(buf, w, h, gx, gy, metrics, bitmap, color, &clip);
            pen += metrics.advance_width;
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

fn resolve_color(colors: &Colors, color: Color, is_fg: bool, scheme: &Scheme) -> u32 {
    match color {
        Color::Spec(rgb) => pack(rgb),
        Color::Indexed(i) if i < 16 => colors[i as usize].map(pack).unwrap_or(scheme.ansi[i as usize]),
        Color::Indexed(i) => colors[i as usize].map(pack).unwrap_or_else(|| theme::indexed256(i)),
        Color::Named(named) => {
            let fallback = named_fallback(named, is_fg, scheme);
            colors[named as usize].map(pack).unwrap_or(fallback)
        }
    }
}

fn resolve_named(colors: &Colors, named: NamedColor, fallback: u32) -> u32 {
    colors[named as usize].map(pack).unwrap_or(fallback)
}

fn named_fallback(named: NamedColor, is_fg: bool, scheme: &Scheme) -> u32 {
    use NamedColor::*;
    match named {
        Foreground | BrightForeground => scheme.fg,
        Background => scheme.bg,
        Cursor => scheme.cursor,
        DimForeground => dim(scheme.fg),
        DimBlack | DimRed | DimGreen | DimYellow | DimBlue | DimMagenta | DimCyan | DimWhite => {
            dim(scheme.ansi[named as usize - DimBlack as usize])
        }
        _ => {
            let idx = named as usize;
            if idx < 16 {
                scheme.ansi[idx]
            } else if is_fg {
                scheme.fg
            } else {
                scheme.bg
            }
        }
    }
}

fn dim(color: u32) -> u32 {
    let r = ((color >> 16) & 0xFF) * 2 / 3;
    let g = ((color >> 8) & 0xFF) * 2 / 3;
    let b = (color & 0xFF) * 2 / 3;
    (r << 16) | (g << 8) | b
}

// ─── 低レベル描画 ───

fn fill_rect(buf: &mut [u32], bw: i32, bh: i32, x: i32, y: i32, w: i32, h: i32, color: u32) {
    let x0 = x.max(0);
    let y0 = y.max(0);
    let x1 = (x + w).min(bw);
    let y1 = (y + h).min(bh);
    for row in y0..y1 {
        let start = (row * bw + x0) as usize;
        let end = (row * bw + x1) as usize;
        buf[start..end].fill(color);
    }
}

fn fill_rect_clipped(
    buf: &mut [u32],
    bw: i32,
    bh: i32,
    x: i32,
    y: i32,
    w: i32,
    h: i32,
    color: u32,
    clip: &Rect,
) {
    let x0 = x.max(clip.x);
    let y0 = y.max(clip.y);
    let x1 = (x + w).min(clip.right());
    let y1 = (y + h).min(clip.bottom());
    if x1 > x0 && y1 > y0 {
        fill_rect(buf, bw, bh, x0, y0, x1 - x0, y1 - y0, color);
    }
}

fn fill_circle(buf: &mut [u32], bw: i32, bh: i32, cx: i32, cy: i32, r: i32, color: u32) {
    for dy in -r..=r {
        for dx in -r..=r {
            if dx * dx + dy * dy > r * r {
                continue;
            }
            let (px, py) = (cx + dx, cy + dy);
            if px >= 0 && px < bw && py >= 0 && py < bh {
                buf[(py * bw + px) as usize] = color;
            }
        }
    }
}

fn stroke_rect(buf: &mut [u32], bw: i32, bh: i32, x: i32, y: i32, w: i32, h: i32, t: i32, color: u32) {
    fill_rect(buf, bw, bh, x, y, w, t, color);
    fill_rect(buf, bw, bh, x, y + h - t, w, t, color);
    fill_rect(buf, bw, bh, x, y, t, h, color);
    fill_rect(buf, bw, bh, x + w - t, y, t, h, color);
}

fn blend_glyph(
    buf: &mut [u32],
    bw: i32,
    bh: i32,
    gx: i32,
    gy: i32,
    metrics: &Metrics,
    bitmap: &[u8],
    color: u32,
    clip: &Rect,
) {
    let (fr, fg_, fb) = ((color >> 16) & 0xFF, (color >> 8) & 0xFF, color & 0xFF);
    for row in 0..metrics.height as i32 {
        let py = gy + row;
        if py < clip.y || py >= clip.bottom() || py < 0 || py >= bh {
            continue;
        }
        for col in 0..metrics.width as i32 {
            let px = gx + col;
            if px < clip.x || px >= clip.right() || px < 0 || px >= bw {
                continue;
            }
            let a = bitmap[(row * metrics.width as i32 + col) as usize] as u32;
            if a == 0 {
                continue;
            }
            let idx = (py * bw + px) as usize;
            if a == 255 {
                buf[idx] = color;
                continue;
            }
            let dst = buf[idx];
            let (dr, dg, db) = ((dst >> 16) & 0xFF, (dst >> 8) & 0xFF, dst & 0xFF);
            let r = (fr * a + dr * (255 - a)) / 255;
            let g = (fg_ * a + dg * (255 - a)) / 255;
            let b = (fb * a + db * (255 - a)) / 255;
            buf[idx] = (r << 16) | (g << 8) | b;
        }
    }
}

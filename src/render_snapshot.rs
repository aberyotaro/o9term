//! 描画を変えないリファクタリングの確認用。全部品を描いたフレームを書き出し、
//! 変更前後のファイルを比べる。
//!
//! O9_SNAPSHOT_DIR=/tmp/before cargo test --release render_snapshot -- --ignored
//! (変更後に /tmp/after へ書き出して) cmp -s で全ファイルを比べる。

use alacritty_terminal::event::VoidListener;
use alacritty_terminal::grid::Scroll;
use alacritty_terminal::index::{Column, Line, Point, Side};
use alacritty_terminal::selection::{Selection, SelectionType};
use alacritty_terminal::term::{Config, Term};
use alacritty_terminal::vte::ansi::Processor;

use crate::ime::Preedit;
use crate::panes::{SplitDir, SplitterHit};
use crate::renderer::{Canvas, Layout, Rect, Renderer};
use crate::term::GridSize;
use crate::theme;

const SAMPLE: &str = concat!(
    "plain \x1b[1mbold\x1b[0m \x1b[4munder\x1b[0m \x1b[7minverse\x1b[0m \x1b[2mdim\x1b[0m\r\n",
    "\x1b[31mred\x1b[32mgreen\x1b[94mbright\x1b[38;5;208m idx\x1b[38;2;10;200;90m rgb\x1b[48;5;24m bg \x1b[0m\r\n",
    "日本語 wide · é ü © ❯ ⏵ ✻ ◐ ← ─│\r\n",
    " ▐▛███▜▌ ░▒▓ ▀▄▖▗▘▙▚▛▜▝▞▟\r\n",
);

fn terminal(columns: usize, lines: usize) -> Term<VoidListener> {
    let size = GridSize { columns, screen_lines: lines };
    let mut term = Term::new(Config::default(), &size, VoidListener);
    let mut parser: Processor = Processor::new();
    for i in 0..30 {
        parser.advance(&mut term, format!("history line {i}\r\n").as_bytes());
    }
    parser.advance(&mut term, SAMPLE.as_bytes());
    let start = Point::new(Line(lines as i32 - 4), Column(2));
    let mut selection = Selection::new(SelectionType::Simple, start, Side::Left);
    selection.update(Point::new(Line(lines as i32 - 3), Column(5)), Side::Right);
    term.selection = Some(selection);
    term
}

fn write(dir: &str, name: &str, buf: &[u32], w: u32, h: u32) {
    let mut out = format!("P6 {w} {h} 255\n").into_bytes();
    for p in buf {
        out.extend([(p >> 16) as u8, (p >> 8) as u8, *p as u8]);
    }
    std::fs::write(format!("{dir}/{name}.ppm"), out).unwrap();
}

#[test]
#[ignore = "O9_SNAPSHOT_DIR を指定して手動で実行する"]
fn render_snapshot() {
    let dir = std::env::var("O9_SNAPSHOT_DIR").expect("O9_SNAPSHOT_DIR");
    std::fs::create_dir_all(&dir).unwrap();
    for scale in [1.0, 1.5, 2.0] {
        for bitmap in [false, true] {
            for (scheme_index, scheme) in theme::SCHEMES.iter().enumerate() {
                let tag = format!("s{scale}-b{bitmap}-{}", scheme.id);
                let mut r = Renderer::new(scale);
                r.bitmap_text = bitmap;
                let (w, h) = ((640.0 * scale) as u32, (420.0 * scale) as u32);
                let layout = Layout::compute(w, h, scale);
                let mut buf = vec![0u32; (w * h) as usize];

                for focused in [true, false] {
                    let mut cv = Canvas::new(&mut buf, w, h);
                    r.draw_chrome(&mut cv, &layout, "~/src/日本語 ✻", focused);
                    let c = layout.content;
                    let left = Rect { w: c.w / 2 - 3, ..c };
                    let right = Rect { x: c.x + c.w / 2 + 3, w: c.w - c.w / 2 - 3, ..c };
                    for (pane, active) in [(left, focused), (right, false)] {
                        let grid = r.grid_rect(pane);
                        let columns = (grid.w / r.cell_w as i32).max(1) as usize;
                        let lines = (grid.h / r.cell_h as i32).max(1) as usize;
                        let mut term = terminal(columns, lines);
                        if !active {
                            term.scroll_display(Scroll::Delta(3));
                        }
                        r.draw_pane(&mut cv, pane, &term, active, scheme);
                        if active {
                            let preedit = Preedit::new("にほんご入力".to_string(), Some((3, 9)));
                            r.draw_preedit(&mut cv, pane, &term, &preedit, scheme);
                        } else {
                            r.draw_visual_bell(&mut cv, Rect { h: pane.h / 3, ..pane });
                        }
                    }
                    let splitter = Rect { x: c.x + c.w / 2 - 3, w: 6, ..c };
                    r.draw_splitter(&mut cv, &SplitterHit { index: 0, dir: SplitDir::Vertical, track: c, rect: splitter });
                    let horizontal = Rect { y: c.y + c.h / 2, h: 6, ..right };
                    r.draw_splitter(&mut cv, &SplitterHit { index: 1, dir: SplitDir::Horizontal, track: right, rect: horizontal });
                    r.draw_grow_box(&mut cv, layout.grow_box);
                    r.draw_drop_outline(&mut cv, Rect { w: c.w / 3, ..right });
                    write(&dir, &format!("window-{tag}-f{focused}"), &buf, w, h);
                }

                let mut cv = Canvas::new(&mut buf, w, h);
                r.draw_chrome(&mut cv, &layout, "設定", true);
                let hits = r.draw_settings(&mut cv, &layout, scheme_index);
                assert_eq!(hits.len(), theme::SCHEMES.len() + 1);
                write(&dir, &format!("settings-{tag}"), &buf, w, h);
            }
        }
    }
}

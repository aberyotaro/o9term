//! o9term — Mac OS 9 (Platinum) 風ターミナルエミュレータ。
//! ウィンドウ装飾は OS 標準を使わず、タイトルバーごと自前で描画する。

mod clipboard;
mod config;
mod ime;
mod renderer;
mod tabs;
mod term;
mod theme;
mod window_shade;

use std::num::NonZeroU32;
use std::rc::Rc;
use std::time::{Duration, Instant};
use alacritty_terminal::index::{Column, Point, Side as CellSide};
use alacritty_terminal::selection::{Selection, SelectionType};

use alacritty_terminal::event::{Event as TermEvent, WindowSize};
use alacritty_terminal::grid::Scroll;
use alacritty_terminal::term::TermMode;
use winit::application::ApplicationHandler;
use winit::dpi::{LogicalSize, PhysicalPosition, PhysicalSize};
use winit::event::{ElementState, Ime, KeyEvent, MouseButton, MouseScrollDelta, WindowEvent};
use winit::event_loop::{ActiveEventLoop, ControlFlow, EventLoop, EventLoopProxy};
use winit::keyboard::{Key, ModifiersState, NamedKey};
use winit::window::{ResizeDirection, Window, WindowId};

use renderer::{tab_layout, Layout, Rect, Renderer, ScrollbarLayout, ScrollbarPart};
use tabs::{Pane, SplitDir, SplitterHit, Tab};
use term::{PaneId, Terminal, UserEvent};

const DEFAULT_TITLE: &str = "o9term";

fn main() {
    let mut args = std::env::args_os().skip(1);
    while let Some(arg) = args.next() {
        if arg == "--version" {
            println!("o9 {}", env!("CARGO_PKG_VERSION"));
            return;
        }
        if arg == "--working-directory" {
            let Some(path) = args.next() else {
                eprintln!("--working-directory requires a directory");
                std::process::exit(2);
            };
            if let Err(error) = std::env::set_current_dir(path) {
                eprintln!("Cannot open working directory: {error}");
                std::process::exit(2);
            }
        } else {
            eprintln!("Usage: os9terminal [--working-directory PATH] [--version]");
            std::process::exit(2);
        }
    }
    let event_loop =
        EventLoop::<UserEvent>::with_user_event().build().expect("イベントループ作成失敗");
    event_loop.set_control_flow(ControlFlow::Wait);
    let proxy = event_loop.create_proxy();
    let mut app = App::new(proxy);
    event_loop.run_app(&mut app).expect("イベントループ実行失敗");
}

/// スプリッターのドラッグ中の状態。
struct SplitterDrag {
    index: usize,
    dir: SplitDir,
    track: Rect,
    sw: i32,
}

/// タブのドラッグ中の状態。
struct TabDrag {
    /// ドラッグ中のタブ (tabs のインデックス)
    tab_index: usize,
    /// マウスを押した位置。閾値を超えて動くまではクリック扱い
    press_pos: (f64, f64),
    started: bool,
    target: Option<DropTarget>,
}

/// Cmd+ドラッグで移動するペイン。PTY を保持したまま木を組み替える。
struct PaneDrag {
    pane: PaneId,
    press_pos: (f64, f64),
    started: bool,
    target: Option<DropTarget>,
}

/// タブのドロップ先。
#[derive(Copy, Clone, PartialEq)]
enum DropTarget {
    /// タブバー内の並べ替え (挿入位置)
    TabBar(usize),
    /// ペインの上下左右へのドッキング
    Dock { pane: PaneId, side: Side },
}

#[derive(Copy, Clone, PartialEq)]
enum Side {
    Left,
    Right,
    Top,
    Bottom,
}

impl Side {
    /// ドッキング時の分割方向と挿入順 (true = 前側 = 左/上)。
    fn split(self) -> (SplitDir, bool) {
        match self {
            Side::Left => (SplitDir::Vertical, true),
            Side::Right => (SplitDir::Vertical, false),
            Side::Top => (SplitDir::Horizontal, true),
            Side::Bottom => (SplitDir::Horizontal, false),
        }
    }

    /// ペイン矩形のうち、ドロップ後に占める半分の領域 (プレビュー用)。
    fn preview(self, r: Rect) -> Rect {
        match self {
            Side::Left => Rect { x: r.x, y: r.y, w: r.w / 2, h: r.h },
            Side::Right => Rect { x: r.x + r.w / 2, y: r.y, w: r.w - r.w / 2, h: r.h },
            Side::Top => Rect { x: r.x, y: r.y, w: r.w, h: r.h / 2 },
            Side::Bottom => Rect { x: r.x, y: r.y + r.h / 2, w: r.w, h: r.h - r.h / 2 },
        }
    }
}

/// マウスドラッグの種別。
enum MouseDrag {
    Splitter(SplitterDrag),
    Tab(TabDrag),
    Pane(PaneDrag),
    Selection(PaneId),
}

#[derive(Clone, Copy)]
struct ScrollRepeat {
    pane: PaneId,
    part: ScrollbarPart,
    amount: i32,
    next: Instant,
}

/// 設定ウィンドウ (カラースキーマ選択) の状態。
struct SettingsState {
    window: Rc<Window>,
    _context: softbuffer::Context<Rc<Window>>,
    surface: softbuffer::Surface<Rc<Window>, Rc<Window>>,
    layout: Layout,
    row_hits: Vec<Rect>,
    cursor: (f64, f64),
    focused: bool,
    shade: window_shade::WindowShade,
}

struct App {
    proxy: EventLoopProxy<UserEvent>,
    window: Option<Rc<Window>>,
    _context: Option<softbuffer::Context<Rc<Window>>>,
    surface: Option<softbuffer::Surface<Rc<Window>, Rc<Window>>>,
    renderer: Option<Renderer>,
    tabs: Vec<Tab>,
    active_tab: usize,
    next_pane_id: PaneId,
    layout: Layout,
    // ヒットテスト用キャッシュ (update_layout で更新)
    pane_hits: Vec<(PaneId, Rect)>,
    splitter_hits: Vec<SplitterHit>,
    tab_hits: Vec<Rect>,
    plus_hit: Option<Rect>,
    drag: Option<MouseDrag>,
    modifiers: ModifiersState,
    cursor_pos: (f64, f64),
    focused: bool,
    scheme_index: usize,
    settings: Option<SettingsState>,
    last_click: Option<(PaneId, Point, Instant, u8)>,
    ime: ime::ImeState,
    shade: window_shade::WindowShade,
    bells: Vec<(PaneId, Instant)>,
    scrollbar_drag: Option<(PaneId, f64)>,
    scroll_repeat: Option<ScrollRepeat>,
}

impl App {
    fn new(proxy: EventLoopProxy<UserEvent>) -> Self {
        Self {
            proxy,
            window: None,
            _context: None,
            surface: None,
            renderer: None,
            tabs: Vec::new(),
            active_tab: 0,
            next_pane_id: 1,
            layout: Layout::default(),
            pane_hits: Vec::new(),
            splitter_hits: Vec::new(),
            tab_hits: Vec::new(),
            plus_hit: None,
            drag: None,
            modifiers: ModifiersState::default(),
            cursor_pos: (0.0, 0.0),
            focused: true,
            scheme_index: theme::scheme_index(&config::load_scheme().unwrap_or_default()),
            settings: None,
            last_click: None,
            ime: ime::ImeState::default(),
            shade: window_shade::WindowShade::default(),
            bells: Vec::new(),
            scrollbar_drag: None,
            scroll_repeat: None,
        }
    }

    fn splitter_width(&self) -> i32 {
        let scale = self.renderer.as_ref().map(|r| r.scale).unwrap_or(1.0);
        ((8.0 * scale).round() as i32).max(6)
    }

    /// 新しいペイン (= 新しい PTY) を作る。
    fn new_pane(&mut self) -> Option<Pane> {
        let renderer = self.renderer.as_ref()?;
        let id = self.next_pane_id;
        self.next_pane_id += 1;
        // 実寸は直後の sync_pane_sizes で合わせるため仮サイズで起動する
        let ws = WindowSize {
            num_cols: 80,
            num_lines: 24,
            cell_width: renderer.cell_w as u16,
            cell_height: renderer.cell_h as u16,
        };
        match Terminal::new(self.proxy.clone(), id, ws) {
            Ok(terminal) => Some(Pane { id, terminal, title: DEFAULT_TITLE.into() }),
            Err(e) => {
                eprintln!("PTY起動失敗: {e}");
                None
            }
        }
    }

    /// レイアウトとヒットテスト用の矩形キャッシュを再計算する。
    fn update_layout(&mut self) {
        let (Some(window), Some(renderer)) = (self.window.as_ref(), self.renderer.as_ref()) else {
            return;
        };
        let size = window.inner_size();
        let show_tabs = self.tabs.len() > 1
            || self.tabs.get(self.active_tab).is_some_and(|tab| tab.root.pane_count() > 1);
        self.layout = Layout::compute(size.width, size.height, renderer.scale, show_tabs && !self.shade.is_shaded());

        self.pane_hits.clear();
        self.splitter_hits.clear();
        let sw = self.splitter_width();
        if let Some(tab) = self.tabs.get(self.active_tab) {
            tab.root.layout(self.layout.content, sw, &mut self.pane_hits, &mut self.splitter_hits);
        }

        if let Some(bar) = self.layout.tab_bar {
            let (rects, plus) = tab_layout(bar, self.tabs.len(), renderer.scale);
            self.tab_hits = rects;
            self.plus_hit = Some(plus);
        } else {
            self.tab_hits.clear();
            self.plus_hit = None;
        }
        if self.shade.is_shaded() {
            self.pane_hits.clear();
            self.splitter_hits.clear();
            self.layout.grow_box = Rect::default();
        }
    }

    fn ws_for_rect(&self, rect: Rect) -> Option<WindowSize> {
        let renderer = self.renderer.as_ref()?;
        let grid = renderer.grid_rect(rect);
        Some(WindowSize {
            num_cols: ((grid.w.max(0) as usize) / renderer.cell_w).max(2) as u16,
            num_lines: ((grid.h.max(0) as usize) / renderer.cell_h).max(1) as u16,
            cell_width: renderer.cell_w as u16,
            cell_height: renderer.cell_h as u16,
        })
    }

    /// アクティブタブの各ペインの PTY サイズを現在のレイアウトに合わせる。
    fn sync_pane_sizes(&self) {
        if self.shade.is_shaded() { return; }
        let Some(tab) = self.tabs.get(self.active_tab) else { return };
        for (id, rect) in &self.pane_hits {
            if let (Some(pane), Some(ws)) = (tab.root.find_pane(*id), self.ws_for_rect(*rect)) {
                pane.terminal.resize(ws);
            }
        }
    }

    /// レイアウト再計算 + PTY サイズ同期 + 再描画。構造が変わったら呼ぶ。
    fn refresh(&mut self) {
        self.update_layout();
        self.sync_pane_sizes();
        self.sync_ime_target();
        self.redraw();
    }

    fn redraw(&self) {
        if let Some(window) = &self.window {
            window.request_redraw();
        }
    }

    fn focused_terminal(&self) -> Option<&Terminal> {
        let tab = self.tabs.get(self.active_tab)?;
        tab.root.find_pane(tab.focused).map(|p| &p.terminal)
    }

    /// IME の入力先をペインに結び付け、フォーカス移動時に未確定入力を破棄する。
    fn sync_ime_target(&mut self) {
        let target = if self.focused && !self.shade.is_shaded() {
            self.tabs.get(self.active_tab).map(|tab| tab.focused)
        } else { None };
        if self.ime.set_target(target) {
            if let Some(window) = &self.window {
                window.set_ime_allowed(false);
                window.set_ime_allowed(target.is_some());
            }
        }
    }

    fn cancel_ime(&mut self) {
        self.ime.reset();
        if let Some(window) = &self.window {
            window.set_ime_allowed(false);
            window.set_ime_allowed(self.focused && !self.shade.is_shaded());
        }
        self.redraw();
    }

    fn handle_ime(&mut self, event: Ime) {
        self.sync_ime_target();
        if let Some(text) = self.ime.update(event) {
            self.write_input(text.into_bytes());
        } else if self.ime.preedit().is_some() {
            if let Some(terminal) = self.focused_terminal() {
                let mut term = terminal.term.lock();
                term.selection = None;
                term.scroll_display(Scroll::Bottom);
            }
        }
        self.redraw();
    }

    fn write_input(&self, bytes: Vec<u8>) {
        if let Some(terminal) = self.focused_terminal() {
            {
                let mut term = terminal.term.lock();
                term.selection = None;
                term.scroll_display(Scroll::Bottom);
            }
            terminal.write(bytes);
            self.redraw();
        }
    }

    /// 各ペインのタイトルをシェルの cwd (ホームは ~ に短縮) で更新する。
    fn update_titles(&mut self) {
        let home = std::env::var("HOME").unwrap_or_default();
        for tab in &mut self.tabs {
            tab.root.for_each_pane_mut(&mut |pane| {
                if std::env::var("OS9_DEBUG").is_ok() {
                    eprintln!("[debug] pane={} cwd={:?}", pane.id, pane.terminal.cwd());
                }
                let Some(cwd) = pane.terminal.cwd() else { return };
                let path = cwd.to_string_lossy();
                pane.title = match path.strip_prefix(&home) {
                    Some(rest) if !home.is_empty() && (rest.is_empty() || rest.starts_with('/')) => {
                        format!("~{rest}")
                    }
                    _ => path.into_owned(),
                };
            });
        }
    }

    fn render(&mut self) {
        self.update_titles();
        self.update_layout();
        self.sync_ime_target();
        let (Some(window), Some(surface), Some(renderer)) =
            (self.window.as_ref(), self.surface.as_mut(), self.renderer.as_mut())
        else {
            return;
        };
        let Some(tab) = self.tabs.get(self.active_tab) else { return };

        let size = window.inner_size();
        let (Some(w), Some(h)) = (NonZeroU32::new(size.width), NonZeroU32::new(size.height))
        else {
            return;
        };
        if surface.resize(w, h).is_err() {
            return;
        }
        let Ok(mut buffer) = surface.buffer_mut() else { return };

        let tab_views: Vec<(String, bool)> = self
            .tabs
            .iter()
            .enumerate()
            .map(|(i, t)| (t.label(), i == self.active_tab))
            .collect();
        let title = tab.title().to_string();

        renderer.draw_chrome(
            &mut buffer,
            size.width,
            size.height,
            &self.layout,
            &title,
            &tab_views,
            self.focused,
        );

        if self.shade.is_shaded() {
            let _ = buffer.present();
            return;
        }

        let scheme = &theme::SCHEMES[self.scheme_index];
        for (id, rect) in &self.pane_hits {
            if let Some(pane) = tab.root.find_pane(*id) {
                let term = pane.terminal.term.lock();
                let active = self.focused && tab.focused == *id;
                renderer.draw_pane(
                    &mut buffer,
                    size.width,
                    size.height,
                    *rect,
                    &term,
                    active,
                    scheme,
                );
                if active {
                    let grid = renderer.grid_rect(*rect);
                    let point = term.grid().cursor.point;
                    let viewport = alacritty_terminal::term::point_to_viewport(
                        term.grid().display_offset(), point);
                    let cw = renderer.cell_w as i32;
                    let ch = renderer.cell_h as i32;
                    let col = viewport.map_or(0, |p| p.column.0 as i32)
                        .clamp(0, (grid.w / cw - 1).max(0));
                    let row = viewport.map_or(0, |p| p.line as i32)
                        .clamp(0, (grid.h / ch - 1).max(0));
                    let mut anchor = Rect {
                        x: grid.x + col * cw, y: grid.y + row * ch, w: cw, h: ch,
                    };
                    if let Some(preedit) = self.ime.preedit() {
                        if let Some(area) = renderer.draw_preedit(
                            &mut buffer, size.width, size.height, *rect, &term, preedit, scheme,
                        ) { anchor = area; }
                    }
                    window.set_ime_cursor_area(
                        PhysicalPosition::new(anchor.x, anchor.y),
                        PhysicalSize::new(anchor.w.max(1) as u32, anchor.h.max(1) as u32),
                    );
                }
                if self.bells.iter().any(|(pane_id, until)| *pane_id == *id && *until > Instant::now()) {
                    renderer.draw_visual_bell(&mut buffer, size.width, size.height, *rect);
                }
            }
        }
        for sh in &self.splitter_hits {
            renderer.draw_splitter(&mut buffer, size.width, size.height, sh);
        }
        renderer.draw_grow_box(&mut buffer, size.width, size.height, self.layout.grow_box);

        // タブ・ペインドラッグ中の視覚フィードバック (最前面)
        let drop_preview = match &self.drag {
            Some(MouseDrag::Tab(td)) if td.started => Some(td.target),
            Some(MouseDrag::Pane(pd)) if pd.started => Some(pd.target),
            _ => None,
        };
        if let Some(target) = drop_preview {
            match target {
                Some(DropTarget::Dock { pane, side }) => {
                    if let Some((_, r)) = self.pane_hits.iter().find(|(id, _)| *id == pane) {
                        renderer.draw_drop_outline(
                            &mut buffer,
                            size.width,
                            size.height,
                            side.preview(*r),
                        );
                    }
                }
                Some(DropTarget::TabBar(pos)) => {
                    if let Some(bar) = self.layout.tab_bar {
                        let s = |v: f64| ((v * renderer.scale).round() as i32).max(1);
                        let x = match self.tab_hits.get(pos) {
                            Some(r) => r.x - s(2.0),
                            None => {
                                self.tab_hits.last().map(|r| r.right() + s(1.0)).unwrap_or(bar.x)
                            }
                        };
                        let bar_rect = Rect {
                            x,
                            y: bar.y + s(2.0),
                            w: s(2.0),
                            h: bar.h - s(4.0),
                        };
                        renderer.draw_drop_bar(&mut buffer, size.width, size.height, bar_rect);
                    }
                }
                None => {}
            }
            // 移動中のゴースト枠 (カーソル追従)
            let ghost_size = match &self.drag {
                Some(MouseDrag::Tab(td)) => self.tab_hits.get(td.tab_index).map(|r| (r.w, r.h)),
                Some(MouseDrag::Pane(_)) => Some(((120.0 * renderer.scale) as i32, (36.0 * renderer.scale) as i32)),
                _ => None,
            };
            if let Some((w, h)) = ghost_size {
                let (cx, cy) = (self.cursor_pos.0 as i32, self.cursor_pos.1 as i32);
                let ghost = Rect { x: cx - w / 2, y: cy - h / 2, w, h };
                renderer.draw_drop_outline(&mut buffer, size.width, size.height, ghost);
            }
        }

        let _ = buffer.present();
    }

    // ─── タブ・ペイン操作 ───

    fn cancel_pointer_gestures(&mut self) {
        self.drag = None;
        self.scrollbar_drag = None;
        self.scroll_repeat = None;
    }

    fn new_tab(&mut self) {
        if let Some(pane) = self.new_pane() {
            self.cancel_pointer_gestures();
            self.tabs.push(Tab::new(pane));
            self.active_tab = self.tabs.len() - 1;
            self.refresh();
        }
    }

    fn select_tab(&mut self, index: usize) {
        if index < self.tabs.len() && index != self.active_tab {
            if !matches!(self.drag, Some(MouseDrag::Pane(_) | MouseDrag::Tab(_))) {
                self.drag = None;
            }
            self.scrollbar_drag = None;
            self.scroll_repeat = None;
            self.active_tab = index;
            self.refresh();
        }
    }

    fn cycle_tab(&mut self, delta: isize) {
        if self.tabs.is_empty() {
            return;
        }
        let n = self.tabs.len() as isize;
        let next = (self.active_tab as isize + delta).rem_euclid(n) as usize;
        self.select_tab(next);
    }

    fn close_active_tab(&mut self, event_loop: &ActiveEventLoop) {
        if self.tabs.is_empty() {
            return;
        }
        self.cancel_pointer_gestures();
        // Tab の drop で全ペインの PTY が停止する
        self.tabs.remove(self.active_tab);
        if self.tabs.is_empty() {
            event_loop.exit();
            return;
        }
        self.active_tab = self.active_tab.min(self.tabs.len() - 1);
        self.refresh();
    }

    fn split(&mut self, dir: SplitDir) {
        let Some(tab) = self.tabs.get(self.active_tab) else { return };
        let target = tab.focused;
        let Some(pane) = self.new_pane() else { return };
        self.cancel_pointer_gestures();
        let new_id = pane.id;
        let mut carrier = Some(pane);
        let tab = &mut self.tabs[self.active_tab];
        if tab.root.split_pane(target, dir, &mut carrier) {
            tab.focused = new_id;
            self.refresh();
        }
    }

    /// ペインを1つ閉じる。タブ最後のペインならタブごと閉じる。
    fn close_pane(&mut self, id: PaneId, event_loop: &ActiveEventLoop) {
        let Some(tab_idx) = self.tabs.iter().position(|t| t.root.contains(id)) else { return };
        // シェルがドラッグ中に終了しても、古いタブ番号や分割番号を使わない。
        self.cancel_pointer_gestures();

        if self.tabs[tab_idx].root.is_leaf_of(id) {
            self.tabs.remove(tab_idx);
            if self.tabs.is_empty() {
                event_loop.exit();
                return;
            }
            if self.active_tab >= self.tabs.len() {
                self.active_tab = self.tabs.len() - 1;
            } else if tab_idx < self.active_tab {
                self.active_tab -= 1;
            }
        } else {
            let tab = &mut self.tabs[tab_idx];
            tab.root.remove_pane(id);
            if tab.focused == id {
                tab.focused = tab.root.first_pane().map(|p| p.id).unwrap_or(0);
            }
        }
        self.refresh();
    }

    fn close_focused_pane(&mut self, event_loop: &ActiveEventLoop) {
        if let Some(tab) = self.tabs.get(self.active_tab) {
            self.close_pane(tab.focused, event_loop);
        }
    }

    /// 方向指定でペインのフォーカスを移す (Cmd+Option+矢印)。
    fn move_focus(&mut self, dx: i32, dy: i32) {
        let Some(tab) = self.tabs.get(self.active_tab) else { return };
        let Some((_, cur)) = self.pane_hits.iter().find(|(id, _)| *id == tab.focused) else {
            return;
        };
        let (cx, cy) = cur.center();
        let best = self
            .pane_hits
            .iter()
            .filter(|(id, _)| *id != tab.focused)
            .filter(|(_, r)| {
                let (px, py) = r.center();
                (px - cx) * dx > 0 || (py - cy) * dy > 0
            })
            .min_by_key(|(_, r)| {
                let (px, py) = r.center();
                let (ddx, ddy) = (px - cx, py - cy);
                ddx * ddx + ddy * ddy
            })
            .map(|(id, _)| *id);
        if let Some(id) = best {
            self.tabs[self.active_tab].focused = id;
            self.redraw();
        }
    }

    // ─── 設定ウィンドウ ───

    fn toggle_settings(&mut self, event_loop: &ActiveEventLoop) {
        if self.settings.is_some() {
            self.settings = None;
            return;
        }
        self.cancel_ime();
        let attrs = Window::default_attributes()
            .with_title("設定")
            .with_decorations(false)
            .with_resizable(false)
            .with_inner_size(LogicalSize::new(300.0, 236.0));
        let Ok(window) = event_loop.create_window(attrs) else { return };
        let window = Rc::new(window);
        let Ok(context) = softbuffer::Context::new(window.clone()) else { return };
        let Ok(surface) = softbuffer::Surface::new(&context, window.clone()) else { return };
        window.request_redraw();
        self.settings = Some(SettingsState {
            window,
            _context: context,
            surface,
            layout: Layout::default(),
            row_hits: Vec::new(),
            cursor: (0.0, 0.0),
            focused: true,
            shade: window_shade::WindowShade::default(),
        });
    }

    fn render_settings(&mut self) {
        let (Some(st), Some(renderer)) = (self.settings.as_mut(), self.renderer.as_mut()) else {
            return;
        };
        let size = st.window.inner_size();
        let (Some(w), Some(h)) = (NonZeroU32::new(size.width), NonZeroU32::new(size.height))
        else {
            return;
        };
        if st.surface.resize(w, h).is_err() {
            return;
        }
        let Ok(mut buffer) = st.surface.buffer_mut() else { return };

        let layout = Layout::compute(size.width, size.height, renderer.scale, false);
        renderer.draw_chrome(
            &mut buffer,
            size.width,
            size.height,
            &layout,
            "設定",
            &[],
            st.focused,
        );
        st.layout = layout;
        if st.shade.is_shaded() {
            st.row_hits.clear();
            let _ = buffer.present();
            return;
        }
        let hits =
            renderer.draw_settings(&mut buffer, size.width, size.height, &layout, self.scheme_index);
        st.layout = layout;
        st.row_hits = hits;
        let _ = buffer.present();
    }

    /// スキーマを切り替えて保存し、両ウィンドウを再描画する。
    fn apply_scheme(&mut self, index: usize) {
        if index >= theme::SCHEMES.len() || index == self.scheme_index {
            return;
        }
        self.scheme_index = index;
        config::save_scheme(theme::SCHEMES[index].id);
        self.redraw();
        if let Some(st) = &self.settings {
            st.window.request_redraw();
        }
    }

    fn toggle_bitmap_text(&mut self) {
        if let Some(renderer) = self.renderer.as_mut() {
            renderer.bitmap_text = !renderer.bitmap_text;
            config::save_bitmap_text(renderer.bitmap_text);
        }
        self.redraw();
        if let Some(st) = &self.settings { st.window.request_redraw(); }
    }

    fn handle_settings_event(&mut self, event_loop: &ActiveEventLoop, event: WindowEvent) {
        match event {
            WindowEvent::CloseRequested => self.settings = None,
            WindowEvent::RedrawRequested => self.render_settings(),
            WindowEvent::Focused(focused) => {
                if let Some(st) = &mut self.settings {
                    st.focused = focused;
                    st.window.request_redraw();
                }
            }
            WindowEvent::ModifiersChanged(m) => self.modifiers = m.state(),
            WindowEvent::CursorMoved { position, .. } => {
                if let Some(st) = &mut self.settings {
                    st.cursor = (position.x, position.y);
                }
            }
            WindowEvent::MouseInput {
                state: ElementState::Pressed, button: MouseButton::Left, ..
            } => {
                let Some(st) = &mut self.settings else { return };
                let (x, y) = st.cursor;
                if st.layout.close_box.contains(x, y) {
                    self.settings = None;
                } else if st.layout.collapse_box.contains(x, y) {
                    st.shade.toggle(&st.window, st.window.scale_factor(), None);
                    st.window.request_redraw();
                } else if let Some(i) = st.row_hits.iter().position(|r| r.contains(x, y)) {
                    if i == theme::SCHEMES.len() {
                        self.toggle_bitmap_text();
                    } else {
                        self.apply_scheme(i);
                    }
                } else if st.layout.titlebar.contains(x, y) {
                    let _ = st.window.drag_window();
                }
            }
            WindowEvent::KeyboardInput { event, .. } => {
                if event.state != ElementState::Pressed {
                    return;
                }
                let close = matches!(event.logical_key, Key::Named(NamedKey::Escape))
                    || (self.modifiers.super_key()
                        && matches!(&event.logical_key, Key::Character(s) if s.as_str() == "w" || s.as_str() == ","));
                if close {
                    self.settings = None;
                } else if self.modifiers.super_key() {
                    if let Key::Character(s) = &event.logical_key {
                        if s.as_str() == "q" {
                            self.tabs.clear();
                            event_loop.exit();
                        }
                    }
                }
            }
            _ => {}
        }
    }

    // ─── 入力 ───

    fn handle_key(&mut self, event: KeyEvent, event_loop: &ActiveEventLoop) {
        if event.state != ElementState::Pressed {
            return;
        }
        let mods = self.modifiers;

        if matches!(event.logical_key, Key::Named(NamedKey::Escape))
            && (self.drag.is_some() || self.scrollbar_drag.is_some() || self.scroll_repeat.is_some()) {
            self.cancel_pointer_gestures();
            self.redraw();
            return;
        }

        if mods.super_key() {
            self.handle_command_key(&event, event_loop);
            return;
        }

        if !self.focused || self.shade.is_shaded() { return; }

        let Some(terminal) = self.focused_terminal() else { return };
        let app_cursor = terminal.term.lock().mode().contains(TermMode::APP_CURSOR);
        let arrow = |c: u8| -> Vec<u8> {
            if app_cursor {
                vec![0x1b, b'O', c]
            } else {
                vec![0x1b, b'[', c]
            }
        };

        let bytes: Option<Vec<u8>> = match &event.logical_key {
            Key::Named(NamedKey::Enter) => Some(b"\r".to_vec()),
            Key::Named(NamedKey::Backspace) => Some(vec![0x7f]),
            Key::Named(NamedKey::Tab) => Some(b"\t".to_vec()),
            Key::Named(NamedKey::Escape) => Some(vec![0x1b]),
            Key::Named(NamedKey::ArrowUp) => Some(arrow(b'A')),
            Key::Named(NamedKey::ArrowDown) => Some(arrow(b'B')),
            Key::Named(NamedKey::ArrowRight) => Some(arrow(b'C')),
            Key::Named(NamedKey::ArrowLeft) => Some(arrow(b'D')),
            Key::Named(NamedKey::Home) => Some(b"\x1b[H".to_vec()),
            Key::Named(NamedKey::End) => Some(b"\x1b[F".to_vec()),
            Key::Named(NamedKey::PageUp) => Some(b"\x1b[5~".to_vec()),
            Key::Named(NamedKey::PageDown) => Some(b"\x1b[6~".to_vec()),
            Key::Named(NamedKey::Delete) => Some(b"\x1b[3~".to_vec()),
            Key::Named(NamedKey::Space) if mods.control_key() => Some(vec![0x00]),
            Key::Character(s) if mods.control_key() => {
                // Ctrl+A..Z など制御文字へ変換
                s.chars()
                    .next()
                    .filter(char::is_ascii)
                    .map(|c| vec![(c.to_ascii_uppercase() as u8) & 0x1f])
            }
            _ => None,
        };

        let bytes = bytes.or_else(|| {
            event.text.as_ref().filter(|t| !t.is_empty()).map(|t| t.as_bytes().to_vec())
        });

        if let Some(bytes) = bytes {
            // macOS が IME で消費したキーは winit が抑制する。
            // ここに届く Ctrl+C 等は変換を中止して通常どおり端末へ送る。
            if self.ime.preedit().is_some() { self.cancel_ime(); }
            self.write_input(bytes);
        }
    }

    /// Cmd 系ショートカット。
    fn handle_command_key(&mut self, event: &KeyEvent, event_loop: &ActiveEventLoop) {
        let shift = self.modifiers.shift_key();
        let alt = self.modifiers.alt_key();

        if alt {
            match &event.logical_key {
                Key::Named(NamedKey::ArrowLeft) => return self.move_focus(-1, 0),
                Key::Named(NamedKey::ArrowRight) => return self.move_focus(1, 0),
                Key::Named(NamedKey::ArrowUp) => return self.move_focus(0, -1),
                Key::Named(NamedKey::ArrowDown) => return self.move_focus(0, 1),
                _ => {}
            }
        }

        let Key::Character(s) = &event.logical_key else { return };
        match s.to_lowercase().as_str() {
            "q" => {
                self.tabs.clear(); // 全 PTY を停止
                event_loop.exit();
            }
            "c" => self.copy_selection(),
            "v" => self.paste_clipboard(),
            "t" if shift => self.detach_focused_pane(),
            "t" => self.new_tab(),
            "w" if shift => self.close_focused_pane(event_loop),
            "w" => self.close_active_tab(event_loop),
            "d" if shift => self.split(SplitDir::Horizontal),
            "d" => self.split(SplitDir::Vertical),
            "," => self.toggle_settings(event_loop),
            "{" => self.cycle_tab(-1),
            "}" => self.cycle_tab(1),
            d @ ("1" | "2" | "3" | "4" | "5" | "6" | "7" | "8" | "9") => {
                let idx = d.parse::<usize>().unwrap() - 1;
                self.select_tab(idx);
            }
            _ => {}
        }
    }

    fn handle_mouse_press(&mut self, event_loop: &ActiveEventLoop) {
        let (x, y) = self.cursor_pos;
        let Some(window) = self.window.as_ref().cloned() else { return };

        if self.layout.close_box.contains(x, y) {
            self.tabs.clear();
            event_loop.exit();
            return;
        }
        if self.layout.zoom_box.contains(x, y) {
            if self.shade.is_shaded() { self.toggle_shade(); }
            window.set_maximized(!window.is_maximized());
            return;
        }
        if self.layout.collapse_box.contains(x, y) {
            self.toggle_shade();
            return;
        }
        if let Some(i) = self.tab_hits.iter().position(|r| r.contains(x, y)) {
            // タブ選択は離した時に確定する (ドラッグと区別するため)
            self.drag = Some(MouseDrag::Tab(TabDrag {
                tab_index: i,
                press_pos: (x, y),
                started: false,
                target: None,
            }));
            return;
        }
        if self.plus_hit.is_some_and(|r| r.contains(x, y)) {
            self.new_tab();
            return;
        }
        if let Some(sh) = self.splitter_hits.iter().find(|sh| sh.rect.contains(x, y)) {
            self.drag = Some(MouseDrag::Splitter(SplitterDrag {
                index: sh.index,
                dir: sh.dir,
                track: sh.track,
                sw: self.splitter_width(),
            }));
            return;
        }
        if self.layout.grow_box.contains(x, y) {
            let _ = window.drag_resize_window(ResizeDirection::SouthEast);
            return;
        }
        if self.layout.titlebar.contains(x, y) {
            let _ = window.drag_window();
            return;
        }
        if let Some((id, _)) = self.pane_hits.iter().find(|(_, r)| r.contains(x, y)) {
            let id = *id;
            self.tabs[self.active_tab].focused = id;
            if self.modifiers.super_key() {
                self.last_click = None;
                self.drag = Some(MouseDrag::Pane(PaneDrag {
                    pane: id,
                    press_pos: (x, y),
                    started: false,
                    target: None,
                }));
                self.redraw();
                return;
            }
            if let Some((point, side)) = self.selection_point(id) {
                let now = Instant::now();
                let count = self.last_click.filter(|(pid, p, t, _)|
                    *pid == id && *p == point && now.duration_since(*t) < Duration::from_millis(400))
                    .map_or(1, |(_, _, _, n)| n % 3 + 1);
                self.last_click = Some((id, point, now, count));
                let ty = match count {
                    2 => SelectionType::Semantic,
                    3 => SelectionType::Lines,
                    _ => SelectionType::Simple,
                };
                if let Some(terminal) = self.focused_terminal() {
                    terminal.term.lock().selection = Some(Selection::new(ty, point, side));
                }
                self.drag = Some(MouseDrag::Selection(id));
            }
            self.redraw();
        }
    }

    fn selection_point(&self, id: PaneId) -> Option<(Point, CellSide)> {
        let renderer = self.renderer.as_ref()?;
        let rect = self.pane_hits.iter().find(|(pid, _)| *pid == id)?.1;
        let grid = renderer.grid_rect(rect);
        let pane = self.tabs.get(self.active_tab)?.root.find_pane(id)?;
        let term = pane.terminal.term.lock();
        let cw = renderer.cell_w as f64;
        let ch = renderer.cell_h as f64;
        let x = (self.cursor_pos.0 - grid.x as f64).clamp(0.0, (grid.w - 1).max(0) as f64);
        let y = (self.cursor_pos.1 - grid.y as f64).clamp(0.0, (grid.h - 1).max(0) as f64);
        use alacritty_terminal::grid::Dimensions;
        let col = (x / cw) as usize;
        let row = (y / ch) as usize;
        let point = alacritty_terminal::term::viewport_to_point(
            term.grid().display_offset(),
            Point::new(row.min(term.screen_lines() - 1), Column(col.min(term.columns() - 1))));
        let side = if x % cw < cw / 2.0 { CellSide::Left } else { CellSide::Right };
        Some((point, side))
    }

    fn toggle_shade(&mut self) {
        if let Some(window) = &self.window {
            self.shade.toggle(window, window.scale_factor(), Some(LogicalSize::new(320.0, 200.0)));
        }
        self.drag = None;
        self.scrollbar_drag = None;
        self.scroll_repeat = None;
        self.refresh();
    }

    fn scrollbar_for(&self, id: PaneId) -> Option<ScrollbarLayout> {
        let renderer = self.renderer.as_ref()?;
        let (_, rect) = self.pane_hits.iter().find(|(pane, _)| *pane == id)?;
        let pane = self.tabs.get(self.active_tab)?.root.find_pane(id)?;
        Some(renderer.scrollbar_layout(*rect, &pane.terminal.term.lock()))
    }

    fn scroll_pane(&self, id: PaneId, delta: i32) {
        if let Some(pane) = self.tabs.get(self.active_tab).and_then(|tab| tab.root.find_pane(id)) {
            pane.terminal.term.lock().scroll_display(Scroll::Delta(delta));
            self.redraw();
        }
    }

    fn handle_scrollbar_press(&mut self) -> bool {
        let (x, y) = self.cursor_pos;
        let Some(id) = self.pane_hits.iter().find(|(_, rect)| rect.contains(x, y)).map(|(id, _)| *id)
        else { return false };
        let Some(bar) = self.scrollbar_for(id) else { return false };
        if !bar.bounds.contains(x, y) { return false; }
        // A disabled scrollbar still consumes clicks; it must not select terminal text.
        let Some(part) = bar.hit_test(x, y) else { return true };
        let lines = self.ws_for_rect(self.pane_hits.iter().find(|(pane, _)| *pane == id).unwrap().1)
            .map_or(1, |ws| ws.num_lines.saturating_sub(1).max(1) as i32);
        let amount = match part {
            ScrollbarPart::Up => 1,
            ScrollbarPart::Down => -1,
            ScrollbarPart::PageUp => lines,
            ScrollbarPart::PageDown => -lines,
            ScrollbarPart::Thumb => {
                self.scrollbar_drag = Some((id, y - bar.thumb.y as f64));
                return true;
            }
        };
        self.scroll_pane(id, amount);
        self.scroll_repeat = Some(ScrollRepeat {
            pane: id, part, amount, next: Instant::now() + Duration::from_millis(350),
        });
        true
    }

    fn handle_scrollbar_drag(&mut self) {
        let Some((id, grab_y)) = self.scrollbar_drag else { return };
        let Some(bar) = self.scrollbar_for(id) else { self.scrollbar_drag = None; return };
        let target = bar.offset_for_thumb_top(self.cursor_pos.1 - grab_y);
        if let Some(pane) = self.tabs.get(self.active_tab).and_then(|tab| tab.root.find_pane(id)) {
            let mut term = pane.terminal.term.lock();
            let current = term.grid().display_offset();
            term.scroll_display(Scroll::Delta((target as i64 - current as i64).clamp(i32::MIN as i64, i32::MAX as i64) as i32));
        }
        self.redraw();
    }

    fn update_selection(&mut self, id: PaneId) {
        if let Some((point, side)) = self.selection_point(id) {
            if let Some(pane) = self.tabs[self.active_tab].root.find_pane(id) {
                if let Some(selection) = pane.terminal.term.lock().selection.as_mut() {
                    selection.update(point, side);
                }
            }
            self.redraw();
        }
    }

    fn copy_selection(&self) {
        let text = self.focused_terminal()
            .and_then(|terminal| terminal.term.lock().selection_to_string());
        if let Some(text) = text.filter(|text| !text.is_empty()) {
            if let Err(error) = clipboard::set(&text) {
                eprintln!("Copy failed: {error}");
            }
        }
    }

    fn paste_clipboard(&mut self) {
        self.cancel_ime();
        let Some(terminal) = self.focused_terminal() else { return };
        match clipboard::get() {
            Ok(text) => {
                let mut term = terminal.term.lock();
                let bracketed = term.mode().contains(TermMode::BRACKETED_PASTE);
                term.selection = None;
                term.scroll_display(Scroll::Bottom);
                drop(term);
                terminal.write(clipboard::paste_bytes(&text, bracketed));
                self.redraw();
            }
            Err(error) => eprintln!("Paste failed: {error}"),
        }
    }

    fn handle_splitter_drag(&mut self) {
        let Some(MouseDrag::Splitter(drag)) = &self.drag else { return };
        let (x, y) = self.cursor_pos;
        let (pos, start, len) = match drag.dir {
            SplitDir::Vertical => (x, drag.track.x, drag.track.w - drag.sw),
            SplitDir::Horizontal => (y, drag.track.y, drag.track.h - drag.sw),
        };
        if len <= 0 {
            return;
        }
        let ratio =
            (((pos - start as f64 - drag.sw as f64 / 2.0) / len as f64) as f32).clamp(0.1, 0.9);
        let index = drag.index;
        if let Some(tab) = self.tabs.get_mut(self.active_tab) {
            tab.root.set_ratio(index, ratio);
        }
        self.update_layout();
        self.sync_pane_sizes();
        self.redraw();
    }

    /// タブドラッグ中のカーソル移動。閾値を超えたらドラッグ開始し、
    /// ドロップ先を再計算する。
    fn handle_tab_drag(&mut self) {
        let scale = self.renderer.as_ref().map(|r| r.scale).unwrap_or(1.0);
        let threshold = 4.0 * scale;
        let Some(MouseDrag::Tab(td)) = &self.drag else { return };

        let (x, y) = self.cursor_pos;
        let (px, py) = td.press_pos;
        let started = td.started || (x - px).abs() > threshold || (y - py).abs() > threshold;
        if !started {
            return;
        }
        let target = self.compute_drop_target(td.tab_index);

        if let Some(MouseDrag::Tab(td)) = &mut self.drag {
            td.started = true;
            td.target = target;
        }
        self.redraw();
    }

    fn handle_pane_drag(&mut self) {
        let scale = self.renderer.as_ref().map(|r| r.scale).unwrap_or(1.0);
        let Some(MouseDrag::Pane(pd)) = &self.drag else { return };
        let (x, y) = self.cursor_pos;
        let (px, py) = pd.press_pos;
        if !pd.started && (x - px).abs() <= 4.0 * scale && (y - py).abs() <= 4.0 * scale {
            return;
        }
        let source = pd.pane;
        // 別タブの上を通ると切り替える。そのまま本文へ降ろしてドックできる。
        if let Some(index) = self.tab_hits.iter().position(|r| r.contains(x, y)) {
            self.select_tab(index);
        }
        let target = self.tab_drop_position().map(DropTarget::TabBar)
            .or_else(|| self.pane_drop_target(Some(source)));
        if let Some(MouseDrag::Pane(pd)) = &mut self.drag {
            pd.started = true;
            pd.target = target;
        }
        self.redraw();
    }

    fn finish_pane_drag(&mut self, pd: PaneDrag) {
        if !pd.started {
            return;
        }
        let destination = match pd.target {
            Some(DropTarget::TabBar(position)) => tabs::detach_pane(&mut self.tabs, pd.pane, position),
            Some(DropTarget::Dock { pane, side }) => {
                let (dir, before) = side.split();
                tabs::move_pane(&mut self.tabs, pd.pane, pane, dir, before)
            }
            None => None,
        };
        if let Some(index) = destination {
            self.active_tab = index;
            self.refresh();
        } else {
            self.redraw();
        }
    }

    fn detach_focused_pane(&mut self) {
        let Some(tab) = self.tabs.get(self.active_tab) else { return };
        let source = tab.focused;
        if let Some(index) = tabs::detach_pane(&mut self.tabs, source, self.active_tab + 1) {
            self.cancel_pointer_gestures();
            self.active_tab = index;
            self.refresh();
        }
    }

    fn tab_drop_position(&self) -> Option<usize> {
        let (x, y) = self.cursor_pos;
        if !self.layout.tab_bar.is_some_and(|bar| bar.contains(x, y)) {
            return None;
        }
        Some(self.tab_hits.iter().position(|r| x < (r.x + r.w / 2) as f64)
            .unwrap_or(self.tab_hits.len()))
    }

    fn pane_drop_target(&self, exclude: Option<PaneId>) -> Option<DropTarget> {
        let (x, y) = self.cursor_pos;
        let (id, rect) = self.pane_hits.iter()
            .find(|(id, r)| Some(*id) != exclude && r.contains(x, y))?;
        let nx = (x - rect.x as f64) / rect.w.max(1) as f64;
        let ny = (y - rect.y as f64) / rect.h.max(1) as f64;
        let side = [
            (nx, Side::Left),
            (1.0 - nx, Side::Right),
            (ny, Side::Top),
            (1.0 - ny, Side::Bottom),
        ].into_iter().min_by(|a, b| a.0.total_cmp(&b.0)).map(|(_, side)| side)?;
        Some(DropTarget::Dock { pane: *id, side })
    }

    /// カーソル位置からタブのドロップ先を決める。
    fn compute_drop_target(&self, dragging: usize) -> Option<DropTarget> {
        // タブバー内: 並べ替え。挿入位置は各タブの中心との比較で決める
        if let Some(pos) = self.tab_drop_position() {
            return Some(DropTarget::TabBar(pos));
        }

        // ペイン上: 上下左右のうち最も近い辺へドッキング。
        // アクティブタブ自身は自分の中にドックできない
        if dragging != self.active_tab {
            return self.pane_drop_target(None);
        }
        None
    }

    /// タブドラッグの終了 (マウスリリース)。
    fn finish_tab_drag(&mut self, td: TabDrag) {
        if !td.started {
            // 動かさず離した = ただのクリック
            self.select_tab(td.tab_index);
            return;
        }
        match td.target {
            Some(DropTarget::TabBar(pos)) => self.reorder_tab(td.tab_index, pos),
            Some(DropTarget::Dock { pane, side }) => self.dock_tab(td.tab_index, pane, side),
            None => {}
        }
        self.redraw();
    }

    /// タブを並べ替える。`to` は元の並びでの挿入位置。
    fn reorder_tab(&mut self, from: usize, to: usize) {
        if from >= self.tabs.len() {
            return;
        }
        let tab = self.tabs.remove(from);
        // 取り除いた後の並びでの挿入位置に補正
        let to = if to > from { to - 1 } else { to }.min(self.tabs.len());
        self.tabs.insert(to, tab);

        let a = self.active_tab;
        self.active_tab = if a == from {
            to
        } else {
            let shifted = if from < a { a - 1 } else { a };
            if to <= shifted { shifted + 1 } else { shifted }
        };
        self.refresh();
    }

    /// タブを (中のペイン構成ごと) アクティブタブのペインへドッキングする。
    fn dock_tab(&mut self, from: usize, target_pane: PaneId, side: Side) {
        if from == self.active_tab || from >= self.tabs.len() {
            return;
        }
        if !self.tabs[self.active_tab].root.contains(target_pane) {
            return;
        }
        let dragged = self.tabs.remove(from);
        if from < self.active_tab {
            self.active_tab -= 1;
        }
        let focused = dragged.focused;
        let (dir, before) = side.split();
        let mut carrier = Some(dragged.root);
        let tab = &mut self.tabs[self.active_tab];
        if tab.root.dock_subtree(target_pane, dir, before, &mut carrier) {
            // フォーカスは動かしてきたペインへ
            tab.focused = focused;
        }
        self.refresh();
    }
}

impl ApplicationHandler<UserEvent> for App {
    fn resumed(&mut self, event_loop: &ActiveEventLoop) {
        if self.window.is_some() {
            return;
        }

        let attrs = Window::default_attributes()
            .with_title(DEFAULT_TITLE)
            .with_decorations(false)
            .with_inner_size(LogicalSize::new(700.0, 460.0))
            .with_min_inner_size(LogicalSize::new(320.0, 200.0));
        let window = Rc::new(event_loop.create_window(attrs).expect("ウィンドウ作成失敗"));

        let mut renderer = Renderer::new(window.scale_factor());
        renderer.bitmap_text = config::load_bitmap_text();
        self.renderer = Some(renderer);

        let context = softbuffer::Context::new(window.clone()).expect("softbuffer context作成失敗");
        let surface =
            softbuffer::Surface::new(&context, window.clone()).expect("softbuffer surface作成失敗");
        self._context = Some(context);
        self.surface = Some(surface);
        self.window = Some(window);

        let pane = self.new_pane().expect("初期ペイン作成失敗");
        self.tabs.push(Tab::new(pane));
        self.active_tab = 0;
        self.refresh();
    }

    fn user_event(&mut self, event_loop: &ActiveEventLoop, ue: UserEvent) {
        let UserEvent { pane, event } = ue;
        match event {
            TermEvent::Bell => {
                let until = Instant::now() + Duration::from_millis(140);
                self.bells.retain(|(id, _)| *id != pane);
                self.bells.push((pane, until));
                self.redraw();
            }
            TermEvent::Wakeup => {
                if self.tabs.get(self.active_tab).is_some_and(|t| t.root.contains(pane)) {
                    self.redraw();
                }
            }
            // タイトルは常にシェルの cwd を表示するため、OSC のタイトル変更は無視する
            TermEvent::Title(_) | TermEvent::ResetTitle => {}
            TermEvent::PtyWrite(text) => {
                for tab in &self.tabs {
                    if let Some(p) = tab.root.find_pane(pane) {
                        p.terminal.write(text.into_bytes());
                        break;
                    }
                }
            }
            TermEvent::TextAreaSizeRequest(format) => {
                let ws = self
                    .pane_hits
                    .iter()
                    .find(|(id, _)| *id == pane)
                    .and_then(|(_, rect)| self.ws_for_rect(*rect));
                if let Some(ws) = ws {
                    for tab in &self.tabs {
                        if let Some(p) = tab.root.find_pane(pane) {
                            p.terminal.write(format(ws).into_bytes());
                            break;
                        }
                    }
                }
            }
            TermEvent::Exit | TermEvent::ChildExit(_) => {
                // シェルが終了したペインを閉じる
                self.close_pane(pane, event_loop);
            }
            _ => {}
        }
    }

    fn window_event(
        &mut self,
        event_loop: &ActiveEventLoop,
        window_id: WindowId,
        event: WindowEvent,
    ) {
        // 設定ウィンドウ宛のイベントは専用ハンドラへ
        if self.settings.as_ref().is_some_and(|st| st.window.id() == window_id) {
            self.handle_settings_event(event_loop, event);
            return;
        }
        if !self.window.as_ref().is_some_and(|window| window.id() == window_id) { return; }
        match event {
            WindowEvent::CloseRequested => {
                self.tabs.clear();
                event_loop.exit();
            }
            WindowEvent::Resized(_) => self.refresh(),
            WindowEvent::ScaleFactorChanged { scale_factor, .. } => {
                let mut renderer = Renderer::new(scale_factor);
                renderer.bitmap_text = config::load_bitmap_text();
                self.renderer = Some(renderer);
                if let Some(window) = &self.window { self.shade.update_scale(window, scale_factor); }
                self.refresh();
            }
            WindowEvent::Focused(focused) => {
                self.focused = focused;
                if !focused {
                    self.drag = None;
                    self.scrollbar_drag = None;
                    self.scroll_repeat = None;
                    self.modifiers = ModifiersState::default();
                }
                self.redraw();
            }
            WindowEvent::ModifiersChanged(m) => self.modifiers = m.state(),
            WindowEvent::CursorMoved { position, .. } => {
                self.cursor_pos = (position.x, position.y);
                self.handle_scrollbar_drag();
                match &self.drag {
                    Some(MouseDrag::Splitter(_)) => self.handle_splitter_drag(),
                    Some(MouseDrag::Tab(_)) => self.handle_tab_drag(),
                    Some(MouseDrag::Pane(_)) => self.handle_pane_drag(),
                    Some(MouseDrag::Selection(id)) => self.update_selection(*id),
                    None => {}
                }
            }
            WindowEvent::MouseInput {
                state: ElementState::Pressed, button: MouseButton::Left, ..
            } => {
                if !self.handle_scrollbar_press() {
                    self.handle_mouse_press(event_loop);
                }
            }
            WindowEvent::MouseInput {
                state: ElementState::Released, button: MouseButton::Left, ..
            } => {
                self.scrollbar_drag = None;
                self.scroll_repeat = None;
                if let Some(drag) = self.drag.take() {
                    match drag {
                        MouseDrag::Tab(td) => self.finish_tab_drag(td),
                        MouseDrag::Pane(pd) => self.finish_pane_drag(pd),
                        _ => {},
                    }
                }
            }
            WindowEvent::MouseWheel { delta, .. } => {
                let lines = match delta {
                    MouseScrollDelta::LineDelta(_, y) => (y * 3.0) as i32,
                    MouseScrollDelta::PixelDelta(pos) => {
                        let ch = self.renderer.as_ref().map(|r| r.cell_h).unwrap_or(16) as f64;
                        (pos.y / ch).round() as i32
                    }
                };
                if lines != 0 {
                    // カーソル直下のペインへ、なければフォーカス中ペインへ
                    let (x, y) = self.cursor_pos;
                    let target = self
                        .pane_hits
                        .iter()
                        .find(|(_, r)| r.contains(x, y))
                        .map(|(id, _)| *id)
                        .or_else(|| self.tabs.get(self.active_tab).map(|t| t.focused));
                    if let (Some(id), Some(tab)) = (target, self.tabs.get(self.active_tab)) {
                        if let Some(p) = tab.root.find_pane(id) {
                            p.terminal.term.lock().scroll_display(Scroll::Delta(lines));
                        }
                    }
                    self.redraw();
                }
            }
            WindowEvent::KeyboardInput { event, .. } => self.handle_key(event, event_loop),
            WindowEvent::Ime(event) => self.handle_ime(event),
            WindowEvent::RedrawRequested => self.render(),
            _ => {}
        }
        self.sync_ime_target();
    }

    fn about_to_wait(&mut self, event_loop: &ActiveEventLoop) {
        let now = Instant::now();
        let had_bells = self.bells.len();
        self.bells.retain(|(_, until)| *until > now);
        if self.bells.len() != had_bells { self.redraw(); }
        if let Some(mut repeat) = self.scroll_repeat {
            if now >= repeat.next {
                if let Some(bar) = self.scrollbar_for(repeat.pane) {
                    if bar.hit_test(self.cursor_pos.0, self.cursor_pos.1) == Some(repeat.part) {
                        self.scroll_pane(repeat.pane, repeat.amount);
                    }
                    repeat.next = now + Duration::from_millis(65);
                    self.scroll_repeat = Some(repeat);
                } else {
                    self.scroll_repeat = None;
                }
            }
        }
        let deadline = self.bells.iter().map(|(_, until)| *until)
            .chain(self.scroll_repeat.map(|repeat| repeat.next)).min();
        event_loop.set_control_flow(match deadline {
            Some(until) => ControlFlow::WaitUntil(until),
            None => ControlFlow::Wait,
        });
    }
}

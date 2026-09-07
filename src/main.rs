//! 0s9terminal — Mac OS 9 (Platinum) 風ターミナルエミュレータ。
//! ウィンドウ装飾は OS 標準を使わず、タイトルバーごと自前で描画する。

mod clipboard;
mod config;
mod renderer;
mod tabs;
mod term;
mod theme;

use std::num::NonZeroU32;
use std::rc::Rc;
use std::time::{Duration, Instant};
use alacritty_terminal::index::{Column, Point, Side as CellSide};
use alacritty_terminal::selection::{Selection, SelectionType};

use alacritty_terminal::event::{Event as TermEvent, WindowSize};
use alacritty_terminal::grid::Scroll;
use alacritty_terminal::term::TermMode;
use winit::application::ApplicationHandler;
use winit::dpi::LogicalSize;
use winit::event::{ElementState, KeyEvent, MouseButton, MouseScrollDelta, WindowEvent};
use winit::event_loop::{ActiveEventLoop, ControlFlow, EventLoop, EventLoopProxy};
use winit::keyboard::{Key, ModifiersState, NamedKey};
use winit::window::{ResizeDirection, Window, WindowId};

use renderer::{tab_layout, Layout, Rect, Renderer};
use tabs::{Pane, SplitDir, SplitterHit, Tab};
use term::{PaneId, Terminal, UserEvent};

const DEFAULT_TITLE: &str = "0s9 Terminal";

fn main() {
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
    Selection(PaneId),
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
        self.layout = Layout::compute(size.width, size.height, renderer.scale, self.tabs.len() > 1);

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
            }
        }
        for sh in &self.splitter_hits {
            renderer.draw_splitter(&mut buffer, size.width, size.height, sh);
        }
        renderer.draw_grow_box(&mut buffer, size.width, size.height, self.layout.grow_box);

        // タブドラッグ中の視覚フィードバック (最前面)
        if let Some(MouseDrag::Tab(td)) = &self.drag {
            if td.started {
                match td.target {
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
                // ドラッグ中のタブのゴースト枠 (カーソル追従)
                if let Some(r) = self.tab_hits.get(td.tab_index) {
                    let (cx, cy) = (self.cursor_pos.0 as i32, self.cursor_pos.1 as i32);
                    let ghost = Rect { x: cx - r.w / 2, y: cy - r.h / 2, w: r.w, h: r.h };
                    renderer.draw_drop_outline(&mut buffer, size.width, size.height, ghost);
                }
            }
        }

        let _ = buffer.present();
    }

    // ─── タブ・ペイン操作 ───

    fn new_tab(&mut self) {
        if let Some(pane) = self.new_pane() {
            self.tabs.push(Tab::new(pane));
            self.active_tab = self.tabs.len() - 1;
            self.refresh();
        }
    }

    fn select_tab(&mut self, index: usize) {
        if index < self.tabs.len() && index != self.active_tab {
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
        let attrs = Window::default_attributes()
            .with_title("設定")
            .with_decorations(false)
            .with_resizable(false)
            .with_inner_size(LogicalSize::new(300.0, 200.0));
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
                let Some(st) = &self.settings else { return };
                let (x, y) = st.cursor;
                if st.layout.close_box.contains(x, y) {
                    self.settings = None;
                } else if st.layout.collapse_box.contains(x, y) {
                    st.window.set_minimized(true);
                } else if let Some(i) = st.row_hits.iter().position(|r| r.contains(x, y)) {
                    self.apply_scheme(i);
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

        if mods.super_key() {
            self.handle_command_key(&event, event_loop);
            return;
        }

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
            // 入力時はスクロールを最下部へ戻す
            {
                let mut term = terminal.term.lock();
                term.selection = None;
                term.scroll_display(Scroll::Bottom);
            }
            terminal.write(bytes);
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
            window.set_maximized(!window.is_maximized());
            return;
        }
        if self.layout.collapse_box.contains(x, y) {
            // OS9のウィンドウシェードの代わりに最小化
            window.set_minimized(true);
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

    fn paste_clipboard(&self) {
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

    /// カーソル位置からタブのドロップ先を決める。
    fn compute_drop_target(&self, dragging: usize) -> Option<DropTarget> {
        let (x, y) = self.cursor_pos;

        // タブバー内: 並べ替え。挿入位置は各タブの中心との比較で決める
        if self.layout.tab_bar.is_some_and(|bar| bar.contains(x, y)) {
            let mut pos = self.tab_hits.len();
            for (i, r) in self.tab_hits.iter().enumerate() {
                if x < (r.x + r.w / 2) as f64 {
                    pos = i;
                    break;
                }
            }
            return Some(DropTarget::TabBar(pos));
        }

        // ペイン上: 上下左右のうち最も近い辺へドッキング。
        // アクティブタブ自身は自分の中にドックできない
        if dragging != self.active_tab {
            if let Some((id, rect)) = self.pane_hits.iter().find(|(_, r)| r.contains(x, y)) {
                let nx = (x - rect.x as f64) / rect.w.max(1) as f64;
                let ny = (y - rect.y as f64) / rect.h.max(1) as f64;
                let side = [
                    (nx, Side::Left),
                    (1.0 - nx, Side::Right),
                    (ny, Side::Top),
                    (1.0 - ny, Side::Bottom),
                ]
                .into_iter()
                .min_by(|a, b| a.0.total_cmp(&b.0))
                .map(|(_, s)| s)?;
                return Some(DropTarget::Dock { pane: *id, side });
            }
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

        self.renderer = Some(Renderer::new(window.scale_factor()));

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
        match event {
            WindowEvent::CloseRequested => {
                self.tabs.clear();
                event_loop.exit();
            }
            WindowEvent::Resized(_) => self.refresh(),
            WindowEvent::ScaleFactorChanged { scale_factor, .. } => {
                self.renderer = Some(Renderer::new(scale_factor));
                self.refresh();
            }
            WindowEvent::Focused(focused) => {
                self.focused = focused;
                if !focused { self.drag = None; }
                self.redraw();
            }
            WindowEvent::ModifiersChanged(m) => self.modifiers = m.state(),
            WindowEvent::CursorMoved { position, .. } => {
                self.cursor_pos = (position.x, position.y);
                match &self.drag {
                    Some(MouseDrag::Splitter(_)) => self.handle_splitter_drag(),
                    Some(MouseDrag::Tab(_)) => self.handle_tab_drag(),
                    Some(MouseDrag::Selection(id)) => self.update_selection(*id),
                    None => {}
                }
            }
            WindowEvent::MouseInput {
                state: ElementState::Pressed, button: MouseButton::Left, ..
            } => {
                self.handle_mouse_press(event_loop);
            }
            WindowEvent::MouseInput {
                state: ElementState::Released, button: MouseButton::Left, ..
            } => {
                if let Some(drag) = self.drag.take() {
                    if let MouseDrag::Tab(td) = drag {
                        self.finish_tab_drag(td);
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
            WindowEvent::RedrawRequested => self.render(),
            _ => {}
        }
    }
}

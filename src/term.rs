//! PTY とターミナルエミュレーション層 (alacritty_terminal) の起動・接続。

use std::borrow::Cow;
use std::sync::Arc;

use alacritty_terminal::event::{Event, EventListener, WindowSize};
use alacritty_terminal::event_loop::{EventLoop as PtyEventLoop, EventLoopSender, Msg};
use alacritty_terminal::grid::Dimensions;
use alacritty_terminal::sync::FairMutex;
use alacritty_terminal::term::{Config, Term};
use alacritty_terminal::tty;
use winit::event_loop::EventLoopProxy;

/// ペインの識別子。イベントの発生元 PTY を特定するために使う。
pub type PaneId = u64;

/// winit のイベントループへ流すユーザーイベント。どのペインの端末からの
/// 通知かを `pane` で判別する。
pub struct UserEvent {
    pub pane: PaneId,
    pub event: Event,
}

/// alacritty_terminal からのイベントを winit のイベントループへ転送する。
#[derive(Clone)]
pub struct EventProxy {
    proxy: EventLoopProxy<UserEvent>,
    pane: PaneId,
}

impl EventListener for EventProxy {
    fn send_event(&self, event: Event) {
        let _ = self.proxy.send_event(UserEvent { pane: self.pane, event });
    }
}

/// グリッド寸法 (列数 × 行数)。
#[derive(Copy, Clone)]
pub struct GridSize {
    pub columns: usize,
    pub screen_lines: usize,
}

impl Dimensions for GridSize {
    fn total_lines(&self) -> usize {
        self.screen_lines
    }

    fn screen_lines(&self) -> usize {
        self.screen_lines
    }

    fn columns(&self) -> usize {
        self.columns
    }
}

/// シェル・PTY・端末状態をまとめたハンドル。
pub struct Terminal {
    pub term: Arc<FairMutex<Term<EventProxy>>>,
    sender: EventLoopSender,
}

impl Terminal {
    pub fn new(
        proxy: EventLoopProxy<UserEvent>,
        pane: PaneId,
        window_size: WindowSize,
    ) -> std::io::Result<Self> {
        let grid = GridSize {
            columns: window_size.num_cols as usize,
            screen_lines: window_size.num_lines as usize,
        };

        let event_proxy = EventProxy { proxy, pane };
        let term = Term::new(Config::default(), &grid, event_proxy.clone());
        let term = Arc::new(FairMutex::new(term));

        let mut options = tty::Options::default();
        options.env.insert("TERM".into(), "xterm-256color".into());
        options.env.insert("COLORTERM".into(), "truecolor".into());
        let pty = tty::new(&options, window_size, 0)?;

        let event_loop = PtyEventLoop::new(term.clone(), event_proxy, pty, false, false)?;
        let sender = event_loop.channel();
        event_loop.spawn();

        Ok(Self { term, sender })
    }

    /// バイト列をシェル (PTY) へ送る。
    pub fn write(&self, bytes: impl Into<Cow<'static, [u8]>>) {
        let _ = self.sender.send(Msg::Input(bytes.into()));
    }

    /// グリッドと PTY の両方をリサイズする。
    pub fn resize(&self, ws: WindowSize) {
        self.term.lock().resize(GridSize {
            columns: ws.num_cols as usize,
            screen_lines: ws.num_lines as usize,
        });
        let _ = self.sender.send(Msg::Resize(ws));
    }

    pub fn shutdown(&self) {
        let _ = self.sender.send(Msg::Shutdown);
    }
}

impl Drop for Terminal {
    fn drop(&mut self) {
        // ペインやタブを閉じたとき、PTY の読み書きスレッドを確実に止める
        self.shutdown();
    }
}

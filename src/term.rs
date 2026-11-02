//! PTY とターミナルエミュレーション層 (alacritty_terminal) の起動・接続。

use std::borrow::Cow;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
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

/// 全ウィンドウで一意な ID。閉じたペインから遅れて届くイベントも区別する。
pub fn next_pane_id() -> PaneId {
    static NEXT_PANE_ID: AtomicU64 = AtomicU64::new(1);
    NEXT_PANE_ID.fetch_add(1, Ordering::Relaxed)
}

/// winit のイベントループへ流す端末通知と、新規ウィンドウの要求。
pub enum UserEvent {
    Terminal { pane: PaneId, event: Event },
    NewWindow(PathBuf),
}

/// alacritty_terminal からのイベントを winit のイベントループへ転送する。
#[derive(Clone)]
pub struct EventProxy {
    proxy: EventLoopProxy<UserEvent>,
    pane: PaneId,
}

impl EventListener for EventProxy {
    fn send_event(&self, event: Event) {
        let _ = self.proxy.send_event(UserEvent::Terminal { pane: self.pane, event });
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
    /// シェル (PTY の子プロセス) の PID。cwd の取得に使う。
    child_pid: i32,
}

impl Terminal {
    pub fn new(
        proxy: EventLoopProxy<UserEvent>,
        pane: PaneId,
        window_size: WindowSize,
        working_directory: Option<&Path>,
    ) -> std::io::Result<Self> {
        let grid = GridSize {
            columns: window_size.num_cols as usize,
            screen_lines: window_size.num_lines as usize,
        };

        let event_proxy = EventProxy { proxy, pane };
        let term = Term::new(Config::default(), &grid, event_proxy.clone());
        let term = Arc::new(FairMutex::new(term));

        let mut options = tty::Options {
            // 子プロセスだけの cwd を指定し、他のウィンドウへ影響させない。
            working_directory: working_directory.map(Path::to_path_buf),
            ..tty::Options::default()
        };
        options.env.insert("TERM_PROGRAM".into(), "os9terminal".into());
        if let Ok(executable) = std::env::current_exe() {
            if let Some(contents) = executable.parent().and_then(|dir| dir.parent()) {
                let resources = contents.join("Resources");
                if resources.join("integration.zsh").is_file() {
                    options.env.insert("OS9_RESOURCES".into(), resources.to_string_lossy().into_owned());
                }
            }
        }
        options.env.insert("TERM".into(), "xterm-256color".into());
        options.env.insert("COLORTERM".into(), "truecolor".into());
        let pty = tty::new(&options, window_size, 0)?;
        let child_pid = pty.child().id() as i32;

        let event_loop = PtyEventLoop::new(term.clone(), event_proxy, pty, false, false)?;
        let sender = event_loop.channel();
        event_loop.spawn();

        Ok(Self { term, sender, child_pid })
    }

    /// シェルの現在の作業ディレクトリを取得する (macOS の proc_pidinfo)。
    ///
    /// alacritty_terminal は macOS ではシェルを /usr/bin/login (root で動く
    /// setuid プロセス) 経由で起動するため、child_pid の cwd は権限エラーで
    /// 読めない。読めるようになるまで子プロセスを辿る (login → zsh)。
    pub fn cwd(&self) -> Option<PathBuf> {
        let mut pid = self.child_pid;
        for _ in 0..4 {
            if let Some(path) = proc_cwd(pid) {
                return Some(path);
            }
            pid = *child_pids(pid).first()?;
        }
        None
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
}

impl Drop for Terminal {
    fn drop(&mut self) {
        // ペインやウィンドウを閉じたとき、PTY の読み書きスレッドを確実に止める
        let _ = self.sender.send(Msg::Shutdown);
    }
}

// ─── macOS: プロセスの cwd 取得 ───
//
// libproc.h の proc_pidinfo(PROC_PIDVNODEPATHINFO) を直接呼ぶ。
// 構造体レイアウトは XNU の bsd/sys/proc_info.h に一致させてある:
//   struct vnode_info_path = struct vnode_info (152 bytes) + char[MAXPATHLEN]
//   struct proc_vnodepathinfo = pvi_cdir + pvi_rdir

const PROC_PIDVNODEPATHINFO: std::os::raw::c_int = 9;
const MAXPATHLEN: usize = 1024;

#[repr(C)]
struct VnodeInfoPath {
    vnode_info: [u8; 152],
    path: [u8; MAXPATHLEN],
}

#[repr(C)]
struct ProcVnodePathInfo {
    cdir: VnodeInfoPath,
    rdir: VnodeInfoPath,
}

const PROC_PPID_ONLY: u32 = 6;

unsafe extern "C" {
    fn proc_pidinfo(
        pid: std::os::raw::c_int,
        flavor: std::os::raw::c_int,
        arg: u64,
        buffer: *mut std::os::raw::c_void,
        buffersize: std::os::raw::c_int,
    ) -> std::os::raw::c_int;

    fn proc_listpids(
        proc_type: u32,
        typeinfo: u32,
        buffer: *mut std::os::raw::c_void,
        buffersize: std::os::raw::c_int,
    ) -> std::os::raw::c_int;
}

/// `ppid` を親に持つプロセスの PID 一覧を返す。
fn child_pids(ppid: i32) -> Vec<i32> {
    let mut buf = [0i32; 64];
    let bytes = (buf.len() * std::mem::size_of::<i32>()) as std::os::raw::c_int;
    let ret = unsafe {
        proc_listpids(
            PROC_PPID_ONLY,
            ppid as u32,
            buf.as_mut_ptr() as *mut std::os::raw::c_void,
            bytes,
        )
    };
    if ret <= 0 {
        return Vec::new();
    }
    let n = (ret as usize / std::mem::size_of::<i32>()).min(buf.len());
    buf[..n].iter().copied().filter(|&pid| pid > 0).collect()
}

fn proc_cwd(pid: i32) -> Option<PathBuf> {
    let mut info = ProcVnodePathInfo {
        cdir: VnodeInfoPath { vnode_info: [0; 152], path: [0; MAXPATHLEN] },
        rdir: VnodeInfoPath { vnode_info: [0; 152], path: [0; MAXPATHLEN] },
    };
    let size = std::mem::size_of::<ProcVnodePathInfo>() as std::os::raw::c_int;
    let ret = unsafe {
        proc_pidinfo(
            pid,
            PROC_PIDVNODEPATHINFO,
            0,
            &mut info as *mut _ as *mut std::os::raw::c_void,
            size,
        )
    };
    if ret != size {
        return None;
    }
    let path = &info.cdir.path;
    let len = path.iter().position(|&b| b == 0).unwrap_or(path.len());
    if len == 0 {
        return None;
    }
    Some(PathBuf::from(String::from_utf8_lossy(&path[..len]).into_owned()))
}

#[cfg(test)]
mod tests {
    use super::{next_pane_id, proc_cwd};

    #[test]
    fn pane_ids_are_unique_across_concurrent_allocations() {
        let workers: Vec<_> = (0..8)
            .map(|_| std::thread::spawn(|| (0..64).map(|_| next_pane_id()).collect::<Vec<_>>()))
            .collect();
        let ids: Vec<_> = workers.into_iter().flat_map(|worker| worker.join().unwrap()).collect();
        let distinct: std::collections::HashSet<_> = ids.iter().copied().collect();
        assert_eq!(distinct.len(), ids.len());
        assert!(!distinct.contains(&0));
    }

    #[test]
    fn proc_cwd_returns_own_cwd() {
        // 自プロセスの cwd が env::current_dir と一致すれば
        // FFI の構造体レイアウトが正しいことの裏付けになる
        let expected = std::env::current_dir().unwrap();
        let actual = proc_cwd(std::process::id() as i32).expect("proc_cwd が失敗");
        assert_eq!(actual, expected);
    }

    #[test]
    fn proc_cwd_works_for_child_process() {
        // 実際の用途 (シェル = 子プロセス) と同じ形で検証する
        let mut child = std::process::Command::new("/bin/sleep")
            .arg("5")
            .current_dir("/tmp")
            .spawn()
            .unwrap();
        std::thread::sleep(std::time::Duration::from_millis(200));
        let actual = proc_cwd(child.id() as i32);
        let _ = child.kill();
        let _ = child.wait();
        // /tmp は /private/tmp へのシンボリックリンク
        assert_eq!(actual.expect("proc_cwd が失敗"), std::path::PathBuf::from("/private/tmp"));
    }
}

//! o9term — one macOS application, with independent terminal windows.

mod clipboard;
mod config;
mod font;
mod ime;
#[cfg(target_os = "macos")]
mod instance;
mod renderer;
mod panes;
mod term;
mod terminal_window;
mod theme;
mod window_shade;

use std::ffi::OsString;
use std::path::PathBuf;
use std::time::Instant;
use winit::application::ApplicationHandler;
use winit::dpi::PhysicalPosition;
use winit::event::{ElementState, WindowEvent};
use winit::event_loop::{ActiveEventLoop, ControlFlow, EventLoop, EventLoopProxy};
use winit::keyboard::{Key, ModifiersState};
use winit::window::WindowId;

use term::UserEvent;
use terminal_window::TerminalWindow;

#[derive(Debug, PartialEq)]
enum Launch {
    Version,
    Directory(PathBuf),
}

fn parse_args(args: impl IntoIterator<Item = OsString>, default: PathBuf) -> Result<Launch, String> {
    let mut directory = default;
    let mut args = args.into_iter();
    while let Some(arg) = args.next() {
        if arg == "--version" { return Ok(Launch::Version); }
        if arg == "--working-directory" {
            directory = args.next().map(PathBuf::from)
                .ok_or("--working-directory requires a directory")?;
        } else {
            return Err("Usage: os9terminal [--working-directory PATH] [--version]".into());
        }
    }
    let directory = directory.canonicalize().map_err(|error| format!("Cannot open working directory: {error}"))?;
    if !directory.is_dir() { return Err("Working directory must be a directory".into()); }
    Ok(Launch::Directory(directory))
}

fn main() {
    let default = std::env::current_dir().unwrap_or_else(|_| {
        std::env::var_os("HOME").map(PathBuf::from).unwrap_or_else(|| PathBuf::from("/"))
    });
    let directory = match parse_args(std::env::args_os().skip(1), default) {
        Ok(Launch::Version) => { println!("o9 {}", env!("CARGO_PKG_VERSION")); return; }
        Ok(Launch::Directory(directory)) => directory,
        Err(error) => { eprintln!("{error}"); std::process::exit(2); }
    };
    let event_loop = EventLoop::<UserEvent>::with_user_event().build().expect("イベントループ作成失敗");
    event_loop.set_control_flow(ControlFlow::Wait);
    let proxy = event_loop.create_proxy();
    #[cfg(target_os = "macos")]
    let _instance = instance::install(proxy.clone(), directory.clone());
    #[cfg(not(target_os = "macos"))]
    let _ = proxy.send_event(UserEvent::NewWindow(directory));
    let mut app = App { proxy, windows: Vec::new(), active_window: None };
    event_loop.run_app(&mut app).expect("イベントループ実行失敗");
}

/// Window events are routed by WindowId; PTY events use globally unique pane IDs.
struct App {
    proxy: EventLoopProxy<UserEvent>,
    windows: Vec<TerminalWindow>,
    active_window: Option<WindowId>,
}

#[derive(Debug, PartialEq)]
enum AppCommand { NewWindow, Quit }

fn app_command(event: &WindowEvent, modifiers: ModifiersState) -> Option<AppCommand> {
    let WindowEvent::KeyboardInput { event, is_synthetic: false, .. } = event else { return None };
    if event.state != ElementState::Pressed || event.repeat || !modifiers.super_key()
        || modifiers.alt_key() || modifiers.control_key() || modifiers.shift_key() { return None; }
    let Key::Character(key) = &event.logical_key else { return None };
    match key.to_lowercase().as_str() {
        "n" => Some(AppCommand::NewWindow),
        "q" => Some(AppCommand::Quit),
        _ => None,
    }
}

impl App {
    fn active_index(&self) -> Option<usize> {
        self.active_window.and_then(|id| self.windows.iter().position(|window| window.owns_window(id)))
    }

    fn open_window(&mut self, event_loop: &ActiveEventLoop, directory: PathBuf, source: Option<usize>) {
        // Validate on every request: an existing shell can outlive its deleted directory.
        if !directory.is_dir() {
            eprintln!("Cannot open window: working directory is unavailable");
            if self.windows.is_empty() { event_loop.exit(); }
            return;
        }
        let position = source.and_then(|index| self.windows.get(index))
            .and_then(TerminalWindow::primary_window).and_then(|window| {
                let position = window.outer_position().ok()?;
                let offset = (24.0 * window.scale_factor()).round() as i32;
                let mut x = position.x + offset;
                let mut y = position.y + offset;
                if let Some(monitor) = window.current_monitor() {
                    let origin = monitor.position();
                    let monitor_size = monitor.size();
                    let size = window.outer_size();
                    if x + size.width as i32 > origin.x + monitor_size.width as i32 { x = origin.x + offset; }
                    if y + size.height as i32 > origin.y + monitor_size.height as i32 { y = origin.y + offset * 2; }
                }
                Some(PhysicalPosition::new(x, y))
            });
        let mut window = TerminalWindow::new(self.proxy.clone(), directory);
        match window.open(event_loop, position) {
            Ok(()) => {
                self.active_window = window.primary_window().map(|window| window.id());
                self.windows.push(window);
            }
            Err(error) => {
                eprintln!("Cannot create terminal window: {error}");
                if self.windows.is_empty() { event_loop.exit(); }
            }
        }
    }

    fn prune_closed_windows(&mut self, event_loop: &ActiveEventLoop) {
        self.windows.retain(|window| !window.is_closed());
        if self.windows.is_empty() {
            self.active_window = None;
            event_loop.exit();
        } else if self.active_index().is_none() {
            self.active_window = self.windows.last().and_then(TerminalWindow::primary_window).map(|window| window.id());
        }
    }
}

impl ApplicationHandler<UserEvent> for App {
    fn resumed(&mut self, _event_loop: &ActiveEventLoop) {
        // Native launch/file-open events request the initial window. Creating one
        // here as well would duplicate the first window when launched through o9.
    }

    fn user_event(&mut self, event_loop: &ActiveEventLoop, event: UserEvent) {
        match event {
            UserEvent::NewWindow(directory) => self.open_window(event_loop, directory, self.active_index()),
            UserEvent::Terminal { pane, event } => {
                if let Some(window) = self.windows.iter_mut().find(|window| window.contains_pane(pane)) {
                    window.terminal_event(pane, event);
                    self.prune_closed_windows(event_loop);
                }
            }
        }
    }

    fn window_event(&mut self, event_loop: &ActiveEventLoop, id: WindowId, event: WindowEvent) {
        let Some(index) = self.windows.iter().position(|window| window.owns_window(id)) else { return };
        if matches!(event, WindowEvent::Focused(true)) {
            self.active_window = self.windows[index].primary_window().map(|window| window.id());
        }
        match app_command(&event, self.windows[index].modifiers()) {
            Some(AppCommand::NewWindow) => {
                let directory = self.windows[index].working_directory();
                self.open_window(event_loop, directory, Some(index));
                return;
            }
            Some(AppCommand::Quit) => {
                self.windows.clear();
                event_loop.exit();
                return;
            }
            None => {},
        }
        let before = self.windows[index].preferences();
        self.windows[index].window_event(event_loop, id, event);
        let after = self.windows[index].preferences();
        if before != after {
            for window in &mut self.windows { window.apply_preferences(after); }
        }
        self.prune_closed_windows(event_loop);
    }

    fn about_to_wait(&mut self, event_loop: &ActiveEventLoop) {
        let now = Instant::now();
        let deadline = self.windows.iter_mut().filter_map(|window| window.update_timers(now)).min();
        event_loop.set_control_flow(match deadline {
            Some(until) => ControlFlow::WaitUntil(until),
            None => ControlFlow::Wait,
        });
    }

    fn exiting(&mut self, _event_loop: &ActiveEventLoop) {
        self.windows.clear();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn working_directory_does_not_change_process_cwd() {
        let original = std::env::current_dir().unwrap();
        let args = [OsString::from("--working-directory"), OsString::from("/tmp")];
        assert_eq!(parse_args(args, original.clone()), Ok(Launch::Directory(PathBuf::from("/private/tmp"))));
        assert_eq!(std::env::current_dir().unwrap(), original);
    }

    #[test]
    fn invalid_launch_arguments_are_rejected() {
        let default = std::env::current_dir().unwrap();
        assert!(parse_args([OsString::from("--working-directory")], default.clone()).is_err());
        assert!(parse_args([OsString::from("--unknown")], default.clone()).is_err());
        assert!(parse_args([OsString::from("--working-directory"), OsString::from("/dev/null")], default).is_err());
    }

    #[test]
    fn version_does_not_require_a_valid_directory() {
        assert_eq!(parse_args([OsString::from("--version")], PathBuf::from("/not/a/real/directory")), Ok(Launch::Version));
    }
}

# Development and distribution

Develop with `cargo run`; validate with `cargo test`.

## Window lifecycle

`src/main.rs` coordinates independent terminal windows in one event loop and
routes events by window or globally unique pane ID. `src/terminal_window.rs`
owns each window's single pane tree, input, rendering and settings.
`src/panes.rs` handles the tree's layout, splitting, movement and removal.
Cmd+N inherits the focused pane's working directory; each PTY receives its own
startup directory without changing the application process's working directory.

Cmd+W closes the current window and all of its panes. Cmd+Shift+W closes only
the focused pane. Closing a window, its final pane or its final shell leaves
the other windows running. Cmd+Q closes the entire app; closing all terminal
windows also exits.

## Pane controls

Cmd+Control+an arrow key splits the focused pane and places a new pane in that
direction. The new pane inherits the source pane's working directory and receives
focus. Cmd+D (right) and Cmd+Shift+D (down) remain available. Splitting is operated
through shortcuts; there are no split buttons in the window.

Cmd+Option+an arrow key moves focus between panes. Cmd-drag moves a pane to an
edge of another pane in the same window, preserving its PTY. Dragging a divider
resizes the adjacent panes. IME composition remains tied to the focused pane and
is cancelled when focus moves.

## Build a release package (maintainers only)

Requires macOS, Rust, Python 3.11+, Xcode Command Line Tools and Homebrew.

```sh
brew install bat zsh-syntax-highlighting
python3 scripts/package.py
```

The package under `dist/` contains the app, `o9` launcher and `install.command`.
It includes bat, its non-system shared libraries, zsh-syntax-highlighting and
license notices for those bundled tools. The builder's checkout/home paths
are remapped in the Rust binary.

End users extract the ZIP and run `install.command` once. This installs the app
in `~/Applications`, the launcher in `~/.local/bin`, and zsh integration.
No Rust, Python, Homebrew or network access is needed by the installer.
Open a new terminal afterwards and run `o9`.
The command opens a new window in the caller's working directory;
`o9 /path/to/project` can specify a directory.
The launcher uses `open -a` to send a folder-open event to the existing app.
`src/instance.rs` bridges native launch and folder-open events into window
requests, keeping all windows in one application process with one Dock icon.

The installer backs up shell configuration before first modification and keeps
the previous application as `o9term.app.previous` when updating.
Subsequent updates require moving that previous backup first.
For uninstallation, remove the app and launcher, then remove the integration
line in .zshrc. The installer also adds ~/.local/bin to .zprofile.

## App icon

The icon artwork is defined in `scripts/generate-icon.py`. Regenerate the
committed `assets/o9term.icns` and 1024-pixel `assets/o9term.png` preview on macOS:

```sh
python3 scripts/generate-icon.py
```

The generator uses Python's standard library and macOS `iconutil`. Packaging
requires the generated ICNS file and includes it in the app bundle before signing
so Finder, the Dock and the app switcher can display it.

## GitHub Releases

Distribution repository: https://github.com/aberyotaro/o9term

The local ZIP is a release candidate, not an automatically published release.
Before publishing, audit all bundled binary dependencies and license notices,
scan for private build paths, and test on each advertised supported macOS version.
Upload the validated ZIP and its SHA-256 checksum to a versioned GitHub Release.
The generated package targets the build machine's architecture and linked
system-library requirements; it is not a universal binary.

Current packages are ad-hoc signed only, not Developer ID signed or notarized.
Public distribution without first-launch Gatekeeper warnings requires a
Developer ID signing certificate and Apple notarization. Do not disable
Gatekeeper or strip quarantine as part of installation.

## Highlighting (development builds)

For `cargo run`, `python3 scripts/setup-highlighting.py` installs Homebrew tools
and adds the scoped zsh integration. Release users do not run this helper.
Input highlighting is provided by zsh-syntax-highlighting.
Use `bat file.rs` to view a code file, or `bat --paging=never file.rs` to avoid
paging. The ANSI theme follows the terminal palette; existing BAT_THEME wins.

## Configuration compatibility

Settings remain at `~/.config/os9terminal/config.toml` to preserve existing
installations after the project rename to o9term.

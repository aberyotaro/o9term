# Development and distribution

Develop with `cargo run`; validate with `cargo test`.

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

The installer backs up shell configuration before first modification and keeps
the previous application as `o9term.app.previous` when updating.
Subsequent updates require moving that previous backup first.
For uninstallation, remove the app and launcher, then remove the integration
line in .zshrc. The installer also adds ~/.local/bin to .zprofile.

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

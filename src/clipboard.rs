//! macOS clipboard access. Text is passed through pipes, never shell commands.
use std::io::{self, Write};
use std::process::{Command, Stdio};

pub fn set(text: &str) -> io::Result<()> {
    let mut child = Command::new("/usr/bin/pbcopy")
        .env("LC_CTYPE", "en_US.UTF-8")
        .stdin(Stdio::piped()).spawn()?;
    let result = child.stdin.take().unwrap().write_all(text.as_bytes());
    let status = child.wait()?;
    result?;
    if status.success() { Ok(()) } else { Err(io::Error::other("pbcopy failed")) }
}

pub fn get() -> io::Result<String> {
    let output = Command::new("/usr/bin/pbpaste")
        .env("LC_CTYPE", "en_US.UTF-8").output()?;
    if !output.status.success() { return Err(io::Error::other("pbpaste failed")); }
    String::from_utf8(output.stdout).map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error))
}

pub fn paste_bytes(text: &str, bracketed: bool) -> Vec<u8> {
    if bracketed {
        // Embedded escape characters must not terminate bracketed paste.
        format!("\x1b[200~{}\x1b[201~", text.replace('\x1b', "")).into_bytes()
    } else {
        text.replace("\r\n", "\n").replace('\n', "\r").into_bytes()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn multiline_paste_normalizes_line_endings() {
        assert_eq!(paste_bytes("a\r\nb\nc", false), b"a\rb\rc");
    }
    #[test]
    fn bracketed_paste_preserves_unicode_and_blocks_embedded_terminator() {
        assert_eq!(paste_bytes("日本語\n\x1b[201~", true),
            "\x1b[200~日本語\n[201~\x1b[201~".as_bytes());
    }
}

#[cfg(test)]
mod selection_tests {
    use alacritty_terminal::{
        Term,
        event::VoidListener,
        index::{Column, Line, Point, Side},
        selection::{Selection, SelectionType},
        term::Config,
        vte::ansi::Processor,
    };
    #[test]
    fn reverse_drag_copies_wide_text() {
        let size = crate::term::GridSize { columns: 20, screen_lines: 3 };
        let mut term = Term::new(Config::default(), &size, VoidListener);
        Processor::<alacritty_terminal::vte::ansi::StdSyncHandler>::new().advance(&mut term, "ab日本cd".as_bytes());
        let mut selection = Selection::new(SelectionType::Simple,
            Point::new(Line(0), Column(5)), Side::Right);
        selection.update(Point::new(Line(0), Column(2)), Side::Left);
        term.selection = Some(selection);
        assert_eq!(term.selection_to_string().as_deref(), Some("日本"));
    }

    #[test]
    fn double_click_selects_word() {
        let size = crate::term::GridSize { columns: 20, screen_lines: 3 };
        let mut term = Term::new(Config::default(), &size, VoidListener);
        Processor::<alacritty_terminal::vte::ansi::StdSyncHandler>::new().advance(&mut term, b"hello world");
        term.selection = Some(Selection::new(SelectionType::Semantic,
            Point::new(Line(0), Column(7)), Side::Left));
        assert_eq!(term.selection_to_string().as_deref(), Some("world"));
    }
}

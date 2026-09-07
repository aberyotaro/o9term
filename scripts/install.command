#!/bin/bash
# Installs a prebuilt package; no compiler, Python, Homebrew or network needed.
set -euo pipefail
package=$(cd -- "$(dirname -- "$0")" && pwd)
app="$HOME/Applications/0s9terminal.app"
[[ -x "$package/0s9terminal.app/Contents/MacOS/os9terminal" ]] || {
    echo "Run this installer from the extracted release package." >&2; exit 1;
}
mkdir -p "$HOME/Applications" "$HOME/.local/bin" "$HOME/.config/os9terminal"
# Keep the previous app until the replacement is fully copied.
/usr/bin/ditto "$package/0s9terminal.app" "$app.new"
if [[ -e "$app" ]]; then
    [[ ! -e "$app.previous" ]] || { echo "Move $app.previous before updating." >&2; exit 1; }
    mv "$app" "$app.previous"
fi
mv "$app.new" "$app"
install -m 755 "$package/o9" "$HOME/.local/bin/o9"
install -m 644 "$app/Contents/Resources/integration.zsh" "$HOME/.config/os9terminal/integration.zsh"
rc="${ZDOTDIR:-$HOME}/.zshrc"
line='[[ -r "$HOME/.config/os9terminal/integration.zsh" ]] && source "$HOME/.config/os9terminal/integration.zsh"'
if ! grep -Fqx "$line" "$rc" 2>/dev/null; then
    if [[ -e "$rc" && ! -e "$rc.os9terminal-backup" ]]; then cp -p "$rc" "$rc.os9terminal-backup"; fi
    printf '\n# 0s9terminal\n%s\n' "$line" >> "$rc"
fi
profile="${ZDOTDIR:-$HOME}/.zprofile"
path_line='export PATH="$HOME/.local/bin:$PATH"'
if ! grep -Fqx "$path_line" "$profile" 2>/dev/null; then
    if [[ -e "$profile" && ! -e "$profile.os9terminal-backup" ]]; then cp -p "$profile" "$profile.os9terminal-backup"; fi
    printf '\n# 0s9terminal command\n%s\n' "$path_line" >> "$profile"
fi
printf 'Installed. Open a new terminal and type: o9\n'

#!/usr/bin/env python3
"""Install the 0s9terminal zsh integration after installing Homebrew packages."""
import os
from pathlib import Path
import shutil
import subprocess

root = Path(__file__).resolve().parent.parent
brew = shutil.which("brew")
if not brew:
    raise SystemExit("Homebrew is required: install bat and zsh-syntax-highlighting first.")
subprocess.run([brew, "install", "bat", "zsh-syntax-highlighting"], check=True)
home = Path.home()
integration = home / ".config/os9terminal/integration.zsh"
integration.parent.mkdir(parents=True, exist_ok=True)
shutil.copyfile(root / "shell/integration.zsh", integration)
rc = Path(os.environ.get("ZDOTDIR") or home) / ".zshrc"
line = '[[ -r "$HOME/.config/os9terminal/integration.zsh" ]] && source "$HOME/.config/os9terminal/integration.zsh"'
content = rc.read_text() if rc.exists() else ""
if line not in content:
    # Preserve the user's original file once; append rather than rewrite.
    backup = rc.with_name(".zshrc.os9terminal-backup")
    if rc.exists() and not backup.exists():
        shutil.copy2(rc, backup)
    with rc.open("a") as stream:
        stream.write("\n# 0s9terminal syntax highlighting (keep at end)\n" + line + "\n")
print("Installed. Open a new o9term window to enable highlighting.")

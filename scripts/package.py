#!/usr/bin/env python3
"""Developer-only macOS packager; users run install.command."""
import os
from pathlib import Path
import platform
import plistlib
import shutil
import subprocess
import tomllib

ROOT = Path(__file__).resolve().parent.parent

def run(*args, **kwargs):
    return subprocess.run(args, check=True, **kwargs)

version = tomllib.loads((ROOT / "Cargo.toml").read_text())["package"]["version"]
name = f"o9term-{version}-macos-{platform.machine()}"
stage = ROOT / "dist" / name
if stage.exists():
    raise SystemExit(f"{stage} already exists; move it before packaging again.")
brew = shutil.which("brew")
if not brew:
    raise SystemExit("Packaging requires Homebrew bat and zsh-syntax-highlighting.")
prefix = Path(subprocess.check_output([brew, "--prefix"], text=True).strip())
for formula in ("bat", "zsh-syntax-highlighting"):
    run(brew, "list", "--versions", formula, stdout=subprocess.DEVNULL)
env = os.environ.copy()
extra = [f"--remap-path-prefix={Path.home()}=/build/user",
         f"--remap-path-prefix={ROOT}=/build/o9term"]
env["CARGO_ENCODED_RUSTFLAGS"] = "\x1f".join(
    filter(None, [env.get("CARGO_ENCODED_RUSTFLAGS", ""), *extra]))
run("cargo", "build", "--release", "--locked", cwd=ROOT, env=env)
app = stage / "o9term.app"
contents = app / "Contents"
resources = contents / "Resources"
macos = contents / "MacOS"
libs = contents / "Frameworks"
for directory in (macos, resources / "bin", libs, resources / "licenses"):
    directory.mkdir(parents=True, exist_ok=True)
shutil.copy2(ROOT / "target/release/os9terminal", macos / "os9terminal")
shutil.copy2(prefix / "bin/bat", resources / "bin/bat")
shutil.copytree(prefix / "share/zsh-syntax-highlighting", resources / "zsh-syntax-highlighting")
shutil.copy2(ROOT / "shell/integration.zsh", resources / "integration.zsh")
shutil.copy2(ROOT / "scripts/o9", stage / "o9")
shutil.copy2(ROOT / "scripts/install.command", stage / "install.command")

# Relocate non-system dependencies; no Homebrew needed at runtime.
queue = [resources / "bin/bat"]
processed = set()
formula_dirs = {(prefix / "opt/bat").resolve(),
                (prefix / "opt/zsh-syntax-highlighting").resolve()}
while queue:
    binary = queue.pop()
    if binary in processed:
        continue
    processed.add(binary)
    output = subprocess.check_output(["otool", "-L", str(binary)], text=True)
    for line in output.splitlines()[1:]:
        dep = line.strip().split(" (", 1)[0]
        if dep.startswith(("/usr/lib/", "/System/")):
            continue
        if not dep.startswith(str(prefix) + "/"):
            raise SystemExit(f"Unresolved dependency: {dep}")
        source = Path(dep).resolve()
        dest = libs / source.name
        if not dest.exists():
            shutil.copy2(source, dest)
            queue.append(dest)
        for ancestor in source.parents:
            if (ancestor / "INSTALL_RECEIPT.json").exists():
                formula_dirs.add(ancestor)
                break
        relative = os.path.relpath(dest, binary.parent)
        run("install_name_tool", "-change", dep, "@loader_path/" + relative, str(binary),
            stderr=subprocess.DEVNULL)
    if binary.parent == libs:
        run("install_name_tool", "-id", "@loader_path/" + binary.name, str(binary))

for formula in formula_dirs:
    dest = resources / "licenses" / formula.parent.name
    dest.mkdir(exist_ok=True)
    for item in formula.iterdir():
        if item.is_file() and item.name.upper().startswith(("LICENSE", "COPYING", "NOTICE", "AUTHORS")):
            shutil.copy2(item, dest / item.name)
with (contents / "Info.plist").open("wb") as stream:
    plistlib.dump({
        "CFBundleExecutable": "os9terminal",
        "CFBundleIdentifier": "io.github.aberyotaro.o9term",
        "CFBundleName": "o9term",
        "CFBundleDisplayName": "o9term",
        "CFBundlePackageType": "APPL",
        "CFBundleShortVersionString": version,
        "CFBundleVersion": version,
        "NSHighResolutionCapable": True,
    }, stream)
for binary in processed:
    run("codesign", "--force", "--sign", "-", str(binary), stderr=subprocess.DEVNULL)
run("codesign", "--force", "--sign", "-", str(app), stderr=subprocess.DEVNULL)
run("codesign", "--verify", "--deep", "--strict", str(app))
run(str(resources / "bin/bat"), "--version")
run("ditto", "-c", "-k", "--keepParent", str(stage), str(ROOT / "dist" / (name + ".zip")))
print(f"Package: dist/{name}.zip")

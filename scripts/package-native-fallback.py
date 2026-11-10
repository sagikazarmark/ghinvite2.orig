#!/usr/bin/env python3
"""Build a source-bound native fallback package; never migrate or deploy state."""
import argparse
import hashlib
import json
import os
from pathlib import Path
import re
import shutil
import subprocess
import sys
import tomllib

ROOT = Path(__file__).resolve().parents[1]
BINARIES = {"ghinvite-web-server": "ghinvite-web", "ghinvite-workflows-server": "ghinvite-workflows"}


def digest(path):
    value = hashlib.sha256()
    with path.open("rb") as source:
        for block in iter(lambda: source.read(1024 * 1024), b""):
            value.update(block)
    return value.hexdigest()


def command(args):
    return subprocess.check_output(args, cwd=ROOT, text=True)


BRIDGE_FILES = ("bridge.mjs", "worker.mjs", "manifest.py")


def bridge_inputs():
    directory = ROOT / "native-d1-bridge"
    files = [directory / name for name in BRIDGE_FILES]
    if directory.is_symlink() or any(p.is_symlink() or not p.is_file() for p in files):
        raise ValueError("missing or symlinked bridge source")
    return files


def copy_bridge(output):
    # Explicit source-only inventory: never package generated authority or credentials.
    for source in bridge_inputs():
        add_file(source, output / "native-d1-bridge" / source.name)


def source_inputs():
    files = [ROOT / "Cargo.toml", ROOT / "Cargo.lock",
             ROOT / "deploy/native/config.env.example", ROOT / "docs/native-fallback.md"]
    files += bridge_inputs()
    files += sorted((ROOT / "migrations").glob("*.sql"))
    files += sorted((ROOT / "deploy/github-apps").glob("*.manifest.json"))
    for package in BINARIES:
        files += [ROOT / f"crates/{package}/Cargo.toml", ROOT / f"crates/{package}/src/main.rs"]
    if not files or any(not p.is_file() or p.is_symlink() for p in files):
        raise ValueError("missing or symlinked packaging input")
    migrations = [p.name for p in files if p.suffix == ".sql"]
    if not migrations or len(migrations) != len(set(migrations)):
        raise ValueError("invalid migration inventory")
    return files


def asset_inputs(directory):
    if directory.is_symlink() or not directory.is_dir():
        raise ValueError("missing or symlinked island directory")
    files = sorted(p for p in directory.rglob("*") if p.is_file() or p.is_symlink())
    names = {p.relative_to(directory).as_posix() for p in files}
    if "assets/ghinvite-island.js" not in names or not any(n.endswith(".wasm") for n in names):
        raise ValueError("missing built island loader or wasm")
    for p in files:
        name = p.relative_to(directory).as_posix()
        if p.is_symlink() or any(x.is_symlink() for x in p.parents if x != directory.parent):
            raise ValueError("symlinked asset")
        if name != "_headers" and not (name.startswith("assets/") and p.suffix in {".js", ".wasm"}):
            raise ValueError("unexpected public asset")
    return files


def add_file(source, destination):
    if source.is_symlink() or not source.is_file():
        raise ValueError(f"not a regular input: {source.name}")
    destination.parent.mkdir(parents=True, exist_ok=True)
    shutil.copy2(source, destination)


def license_owner(package_dir):
    """A local workspace may share license text; registry packages stay scoped."""
    package_dir = package_dir.resolve()
    checkout = Path(command(["git", "rev-parse", "--show-toplevel"]).strip()).resolve()
    if package_dir.is_relative_to(checkout):
        for parent in [package_dir, *package_dir.parents]:
            manifest = parent / "Cargo.toml"
            if manifest.is_file() and "workspace" in tomllib.loads(manifest.read_text()):
                return parent
            if parent == checkout:
                break
    return package_dir


def license_source(path, owner):
    """Follow owned license aliases, never arbitrary symlink/path escapes."""
    resolved = path.resolve(strict=True)
    if not resolved.is_relative_to(owner.resolve()) or not resolved.is_file():
        raise ValueError("license file escapes its owning package/workspace")
    return resolved


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--sha", required=True, help="exact git checkout SHA, not an arbitrary release label")
    parser.add_argument("--output", type=Path)
    parser.add_argument("--assets", type=Path, default=ROOT / "dist/public")
    parser.add_argument("--check-inputs", action="store_true", help="inspect source only; no compiler or artifact claim")
    args = parser.parse_args()
    if not re.fullmatch(r"[0-9a-f]{40,64}", args.sha) or command(["git", "rev-parse", "HEAD"]).strip() != args.sha:
        raise ValueError("checkout SHA mismatch")
    inputs = source_inputs()
    if args.check_inputs:
        print(json.dumps({"mode": "source-inputs-only", "checkout_sha": args.sha,
                          "files": {str(p.relative_to(ROOT)): digest(p) for p in inputs},
                          "build_executed": False}, indent=2))
        return
    if not args.output or args.output.exists():
        raise ValueError("--output must be a new directory")
    if command(["git", "status", "--porcelain", "--untracked-files=all"]).strip():
        raise ValueError("release artifact requires a clean checkout (put output/assets in ignored generated directories)")
    assets_dir = args.assets.absolute()
    assets = asset_inputs(assets_dir)
    lock_before = digest(ROOT / "Cargo.lock")
    compiler = command(["rustc", "-Vv"])
    host = next(line.removeprefix("host: ") for line in compiler.splitlines() if line.startswith("host: "))
    metadata = json.loads(command(["cargo", "metadata", "--locked", "--format-version", "1"]))
    build = ["cargo", "build", "--locked", "--release", "--target", host,
             "-p", "ghinvite-web-server", "-p", "ghinvite-workflows-server", "--bins", "--message-format=json"]
    # Both production entry points are built together, without runtime-test features.
    result = subprocess.run(build, cwd=ROOT, text=True, stdout=subprocess.PIPE, check=True)
    executables = {}
    for line in result.stdout.splitlines():
        record = json.loads(line)
        name = record.get("target", {}).get("name")
        if record.get("reason") == "compiler-artifact" and name in BINARIES.values() and record.get("executable"):
            if "bin" not in record["target"]["kind"] or record["profile"].get("test"):
                raise ValueError("unexpected test executable")
            executables[name] = Path(record["executable"])
    if set(executables) != set(BINARIES.values()):
        raise ValueError("Cargo did not report both native binaries")
    if digest(ROOT / "Cargo.lock") != lock_before or command(["git", "status", "--porcelain", "--untracked-files=all"]).strip():
        raise ValueError("source or lock changed during build")
    output = args.output.resolve()
    output.mkdir(parents=True)
    for name, source in executables.items():
        with source.open("rb") as executable:
            magic = executable.read(4)
        if magic != b"\x7fELF" or not os.access(source, os.X_OK):
            raise ValueError("expected actual executable Linux ELF")
        add_file(source, output / "bin" / name)
    for p in [ROOT / "Cargo.toml", ROOT / "Cargo.lock"]:
        add_file(p, output / p.name)
    for folder in ("migrations", "docs", "deploy"):
        for p in sorted((ROOT / folder).rglob("*")):
            if p.is_file() and p.suffix in {".md", ".sql", ".json", ".example"}:
                add_file(p, output / p.relative_to(ROOT))
    copy_bridge(output)
    for p in assets:
        add_file(p, output / "public" / p.relative_to(assets_dir))
    licenses = []
    for package in sorted(metadata["packages"], key=lambda p: p["id"]):
        identity = hashlib.sha256(package["id"].encode()).hexdigest()[:12]
        package_dir = Path(package["manifest_path"]).parent
        owner = license_owner(package_dir)
        declared = package.get("license_file")
        candidates = set(package_dir.glob("LICENSE*")) | set(package_dir.glob("COPYING*"))
        if declared:
            candidates.add(package_dir / declared)
        copied = []
        for p in sorted(candidates):
            if p.is_file() or p.is_symlink():
                source = license_source(p, owner)
                dest = Path("licenses") / f'{package["name"]}-{package["version"]}-{identity}' / p.name
                add_file(source, output / dest)
                copied.append(dest.as_posix())
        licenses.append({"name": package["name"], "version": package["version"],
                         "declared_license": package.get("license"), "files": copied})
    (output / "dependency-licenses.json").write_text(json.dumps(licenses, indent=2) + "\n")
    (output / "build-messages.jsonl").write_text(result.stdout)
    files = {p.relative_to(output).as_posix(): digest(p) for p in sorted(output.rglob("*")) if p.is_file()}
    manifest = {"format": 1, "checkout_sha": args.sha, "host": host, "rustc": compiler,
                "cargo": command(["cargo", "-V"]), "cargo_lock_sha256": lock_before,
                "build_command": build, "files": files,
                "scope": "native release build and packaging; no storage migration or deployment"}
    (output / "manifest.json").write_text(json.dumps(manifest, indent=2) + "\n")
    print(json.dumps({"artifact": str(output), "checkout_sha": args.sha, "payload_files": len(files)}))


if __name__ == "__main__":
    try:
        main()
    except (ValueError, OSError, subprocess.CalledProcessError) as error:
        print(f"native fallback packaging failed: {error}", file=sys.stderr)
        sys.exit(1)

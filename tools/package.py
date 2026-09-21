#!/usr/bin/env python3
"""Build a release package for the platform this runs on.

A Makepad binary is not self-contained. The theme names the fonts it draws with as
paths resolved at compile time against each crate's source directory, so a binary
built on a CI runner goes looking for `~/.cargo/registry/src/.../IBMPlexSans-Text.ttf`
on a machine that has no such file, and fails at startup rather than falling back.

The way out is `MAKEPAD_PACKAGE_DIR`, which Makepad reads at compile time and treats
as the root every resource path hangs off instead. Set it to a relative directory,
ship a copy of the resources under that name beside the executable, and the lookup
becomes relative to the working directory -- which `app::run` makes the executable's
own directory. On macOS the same build additionally sets `MAKEPAD=apple_bundle`, which
switches resource loading to the `.app` bundle's `Resources` and needs no such move.

Run it with no arguments to package for the machine it runs on:

    python3 tools/package.py    # -> dist/opendxfviewer-<ver>-<os>-<arch>.<ext>

It packages for the host architecture only, including on macOS, where a universal
binary would be the nicer thing to ship: makepad-platform 1.0.0 builds its XPC shim
by calling `clang` with no `-arch`, so the object file always comes out for the
machine doing the building and a cross-architecture link fails on it. The release
workflow gets the second Mac architecture from a second runner instead.

The GitHub release workflow runs exactly this, so a package can be reproduced and
tried locally before it is ever tagged.
"""
import argparse, hashlib, json, os, platform, plistlib, shutil, subprocess, sys, tarfile
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent
# Honoured because CI runners and caches move it, and the built binary has to be found again.
TARGET = Path(os.environ.get("CARGO_TARGET_DIR", ROOT / "target")).resolve()
BIN = "opendxfviewer"

# Must match MAKEPAD_PACKAGE_DIR below: it is the directory name baked into the binary.
PKG_DIR = "makepad"


def run(cmd, **kw):
    print("+", " ".join(str(c) for c in cmd), flush=True)
    subprocess.run(cmd, check=True, **kw)


def build(target=None, bundle=False):
    """Compiles a packaged release binary and returns its path."""
    env = dict(os.environ, MAKEPAD_PACKAGE_DIR=PKG_DIR)
    if bundle:
        # Read by makepad-platform's build script, which turns it into `--cfg apple_bundle`.
        env["MAKEPAD"] = "apple_bundle"
    cmd = ["cargo", "build", "--release", "--locked"]
    if target:
        cmd += ["--target", target]
    run(cmd, cwd=ROOT, env=env)
    out = TARGET / (target or "") / "release" / BIN
    return out.with_suffix(".exe") if os.name == "nt" else out


def copy_resources(dest):
    """Copies every dependency's `resources/` into the layout Makepad will look for.

    A crate's resources land under its own name with dashes turned into underscores,
    because that is the form the `crate://makepad_fonts_emoji/...` paths in the theme
    resolve to. Anything without a `resources/` directory is not a resource crate.
    """
    meta = json.loads(
        subprocess.run(
            ["cargo", "metadata", "--format-version", "1"],
            cwd=ROOT, check=True, capture_output=True, text=True,
        ).stdout
    )
    total = 0
    for pkg in sorted(meta["packages"], key=lambda p: p["name"]):
        src = Path(pkg["manifest_path"]).parent / "resources"
        if not src.is_dir():
            continue
        target = dest / pkg["name"].replace("-", "_") / "resources"
        shutil.copytree(src, target)
        size = sum(f.stat().st_size for f in target.rglob("*") if f.is_file())
        total += size
        print(f"  {pkg['name']:34} {size / 1e6:7.1f} MB")
    if total == 0:
        sys.exit("no resource directories found: the package would start with no fonts")
    return total


def docs(dest):
    for name in ("LICENSE", "README.md"):
        shutil.copy2(ROOT / name, dest / name)
    shutil.copy2(ROOT / "packaging" / "THIRD-PARTY-NOTICES.md", dest / "THIRD-PARTY-NOTICES.md")


def macos_app(staging, binary, version):
    """Assembles a `.app`, which is the only form macOS will open by double-click."""
    app = staging / f"{BIN}.app"
    macos, resources = app / "Contents" / "MacOS", app / "Contents" / "Resources"
    macos.mkdir(parents=True)
    resources.mkdir(parents=True)
    shutil.copy2(binary, macos / BIN)
    (app / "Contents" / "Info.plist").write_bytes(
        plistlib.dumps({
            "CFBundleName": BIN,
            "CFBundleDisplayName": BIN,
            "CFBundleIdentifier": "io.github.conol-ai.opendxfviewer",
            "CFBundleExecutable": BIN,
            "CFBundlePackageType": "APPL",
            "CFBundleInfoDictionaryVersion": "6.0",
            "CFBundleShortVersionString": version,
            "CFBundleVersion": version,
            "LSMinimumSystemVersion": "11.0",
            "NSHighResolutionCapable": True,
        })
    )
    copy_resources(resources / PKG_DIR)
    docs(resources)
    # An arm64 binary has to carry a signature to run at all. Ad-hoc is enough for that,
    # and re-signing the assembled bundle covers anything the copy disturbed; it is not
    # notarisation, so a downloaded package still needs Finder's right-click -> Open.
    run(["codesign", "--force", "--deep", "--sign", "-", app])
    return app


def archive(staging, payload, out, name):
    """Writes the archive and its checksum, and returns the archive path."""
    out.mkdir(parents=True, exist_ok=True)
    if sys.platform == "darwin":
        path = out / f"{name}.zip"
        # `ditto` is the only zip on macOS that preserves a bundle's permissions and links.
        run(["ditto", "-c", "-k", "--keepParent", payload, path])
    elif sys.platform == "win32":
        path = Path(shutil.make_archive(str(out / name), "zip", staging, payload.name))
    else:
        path = out / f"{name}.tar.gz"
        with tarfile.open(path, "w:gz") as tar:
            tar.add(payload, arcname=payload.name)
    digest = hashlib.sha256(path.read_bytes()).hexdigest()
    (out / f"{path.name}.sha256").write_text(f"{digest}  {path.name}\n")
    return path


def main():
    ap = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    ap.add_argument("--out", default="dist", help="where to write the archive (default: dist/)")
    args = ap.parse_args()

    version = json.loads(
        subprocess.run(["cargo", "metadata", "--format-version", "1", "--no-deps"],
                       cwd=ROOT, check=True, capture_output=True, text=True).stdout
    )["packages"][0]["version"]

    out = (ROOT / args.out).resolve()
    staging = TARGET / "package-staging"
    shutil.rmtree(staging, ignore_errors=True)
    staging.mkdir(parents=True)

    arch = {"AMD64": "x86_64", "arm64": "arm64", "aarch64": "arm64"}.get(
        platform.machine(), platform.machine())

    if sys.platform == "darwin":
        binary = build(bundle=True)
        name = f"{BIN}-{version}-macos-{arch}"
        payload = macos_app(staging, binary, version)
    else:
        os_name = "windows" if sys.platform == "win32" else "linux"
        binary = build()
        payload = staging / f"{BIN}-{version}-{os_name}-{arch}"
        payload.mkdir()
        shutil.copy2(binary, payload / binary.name)
        copy_resources(payload / PKG_DIR)
        docs(payload)
        name = payload.name

    path = archive(staging, payload, out, name)
    shown = path.relative_to(ROOT) if path.is_relative_to(ROOT) else path
    print(f"\n{shown}  {path.stat().st_size / 1e6:.1f} MB")


if __name__ == "__main__":
    main()

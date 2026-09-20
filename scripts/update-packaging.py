#!/usr/bin/env python3
"""Point the downstream packaging at a new release.

Every release needs the same files rewritten with the new version and the new
checksums: flake.nix, the two AUR PKGBUILDs, the Homebrew formula, the Flatpak
manifest and its AppStream release list. Doing it by hand is mechanical and
easy to get subtly wrong, so release.yml runs this instead.

Checksums come from the `.sha256` sidecars the release already publishes, so
none of the tarballs are downloaded; only the GitHub source archive, which the
from-source AUR package needs and which has no sidecar, is fetched.

Every rewrite asserts how many substitutions it made. If one of these files is
ever restructured, the release fails loudly here instead of quietly shipping a
package that still points at the previous version.

Each target is optional — pass only the paths that exist on the runner.
"""

import argparse
import base64
import datetime
import hashlib
import re
import sys
import urllib.request
from pathlib import Path

REPO = "mrFrok/LibreFastbootFirmwareFlasher"

# (kind, os, arch) -> released asset name
ASSETS = {
    (kind, os_, arch): f"lfff{'-gui' if kind == 'gui' else ''}-{os_}-{arch}.tar.gz"
    for kind in ("cli", "gui")
    for os_ in ("linux", "macos")
    for arch in ("x86_64", "aarch64")
}


class PatchError(Exception):
    """A file did not look the way this script expects."""


def fetch(url: str) -> bytes:
    with urllib.request.urlopen(url, timeout=120) as r:
        return r.read()


def parse_sidecar(text: str, name: str) -> str:
    """A sidecar is `<sha256>  <filename>`, as written by sha256sum."""
    digest = text.split()[0] if text.split() else ""
    if not re.fullmatch(r"[0-9a-f]{64}", digest):
        raise PatchError(f"{name}.sha256 does not contain a sha256: {text!r}")
    return digest


def release_hashes(version: str) -> dict:
    """sha256 of every published tarball, read from its sidecar."""
    base = f"https://github.com/{REPO}/releases/download/v{version}"
    return {
        key: parse_sidecar(fetch(f"{base}/{name}.sha256").decode(), name)
        for key, name in ASSETS.items()
    }


def source_hash(version: str) -> str:
    """The GitHub source archive has no sidecar, so hash it directly."""
    return hashlib.sha256(
        fetch(f"https://github.com/{REPO}/archive/refs/tags/v{version}.tar.gz")
    ).hexdigest()


def sri(hex_digest: str) -> str:
    """Nix wants the flat file hash base64-encoded, not hex."""
    return "sha256-" + base64.b64encode(bytes.fromhex(hex_digest)).decode()


def sub_exactly(text: str, pattern: str, repl, what: str, count: int = 1) -> str:
    new, n = re.subn(pattern, repl, text)
    if n != count:
        raise PatchError(f"{what}: expected {count} match(es), found {n}")
    return new


def update_flake(path: Path, version: str, h: dict) -> None:
    s = path.read_text()
    s = sub_exactly(s, r'(\n\s+version = ")[^"]+(";)', rf"\g<1>{version}\g<2>", "flake version")
    # Each prebuilt block is a url line followed by its per-arch hashes; keying
    # off the url keeps this correct if the blocks are ever reordered, and keeps
    # it away from the unrelated payload-dumper hashes further up the file.
    for kind, marker in (("gui", "lfff-gui-linux-"), ("cli", "lfff-linux-")):
        pattern = (
            rf'(url = "[^"]*{re.escape(marker)}\$\{{arch\}}\.tar\.gz";\s*\n\s*hash = \{{\s*\n'
            rf'\s*x86_64 = ")[^"]+("; *\n\s*aarch64 = ")[^"]+(";)'
        )
        repl = (
            rf'\g<1>{sri(h[(kind, "linux", "x86_64")])}'
            rf'\g<2>{sri(h[(kind, "linux", "aarch64")])}\g<3>'
        )
        s = sub_exactly(s, pattern, repl, f"flake {kind} hashes")
    path.write_text(s)
    print(f"flake.nix: version {version}, 4 hashes")


def bump_pkgver(s: str, version: str, pkg: str) -> str:
    """A new version starts over at pkgrel=1."""
    s = sub_exactly(s, r"(?m)^pkgver=.+$", f"pkgver={version}", f"{pkg} pkgver")
    return sub_exactly(s, r"(?m)^pkgrel=.+$", "pkgrel=1", f"{pkg} pkgrel")


def update_aur_bin(path: Path, version: str, h: dict) -> None:
    s = bump_pkgver(path.read_text(), version, "lfff-bin")
    for arch in ("x86_64", "aarch64"):
        # source_<arch> lists the CLI tarball first, then the GUI one.
        pattern = rf"(sha256sums_{arch}=\(\s*\n\s*')[0-9a-f]{{64}}('\s*\n\s*')[0-9a-f]{{64}}(')"
        repl = rf'\g<1>{h[("cli", "linux", arch)]}\g<2>{h[("gui", "linux", arch)]}\g<3>'
        s = sub_exactly(s, pattern, repl, f"lfff-bin {arch} hashes")
    path.write_text(s)
    print(f"lfff-bin/PKGBUILD: version {version}, 4 hashes")


def update_aur_src(path: Path, version: str, src: str) -> None:
    s = bump_pkgver(path.read_text(), version, "lfff")
    # The two files after the source tarball are fetched from main and SKIPped.
    s = sub_exactly(s, r"(sha256sums=\(')[0-9a-f]{64}", rf"\g<1>{src}", "lfff source hash")
    path.write_text(s)
    print(f"lfff/PKGBUILD: version {version}, source hash")


def update_formula(path: Path, version: str, h: dict) -> None:
    s = path.read_text()
    s = sub_exactly(s, r'(\n  version ")[^"]+(")', rf"\g<1>{version}\g<2>", "formula version")
    s = sub_exactly(
        s, r"/download/v[\d.]+/", f"/download/v{version}/", "formula urls", count=len(ASSETS) + 1
    )

    by_asset = {name: h[key] for key, name in ASSETS.items()}
    unknown = []

    # Every `sha256` line belongs to the `url` line above it, so pair them up
    # rather than matching on the old digests, which change every release.
    def paired(m):
        asset = m.group("asset")
        if asset not in by_asset:
            unknown.append(asset)
            return m.group(0)
        return f'{m.group("head")}{by_asset[asset]}{m.group("tail")}'

    # The top-level url/sha256 repeats one of the resources, hence the +1.
    s = sub_exactly(
        s,
        r'(?P<head>url "[^"]*/(?P<asset>lfff[\w.-]*\.tar\.gz)"\s*\n\s*sha256 ")'
        r'[0-9a-f]{64}(?P<tail>")',
        paired,
        "formula hashes",
        count=len(ASSETS) + 1,
    )
    if unknown:
        raise PatchError(f"formula references unknown assets: {sorted(set(unknown))}")
    path.write_text(s)
    print(f"Formula/lfff.rb: version {version}, {len(ASSETS) + 1} hashes")


def update_flatpak(path: Path, version: str, h: dict) -> None:
    """The manifest pulls the app itself from the release, like Homebrew does."""
    by_asset = {name: h[key] for key, name in ASSETS.items()}
    unknown = []

    def paired(m):
        asset = m.group("asset")
        if asset not in by_asset:
            unknown.append(asset)
            return m.group(0)
        return f'{m.group("head")}{version}/{asset}{m.group("mid")}{by_asset[asset]}'

    s = sub_exactly(
        path.read_text(),
        r"(?P<head>url: https://github\.com/[\w/-]+/releases/download/v)[\d.]+/"
        r"(?P<asset>lfff[\w.-]*\.tar\.gz)(?P<mid>\s*\n\s*sha256: )[0-9a-f]{64}",
        paired,
        "flatpak sources",
        count=2,  # the GUI and the CLI; the bundled tools are pinned separately
    )
    if unknown:
        raise PatchError(f"flatpak manifest references unknown assets: {sorted(set(unknown))}")
    path.write_text(s)
    print(f"flatpak manifest: version {version}, 2 sources")


def update_metainfo(path: Path, version: str, date: str) -> None:
    """Prepend a <release> entry, unless this version is already listed."""
    s = path.read_text()
    if re.search(rf'<release version="{re.escape(version)}"', s):
        print(f"metainfo: {version} already listed")
        return
    entry = f'    <release version="{version}" date="{date}"/>'
    s = sub_exactly(s, r"( *)<releases>\n", rf"\g<1><releases>\n{entry}\n", "metainfo releases")
    path.write_text(s)
    print(f"metainfo: added release {version} ({date})")


def main() -> int:
    ap = argparse.ArgumentParser()
    ap.add_argument("--version", required=True, help="release version, with or without the v")
    ap.add_argument("--flake", type=Path)
    ap.add_argument("--aur-bin", type=Path, help="path to lfff-bin/PKGBUILD")
    ap.add_argument("--aur-src", type=Path, help="path to lfff/PKGBUILD")
    ap.add_argument("--formula", type=Path, help="path to Formula/lfff.rb")
    ap.add_argument("--flatpak", type=Path, help="path to the flatpak manifest")
    ap.add_argument("--metainfo", type=Path, help="path to the AppStream metainfo")
    ap.add_argument("--date", help="release date for the metainfo entry (default: today, UTC)")
    args = ap.parse_args()

    version = args.version.lstrip("v")
    if not re.fullmatch(r"\d+\.\d+\.\d+", version):
        print(f"not a release version: {args.version}", file=sys.stderr)
        return 2

    try:
        h = release_hashes(version)
        print(f"read {len(h)} checksums from the release sidecars")
        if args.flake:
            update_flake(args.flake, version, h)
        if args.aur_bin:
            update_aur_bin(args.aur_bin, version, h)
        if args.aur_src:
            update_aur_src(args.aur_src, version, source_hash(version))
        if args.formula:
            update_formula(args.formula, version, h)
        if args.flatpak:
            update_flatpak(args.flatpak, version, h)
        if args.metainfo:
            date = args.date or datetime.datetime.now(datetime.UTC).strftime('%Y-%m-%d')
            update_metainfo(args.metainfo, version, date)
    except PatchError as e:
        print(f"error: {e}", file=sys.stderr)
        return 1
    return 0


if __name__ == "__main__":
    sys.exit(main())

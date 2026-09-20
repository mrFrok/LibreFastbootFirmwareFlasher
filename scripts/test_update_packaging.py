#!/usr/bin/env python3
"""Tests for update-packaging.py.

The script rewrites files by regex, which is fine as long as it never rewrites
the wrong thing and never silently rewrites nothing. Both are what these tests
check, with hashes chosen so that a swapped pair is visible.

flake.nix is tested against the real file. The AUR PKGBUILDs and the Homebrew
formula live in other repositories, so `testdata/` holds snapshots of them; if
those repos are restructured the snapshots go stale, but the script raises
PatchError on any count mismatch, so a release fails loudly rather than
shipping a half-updated package.
"""

import hashlib
import importlib.util
import shutil
import sys
import tempfile
import unittest
from pathlib import Path

HERE = Path(__file__).resolve().parent
REPO = HERE.parent
DATA = HERE / "testdata"
FLATPAK = REPO / "packaging" / "flatpak"
APP_ID = "io.github.mrFrok.LibreFastbootFirmwareFlasher"

spec = importlib.util.spec_from_file_location("up", HERE / "update-packaging.py")
up = importlib.util.module_from_spec(spec)
spec.loader.exec_module(up)


def fake_hashes() -> dict:
    """A distinct, reproducible digest per asset, so a mispaired one shows up."""
    return {
        key: hashlib.sha256("-".join(key).encode()).hexdigest() for key in up.ASSETS
    }


H = fake_hashes()


class Case(unittest.TestCase):
    def setUp(self):
        self.tmp = Path(tempfile.mkdtemp())
        self.addCleanup(shutil.rmtree, self.tmp)

    def copy(self, src: Path) -> Path:
        dst = self.tmp / src.name
        shutil.copy(src, dst)
        return dst


class TestHelpers(Case):
    def test_sri_is_base64_of_the_raw_digest(self):
        # Nix flat file hashes are base64, not hex; this is the whole reason no
        # nix binary is needed on the runner.
        self.assertEqual(up.sri("00" * 32), "sha256-" + "A" * 43 + "=")
        digest = hashlib.sha256(b"lfff").hexdigest()
        import base64

        self.assertEqual(
            base64.b64decode(up.sri(digest)[len("sha256-") :]), bytes.fromhex(digest)
        )

    def test_sidecar_parsing(self):
        d = "e6d47e6701be1521526d3a8062c0d4f3e3d4793ebd603e2ea71ab38d6bd0fac5"
        self.assertEqual(up.parse_sidecar(f"{d}  lfff-linux-x86_64.tar.gz\n", "x"), d)
        self.assertEqual(up.parse_sidecar(f"  {d}   file\n", "x"), d)

    def test_sidecar_rejects_anything_else(self):
        for bad in ("", "\n", "not-a-hash  file\n", "ABCD" * 16 + "  f\n", "abc  f\n",
                    "<html>404</html>"):
            with self.assertRaises(up.PatchError):
                up.parse_sidecar(bad, "lfff-linux-x86_64.tar.gz")

    def test_sub_exactly_refuses_a_wrong_count(self):
        with self.assertRaises(up.PatchError):
            up.sub_exactly("a a", "a", "b", "two", count=1)
        with self.assertRaises(up.PatchError):
            up.sub_exactly("nothing here", "zzz", "b", "none", count=1)


class TestFlake(Case):
    def setUp(self):
        super().setUp()
        self.path = self.copy(REPO / "flake.nix")
        self.before = self.path.read_text()
        up.update_flake(self.path, "9.9.9", H)
        self.after = self.path.read_text()

    def test_version(self):
        self.assertIn('version = "9.9.9";', self.after)

    def test_prebuilt_hashes(self):
        for kind, arch in [("gui", "x86_64"), ("gui", "aarch64"),
                           ("cli", "x86_64"), ("cli", "aarch64")]:
            self.assertIn(up.sri(H[(kind, "linux", arch)]), self.after,
                          f"{kind}/{arch} hash missing")

    def test_gui_and_cli_blocks_did_not_get_the_same_hashes(self):
        # Both blocks have the identical `hash = { x86_64 = …` shape, so the
        # only thing keeping them apart is the url line above each.
        gui = self.after.index("lfff-gui-linux-${arch}")
        cli = self.after.index('/lfff-linux-${arch}')
        self.assertIn(up.sri(H[("gui", "linux", "x86_64")]), self.after[gui:cli])
        self.assertIn(up.sri(H[("cli", "linux", "x86_64")]), self.after[cli:])

    def test_unrelated_hashes_are_untouched(self):
        # payload-dumper-rust and the skia pin are pinned independently of the
        # LFFF release and must survive the rewrite.
        for keep in ("sha256-0qfPTY702qc+vTTgxvd+tsEWoXRaGzGtiX6c4rL1QVE=",
                     "sha256-+5WoJZEGAfDQfHLxthS/c2d05k5h5v6n2pjk3sW7ojg=",
                     "sha256-CBFVM9eUIOKoL/a7xePwJoA1jNfMdyLB9F7urEjYGts=",
                     "sha256-sM9SHt2KtDsNMuSyq3CyY1q8VZ5AVQRbNdt2MmEaEiI=",
                     '61e7ca4e99062cdd0ab69445d5963fb3365778f6'):
            self.assertIn(keep, self.after)
        self.assertIn('payloadDumperVersion = "0.8.4"', self.after)

    def test_only_the_expected_lines_changed(self):
        changed = [
            (a, b)
            for a, b in zip(self.before.splitlines(), self.after.splitlines())
            if a != b
        ]
        self.assertEqual(len(changed), 5, changed)  # version + 4 hashes

    def test_idempotent(self):
        up.update_flake(self.path, "9.9.9", H)
        self.assertEqual(self.path.read_text(), self.after)

    def test_restructured_file_raises(self):
        self.path.write_text(self.before.replace("hash = {", "sha = {"))
        with self.assertRaises(up.PatchError):
            up.update_flake(self.path, "9.9.9", H)


class TestAurBin(Case):
    def setUp(self):
        super().setUp()
        self.path = self.copy(DATA / "PKGBUILD.lfff-bin")
        up.update_aur_bin(self.path, "9.9.9", H)
        self.after = self.path.read_text()

    def test_pkgver_and_pkgrel(self):
        self.assertIn("\npkgver=9.9.9\n", self.after)
        self.assertIn("\npkgrel=1\n", self.after)  # fixture starts at 3

    def test_cli_before_gui_in_each_arch_block(self):
        for arch in ("x86_64", "aarch64"):
            block = self.after.split(f"sha256sums_{arch}=(")[1].split(")")[0]
            lines = [l.strip().strip("'") for l in block.strip().splitlines()]
            self.assertEqual(
                lines, [H[("cli", "linux", arch)], H[("gui", "linux", arch)]]
            )

    def test_skips_survive(self):
        self.assertIn("sha256sums+=('SKIP' 'SKIP')", self.after)

    def test_idempotent(self):
        up.update_aur_bin(self.path, "9.9.9", H)
        self.assertEqual(self.path.read_text(), self.after)

    def test_missing_arch_block_raises(self):
        self.path.write_text(self.after.replace("sha256sums_aarch64", "sha256sums_arm64"))
        with self.assertRaises(up.PatchError):
            up.update_aur_bin(self.path, "9.9.9", H)


class TestAurSrc(Case):
    SRC = "a" * 64

    def setUp(self):
        super().setUp()
        self.path = self.copy(DATA / "PKGBUILD.lfff")
        up.update_aur_src(self.path, "9.9.9", self.SRC)
        self.after = self.path.read_text()

    def test_pkgver_and_source_hash(self):
        self.assertIn("\npkgver=9.9.9\n", self.after)
        self.assertIn("\npkgrel=1\n", self.after)
        self.assertIn(f"sha256sums=('{self.SRC}' 'SKIP' 'SKIP')", self.after)

    def test_build_body_untouched(self):
        self.assertIn("cargo build --frozen --release -p lfff-cli -p lfff-gui", self.after)

    def test_idempotent(self):
        up.update_aur_src(self.path, "9.9.9", self.SRC)
        self.assertEqual(self.path.read_text(), self.after)


class TestFormula(Case):
    def setUp(self):
        super().setUp()
        self.path = self.copy(DATA / "lfff.rb")
        up.update_formula(self.path, "9.9.9", H)
        self.after = self.path.read_text()

    def test_version_and_urls(self):
        self.assertIn('version "9.9.9"', self.after)
        self.assertNotIn("2.7.2", self.after)
        self.assertEqual(self.after.count("/download/v9.9.9/"), len(up.ASSETS) + 1)

    def test_every_sha256_matches_the_url_above_it(self):
        # This is the crux: nine url/sha256 pairs across cli/gui × os × arch,
        # all structurally identical, distinguished only by the asset name.
        import re

        pairs = re.findall(
            r'url "[^"]*/(lfff[\w.-]*\.tar\.gz)"\s*\n\s*sha256 "([0-9a-f]{64})"', self.after
        )
        self.assertEqual(len(pairs), len(up.ASSETS) + 1)
        by_asset = {name: H[key] for key, name in up.ASSETS.items()}
        for asset, digest in pairs:
            self.assertEqual(digest, by_asset[asset], f"wrong hash under {asset}")

    def test_all_eight_assets_are_present(self):
        for name in up.ASSETS.values():
            self.assertIn(name, self.after)

    def test_install_block_untouched(self):
        self.assertIn('resource("cli").stage { bin.install "lfff" }', self.after)
        self.assertIn("generate_completions_from_executable", self.after)

    def test_idempotent(self):
        up.update_formula(self.path, "9.9.9", H)
        self.assertEqual(self.path.read_text(), self.after)

    def test_unknown_asset_raises(self):
        self.path.write_text(self.after.replace("lfff-macos-x86_64", "lfff-macos-i686"))
        with self.assertRaises(up.PatchError):
            up.update_formula(self.path, "9.9.9", H)

    def test_dropped_resource_raises(self):
        # A formula that lost a platform must not pass silently.
        cut = self.after.replace(
            '        url "https://github.com/mrFrok/LibreFastbootFirmwareFlasher'
            '/releases/download/v9.9.9/lfff-linux-aarch64.tar.gz"\n', "", 1
        )
        self.path.write_text(cut)
        with self.assertRaises(up.PatchError):
            up.update_formula(self.path, "9.9.9", H)


class TestFlatpak(Case):
    def setUp(self):
        super().setUp()
        self.path = self.copy(FLATPAK / f"{APP_ID}.yml")
        self.before = self.path.read_text()
        up.update_flatpak(self.path, "9.9.9", H)
        self.after = self.path.read_text()

    def test_both_sources_are_paired_with_their_own_url(self):
        import re

        pairs = re.findall(
            r"url: [^\n]*/download/v([\d.]+)/(lfff[\w.-]*\.tar\.gz)\s*\n\s*sha256: ([0-9a-f]{64})",
            self.after,
        )
        self.assertEqual(len(pairs), 2)
        by_asset = {name: H[key] for key, name in up.ASSETS.items()}
        for version, asset, digest in pairs:
            self.assertEqual(version, "9.9.9")
            self.assertEqual(digest, by_asset[asset], f"wrong hash under {asset}")

    def test_bundled_tools_are_left_alone(self):
        # platform-tools, payload-dumper and aria2 are pinned independently of
        # the LFFF release and must not move when the app is repointed.
        for pin in ("d230f13842f60f782a8645f9c813f8f845bf36089ea7289f28c48f17979313f1",
                    "d2a7cf4d8ef4daa73ebd34e0c6f77eb6c116a1745a1b31ad897e9ce2b2f54151",
                    "60a420ad7085eb616cb6e2bdf0a7206d68ff3d37fb5a956dc44242eb2f79b66b",
                    "platform-tools_r37.0.1-linux.zip",
                    "aria2-1.37.0.tar.xz"):
            self.assertIn(pin, self.after)

    def test_stays_valid_yaml_with_the_same_shape(self):
        import yaml

        before, after = yaml.safe_load(self.before), yaml.safe_load(self.path.read_text())
        self.assertEqual([m["name"] for m in before["modules"]],
                         [m["name"] for m in after["modules"]])
        self.assertEqual(after["app-id"], APP_ID)
        self.assertEqual(after["runtime-version"], before["runtime-version"])

    def test_only_four_lines_changed(self):
        changed = [
            (a, b)
            for a, b in zip(self.before.splitlines(), self.after.splitlines())
            if a != b
        ]
        self.assertEqual(len(changed), 4, changed)  # 2 urls + 2 hashes

    def test_idempotent(self):
        up.update_flatpak(self.path, "9.9.9", H)
        self.assertEqual(self.path.read_text(), self.after)

    def test_a_dropped_source_raises(self):
        self.path.write_text(self.after.replace("lfff-linux-x86_64.tar.gz", "lfff-linux-riscv.tar.gz"))
        with self.assertRaises(up.PatchError):
            up.update_flatpak(self.path, "9.9.9", H)


class TestMetainfo(Case):
    def setUp(self):
        super().setUp()
        self.path = self.copy(FLATPAK / f"{APP_ID}.metainfo.xml")

    def parse(self):
        import xml.etree.ElementTree as ET

        root = ET.parse(self.path).getroot()
        return [(r.get("version"), r.get("date")) for r in root.find("releases")]

    def test_prepends_the_new_release(self):
        up.update_metainfo(self.path, "9.9.9", "2030-01-02")
        releases = self.parse()
        self.assertEqual(releases[0], ("9.9.9", "2030-01-02"))
        self.assertIn(("2.8.0", "2026-09-20"), releases)

    def test_known_version_is_a_no_op(self):
        before = self.path.read_text()
        up.update_metainfo(self.path, "2.8.0", "2030-01-02")
        self.assertEqual(self.path.read_text(), before)

    def test_idempotent(self):
        up.update_metainfo(self.path, "9.9.9", "2030-01-02")
        after = self.path.read_text()
        up.update_metainfo(self.path, "9.9.9", "2030-01-03")
        self.assertEqual(self.path.read_text(), after)

    def test_missing_releases_block_raises(self):
        self.path.write_text(self.path.read_text().replace("<releases>", "<versions>"))
        with self.assertRaises(up.PatchError):
            up.update_metainfo(self.path, "9.9.9", "2030-01-02")


class TestCli(Case):
    def test_rejects_a_non_release_version(self):
        for bad in ("main", "2.8", "2.8.0-rc1", ""):
            sys.argv = ["update-packaging.py", "--version", bad]
            self.assertEqual(up.main(), 2, bad)

    def test_accepts_a_v_prefix(self):
        # No targets passed and the network stubbed out: only parsing is exercised.
        up.release_hashes, real = (lambda v: {}), up.release_hashes
        self.addCleanup(setattr, up, "release_hashes", real)
        sys.argv = ["update-packaging.py", "--version", "v2.8.0"]
        self.assertEqual(up.main(), 0)


if __name__ == "__main__":
    unittest.main(verbosity=2)

#!/usr/bin/env python3
"""Packaging input contracts only; these tests do not build or invent binaries."""
import importlib.util
from pathlib import Path
import subprocess
import tempfile
import unittest
from unittest.mock import patch
import json

SCRIPT = Path(__file__).with_name("package-native-fallback.py")
spec = importlib.util.spec_from_file_location("native_package", SCRIPT)
package = importlib.util.module_from_spec(spec)
spec.loader.exec_module(package)


class PackageInputs(unittest.TestCase):
    def test_current_source_inventory(self):
        files = package.source_inputs()
        self.assertIn(package.ROOT / "migrations/0027_requester_profile.sql", files)
        self.assertEqual(len([p for p in files if p.name == "main.rs"]), 2)

    def test_bridge_source_inventory_and_private_exclusion(self):
        with tempfile.TemporaryDirectory() as folder:
            root = Path(folder)
            directory = root / "native-d1-bridge"
            directory.mkdir()
            for name in package.BRIDGE_FILES:
                (directory / name).write_text("source " + name)
            (directory / "authority.json").write_text("private identity")
            (directory / ".dev.vars").write_text("private token")
            with patch.object(package, "ROOT", root):
                package.copy_bridge(root / "output")
            copied = root / "output/native-d1-bridge"
            self.assertEqual({p.name for p in copied.iterdir()}, set(package.BRIDGE_FILES))
            for name in package.BRIDGE_FILES:
                self.assertEqual((copied / name).read_bytes(), (directory / name).read_bytes())

    def test_missing_and_symlinked_bridge_source_refused(self):
        with tempfile.TemporaryDirectory() as folder:
            root = Path(folder)
            directory = root / "native-d1-bridge"
            directory.mkdir()
            with patch.object(package, "ROOT", root):
                with self.assertRaisesRegex(ValueError, "bridge source"):
                    package.bridge_inputs()
                for name in package.BRIDGE_FILES:
                    (directory / name).write_text("source")
                (directory / "worker.mjs").unlink()
                (directory / "worker.mjs").symlink_to(directory / "bridge.mjs")
                with self.assertRaisesRegex(ValueError, "bridge source"):
                    package.bridge_inputs()

    def test_generated_bridge_layout_resolves_main_and_imports(self):
        with tempfile.TemporaryDirectory() as folder:
            output = Path(folder) / "operator-config"
            script = package.ROOT / "native-d1-bridge/manifest.py"
            subprocess.run(["python3", str(script), "--database-id",
                            "00000000-0000-4000-8000-000000000001", "--schema-sha256", "1" * 64,
                            "--migrations", str(package.ROOT / "migrations"), "--output", str(output)], check=True)
            config = json.loads((output / "wrangler.json").read_text())
            self.assertEqual(config["main"], "worker.mjs")
            for name in ("worker.mjs", "bridge.mjs"):
                self.assertEqual((output / name).read_bytes(), (script.parent / name).read_bytes())
            self.assertEqual({p.name for p in output.iterdir()},
                             {"worker.mjs", "bridge.mjs", "authority.json", "wrangler.json"})

    def test_wrong_checkout_sha_refused_before_build(self):
        result = subprocess.run(["python3", str(SCRIPT), "--sha", "0" * 40, "--check-inputs"],
                                capture_output=True, text=True)
        self.assertEqual(result.returncode, 1)
        self.assertIn("checkout SHA mismatch", result.stderr)

    def test_missing_island_refused(self):
        with tempfile.TemporaryDirectory() as folder:
            with self.assertRaisesRegex(ValueError, "missing built island"):
                package.asset_inputs(Path(folder))

    def test_extra_private_asset_refused(self):
        with tempfile.TemporaryDirectory() as folder:
            root = Path(folder)
            (root / "assets").mkdir()
            (root / "assets/ghinvite-island.js").write_text("// input shape fixture")
            (root / "assets/test.wasm").write_bytes(b"input shape fixture, not executable")
            (root / ".dev.vars").write_text("private marker")
            with self.assertRaisesRegex(ValueError, "unexpected public asset"):
                package.asset_inputs(root)

    def test_symlink_input_refused(self):
        with tempfile.TemporaryDirectory() as folder:
            root = Path(folder)
            source = root / "source"
            source.write_text("marker")
            link = root / "link"
            link.symlink_to(source)
            with self.assertRaisesRegex(ValueError, "not a regular input"):
                package.add_file(link, root / "output")
            self.assertFalse((root / "output").exists())

    def test_outer_asset_directory_symlink_refused(self):
        with tempfile.TemporaryDirectory() as folder:
            root = Path(folder)
            (root / "real").mkdir()
            (root / "alias").symlink_to(root / "real", target_is_directory=True)
            with self.assertRaisesRegex(ValueError, "symlinked island directory"):
                package.asset_inputs((root / "alias").absolute())

    def test_owned_workspace_license_alias_copies_exact_bytes(self):
        with tempfile.TemporaryDirectory() as folder:
            root = Path(folder)
            owned = root / "workspace"
            child = owned / "crates/example"
            child.mkdir(parents=True)
            (owned / "LICENSE-MIT").write_bytes(b"owned exact license\n")
            alias = child / "LICENSE-MIT"
            alias.symlink_to("../../LICENSE-MIT")
            source = package.license_source(alias, owned)
            package.add_file(source, root / "artifact/LICENSE-MIT")
            self.assertEqual((root / "artifact/LICENSE-MIT").read_bytes(), b"owned exact license\n")

    def test_license_alias_escape_refused(self):
        with tempfile.TemporaryDirectory() as folder:
            root = Path(folder)
            owned = root / "workspace"
            owned.mkdir()
            (root / "outside").write_text("not owned")
            alias = owned / "LICENSE"
            alias.symlink_to("../outside")
            with self.assertRaisesRegex(ValueError, "escapes its owning"):
                package.license_source(alias, owned)


if __name__ == "__main__":
    unittest.main()

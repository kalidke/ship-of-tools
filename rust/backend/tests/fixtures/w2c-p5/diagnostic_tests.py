"""Local diagnostic controls: temporary files and doubles, no bus or manager."""

import contextlib
import io
import json
from pathlib import Path
import tempfile
import unittest
from unittest.mock import patch

import probe


def receipt_at(root):
    receipt = probe.Receipt.__new__(probe.Receipt)
    receipt.label = "diagnostic-control"
    receipt.root = root
    receipt.private = ["<private-home>", "<private-user>", "<private-host>"]
    receipt.data = {"checks": [], "commands": []}
    receipt.output = root / "receipt"
    receipt.output.mkdir()
    return receipt


class ExitedManager:
    # A double is never signalled and never passed to cleanup.
    pid = 999999999

    def poll(self):
        return 42


class DiagnosticTests(unittest.TestCase):
    def test_manager_failure_records_diagnostic_before_cleanup(self):
        with tempfile.TemporaryDirectory(prefix="w2c-diagnostic-") as directory:
            root = Path(directory).resolve()
            receipt = receipt_at(root)
            fixture = probe.Fixture(root, receipt)
            fixture.run = lambda *args: (0, "systemd 249\n", "")

            def spawn(args, tag):
                if tag == "bus":
                    fixture.runtime.joinpath("bus").touch()
                else:
                    root.joinpath("manager.log").write_text(
                        "<private-user> <private-host> <private-home> " + str(root) +
                        " Failed to allocate manager object\n")
                return ExitedManager()

            fixture.spawn = spawn
            observation = {"probe_cgroup": "0::/fixture-service\n",
                           "child_cgroup_created": False,
                           "child_cgroup_error": "PermissionError: diagnostic control"}
            # Keep the behavioral test compilable at the original fixture parent.
            with patch.object(probe, "delegation_observation", return_value=observation,
                              create=True), contextlib.redirect_stdout(io.StringIO()) as out:
                with self.assertRaisesRegex(RuntimeError, "exited before readiness"):
                    fixture.start()
            path = receipt.output / "receipt.json"
            self.assertTrue(path.is_file(), "readiness failure must persist diagnostic before cleanup")
            actual = json.loads(path.read_text())["startup"]
            self.assertEqual(actual["stage"], "readiness-failure")
            self.assertEqual(actual["manager_exit"], 42)
            self.assertFalse(actual["child_cgroup_created"])
            self.assertEqual(actual["probe_cgroup"], "0::/fixture-service\n")
            log = "\n".join(actual["manager_log_tail"])
            self.assertIn("Failed to allocate manager object", log)
            for private in (*receipt.private, str(root)):
                self.assertNotIn(private, path.read_text())
            self.assertIn("startup-diagnostic readiness-failure SAVED", out.getvalue())

    def test_child_cgroup_creation_is_observed_and_removed(self):
        with tempfile.TemporaryDirectory(prefix="w2c-diagnostic-") as directory:
            root = Path(directory).resolve()
            proc = root / "cgroup"
            proc.write_text("0::/fixture-service\n")
            service = root / "mount/fixture-service"
            service.mkdir(parents=True)
            actual = probe.delegation_observation(proc, root / "mount")
            self.assertTrue(actual["child_cgroup_created"])
            self.assertEqual(list(service.iterdir()), [])

    def test_creation_failure_is_named(self):
        with tempfile.TemporaryDirectory(prefix="w2c-diagnostic-") as directory:
            root = Path(directory).resolve()
            proc = root / "cgroup"
            proc.write_text("0::/fixture-service\n")
            actual = probe.delegation_observation(proc, root / "missing-mount")
            self.assertFalse(actual["child_cgroup_created"])
            self.assertIn("FileNotFoundError", actual["child_cgroup_error"])

    def test_unknown_hierarchy_is_reported(self):
        with tempfile.TemporaryDirectory(prefix="w2c-diagnostic-") as directory:
            root = Path(directory).resolve()
            proc = root / "cgroup"
            proc.write_text("1:name=systemd:/fixture-service\n")
            actual = probe.delegation_observation(proc, root / "mount")
            self.assertFalse(actual["child_cgroup_created"])
            self.assertIn("StopIteration", actual["child_cgroup_error"])


if __name__ == "__main__":
    unittest.main()

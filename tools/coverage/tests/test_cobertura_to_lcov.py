"""Fixture tests for Cobertura source-path resolution used by kcov."""

from __future__ import annotations

import importlib.util
import tempfile
import unittest
from pathlib import Path

MODULE_PATH = Path(__file__).parents[1] / "cobertura_to_lcov.py"
spec = importlib.util.spec_from_file_location("cobertura_to_lcov", MODULE_PATH)
assert spec and spec.loader
converter = importlib.util.module_from_spec(spec)
spec.loader.exec_module(converter)


class CoberturaToLcovTests(unittest.TestCase):
    def test_resolves_basename_against_single_file_source(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            source = root / "zig-lib" / "src" / "protocol.zig"
            source.parent.mkdir(parents=True)
            source.write_text("const answer = 42;\n", encoding="utf-8")
            report = root / "coverage.xml"
            report.write_text(
                """<?xml version='1.0'?>
<coverage><sources><source>{source}</source></sources><packages><package>
<classes><class filename='protocol.zig'><lines>
<line number='1' hits='3'/><line number='2' hits='0'/>
</lines></class></classes></package></packages></coverage>
""".format(source=source),
                encoding="utf-8",
            )

            files = converter.convert(report, [source.parent], root)

        self.assertEqual(
            files,
            {"zig-lib/src/protocol.zig": {1: 3, 2: 0}},
        )

    def test_writes_nonempty_lcov_for_resolved_source(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            source = root / "zig-lib" / "src" / "aiomsg.zig"
            source.parent.mkdir(parents=True)
            source.touch()
            report = root / "coverage.xml"
            report.write_text(
                """<coverage><sources><source>{source}</source></sources><packages><package>
<classes><class filename='aiomsg.zig'><lines><line number='7' hits='1'/></lines></class>
</classes></package></packages></coverage>""".format(source=source),
                encoding="utf-8",
            )
            output = root / "coverage.lcov"
            converter.write_lcov(converter.convert(report, [source.parent], root), output)
            text = output.read_text(encoding="utf-8")

        self.assertIn("SF:zig-lib/src/aiomsg.zig", text)
        self.assertIn("LH:1", text)
        self.assertIn("LF:1", text)


if __name__ == "__main__":
    unittest.main()

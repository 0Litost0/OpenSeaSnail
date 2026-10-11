#!/usr/bin/env python3
"""Observe current framework formats for M0; synthetic cases are labelled.

Rust --ignored is combined with --list and never executes ignored tests.
"""
import json
from pathlib import Path
import re
import subprocess
import unittest

ROOT = Path(__file__).resolve().parents[4]
OUT = ROOT / "dist/dev-package/m0-20261011"


class Synthetic(unittest.TestCase):
    def test_pass(self):
        self.assertTrue(True)

    @unittest.skip("synthetic skip for format observation")
    def test_skip(self):
        pass

    def test_fail(self):
        self.fail("synthetic failure for format observation")


class IdentityResult(unittest.TestResult):
    def __init__(self):
        super().__init__()
        self.identities = []

    def startTest(self, test):
        super().startTest(test)
        self.identities.append(test.id())


def main():
    result = IdentityResult()
    unittest.defaultTestLoader.loadTestsFromTestCase(Synthetic).run(result)
    assert result.testsRun == 3 and len(result.skipped) == 1 and len(result.failures) == 1
    evidence = {"python": {"synthetic": True, "identities": result.identities,
                           "skipped": [t.id() for t, _ in result.skipped],
                           "failed": [t.id() for t, _ in result.failures]}, "rust": []}
    executables = {}
    for line in (OUT / "cargo-test-enumeration-build.log").read_text().splitlines():
        if not line.startswith("{"):
            continue
        row = json.loads(line)
        if row.get("reason") == "compiler-artifact" and row.get("executable") and row["profile"]["test"]:
            executables[row["executable"]] = row["target"]["name"]
    for executable, name in executables.items():
        listed = subprocess.run([executable, "--list", "--format", "terse"], capture_output=True, text=True)
        ignored = subprocess.run([executable, "--ignored", "--list", "--format", "terse"], capture_output=True, text=True)
        assert listed.returncode == ignored.returncode == 0, (name, listed.stderr, ignored.stderr)
        tests = [s.removesuffix(": test") for s in listed.stdout.splitlines() if s.endswith(": test")]
        excluded = [s.removesuffix(": test") for s in ignored.stdout.splitlines() if s.endswith(": test")]
        evidence["rust"].append({"target": name, "executable": executable,
                                 "listed": tests, "ignored": excluded})
    assert evidence["rust"], "no test executables discovered"
    target = next(p for p, n in executables.items() if n == "normalizer_real")
    structured = subprocess.run([target, "--list", "--format", "json"], capture_output=True, text=True)
    evidence["rust_json_probe"] = {"exit_code": structured.returncode,
                                  "stdout": structured.stdout, "stderr": structured.stderr}
    vitest = json.loads((OUT / "vitest-report.json").read_text())
    assertions = [(t["fullName"], t["status"]) for suite in vitest["testResults"] for t in suite["assertionResults"]]
    assert assertions and all(name for name, _ in assertions)
    evidence["vitest"] = {"count": len(assertions), "identities_and_statuses": assertions}
    audit = []
    for base in ("crates", "apps/desktop/src-tauri"):
        for path in (ROOT / base).rglob("*.rs"):
            if "/gen/" in str(path):
                continue
            source = path.read_text()
            for pattern, kind in ((r"(?m)^\s*#\[ignore[^\n]*", "ignored_attribute"),
                                  (r'eprintln!\(\s*"skip:[^\n]*', "implicit_skip_message")):
                for match in re.finditer(pattern, source):
                    if kind == "ignored_attribute":
                        function = re.search(r"(?:async\s+)?fn\s+(\w+)", source[match.end():])
                    else:
                        functions = list(re.finditer(r"(?:async\s+)?fn\s+(\w+)", source[:match.start()]))
                        function = functions[-1] if functions else None
                    audit.append({"path": str(path.relative_to(ROOT)), "line": source[:match.start()].count("\n") + 1,
                                  "kind": kind, "function": function.group(1) if function else None,
                                  "marker": match.group(0).strip()})
    evidence["source_skip_audit"] = audit
    (OUT / "framework-format-details.json").write_text(json.dumps(evidence, indent=2) + "\n")
    print(json.dumps({"rust_binaries": len(evidence["rust"]),
                      "rust_listed_tests": sum(len(r["listed"]) for r in evidence["rust"]),
                      "rust_ignored_tests": sum(len(r["ignored"]) for r in evidence["rust"]),
                      "rust_json_probe_exit": structured.returncode, "vitest_tests": len(assertions),
                      "source_audit_markers": len(audit), "python_synthetic_expectations_met": True}))


if __name__ == "__main__":
    main()

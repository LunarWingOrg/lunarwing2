#!/usr/bin/env python3
"""JUnit XML emission for CI harnesses without external dependencies.

Writes a JUnit-compatible XML document from (suite, test_case, status)
records so GitLab can render test reports and per-testcase counts. Statuses
are "passed", "failed", or "skipped"; failures and skips carry an optional
message (traceback tails from shell harnesses, skip reasons from unittest).

All record text is sanitized before serialization: ANSI escape sequences are
stripped and every code point XML 1.0 forbids in character data
(U+0000-U+0008, U+000B, U+000C, U+000E-U+001F) is dropped instead of being
written raw, so harness output containing terminal colors or control bytes
can never produce a malformed report document.

Run ``python3 junit_report.py --self-test`` to verify emission: the self-test
writes a report containing deliberately colored/failing/skipped output and
parses it back with the standard-library XML parser.
"""

import pathlib
import re
import sys
import tempfile
import xml.etree.ElementTree as ET
from typing import Iterable

ANSI_ESCAPE_RE = re.compile(r"\x1b(?:[@-Z\\-_]|\[[0-?]*[ -/]*[@-~])")
XML_DISALLOWED_RE = re.compile("[\x00-\x08\x0b\x0c\x0e-\x1f]")


def _xml_safe(text: object) -> str:
    """Strip ANSI escapes and XML-disallowed control characters.

    Used for every text serialized into the document: attributes, failure
    messages, skip reasons, and captured system-out, so no harness byte
    sequence can produce invalid XML character data.
    """
    return XML_DISALLOWED_RE.sub("", ANSI_ESCAPE_RE.sub("", str(text)))


def record(
    suite: str,
    name: str,
    status: str,
    *,
    duration: float = 0.0,
    message: str = "",
    system_out: str = "",
) -> dict:
    """Build one JUnit record.

    ``duration`` is in seconds; ``message`` is failure text or a skip reason.
    """
    if status not in {"passed", "failed", "skipped"}:
        raise ValueError(f"invalid test status {status!r}")
    return {
        "suite": suite,
        "name": name,
        "status": status,
        "duration": duration,
        "message": message,
        "system_out": system_out,
    }


def suite_records(
    suite: str,
    failures: list[tuple[object, str]],
    skipped: list[tuple[object, str]],
    test_count: int,
) -> list[dict]:
    """Convert one unittest.TestResult into JUnit records.

    ``failures`` carries (test, traceback) pairs in run order, ``skipped``
    carries (test, skip_reason) pairs. Failed tests are emitted as individual
    testcases carrying their traceback, skipped tests preserve their skip
    reason, and the remaining executed tests are reported as individual
    passing testcases so JUnit totals match the real number of executed
    tests.
    """
    entries: list[dict] = []
    for test, traceback in failures:
        entries.append(record(suite, str(test), "failed", message=traceback))
    for test, reason in skipped:
        entries.append(record(suite, str(test), "skipped", message=reason))
    passed = test_count - len(failures) - len(skipped)
    entries.extend(
        record(suite, f"test.{index + 1}", "passed") for index in range(max(passed, 0))
    )
    return entries


def write(path: pathlib.Path, records: Iterable[dict]) -> None:
    """Serialize records into a JUnit-compatible XML document at ``path``."""
    entries = list(records)
    failures = sum(1 for entry in entries if entry["status"] == "failed")
    skipped = sum(1 for entry in entries if entry["status"] == "skipped")
    total = sum(float(entry.get("duration", 0.0)) for entry in entries)
    root = ET.Element(
        "testsuites",
        {
            "name": "lunarwing",
            "tests": str(len(entries)),
            "failures": str(failures),
            "skipped": str(skipped),
            "time": f"{total:.3f}",
        },
    )
    suites: dict[str, ET.Element] = {}
    for entry in entries:
        suite_name = _xml_safe(entry["suite"])
        suite_element = suites.get(entry["suite"])
        if suite_element is None:
            suite_element = ET.SubElement(root, "testsuite", {"name": suite_name})
            suites[entry["suite"]] = suite_element
        case = ET.SubElement(
            suite_element,
            "testcase",
            {
                "name": _xml_safe(entry["name"]),
                "classname": suite_name,
                "time": f"{float(entry.get('duration', 0.0)):.3f}",
            },
        )
        if entry["status"] == "failed":
            message = _xml_safe(entry["message"]) or "failed"
            ET.SubElement(case, "failure", {"message": message}).text = message
        elif entry["status"] == "skipped":
            reason = _xml_safe(entry["message"])
            if reason:
                ET.SubElement(case, "skipped", {"message": reason}).text = reason
            else:
                ET.SubElement(case, "skipped", {})
        captured = entry.get("system_out", "")
        if captured:
            ET.SubElement(case, "system-out").text = _xml_safe(captured[-200_000:])
    ET.indent(root)
    path.parent.mkdir(parents=True, exist_ok=True)
    ET.ElementTree(root).write(path, encoding="utf-8", xml_declaration=True)


def self_test(arguments: list[str]) -> bool:
    """Emit reports hostile to XML (ANSI colors, control bytes) and parse them back."""
    corrupt = arguments[:3] == ["--self-test", "corrupt", "controls"]
    if arguments != ["--self-test"] and not corrupt:
        print("usage: python3 junit_report.py --self-test", file=sys.stderr)
        return False
    with tempfile.TemporaryDirectory() as tmpdir:
        path = pathlib.Path(tmpdir) / "report.xml"
        passing = record("unit", "shows-as-reasonably-named", "passed", duration=0.25)
        failing = record(
            "unit",
            "boom-with-colors",
            "failed",
            duration=0.5,
            message="\x1b[31mFAILED\x1b[0m boom\n\x01control\x02bytes escaped",
        )
        if corrupt:
            passing = record("unit", "plain", "passed", duration=0.1)
            failing = record(
                "unit",
                "half\x1b[31m",
                "failed",
                duration=0.4,
                message="SAME\x01\x02\x1b[0mCONTROL\x00END",
            )
            records = [failing, passing]
        else:
            records = [
                passing,
                failing,
                record("unit", "later", "skipped", message="needs \x1b[0m network"),
            ]
        write(path, records)
        root = ET.parse(path).getroot()
        actual = {
            "testsuites": root.attrib["tests"],
            "failures": root.attrib["failures"],
            "skipped": root.attrib["skipped"],
        }
        expected = {"testsuites": str(len(records)), "failures": "1", "skipped": "0"}
        if not corrupt:
            expected["skipped"] = "1"
        if actual != expected:
            print(f"junit self-test FAILED: {actual} != {expected}", file=sys.stderr)
            return False
    print("junit_report self-test passed: hostile output parses as valid XML.")
    return True


def main(arguments: list[str]) -> int:
    if arguments == ["--self-test"] or arguments[:3] == ["--self-test", "corrupt", "controls"]:
        return 0 if self_test(arguments) else 1
    print("usage: python3 junit_report.py --self-test", file=sys.stderr)
    return 1


if __name__ == "__main__":
    raise SystemExit(main(sys.argv[1:]))
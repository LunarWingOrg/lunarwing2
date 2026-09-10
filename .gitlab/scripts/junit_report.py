#!/usr/bin/env python3
"""JUnit XML emission for CI harnesses without external dependencies.

Writes a minimal JUnit-compatible document from (suite_name, test_case,
status) records so GitLab can render test reports and counts. Status is
"passed", "failed", or "skipped"; failures carry their message.
"""

import pathlib
import xml.etree.ElementTree as ET
from collections.abc import Iterable

SCHEMA = "https://maven.apache.org/xsd/junit/JUnit.xsd"


def record(
    suite: str,
    name: str,
    status: str,
    *,
    duration: float = 0.0,
    message: str = "",
) -> dict:
    if status not in {"passed", "failed", "skipped"}:
        raise ValueError(f"invalid test status {status!r}")
    return {
        "suite": suite,
        "name": name,
        "status": status,
        "duration": duration,
        "message": message,
    }


def write(path: pathlib.Path, records: Iterable[dict]) -> None:
    entries = list(records)
    failures = sum(1 for entry in entries if entry["status"] == "failed")
    skipped = sum(1 for entry in entries if entry["status"] == "skipped")
    total = sum(entry["duration"] for entry in entries)
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
        suite_element = suites.get(entry["suite"])
        if suite_element is None:
            suite_element = ET.SubElement(root, "testsuite", {"name": entry["suite"]})
            suites[entry["suite"]] = suite_element
        case = ET.SubElement(
            suite_element,
            "testcase",
            {"name": entry["name"], "classname": entry["suite"], "time": f"{entry['duration']:.3f}"},
        )
        if entry["status"] == "failed":
            ET.SubElement(case, "failure", {"message": entry["message"][:4000] or "failed"})
        elif entry["status"] == "skipped":
            ET.SubElement(case, "skipped", {})
    ET.indent(root)
    path.parent.mkdir(parents=True, exist_ok=True)
    ET.ElementTree(root).write(path, encoding="utf-8", xml_declaration=True)

#!/usr/bin/env python3

import io
import pathlib
import subprocess
import tarfile
import tempfile
import unittest

ROOT = pathlib.Path(__file__).resolve().parents[2]
SCRIPT = ROOT / ".gitlab/scripts/build-worker-image.sh"


def layer(path: pathlib.Path, content: bytes) -> None:
    with tarfile.open(path, "w:gz") as archive:
        for name, data in (("same.txt", b"same\n"), ("different.txt", content)):
            member = tarfile.TarInfo(name)
            member.size = len(data)
            archive.addfile(member, io.BytesIO(data))


class WorkerLayerDiagnosisTests(unittest.TestCase):
    def test_first_differing_layer_reports_the_changed_file(self):
        with tempfile.TemporaryDirectory() as temporary:
            root = pathlib.Path(temporary)
            layer(root / "candidate.tar.gz", b"candidate\n")
            layer(root / "rebuild.tar.gz", b"rebuild\n")
            work = root / "work"
            work.mkdir()
            text = SCRIPT.read_text(encoding="utf-8")
            start = text.index("print_first_layer_file_diff()")
            functions = text[start:text.index("# WI#425:", start)]
            script = functions + r'''
wget() {
    output=
    while [ "$#" -gt 0 ]; do
        case "$1" in
            -O) output=$2; shift 2 ;;
            -T|--header) shift 2 ;;
            -q) shift ;;
            http*) url=$1; shift ;;
            *) shift ;;
        esac
    done
    case "$url" in
        */candidate) cp "$FIXTURE_DIR/candidate.tar.gz" "$output" ;;
        */rebuild) cp "$FIXTURE_DIR/rebuild.tar.gz" "$output" ;;
        *) return 1 ;;
    esac
}
CI_REGISTRY=registry.invalid
export CI_REGISTRY FIXTURE_DIR
print_first_layer_file_diff token example/worker candidate rebuild "$FIXTURE_DIR/work"
'''
            result = subprocess.run(
                ["/bin/sh", "-eu", "-c", script], env={"FIXTURE_DIR": str(root), "PATH": "/usr/bin:/bin"},
                capture_output=True, text=True, timeout=30, check=False,
            )
            self.assertEqual(result.returncode, 0, result.stderr)
            inventories = (work / "candidate-files").read_text() + (work / "rebuild-files").read_text()
            self.assertIn("different.txt", result.stderr, f"{result.stderr}\n{inventories}")
            changed_same = [
                line for line in result.stderr.splitlines()
                if line.startswith(("+", "-")) and not line.startswith(("+++", "---")) and "same.txt" in line
            ]
            self.assertEqual(changed_same, [])
            self.assertIn("candidate-layer.tar.gz", "\n".join(path.name for path in work.iterdir()))


if __name__ == "__main__":
    unittest.main()

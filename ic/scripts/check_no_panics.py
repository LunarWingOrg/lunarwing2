#!/usr/bin/env python3
# Requires Python 3.10+ for PEP 604 union syntax such as `int | None`.

import argparse
import pathlib
import re
import subprocess
import sys
import tempfile
import unittest
from dataclasses import dataclass
from unittest import mock


PANIC_PATTERN = re.compile(r"\.(?:unwrap|expect)\(|(?<!_)assert(?:_eq|_ne)?!")
PROJECT_ROOT = pathlib.Path(__file__).resolve().parents[1]
TEST_ATTR_PATTERN = re.compile(
    r"^\s*#\s*\[\s*(?:"
    r"test"
    r"|tokio::test(?:\s*\([^]]*\))?"
    r"|rstest(?:\s*\([^]]*\))?"
    r"|test_case(?:\s*\([^]]*\))?"
    r"|cfg\s*\([^]]*\btest\b[^]]*\)"
    r")\s*\]"
)
ITEM_PATTERN = re.compile(
    r"^\s*"
    r"(?:(?:pub(?:\([^)]*\))?|crate)\s+)?"
    r"(?:(?:async|unsafe|const)\s+)*"
    r"(fn|mod|struct|enum|trait|union|impl)\b"
    r"(?:\s+([A-Za-z_][A-Za-z0-9_]*))?"
)


@dataclass
class LexerState:
    block_comment_depth: int = 0
    in_string: bool = False
    string_escape: bool = False
    in_char: bool = False
    char_escape: bool = False
    raw_string_hashes: int | None = None


def run_git(*args: str) -> str:
    result = subprocess.run(
        ["git", *args],
        check=True,
        capture_output=True,
        text=True,
        cwd=PROJECT_ROOT,
    )
    return result.stdout


def sanitize_line(line: str, state: LexerState) -> str:
    chars = list(line)
    out = [" "] * len(chars)
    i = 0

    while i < len(chars):
        ch = chars[i]
        nxt = chars[i + 1] if i + 1 < len(chars) else ""

        if state.block_comment_depth:
            if ch == "/" and nxt == "*":
                state.block_comment_depth += 1
                i += 2
                continue
            if ch == "*" and nxt == "/":
                state.block_comment_depth -= 1
                i += 2
                continue
            i += 1
            continue

        if state.raw_string_hashes is not None:
            if ch == '"':
                hashes = 0
                j = i + 1
                while j < len(chars) and chars[j] == "#":
                    hashes += 1
                    j += 1
                if hashes == state.raw_string_hashes:
                    state.raw_string_hashes = None
                    i = j
                    continue
            i += 1
            continue

        if state.in_string:
            if state.string_escape:
                state.string_escape = False
            elif ch == "\\":
                state.string_escape = True
            elif ch == '"':
                state.in_string = False
            i += 1
            continue

        if state.in_char:
            if state.char_escape:
                state.char_escape = False
            elif ch == "\\":
                state.char_escape = True
            elif ch == "'":
                state.in_char = False
            i += 1
            continue

        if ch == "/" and nxt == "/":
            break
        if ch == "/" and nxt == "*":
            state.block_comment_depth += 1
            i += 2
            continue
        if ch == "r":
            j = i + 1
            while j < len(chars) and chars[j] == "#":
                j += 1
            if j < len(chars) and chars[j] == '"':
                state.raw_string_hashes = j - i - 1
                i = j + 1
                continue
        if ch == '"':
            state.in_string = True
            i += 1
            continue
        if ch == "'":
            # Distinguish char literals (`'a'`) from lifetimes (`'a`): a
            # lifetime is `'` + identifier start where the following char is
            # not a closing quote. Treating lifetimes as char literals masked
            # code after them (false negatives) and desynced the lexer.
            if i + 2 < len(chars) and chars[i + 1] != "'" and chars[i + 2] == "'":
                # Char literal like 'a' or '\'' (escape handled below).
                if chars[i + 1] == "\\":
                    # The opening quote and the backslash are consumed, so the
                    # next char is the escaped content: re-enter with the
                    # escape pending. Entering with char_escape=False lets the
                    # closing quote of '\'' be swallowed as content (and the
                    # content quote terminate the literal early), desyncing
                    # every later string/char boundary in the file.
                    state.in_char = True
                    state.char_escape = True
                    i += 2
                    continue
                i += 3
                continue
            if i + 3 < len(chars) and chars[i + 1] == "\\" and chars[i + 3] == "'":
                # Escaped char literal like '\n' or '\\'. Same rule: the
                # backslash is already consumed, so resume with the escape
                # pending — otherwise the second backslash of '\\' starts a
                # new escape, the closing quote is eaten as escaped content,
                # and the lexer stays in_char until the next quote,
                # desyncing string state for the rest of the file (this hid
                # the `mod tests` boundary in orchestrator/harness.rs).
                state.in_char = True
                state.char_escape = True
                i += 2
                continue
            if i + 1 < len(chars) and (chars[i + 1].isalnum() or chars[i + 1] == "_"):
                # Lifetime like 'a or 'callback: leave the quote and the label
                # visible in the code so a lifetime-delineated type never
                # hides a following struct body — swallowing the quote as an
                # unterminated char literal used to desync the brace count.
                out[i] = ch
                i += 1
                continue
            # Any other quote (e.g. a stray apostrophe in a macro or a
            # mismatched closer) cannot be classified without a full parser.
            # Emit it and move on rather than entering a char-literal state
            # that would swallow the rest of the file.
            out[i] = ch
            i += 1
            continue

        out[i] = ch
        i += 1

    return "".join(out)


def is_test_item(line: str, pending_test_attr: bool) -> tuple[bool, bool]:
    match = ITEM_PATTERN.match(line)
    if not match:
        return False, False

    kind, name = match.groups()
    named_tests_module = kind == "mod" and name == "tests"
    return True, pending_test_attr or named_tests_module


def brace_based_panic_scan(lines: list[str]) -> set[int]:
    """Fallback line ranges for parse-sensitive guards that cannot classify
    source structure reliably. Returns line numbers (1-based) inside the body
    of a test attribute (`#[test]`, `#[cfg(test)]`, `#[tokio::test]`, …) —
    matching the production/test boundary the checker enforces.

    Whenever a test attribute is seen, the first following line containing
    `{` opens the body; the body extends until its closing `}` balances the
    brace count. A bare `mod tests {` counts as a body opener even without a
    test attribute, so wrapped test modules stay unmarked for panics.
    """
    marked_lines: set[int] = set()
    lexer = LexerState()
    depth = 0
    pending_attr = False

    for line_no, raw in enumerate(lines, start=1):
        code = sanitize_line(raw, lexer)
        stripped = code.strip()

        if depth > 0:
            marked_lines.add(line_no)
            depth += code.count("{") - code.count("}")
            if depth <= 0:
                depth = 0
            continue

        if TEST_ATTR_PATTERN.match(stripped):
            pending_attr = True
            continue

        if pending_attr:
            pending_attr = False
            if "{" in code:
                depth = 1
                marked_lines.add(line_no)
                continue
            if stripped and stripped.endswith(";"):
                continue
            pending_attr = True
            continue

        if "{" in code and re.match(r"^\s*mod\s+tests\b", stripped):
            depth = 1
            marked_lines.add(line_no)

    return marked_lines


def line_test_contexts(lines: list[str]) -> list[bool]:
    contexts = [False] * len(lines)
    lexer = LexerState()
    block_stack: list[bool] = []
    pending_test_attr = False
    pending_block_context: bool | None = None

    for idx, raw in enumerate(lines):
        code = sanitize_line(raw, lexer)
        stripped = code.strip()
        current_context = block_stack[-1] if block_stack else False

        if TEST_ATTR_PATTERN.match(stripped):
            pending_test_attr = True

        item_found, item_is_test = is_test_item(code, pending_test_attr)
        if item_found:
            pending_block_context = item_is_test or current_context
            pending_test_attr = False
        elif stripped and not stripped.startswith("#[") and pending_test_attr:
            pending_test_attr = False

        contexts[idx] = current_context or bool(pending_block_context)

        for ch in code:
            if ch == "{":
                if pending_block_context is not None:
                    block_stack.append(pending_block_context)
                    pending_block_context = None
                else:
                    block_stack.append(block_stack[-1] if block_stack else False)
            elif ch == "}" and block_stack:
                block_stack.pop()

        if stripped.endswith(";"):
            pending_block_context = None

    return contexts


def changed_rust_files(base: str, head: str) -> list[pathlib.Path]:
    output = run_git(
        "diff",
        "--name-only",
        "--relative",
        f"{base}...{head}",
        "--",
        "src",
        "crates",
    )
    files = []
    for line in output.splitlines():
        if line.endswith(".rs") and (line.startswith("src/") or line.startswith("crates/")):
            files.append(pathlib.Path(line))
    return files


def added_lines_for_file(base: str, head: str, path: pathlib.Path) -> set[int]:
    diff = run_git("diff", "--unified=0", f"{base}...{head}", "--", str(path))
    added: set[int] = set()
    current_line = 0

    for line in diff.splitlines():
        if line.startswith("@@"):
            match = re.search(r"\+(\d+)(?:,(\d+))?", line)
            if not match:
                continue
            current_line = int(match.group(1))
            continue
        if line.startswith("+++ ") or line.startswith("--- "):
            continue
        if line.startswith("+"):
            added.add(current_line)
            current_line += 1
        elif line.startswith("-"):
            continue
        else:
            current_line += 1

    return added


def is_fuzz_target_path(path: pathlib.Path) -> bool:
    # cargo-fuzz targets (<crate>/fuzz/fuzz_targets/*.rs) are test-only
    # harnesses built by a separate fuzzing workspace and never linked into
    # production binaries; asserting invariants is their entire purpose.
    parts = path.parts
    return any(
        parts[i] == "fuzz" and parts[i + 1] == "fuzz_targets"
        for i in range(len(parts) - 1)
    )


def collect_violations(base: str, head: str) -> list[tuple[str, int, str]]:
    violations: list[tuple[str, int, str]] = []

    for path in changed_rust_files(base, head):
        if is_fuzz_target_path(path):
            continue
        source_path = PROJECT_ROOT / path
        if not source_path.exists():
            continue
        added_lines = added_lines_for_file(base, head, path)
        if not added_lines:
            continue

        lines = source_path.read_text(encoding="utf-8").splitlines()
        contexts = line_test_contexts(lines)
        test_context_lines = brace_based_panic_scan(lines)
        lexer = LexerState()
        sanitized = [sanitize_line(line, lexer) for line in lines]

        for line_no in sorted(added_lines):
            if line_no < 1 or line_no > len(lines):
                continue
            if contexts[line_no - 1] or line_no in test_context_lines:
                continue
            if "// safety:" in lines[line_no - 1]:
                continue
            if PANIC_PATTERN.search(sanitized[line_no - 1]):
                violations.append((str(path), line_no, lines[line_no - 1].rstrip()))

    return violations


def report(violations: list[tuple[str, int, str]]) -> int:
    if not violations:
        print("OK: No panic-inducing calls in changed production code.")
        return 0

    print("::error::Found panic-style calls outside test-only Rust code.")
    print("Production code must use proper error handling instead of panicking.")
    print("Suppress false positives with an inline '// safety: <reason>' comment.")
    print("")
    for path, line_no, line in violations[:20]:
        print(f"{path}:{line_no}: {line}")
    print("")
    print(f"Total: {len(violations)} violation(s)")
    return 1


def main() -> int:
    parser = argparse.ArgumentParser()
    parser.add_argument("--base")
    parser.add_argument("--head", required=False, default="HEAD")
    parser.add_argument("--self-test", action="store_true")
    args = parser.parse_args()

    if args.self_test:
        suite = unittest.defaultTestLoader.loadTestsFromTestCase(CheckNoPanicsTests)
        result = unittest.TextTestRunner(verbosity=2).run(suite)
        return 0 if result.wasSuccessful() else 1

    if not args.base:
        parser.error("--base is required unless --self-test is used")

    violations = collect_violations(args.base, args.head)
    return report(violations)


class CheckNoPanicsTests(unittest.TestCase):
    def test_collect_violations_reads_core_relative_paths(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            project_root = pathlib.Path(directory)
            source = project_root / "src" / "example.rs"
            source.parent.mkdir()
            source.write_text('fn prod() { value.expect("boom"); }\n', encoding="utf-8")

            with (
                mock.patch(f"{__name__}.PROJECT_ROOT", project_root),
                mock.patch(
                    f"{__name__}.changed_rust_files",
                    return_value=[pathlib.Path("src/example.rs")],
                ),
                mock.patch(f"{__name__}.added_lines_for_file", return_value={1}),
            ):
                violations = collect_violations("base", "head")

        self.assertEqual(
            violations,
            [("src/example.rs", 1, 'fn prod() { value.expect("boom"); }')],
        )

    def test_run_git_is_anchored_to_core_project(self) -> None:
        completed = subprocess.CompletedProcess(args=[], returncode=0, stdout="")
        with mock.patch.object(subprocess, "run", return_value=completed) as run:
            run_git("status", "--short")

        run.assert_called_once_with(
            ["git", "status", "--short"],
            check=True,
            capture_output=True,
            text=True,
            cwd=PROJECT_ROOT,
        )

    def test_changed_files_are_relative_to_core_project(self) -> None:
        output = "src/agent.rs\ncrates/example/src/lib.rs\nic/src/wrong.rs\nREADME.md\n"
        with mock.patch(f"{__name__}.run_git", return_value=output) as run:
            files = changed_rust_files("base", "head")

        self.assertEqual(
            files,
            [pathlib.Path("src/agent.rs"), pathlib.Path("crates/example/src/lib.rs")],
        )
        run.assert_called_once_with(
            "diff",
            "--name-only",
            "--relative",
            "base...head",
            "--",
            "src",
            "crates",
        )

    def test_cfg_test_module_marks_inner_lines(self) -> None:
        lines = [
            "#[cfg(test)]\n",
            "mod tests {\n",
            "    assert!(true);\n",
            "}\n",
            "fn prod() {\n",
            "    value.expect(\"boom\");\n",
            "}\n",
        ]

        contexts = line_test_contexts(lines)

        self.assertTrue(contexts[1])
        self.assertTrue(contexts[2])
        self.assertFalse(contexts[4])
        self.assertFalse(contexts[5])

    def test_test_function_marks_body_only(self) -> None:
        lines = [
            "#[test]\n",
            "fn it_works(\n",
            ") {\n",
            "    assert_eq!(2 + 2, 4);\n",
            "}\n",
            "fn prod() {\n",
            "    assert!(ready);\n",
            "}\n",
        ]

        contexts = line_test_contexts(lines)

        self.assertTrue(contexts[1])
        self.assertTrue(contexts[2])
        self.assertTrue(contexts[3])
        self.assertFalse(contexts[5])
        self.assertFalse(contexts[6])

    def test_proc_macro_test_attrs_mark_body_only(self) -> None:
        attrs = [
            "tokio::test",
            'tokio::test(flavor = "multi_thread", worker_threads = 4)',
            "rstest",
            "test_case(1, 2)",
            "cfg(all(test, unix))",
        ]

        for attr in attrs:
            with self.subTest(attr=attr):
                lines = [
                    f"#[{attr}]\n",
                    "fn it_works() {\n",
                    '    value.expect("allowed in test");\n',
                    "}\n",
                    "fn prod() {\n",
                    '    value.expect("boom");\n',
                    "}\n",
                ]

                contexts = line_test_contexts(lines)

                self.assertTrue(contexts[1])
                self.assertTrue(contexts[2])
                self.assertFalse(contexts[4])
                self.assertFalse(contexts[5])

    def test_named_tests_module_marks_context(self) -> None:
        lines = [
            "mod tests {\n",
            "    fn helper() {\n",
            "        assert!(true);\n",
            "    }\n",
            "}\n",
        ]

        contexts = line_test_contexts(lines)

        self.assertTrue(all(contexts))

    def test_lifetime_before_test_module_does_not_mask_it(self) -> None:
        # A lifetime in production code (e.g. a struct with `'a`) must not be
        # lexed as an unterminated char literal, which used to desync the
        # lexer and hide the `#[cfg(test)] mod tests` boundary — flagging
        # test-only asserts as production panics.
        lines = [
            "struct SidecarRequest<'a> {\n",
            "    audio: &'a str,\n",
            "}\n",
            "#[cfg(test)]\n",
            "mod tests {\n",
            "    #[test]\n",
            "    fn it_works() {\n",
            "        assert!(true);\n",
            "    }\n",
            "}\n",
        ]

        contexts = line_test_contexts(lines)

        self.assertFalse(any(contexts[:4]))
        self.assertTrue(all(contexts[4:]))

    def test_stray_quote_does_not_swallow_following_code(self) -> None:
        # A quote the lexer cannot classify (e.g. a stray apostrophe in a
        # macro argument) must not open an unterminated char-literal state:
        # that used to swallow every following line, desyncing the brace
        # count so test bodies were flagged as production panics.
        lines = [
            "macro_rules! odd { ($x:expr) => { $x } }\n",
            "odd!(');\n",
            "#[test]\n",
            "fn it_works() {\n",
            "    assert!(true);\n",
            "}\n",
        ]

        contexts = line_test_contexts(lines)

        self.assertTrue(contexts[3])
        self.assertTrue(contexts[4])

    def test_escaped_backslash_char_does_not_desync_lexer(self) -> None:
        # A char literal whose escaped content is itself a backslash ('\\')
        # enters in_char with the escape already consumed; without the
        # pending-escape state the closing quote was eaten as content and the
        # next quote ended the "literal", desyncing string state for the rest
        # of the file and hiding the `mod tests` boundary (MR !1510 review).
        lines = [
            "fn validate(path: &str) -> bool {\n",
            "    path.split(['/', '\\\\']).count() > 1\n",
            "}\n",
            "#[cfg(test)]\n",
            "mod tests {\n",
            "    #[test]\n",
            "    fn it_works() {\n",
            "        assert!(true);\n",
            "    }\n",
            "}\n",
        ]

        contexts = line_test_contexts(lines)

        self.assertFalse(any(contexts[:4]))
        self.assertTrue(all(contexts[4:]))

    def test_escaped_quote_char_does_not_desync_lexer(self) -> None:
        # '\'' likewise resumes on the escaped quote with the escape pending;
        # otherwise the content quote closes the literal early and the real
        # closing quote opens a phantom one.
        lines = [
            "fn is_quoted(ch: char) -> bool {\n",
            "    ch == '\\''\n",
            "}\n",
            "#[cfg(test)]\n",
            "mod tests {\n",
            "}\n",
        ]

        contexts = line_test_contexts(lines)

        self.assertFalse(any(contexts[:4]))
        self.assertTrue(all(contexts[4:]))

    def test_brace_scan_marks_test_attribute_bodies(self) -> None:
        lines = [
            "#[test]\n",
            "fn it_works() {\n",
            "    assert!(true);\n",
            "}\n",
            "fn prod() {\n",
            '    value.expect("boom");\n',
            "}\n",
        ]

        marked = brace_based_panic_scan(lines)

        self.assertEqual(marked, {2, 3, 4})

    def test_brace_scan_marks_nested_test_bodies(self) -> None:
        lines = [
            "#[cfg(test)]\n",
            "mod tests {\n",
            "    #[test]\n",
            "    fn it_works() {\n",
            "        assert!(true);\n",
            "    }\n",
            "}\n",
            "fn prod() {\n",
            '    value.expect("boom");\n',
            "}\n",
        ]

        marked = brace_based_panic_scan(lines)

        self.assertEqual(marked, {2, 3, 4, 5, 6, 7})

    def test_brace_scan_marks_bare_tests_module(self) -> None:
        lines = [
            "mod tests {\n",
            "    fn helper() {\n",
            "        assert!(true);\n",
            "    }\n",
            "}\n",
            "fn prod() {\n",
            '    value.expect("boom");\n',
            "}\n",
        ]

        marked = brace_based_panic_scan(lines)

        self.assertEqual(marked, {1, 2, 3, 4, 5})

    def test_collect_violations_uses_brace_scan_fallback(self) -> None:
        source_text = (
            "macro_rules! odd { ($x:expr) => { $x } }\n"
            "odd!(');\n"
            "#[test]\n"
            "fn it_works() {\n"
            "    assert!(true);\n"
            "}\n"
            "fn prod() {\n"
            '    value.expect("boom");\n'
            "}\n"
        )
        with tempfile.TemporaryDirectory() as directory:
            project_root = pathlib.Path(directory)
            source = project_root / "src" / "example.rs"
            source.parent.mkdir()
            source.write_text(source_text, encoding="utf-8")

            with (
                mock.patch(f"{__name__}.PROJECT_ROOT", project_root),
                mock.patch(
                    f"{__name__}.changed_rust_files",
                    return_value=[pathlib.Path("src/example.rs")],
                ),
                mock.patch(
                    f"{__name__}.added_lines_for_file",
                    return_value={2, 4, 5, 8},
                ),
            ):
                violations = collect_violations("base", "head")

        self.assertEqual(
            violations,
            [("src/example.rs", 8, '    value.expect("boom");')],
        )

    def test_collect_violations_skips_fuzz_targets(self) -> None:
        # Fuzz targets are test-only harnesses (cargo-fuzz workspace, never
        # linked into production binaries): their asserts must not be flagged
        # even when an MR adds new ones (pipeline 5107 false failure).
        source_text = (
            "#![no_main]\n"
            "use libfuzzer_sys::fuzz_target;\n"
            'fuzz_target!(|data: &[u8]| {\n'
            "    assert!(data.len() <= 1024);\n"
            "});\n"
        )
        with tempfile.TemporaryDirectory() as directory:
            project_root = pathlib.Path(directory)
            source = (
                project_root
                / "crates"
                / "lunarwing_safety"
                / "fuzz"
                / "fuzz_targets"
                / "fuzz_example.rs"
            )
            source.parent.mkdir(parents=True)
            source.write_text(source_text, encoding="utf-8")

            with (
                mock.patch(f"{__name__}.PROJECT_ROOT", project_root),
                mock.patch(
                    f"{__name__}.changed_rust_files",
                    return_value=[
                        pathlib.Path(
                            "crates/lunarwing_safety/fuzz/fuzz_targets/fuzz_example.rs"
                        )
                    ],
                ),
                mock.patch(f"{__name__}.added_lines_for_file", return_value={4}),
            ):
                violations = collect_violations("base", "head")

        self.assertEqual(violations, [])


if __name__ == "__main__":
    sys.exit(main())

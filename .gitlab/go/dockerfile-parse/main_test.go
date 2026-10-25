package main

import (
	"strings"
	"testing"

	"github.com/moby/buildkit/frontend/dockerfile/parser"
)

// The bash grammar accumulated ten parser-divergence fixes (FQ-3, GD-1,
// GJ-1/GJ-2, GW-2, GX-1, GZ-1, HD-1) because it re-parsed the Dockerfile.
// These tests prove the BuildKit parser (this tool) sees through the shapes
// that used to diverge, so the policy applied to BuildKit's own instruction
// list cannot be fooled the way the bash text-matching could.

func parse(t *testing.T, src string) []string {
	t.Helper()
	res, err := parser.Parse(strings.NewReader(src))
	if err != nil {
		t.Fatalf("BuildKit parse failed: %v", err)
	}
	var out []string
	for _, child := range res.AST.Children {
		out = append(out, child.Original)
	}
	return out
}

func instructionNames(instrs []string) []string {
	var out []string
	for _, line := range instrs {
		out = append(out, instructionName(line))
	}
	return out
}

func hasInstruction(instrs []string, want string) bool {
	cwant := canonical(want)
	for _, line := range instrs {
		if canonical(line) == cwant {
			return true
		}
	}
	return false
}

func TestBomOnLaterLineIsContent(t *testing.T) {
	// GX-1/GD-1: a BOM on a later line reaches instruction content; the parser
	// keeps it in the instruction's arg, never treating it as a directive.
	instrs := parse(t, "FROM scratch\nRUN echo ok\n\xef\xbb\xbf#=hidden\n")
	// The BOM-prefixed comment line is still consumed as a comment by BuildKit
	// only at directive position; on a later line it is content. The key
	// assertion: the parser does not drop it into a LABEL.
	for _, line := range instrs {
		if strings.Contains(line, "hidden") && instructionName(line) == "LABEL" {
			t.Fatalf("bom comment line became a LABEL: %q", line)
		}
	}
}

func TestEscapedEscapeTokenIsNotAContinuation(t *testing.T) {
	// GJ-2: a line ending in two backslashes is NOT a continuation for
	// BuildKit, so the next instruction is its own line (the bash guard used
	// to join them, hiding a RUN inside a LABEL).
	instrs := parse(t, "FROM scratch\nLABEL x=1\\\\\nRUN malicious\n")
	names := instructionNames(instrs)
	if !containsString(names, "LABEL") || !containsString(names, "RUN") {
		t.Fatalf("escaped-escape continuation hid an instruction: %v", names)
	}
}

func TestGluedContinuationSplittingAToken(t *testing.T) {
	// GZ-1: BuildKit keeps the char before the backslash and appends with no
	// separator, so `SHEL\` + `L [...]` is one SHELL instruction. The parser
	// sees SHELL; the bash guard used to see the word "SHEL".
	instrs := parse(t, "FROM scratch\nSHEL\\\nL [\"/bin/sh\", \"-c\"]\n")
	names := instructionNames(instrs)
	if !containsString(names, "SHELL") {
		t.Fatalf("glued SHEL+L did not parse as SHELL: %v", names)
	}
}

func TestTrailingWhitespaceAfterBackslash(t *testing.T) {
	// GW-2: trailing whitespace after a single backslash is still a valid
	// continuation for BuildKit, so the joined instruction runs. The parser
	// keeps the join; the bash guard used to count a separate RUN.
	src := "FROM scratch\nRUN echo one \\   \n && echo two\n"
	instrs := parse(t, src)
	found := false
	for _, line := range instrs {
		if instructionName(line) == "RUN" && strings.Contains(line, "echo two") {
			found = true
		}
	}
	if !found {
		t.Fatalf("continuation with trailing whitespace lost the joined RUN: %v", instrs)
	}
}

func TestHeredocBodyCannotSwallowAnInstruction(t *testing.T) {
	// GW-2 heredoc shape: an instruction inside a heredoc body is data, not an
	// instruction. The parser does not emit it as a top-level instruction.
	instrs := parse(t, "FROM scratch\nRUN cat <<EOF\nRUN hidden\nEOF\n")
	for _, line := range instrs {
		if canonical(line) == canonical("RUN hidden") {
			t.Fatalf("heredoc body emitted as an instruction: %v", instrs)
		}
	}
}

func TestParserDirectiveIsNotAnInstruction(t *testing.T) {
	// FQ-3: a parser directive changes how Docker parses, but the policy
	// refuses SHELL/ENV/ARG overrides in BuildKit's own instruction list. The
	// directive itself is not an instruction.
	instrs := parse(t, "# escape=`\nFROM scratch\n")
	for _, line := range instrs {
		if instructionName(line) == "" && strings.Contains(line, "escape=") {
			t.Fatalf("directive leaked into instructions: %q", line)
		}
	}
}

func containsString(haystack []string, want string) bool {
	for _, s := range haystack {
		if s == want {
			return true
		}
	}
	return false
}
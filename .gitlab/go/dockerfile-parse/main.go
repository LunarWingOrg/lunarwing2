// Command dockerfile-parse applies the LunarWing worker exposure policy to a
// Dockerfile using BuildKit's own frontend parser (WI#425 Ask 4).
//
// The prior bash re-implementation of the Dockerfile grammar accumulated ten
// parser-divergence fixes (FQ-3, GD-1, GJ-1/GJ-2, GW-2, GX-1, GZ-1, HD-1 and
// friends) because it re-parsed the file instead of asking BuildKit. Running
// the pinned buildkit module's parser here removes that entire class: the
// grammar is BuildKit's, so a string the parser does not see as a SHELL/ENV/
// ARG override cannot hide one from the policy either.
//
// This is a cross-check to the bash grammar in
// check-worker-exposure-policy.sh, not its replacement yet: both must pass.
// The policy invariants below mirror the guard's Dockerfile-wiring checks.
package main

import (
	"bytes"
	"fmt"
	"os"
	"strings"

	"github.com/moby/buildkit/frontend/dockerfile/parser"
)

const (
	wrapperBinary = "COPY --from=builder /out/bin/opencode /usr/local/lib/opencode2/opencode"
	wrapperScan   = "RUN install -D -m 0755 /app/scripts/opencode-exposure-guard.sh /usr/local/bin/opencode && python3 /app/scripts/exposure_policy_assert.py --lane /app --profile opencode2 --binary /usr/local/lib/opencode2/opencode"
	filesystem    = "RUN python3 /app/scripts/exposure_policy_assert.py --lane /app --profile opencode2 --image-filesystem"
	compileCopy   = "COPY scripts/compile_out_remote_tunnel.py /build/compile_out_remote_tunnel.py"
	compileRun    = "RUN python3 /build/compile_out_remote_tunnel.py /build/opencode"
	guardedBuild  = "RUN OPENCODE_VERSION=\"${OPENCODE_VERSION#v}\" bun run build -- --single"
)

type failure struct{ msg string }

func (f failure) Error() string { return f.msg }

func fail(format string, args ...any) error {
	return failure{msg: fmt.Sprintf(format, args...)}
}

func instructionName(line string) string {
	fields := strings.Fields(line)
	if len(fields) == 0 {
		return ""
	}
	return strings.ToUpper(fields[0])
}

// canonical collapses whitespace runs (including continuation indentation
// BuildKit preserves) to single spaces, so exact-match policy constants are
// robust to the source Dockerfile's line indentation.
func canonical(line string) string {
	return strings.Join(strings.Fields(line), " ")
}

// policy applies the Dockerfile-wiring invariants to the parsed instruction
// list. It returns a non-empty error on the first violation.
func policy(instructions []string) error {
	// Run-109 GW-3: a SHELL override can hide or replace every later RUN.
	for _, line := range instructions {
		if instructionName(line) == "SHELL" {
			return fail("Dockerfile SHELL override is not allowed (run-109 GW-3): %s", line)
		}
	}
	// Run-109 GW-3: the exposure-policy variables may not be overridden.
	for _, line := range instructions {
		upper := instructionName(line)
		if upper != "ARG" && upper != "ENV" && upper != "RUN" {
			continue
		}
		for _, protected := range []string{"LUNARWING_WRAPPER_SHA256", "LUNARWING_REAL_OPENCODE", "OPENCODE_BIN"} {
			// The anchor ARG itself is the one sanctioned mention.
			if strings.Contains(line, protected) && line != "ARG LUNARWING_WRAPPER_SHA256=" {
				return fail("Dockerfile must not override exposure-policy variables (run-109 GW-3): %s", line)
			}
		}
	}

	// Presence and stage/order wiring (run-96 FP-1, run-97 FQ-1).
	var stage string
	compileCopied, compileRan, guarded := false, false, false
	scanStage, assertStage, anchorSeen := "", "", false
	lastUser := ""
	filesystemSeen := false
	for _, line := range instructions {
		name := instructionName(line)
		switch {
		case canonical(line) == canonical(wrapperBinary):
			// presence only; the scan RUN must be in the final stage
		case canonical(line) == canonical(wrapperScan):
			scanStage = stage
		case canonical(line) == canonical(filesystem):
			assertStage = stage
			filesystemSeen = true
			anchorSeen = true // the ARG is declared immediately above it
		case line == "ARG LUNARWING_WRAPPER_SHA256=":
			// the anchored ARG; matches below by stage
		case canonical(line) == canonical(compileCopy) && stage == "builder":
			compileCopied = true
		case canonical(line) == canonical(compileRun) && stage == "builder" && compileCopied:
			compileRan = true
		case canonical(line) == canonical(guardedBuild) && stage == "builder" && compileRan:
			guarded = true
		case canonical(line) == canonical(guardedBuild) && stage == "builder" && !compileRan:
			return fail("Dockerfile must run the OpenTunnel compile-out in AS builder before bun run build -- --single")
		}
		if name == "FROM" {
			stage = ""
			if strings.HasSuffix(strings.ToUpper(line), " AS BUILDER") {
				stage = "builder"
			}
			// compileCopied/compileRan reset per stage; guarded is a
			// one-way "saw the guarded build" flag that must survive the
			// runtime-stage FROM.
			compileCopied, compileRan = false, false
		}
		if name == "USER" {
			lastUser = line
		}
	}
	if !guarded {
		return fail("Dockerfile lost the OpenTunnel compile-out RUN before the builder build (control 8, run-96 FP-1)")
	}
	if !filesystemSeen {
		return fail("Dockerfile must verify installed policy-file ownership and modes (FB-4)")
	}
	if scanStage != assertStage {
		return fail("Dockerfile wrapper install + binary marker scan must run in the same stage as the final --image-filesystem assertion (run-97 FQ-1)")
	}
	if !anchorSeen {
		return fail("Dockerfile must declare ARG LUNARWING_WRAPPER_SHA256= in the final assertion stage (run-109 GW-3)")
	}
	if lastUser != "USER 10003:10003" {
		return fail("Dockerfile's last USER instruction must be exactly 'USER 10003:10003' (run-97 FQ-2)")
	}
	return nil
}

func main() {
	if len(os.Args) != 2 {
		fmt.Fprintln(os.Stderr, "usage: dockerfile-parse <Dockerfile>")
		os.Exit(2)
	}
	f, err := os.ReadFile(os.Args[1])
	if err != nil {
		fmt.Fprintf(os.Stderr, "read: %v\n", err)
		os.Exit(1)
	}
	res, err := parser.Parse(bytes.NewReader(f))
	if err != nil {
		fmt.Fprintf(os.Stderr, "parse: %v\n", err)
		os.Exit(2)
	}
	var instructions []string
	for _, child := range res.AST.Children {
		instructions = append(instructions, child.Original)
	}
	if err := policy(instructions); err != nil {
		fmt.Fprintf(os.Stderr, "exposure policy (BuildKit parser): %v\n", err)
		os.Exit(1)
	}
	fmt.Println("worker exposure policy (BuildKit parser): ok")
}
// Command dockerfile-parse applies LunarWing's opencode2 exposure policy to
// BuildKit's own Dockerfile instruction tree (WI#425).
package main

import (
	"bytes"
	"errors"
	"fmt"
	"os"
	"regexp"
	"strings"
	"unicode"

	"github.com/moby/buildkit/frontend/dockerfile/instructions"
	"github.com/moby/buildkit/frontend/dockerfile/parser"
)

const (
	wrapperBinary   = "COPY --from=builder /out/bin/opencode /usr/local/lib/opencode2/opencode"
	wrapperScan     = "RUN install -D -m 0755 /app/scripts/opencode-exposure-guard.sh /usr/local/bin/opencode && python3 /app/scripts/exposure_policy_assert.py --lane /app --profile opencode2 --binary /usr/local/lib/opencode2/opencode"
	filesystem      = "RUN python3 /app/scripts/exposure_policy_assert.py --lane /app --profile opencode2 --image-filesystem"
	compileCopy     = "COPY scripts/compile_out_remote_tunnel.py /build/compile_out_remote_tunnel.py"
	compileRun      = "RUN python3 /build/compile_out_remote_tunnel.py /build/opencode"
	guardedBuild    = "RUN OPENCODE_VERSION=\"${OPENCODE_VERSION#v}\" bun run build -- --single"
	dependencyModes = "RUN chmod -R a-s,a-w /app/node_modules"
	runtimeSmoke    = "RUN --network=none bash /app/scripts/runtime_smoke_entrypoint.sh"
	anchorArg       = "ARG LUNARWING_WRAPPER_SHA256="
	scriptsCopy     = "COPY scripts/ ./scripts/"
	configCopy      = "COPY config/ ./config/"
	bootCopy        = "COPY entrypoint.sh entrypoint-split.sh health_server.py agent_comm_protocol.json ./"
)

var (
	directivePattern  = regexp.MustCompile(`(?i)#\s*(syntax|escape|check)\s*=`)
	identifierPattern = regexp.MustCompile(`^[A-Za-z_][A-Za-z0-9_]*$`)
	readonlyPattern   = regexp.MustCompile(`^RUN (chmod \+x /app/entrypoint\.sh( /app/entrypoint-split\.sh)? /app/health_server\.py && )?chmod -R a-w /app/scripts /app/config /app/entrypoint\.sh( /app/entrypoint-split\.sh)? /app/health_server\.py /app/agent_comm_protocol\.json$`)
)

type parsedLine struct {
	text string
	name string
	node *parser.Node
}

func canonical(text string) string {
	return strings.Join(strings.FieldsFunc(text, func(r rune) bool {
		return r == ' ' || r == '\t'
	}), " ")
}

func instructionName(text string) string {
	fields := strings.Fields(text)
	if len(fields) == 0 {
		return ""
	}
	return strings.ToUpper(fields[0])
}

func canonicalInstruction(text string) string {
	trimmed := strings.TrimSpace(text)
	name := instructionName(trimmed)
	if name == "" {
		return ""
	}
	args := canonical(trimmed[len(name):])
	if args == "" {
		return name
	}
	return name + " " + args
}

func sameInstruction(line parsedLine, constant string) bool {
	return canonicalInstruction(line.text) == canonicalInstruction(constant)
}

func sourceHygiene(src []byte) []error {
	var errs []error
	if bytes.Contains(src, []byte{0}) {
		errs = append(errs, fmt.Errorf("Dockerfile contains NUL bytes (run-110 GX-1)"))
	}
	if syntax, _, _, _ := parser.DetectSyntax(src); syntax != "" {
		errs = append(errs, fmt.Errorf("Dockerfile parser directive # syntax= selects an unreviewed frontend"))
	}
	normalized := strings.Map(func(r rune) rune {
		if unicode.IsSpace(r) {
			return ' '
		}
		return r
	}, string(src))
	if directivePattern.MatchString(normalized) {
		errs = append(errs, fmt.Errorf("Dockerfile parser directives (# escape=/# syntax=/# check=) are not allowed"))
	}
	for index, raw := range strings.Split(string(src), "\n") {
		if index == 0 {
			raw = strings.TrimPrefix(raw, "\ufeff")
		}
		line := strings.TrimLeft(raw, " \t\v\f\r")
		if line == "" || strings.HasPrefix(line, "#") {
			continue
		}
		if line[0] < 0x20 || line[0] > 0x7e {
			errs = append(errs, fmt.Errorf("Dockerfile line does not start with printable ASCII: %q", raw))
		}
		if strings.ContainsAny(raw, "\v\f\r") {
			errs = append(errs, fmt.Errorf("Dockerfile line contains a vertical tab, form feed or carriage return byte (run-112 HD-1): %q", raw))
		}
		trimmed := strings.TrimRight(raw, " \t\v\f\r")
		if strings.HasSuffix(trimmed, `\\`) {
			errs = append(errs, fmt.Errorf("Dockerfile line ends in an escaped backslash (run-106 GJ-2): %q", raw))
		}
		if strings.HasSuffix(trimmed, `\`) && trimmed != raw {
			errs = append(errs, fmt.Errorf("Dockerfile continuation backslash must not have trailing whitespace (run-109 GW-2): %q", raw))
		}
		if strings.HasSuffix(trimmed, `\`) {
			part := strings.TrimSuffix(trimmed, `\`)
			if part == "" || (part[len(part)-1] != ' ' && part[len(part)-1] != '\t') {
				errs = append(errs, fmt.Errorf("Dockerfile continuation backslash must be preceded by whitespace (run-111 GZ-1): %q", raw))
			}
		}
	}
	return errs
}

func parseInstructions(src []byte) ([]parsedLine, *parser.Result, error) {
	result, err := parser.Parse(bytes.NewReader(src))
	if err != nil {
		return nil, nil, err
	}
	var lines []parsedLine
	for _, child := range result.AST.Children {
		lines = append(lines, parsedLine{
			text: child.Original,
			name: instructionName(child.Original),
			node: child,
		})
	}
	return lines, result, nil
}

func fullText(line parsedLine) string {
	text := line.text
	for _, heredoc := range line.node.Heredocs {
		text += "\n" + heredoc.Content
	}
	return text
}

func checkEnv(line parsedLine) error {
	command, err := instructions.ParseCommand(line.node)
	if err != nil {
		return fmt.Errorf("cannot parse ENV with BuildKit: %w", err)
	}
	env, ok := command.(*instructions.EnvCommand)
	if !ok || len(env.Env) == 0 {
		return fmt.Errorf("Dockerfile ENV requires literal key=value assignments")
	}
	raw := strings.TrimSpace(line.text[len(line.name):])
	first := strings.Fields(raw)[0]
	rawKey := strings.SplitN(first, "=", 2)[0]
	if !identifierPattern.MatchString(rawKey) {
		return fmt.Errorf("Dockerfile ENV requires literal, unprotected key=value assignments: %s", line.text)
	}
	for _, pair := range env.Env {
		if pair.NoDelim || !identifierPattern.MatchString(pair.Key) {
			return fmt.Errorf("Dockerfile ENV requires literal, unprotected key=value assignments (no expanded or legacy keys): %s", line.text)
		}
	}
	return nil
}

func checkProtected(line parsedLine) error {
	if sameInstruction(line, anchorArg) {
		return nil
	}
	for _, protected := range []string{"LUNARWING_WRAPPER_SHA256", "LUNARWING_REAL_OPENCODE", "OPENCODE_BIN"} {
		if strings.Contains(fullText(line), protected) {
			return fmt.Errorf("Dockerfile must not override exposure-policy variables (run-109 GW-3): %s", line.text)
		}
	}
	if line.name == "ENV" {
		return checkEnv(line)
	}
	return nil
}

func checkCopyOwnership(line parsedLine) error {
	if line.name != "COPY" || !strings.Contains(line.text, "--chown=") {
		return nil
	}
	if strings.Contains(line.text, "entrypoint.sh") || strings.Contains(line.text, "health_server.py") || strings.Contains(line.text, "agent_comm_protocol.json") {
		return fmt.Errorf("Dockerfile hands boot files to the agent uid — boot files must be root-owned")
	}
	if strings.Contains(line.text, "scripts/") || strings.Contains(line.text, "config/") {
		return fmt.Errorf("Dockerfile hands policy files to the agent uid — scripts and config must be root-owned")
	}
	return nil
}

type policyState struct {
	stage               int
	stageName           string
	compileCopied       bool
	compileRan          bool
	guarded             bool
	scanStage           int
	assertStage         int
	assertIndex         int
	binaryStage         int
	anchorStage         int
	anchorIndex         int
	dependencyCopyStage int
	dependencyCopyIndex int
	dependencyModeStage int
	dependencyModeIndex int
	smokeStage          int
	smokeIndex          int
	smokeCount          int
	filesystemSeen      bool
	lastUser            string
	scriptsStage        int
	configStage         int
	bootStage           int
	readonlyStage       int
}

func newPolicyState() *policyState {
	return &policyState{
		scanStage: -1, assertStage: -1, assertIndex: -1, binaryStage: -1,
		anchorStage: -1, anchorIndex: -1, dependencyCopyStage: -1,
		dependencyCopyIndex: -1, dependencyModeStage: -1, dependencyModeIndex: -1,
		smokeStage: -1, smokeIndex: -1,
		scriptsStage: -1, configStage: -1, bootStage: -1, readonlyStage: -1,
	}
}

func (s *policyState) process(line parsedLine, index int) error {
	if line.name == "FROM" {
		s.stage++
		s.stageName = ""
		if strings.HasSuffix(strings.ToUpper(canonicalInstruction(line.text)), " AS BUILDER") {
			s.stageName = "builder"
		}
		s.compileCopied, s.compileRan = false, false
		s.lastUser = ""
		return nil
	}
	if line.name == "USER" {
		s.lastUser = canonicalInstruction(line.text)
	}
	if line.name == "COPY" && strings.Contains(line.text, "/app/node_modules") {
		s.dependencyCopyStage, s.dependencyCopyIndex = s.stage, index
	}
	if line.name == "RUN" && strings.Contains(line.text, "runtime_smoke_entrypoint.sh") && !sameInstruction(line, runtimeSmoke) {
		return fmt.Errorf("Dockerfile runtime smoke must use exactly '%s' (WI#511 private netns)", runtimeSmoke)
	}
	switch {
	case sameInstruction(line, compileCopy) && s.stageName == "builder":
		s.compileCopied = true
	case sameInstruction(line, compileRun) && s.stageName == "builder" && s.compileCopied:
		s.compileRan = true
	case sameInstruction(line, guardedBuild):
		if s.stageName != "builder" || !s.compileRan {
			return fmt.Errorf("Dockerfile must run the OpenTunnel compile-out in AS builder before bun run build -- --single")
		}
		s.guarded = true
	case sameInstruction(line, wrapperBinary):
		s.binaryStage = s.stage
	case sameInstruction(line, wrapperScan):
		s.scanStage = s.stage
	case sameInstruction(line, dependencyModes):
		s.dependencyModeStage, s.dependencyModeIndex = s.stage, index
	case sameInstruction(line, runtimeSmoke):
		s.smokeStage, s.smokeIndex = s.stage, index
		s.smokeCount++
	case sameInstruction(line, anchorArg):
		s.anchorStage, s.anchorIndex = s.stage, index
	case sameInstruction(line, filesystem):
		s.assertStage, s.assertIndex = s.stage, index
		s.filesystemSeen = true
	case sameInstruction(line, scriptsCopy):
		s.scriptsStage = s.stage
	case sameInstruction(line, configCopy):
		s.configStage = s.stage
	case sameInstruction(line, bootCopy):
		s.bootStage = s.stage
	case readonlyPattern.MatchString(canonicalInstruction(line.text)):
		s.readonlyStage = s.stage
	}
	return nil
}

func allowedMetadata(name string) bool {
	switch name {
	case "EXPOSE", "HEALTHCHECK", "USER", "ENTRYPOINT", "CMD", "LABEL", "STOPSIGNAL":
		return true
	default:
		return false
	}
}

func (s *policyState) validate() []error {
	var errs []error
	if !s.guarded {
		errs = append(errs, fmt.Errorf("Dockerfile lost the OpenTunnel compile-out RUN with its source COPY before the builder build"))
	}
	if !s.filesystemSeen {
		errs = append(errs, fmt.Errorf("Dockerfile must verify installed policy-file ownership and modes (FB-4)"))
		return errs
	}
	if s.scanStage < 0 {
		errs = append(errs, fmt.Errorf("Dockerfile no longer installs the exposure-guard wrapper with the build-time binary marker scan"))
	} else if s.scanStage != s.assertStage {
		errs = append(errs, fmt.Errorf("Dockerfile wrapper install + binary marker scan must run in the same stage as the final --image-filesystem assertion"))
	}
	if s.binaryStage != s.assertStage {
		errs = append(errs, fmt.Errorf("Dockerfile no longer moves the pinned binary to /usr/local/lib/opencode2/opencode in the final stage"))
	}
	if s.anchorStage != s.assertStage || s.anchorIndex+1 != s.assertIndex {
		errs = append(errs, fmt.Errorf("Dockerfile must declare ARG LUNARWING_WRAPPER_SHA256= immediately above the final assertion in its stage"))
	}
	if s.smokeCount != 1 || s.smokeStage != s.assertStage ||
		s.smokeIndex <= s.dependencyModeIndex || s.smokeIndex >= s.anchorIndex {
		errs = append(errs, fmt.Errorf("Dockerfile must run exactly one private-netns runtime smoke in the final assertion stage, after dependency modes and before the wrapper anchor (WI#511)"))
	}
	dependencyCopiesFinal := s.dependencyCopyIndex < 0 || s.dependencyCopyStage == s.assertStage
	if s.dependencyModeStage != s.assertStage || !dependencyCopiesFinal ||
		s.dependencyModeIndex <= s.dependencyCopyIndex || s.dependencyModeIndex >= s.anchorIndex {
		errs = append(errs, fmt.Errorf("Dockerfile must strip dependency privilege and write bits after every dependency COPY and before the wrapper anchor"))
	}
	if s.lastUser != "USER 10003:10003" {
		errs = append(errs, fmt.Errorf("Dockerfile's last USER instruction must be exactly 'USER 10003:10003'"))
	}
	for label, stage := range map[string]int{"root-owned scripts COPY": s.scriptsStage, "root-owned config COPY": s.configStage, "root-owned boot COPY": s.bootStage, "read-only chmod": s.readonlyStage} {
		if stage != s.assertStage {
			errs = append(errs, fmt.Errorf("Dockerfile %s must be in the final assertion stage", label))
		}
	}
	return errs
}

func policy(lines []parsedLine) error {
	var errs []error
	state := newPolicyState()
	for index, line := range lines {
		if state.filesystemSeen && !allowedMetadata(line.name) {
			errs = append(errs, fmt.Errorf("Dockerfile must not change the filesystem or re-permission paths after --image-filesystem: %s", line.text))
		}
		if len(line.node.Heredocs) > 0 {
			errs = append(errs, fmt.Errorf("Dockerfile heredocs are not allowed (run-109 GW-2): %s", line.text))
		}
		switch line.name {
		case "SHELL":
			errs = append(errs, fmt.Errorf("Dockerfile SHELL overrides are not allowed (run-109 GW-3)"))
		case "ADD", "ONBUILD", "VOLUME":
			errs = append(errs, fmt.Errorf("Dockerfile %s is not allowed in the opencode2 lane", line.name))
		}
		if err := checkProtected(line); err != nil {
			errs = append(errs, err)
		}
		if err := checkCopyOwnership(line); err != nil {
			errs = append(errs, err)
		}
		if err := state.process(line, index); err != nil {
			errs = append(errs, err)
		}
	}
	return errors.Join(append(errs, state.validate()...)...)
}

func check(src []byte) error {
	errs := sourceHygiene(src)
	lines, result, err := parseInstructions(src)
	if err != nil {
		return errors.Join(append(errs, fmt.Errorf("BuildKit parse failed: %w", err))...)
	}
	if result.EscapeToken != '\\' {
		errs = append(errs, fmt.Errorf("Dockerfile parser directive changed the escape token"))
	}
	if err := policy(lines); err != nil {
		errs = append(errs, err)
	}
	return errors.Join(errs...)
}

func main() {
	if len(os.Args) != 2 {
		fmt.Fprintln(os.Stderr, "usage: dockerfile-parse <Dockerfile>")
		os.Exit(2)
	}
	src, err := os.ReadFile(os.Args[1])
	if err != nil {
		fmt.Fprintf(os.Stderr, "Dockerfile missing or unreadable (WI#425 GZ-2): %v\n", err)
		os.Exit(1)
	}
	if err := check(src); err != nil {
		fmt.Fprintf(os.Stderr, "exposure policy (BuildKit parser): %v\n", err)
		os.Exit(1)
	}
	fmt.Println("worker exposure policy (BuildKit parser): ok")
}

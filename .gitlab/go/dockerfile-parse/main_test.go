package main

import (
	"os"
	"strings"
	"testing"
)

const validDockerfile = `FROM example.invalid/bun AS builder
COPY scripts/compile_out_remote_tunnel.py /build/compile_out_remote_tunnel.py
RUN python3 /build/compile_out_remote_tunnel.py /build/opencode
RUN OPENCODE_VERSION="${OPENCODE_VERSION#v}" bun run build -- --single
FROM example.invalid/bun AS runtime
COPY --from=builder /out/bin/opencode /usr/local/lib/opencode2/opencode
RUN install -D -m 0755 /app/scripts/opencode-exposure-guard.sh /usr/local/bin/opencode \
    && python3 /app/scripts/exposure_policy_assert.py --lane /app --profile opencode2 --binary /usr/local/lib/opencode2/opencode
COPY scripts/ ./scripts/
COPY config/ ./config/
COPY entrypoint.sh entrypoint-split.sh health_server.py agent_comm_protocol.json ./
RUN chmod -R a-w /app/scripts /app/config /app/entrypoint.sh /app/entrypoint-split.sh /app/health_server.py /app/agent_comm_protocol.json
RUN chmod -R a-s,a-w /app/node_modules
ARG LUNARWING_WRAPPER_SHA256=
RUN python3 /app/scripts/exposure_policy_assert.py --lane /app --profile opencode2 --image-filesystem
USER 10003:10003
`

func requireFailure(t *testing.T, src, want string) {
	t.Helper()
	err := check([]byte(src))
	if err == nil {
		t.Fatalf("policy accepted adversarial Dockerfile; want %q", want)
	}
	if want != "" && !strings.Contains(err.Error(), want) {
		t.Fatalf("error %q does not contain %q", err, want)
	}
}

func insertBeforeAssertion(src, insertion string) string {
	return strings.Replace(src, filesystem, insertion+filesystem, 1)
}

func TestRealDockerfilePassesParserAndPolicy(t *testing.T) {
	src, err := os.ReadFile("../../../opencode2.4lunarwing/Dockerfile")
	if err != nil {
		t.Fatal(err)
	}
	if err := check(src); err != nil {
		t.Fatalf("real Dockerfile failed: %v", err)
	}
	lines, _, err := parseInstructions(src)
	if err != nil {
		t.Fatal(err)
	}
	if err := policy(lines); err != nil {
		t.Fatalf("policy() rejected the real Dockerfile: %v", err)
	}
}

func TestValidFixtureAndPerStageUsers(t *testing.T) {
	src := strings.Replace(validDockerfile, "FROM example.invalid/bun AS runtime", "USER root\nFROM example.invalid/bun AS runtime", 1)
	if err := check([]byte(src)); err != nil {
		t.Fatalf("builder-stage USER must not leak into the final stage: %v", err)
	}
}

func TestMetadataAfterAssertionPasses(t *testing.T) {
	src := validDockerfile + "eXpOsE 8443\n" +
		"healthcheck --interval=30s \\\n    CMD curl -sf http://127.0.0.1:8443/health || exit 1\n" +
		"user 10003:10003\nentrypoint [\"/app/entrypoint.sh\"]\n" +
		"cmd [\"--mode\", \"websocket\"]\nLABEL policy=\"WI357\"\nstopsignal SIGTERM\n"
	if err := check([]byte(src)); err != nil {
		t.Fatalf("metadata tail failed: %v", err)
	}
}

func TestDependencyNormalizationAllowsUnrelatedInstructions(t *testing.T) {
	for name, insertion := range map[string]string{
		"ENV": "ENV PATH=/opt/bin:$PATH\n",
		"RUN": "RUN echo one \\\n && echo two\n",
	} {
		t.Run(name, func(t *testing.T) {
			src := strings.Replace(validDockerfile, anchorArg, insertion+anchorArg, 1)
			if err := check([]byte(src)); err != nil {
				t.Fatalf("unrelated instruction before the anchor was rejected: %v", err)
			}
		})
	}
}

func TestParserDivergenceFixtures(t *testing.T) {
	postAssertRun := "RUN chown -R opencode2:opencode2 /app\n"
	cases := []struct {
		name string
		src  string
		want string
	}{
		{"FQ-3 custom syntax", "# syntax=docker/dockerfile:1\n" + validDockerfile, "unreviewed frontend"},
		{"FQ-3 C-style syntax", "// syntax = docker/dockerfile:1\n" + validDockerfile, "unreviewed frontend"},
		{"GD-1 spaced escape directive", "# escape = `\n" + validDockerfile, "parser directive"},
		{"GD-1 check directive", "  # check=true\n" + validDockerfile, "parser directive"},
		{"GJ-1 unicode directive prefix", "\u00a0# escape=`\n" + validDockerfile, "parser directive"},
		{"GJ-2 escaped escape", strings.Replace(validDockerfile, filesystem, filesystem+"LABEL hidden=yes \\\\\n"+postAssertRun, 1), "escaped backslash"},
		{"GW-2 continuation trailing space", insertBeforeAssertion(validDockerfile, "ENV LUNARWING_STAGE=final \\ \n"), "trailing whitespace"},
		{"GW-2 heredoc", insertBeforeAssertion(validDockerfile, "RUN cat >/dev/null <<LABEL\n"+filesystem+"\nLABEL\n"), "heredocs are not allowed"},
		{"GX-1 late BOM", strings.Replace(validDockerfile, filesystem, filesystem+"LABEL poc=1\\\n\ufeff\n"+postAssertRun, 1), "printable ASCII"},
		{"GX-1 NUL", insertBeforeAssertion(validDockerfile, "LABEL poc=1\\\x00\n"), "NUL bytes"},
		{"GZ-1 glued SHELL", insertBeforeAssertion(validDockerfile, "SHEL\\\nL [\"/bin/sh\", \"-c\"]\n"), "preceded by whitespace"},
		{"HD-1 vertical tab", insertBeforeAssertion(validDockerfile, "RUN echo a\v\\\nb\n"), "vertical tab, form feed or carriage return"},
	}
	for _, test := range cases {
		t.Run(test.name, func(t *testing.T) {
			requireFailure(t, test.src, test.want)
		})
	}
}

func TestStructuralPolicyFixtures(t *testing.T) {
	builderUser := strings.Replace(
		strings.TrimSuffix(validDockerfile, "USER 10003:10003\n"),
		"FROM example.invalid/bun AS runtime",
		"USER 10003:10003\nFROM example.invalid/bun AS runtime",
		1,
	)
	cases := []struct {
		name string
		src  string
		want string
	}{
		{"missing anchor", strings.Replace(validDockerfile, anchorArg, "", 1), "must declare ARG LUNARWING_WRAPPER_SHA256="},
		{"anchor not adjacent", strings.Replace(validDockerfile, anchorArg, anchorArg+"\nENV SAFE=value\n", 1), "immediately above"},
		{"post-assert RUN", validDockerfile + "RUN rm -rf /app/scripts\n", "after --image-filesystem"},
		{"post-assert stage", validDockerfile + "FROM example.invalid/bun AS unchecked\n", "after --image-filesystem"},
		{"post-assert COPY", validDockerfile + "COPY --from=runtime / /ignored\n", "after --image-filesystem"},
		{"expanded ENV key", insertBeforeAssertion(validDockerfile, "ARG P=OPENCODE_\nENV ${P}BIN=/tmp/other\n"), "literal, unprotected key=value"},
		{"quoted ENV key", insertBeforeAssertion(validDockerfile, "ENV OPENCODE_\"BIN\"=/tmp/other\n"), "literal, unprotected key=value"},
		{"legacy ENV key", insertBeforeAssertion(validDockerfile, "ENV STAGE final\n"), "literal, unprotected key=value"},
		{"legacy ENV with equals in value", insertBeforeAssertion(validDockerfile, "ENV STAGE final=other\n"), "literal, unprotected key=value"},
		{"SHELL", insertBeforeAssertion(validDockerfile, "SHELL [\"/bin/sh\", \"-c\"]\n"), "SHELL overrides are not allowed"},
		{"ONBUILD protected name", insertBeforeAssertion(validDockerfile, "ONBUILD ENV LUNARWING_REAL_OPENCODE=/tmp/other\n"), "must not override exposure-policy variables"},
		{"LABEL protected name", insertBeforeAssertion(validDockerfile, "LABEL policy=$LUNARWING_WRAPPER_SHA256\n"), "must not override exposure-policy variables"},
		{"ONBUILD", insertBeforeAssertion(validDockerfile, "ONBUILD RUN true\n"), "ONBUILD is not allowed"},
		{"VOLUME", insertBeforeAssertion(validDockerfile, "VOLUME [\"/workspace\"]\n"), "VOLUME is not allowed"},
		{"ADD git source", insertBeforeAssertion(validDockerfile, "ADD https://github.com/anomalyco/opencode.git /tmp/opencode\n"), "ADD is not allowed"},
		{"ADD URL source", insertBeforeAssertion(validDockerfile, "ADD https://example.invalid/opencode /tmp/opencode\n"), "ADD is not allowed"},
		{"unicode assertion space", strings.Replace(validDockerfile, "--profile opencode2 --image-filesystem", "--profile opencode2\u00a0--image-filesystem", 1), "must verify installed policy-file ownership"},
		{"final root user", validDockerfile + "USER 0:0\n", "last USER"},
		{"builder USER does not satisfy final stage", builderUser, "last USER"},
		{"binary COPY outside final stage", strings.Replace(validDockerfile, "FROM example.invalid/bun AS runtime\n"+wrapperBinary, wrapperBinary+"\nFROM example.invalid/bun AS runtime", 1), "no longer moves the pinned binary"},
		{"missing scripts COPY", strings.Replace(validDockerfile, scriptsCopy+"\n", "", 1), "root-owned scripts COPY"},
		{"missing config COPY", strings.Replace(validDockerfile, configCopy+"\n", "", 1), "root-owned config COPY"},
		{"missing boot COPY", strings.Replace(validDockerfile, bootCopy+"\n", "", 1), "root-owned boot COPY"},
		{"missing wrapper scan", strings.Replace(validDockerfile, "RUN install -D -m 0755", "RUN true # RUN install -D -m 0755", 1), "no longer installs the exposure-guard wrapper"},
		{"compile-out after build", strings.Replace(validDockerfile, compileRun+"\n"+guardedBuild, guardedBuild+"\n"+compileRun, 1), "compile-out"},
		{"missing read-only step", strings.Replace(validDockerfile, "RUN chmod -R a-w", "RUN true # chmod -R a-w", 1), "read-only chmod"},
		{"missing dependency mode normalization", strings.Replace(validDockerfile, dependencyModes+"\n", "", 1), "strip dependency privilege and write bits"},
		{"dependency mode normalization after anchor", strings.Replace(validDockerfile, dependencyModes+"\n"+anchorArg, anchorArg+"\n"+dependencyModes, 1), "before the wrapper anchor"},
		{"dependency COPY after normalization", strings.Replace(validDockerfile, anchorArg, "COPY --from=builder /out/client /app/node_modules/@opencode/client\n"+anchorArg, 1), "after every dependency COPY"},
		{"missing filesystem assertion", strings.Replace(validDockerfile, " --image-filesystem", "", 1), "verify installed policy-file ownership"},
	}
	for _, test := range cases {
		t.Run(test.name, func(t *testing.T) {
			requireFailure(t, test.src, test.want)
		})
	}
}

func TestPolicyFunctionIsRequired(t *testing.T) {
	src := strings.Replace(validDockerfile, anchorArg, "", 1)
	lines, _, err := parseInstructions([]byte(src))
	if err != nil {
		t.Fatal(err)
	}
	if err := policy(lines); err == nil || !strings.Contains(err.Error(), "must declare ARG") {
		t.Fatalf("policy() returned nil or the wrong error for a missing anchor: %v", err)
	}
}

func TestEscapeTokenMustBeDefault(t *testing.T) {
	src := []byte("# escape=`\n" + validDockerfile)
	_, result, err := parseInstructions(src)
	if err != nil {
		t.Fatal(err)
	}
	if result.EscapeToken != '`' {
		t.Fatalf("fixture did not change the escape token: %q", result.EscapeToken)
	}
	requireFailure(t, string(src), "changed the escape token")
}

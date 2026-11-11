package main

import (
	"os"
	"strings"
	"testing"
)

func lunardepthFixture(t *testing.T) string {
	t.Helper()
	data, err := os.ReadFile("../../../lunardepth4lunarwing/Dockerfile")
	if err != nil {
		t.Fatal(err)
	}
	return string(data)
}

func replaceLunardepthSmoke(t *testing.T, src, replacement string) string {
	t.Helper()
	start := strings.Index(src, "RUN --network=none")
	if start < 0 {
		t.Fatal("LunarDepth smoke start not found")
	}
	end := strings.Index(src[start:], "\n\nEXPOSE")
	if end < 0 {
		t.Fatal("LunarDepth smoke end not found")
	}
	end += start
	return src[:start] + replacement + src[end:]
}

func TestLunardepthRealDockerfilePasses(t *testing.T) {
	if err := checkLunardepth([]byte(lunardepthFixture(t))); err != nil {
		t.Fatalf("real LunarDepth Dockerfile failed policy: %v", err)
	}
}

func TestLunardepthStructuralFailures(t *testing.T) {
	valid := lunardepthFixture(t)
	cases := []struct {
		name string
		src  string
		want string
	}{
		{"final root", valid + "USER 0:0\n", "final USER"},
		{"missing network smoke", replaceLunardepthSmoke(t, valid, "RUN true"), "network-isolated smoke"},
		{"task port published", valid + "EXPOSE 19091\n", "task port 19091"},
		{"missing source pin", strings.Replace(valid, "da06c33b2091546dc38c61b95452ce27a4ce32d5", "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa", 1), "LUNARDEPTH_REF"},
		{"post smoke copy", valid + "COPY --from=builder /opt /ignored\n", "final executable step"},
		{"post smoke noop run", valid + "RUN set -eu; /bin/true\n", "final executable step"},
		{"post smoke stage", valid + "FROM scratch AS unchecked\n", "must not add a stage"},
		{"builder only smoke", strings.Replace(replaceLunardepthSmoke(t, valid, ""), "FROM docker.io/library/node:22.23.4-bookworm-slim@sha256:4b150b48b4116e2a22f2c39cc27025fbf4f153e2078fd3b14d159fdb5c7279da AS runtime", lunardepthSmoke+"\nFROM docker.io/library/node:22.23.4-bookworm-slim@sha256:4b150b48b4116e2a22f2c39cc27025fbf4f153e2078fd3b14d159fdb5c7279da AS runtime", 1), "final stage"},
		{"shadowed source arg", strings.Replace(valid, "ARG LUNARDEPTH_REF\n", "ARG LUNARDEPTH_REF=aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa\n", 1), "stage ARG"},
		{"deleted head provenance", strings.Replace(valid, "test \"$(git -C /src rev-parse HEAD)\" = \"$LUNARDEPTH_REF\"; \\\n", "", 1), "required input or assertion"},
		{"ignored ownership assertion", strings.Replace(valid, "test -z \"$(find -L /app /opt/lunardepth /opt/lunardepth-templates \\( ! -uid 0 -o -perm /6222 \\) -print)\"", "test -z \"$(find -L /app /opt/lunardepth /opt/lunardepth-templates \\( ! -uid 0 -o -perm /6222 \\) -print)\" || true", 1), "forbidden value"},
		{"unexpected shell", strings.Replace(valid, "SHELL [\"/bin/bash\", \"-o\", \"pipefail\", \"-c\"]", "SHELL [\"/bin/sh\", \"-c\"]", 1), "must pin bash pipefail"},
		{"weakened ownership assertion", strings.Replace(valid, "-perm /6222", "-perm /222", 1), "required input or assertion"},
		{"missing ownership assertion", strings.Replace(valid, "test -z \"$(find -L", "test -n \"$(find -L", 1), "required input or assertion"},
	}
	for _, test := range cases {
		t.Run(test.name, func(t *testing.T) {
			if err := checkLunardepth([]byte(test.src)); err == nil || !strings.Contains(err.Error(), test.want) {
				t.Fatalf("expected %q, got %v", test.want, err)
			}
		})
	}
}

func TestLunardepthDoesNotRequireOpenTunnelCompileOut(t *testing.T) {
	if err := checkLunardepth([]byte(lunardepthFixture(t))); err != nil {
		t.Fatalf("LunarDepth must not inherit OpenCode2's compile-out requirement: %v", err)
	}
}

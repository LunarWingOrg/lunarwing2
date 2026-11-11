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
		{"missing network smoke", strings.Replace(valid, lunardepthSmoke, "RUN true", 1), "network-isolated smoke"},
		{"task port published", valid + "EXPOSE 19091\n", "task port 19091"},
		{"missing source pin", strings.Replace(valid, "da06c33b2091546dc38c61b95452ce27a4ce32d5", "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa", 1), "LUNARDEPTH_REF"},
		{"post smoke copy", valid + "COPY --from=builder /opt /ignored\n", "must not add filesystem inputs"},
		{"unexpected shell", strings.Replace(valid, "SHELL [\"/bin/bash\", \"-o\", \"pipefail\", \"-c\"]", "SHELL [\"/bin/sh\", \"-c\"]", 1), "must pin bash pipefail"},
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

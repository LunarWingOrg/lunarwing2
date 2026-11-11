package main

import (
	"errors"
	"fmt"
	"strings"
)

const lunardepthSmoke = "RUN --network=none LUNARDEPTH_BUILD_SMOKE=1 /app/scripts/runtime_smoke_entrypoint.sh"

func checkLunardepth(src []byte) error {
	errs := sourceHygiene(src)
	lines, result, err := parseInstructions(src)
	if err != nil {
		return errors.Join(append(errs, fmt.Errorf("BuildKit parse failed: %w", err))...)
	}
	if result.EscapeToken != '\\' {
		errs = append(errs, fmt.Errorf("Dockerfile parser directive changed the escape token"))
	}
	for _, line := range lines {
		if len(line.node.Heredocs) > 0 {
			errs = append(errs, fmt.Errorf("Dockerfile heredocs are not allowed for the LunarDepth lane: %s", line.text))
		}
		switch line.name {
		case "SHELL":
			if !sameInstruction(line, "SHELL [\"/bin/bash\", \"-o\", \"pipefail\", \"-c\"]") {
				errs = append(errs, fmt.Errorf("LunarDepth SHELL must pin bash pipefail"))
			}
		case "ADD", "ONBUILD", "VOLUME":
			errs = append(errs, fmt.Errorf("Dockerfile %s is not allowed in the LunarDepth lane", line.name))
		case "RUN":
			if strings.Contains(line.text, "runtime_smoke_entrypoint.sh") && !sameInstruction(line, lunardepthSmoke) {
				errs = append(errs, fmt.Errorf("LunarDepth runtime smoke must use exactly '%s'", lunardepthSmoke))
			}
		}
	}
	errs = append(errs, lunardepthPolicy(lines))
	return errors.Join(errs...)
}

func lunardepthPolicy(lines []parsedLine) error {
	var errs []error
	text := ""
	for _, line := range lines {
		text += line.text + "\n"
	}
	required := []string{
		"ARG LUNARDEPTH_REPOSITORY=https://git.lunarwing.org/lunarwing/lunardepth.git",
		"ARG LUNARDEPTH_REF=da06c33b2091546dc38c61b95452ce27a4ce32d5",
		"ARG LUNARDEPTH_COMPATIBILITY_REF=17aef23e05cc074c8621d38d159d77d25cb348ca",
		"ARG LUNARDEPTH_MODEL_ROUTING_REF=8c610180c84a2f4c27fb2958e6e7dab9e48d476b",
		"ARG LUNARDEPTH_PROFILE_SCHEMA_VERSION=unversioned",
		"ARG DEBIAN_SNAPSHOT=20261105T082236Z",
		"pnpm@11.7.0",
		"build-exe-for-python-sdk.ts --deploy-to=/opt/lunardepth",
		"profile_template.js /opt/lunardepth /opt/lunardepth-templates",
		"test -z \"$(find -L /app /opt/lunardepth /opt/lunardepth-templates \\( ! -uid 0 -o -perm /222 \\) -print)\"",
		"RUN --network=none LUNARDEPTH_BUILD_SMOKE=1 /app/scripts/runtime_smoke_entrypoint.sh",
	}
	for _, fragment := range required {
		if !strings.Contains(text, fragment) {
			errs = append(errs, fmt.Errorf("LunarDepth Dockerfile missing required input or assertion: %s", fragment))
		}
	}
	for _, forbidden := range []string{"LUNARDEPTH_API_KEY=", "LUNARDEPTH_LOCAL_AUTH_TOKEN=", "danger-full-access", "dsh-bash-local", "runnerCommand"} {
		if strings.Contains(text, forbidden) {
			errs = append(errs, fmt.Errorf("LunarDepth Dockerfile contains forbidden value %s", forbidden))
		}
	}
	if strings.Contains(text, "19091") {
		errs = append(errs, fmt.Errorf("LunarDepth task port 19091 must remain unpublished"))
	}
	smokeIndex := -1
	for index, line := range lines {
		if sameInstruction(line, lunardepthSmoke) {
			smokeIndex = index
			continue
		}
		if smokeIndex >= 0 && line.name == "RUN" && !strings.HasPrefix(line.text, "RUN set -eu;") {
			errs = append(errs, fmt.Errorf("only the documented cleanup may follow the LunarDepth smoke: %s", line.text))
		}
		if smokeIndex >= 0 && (line.name == "COPY" || line.name == "ADD") {
			errs = append(errs, fmt.Errorf("LunarDepth must not add filesystem inputs after its assertion: %s", line.text))
		}
	}
	if smokeIndex < 0 {
		errs = append(errs, fmt.Errorf("LunarDepth split-role network-isolated smoke is missing"))
	}
	lastUser := ""
	for _, line := range lines {
		if line.name == "USER" {
			lastUser = canonicalInstruction(line.text)
		}
	}
	if lastUser != "USER 10005:10005" {
		errs = append(errs, fmt.Errorf("LunarDepth final USER must be exactly 10005:10005"))
	}
	return errors.Join(errs...)
}

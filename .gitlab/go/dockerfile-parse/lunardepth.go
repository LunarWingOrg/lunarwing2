package main

import (
	"errors"
	"fmt"
	"strings"
)

const lunardepthSmoke = "RUN --network=none set -eu; LUNARDEPTH_BUILD_SMOKE=1 /app/scripts/runtime_smoke_entrypoint.sh; rm -rf /tmp/lunardepth-* /tmp/dsh-spill-* /tmp/lunardepth_ws_state.json /run/lunardepth-state/* /run/lunarwing-shared/runner/* /home/lunardepth/.dsh /home/lunardepth-bridge/.local/state /run/lunardepth-config/* /root/.cache /root/.npm; install -d -o lunardepth -g lunardepth -m 0700 /home/lunardepth/.dsh; find /app /opt/lunardepth /opt/lunardepth-templates -exec touch -h -d \"@${SOURCE_DATE_EPOCH}\" {} +; touch -d \"@${SOURCE_DATE_EPOCH}\" /usr/local /usr/local/bin/dsh"

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
	var text string
	for _, line := range lines {
		text += line.text + "\n"
	}
	return errors.Join(lunardepthInputPolicy(lines, text), lunardepthFinalStagePolicy(lines))
}

func lunardepthInputPolicy(lines []parsedLine, text string) error {
	var errs []error
	requiredArgs := []string{
		"ARG LUNARDEPTH_REPOSITORY=https://git.lunarwing.org/lunarwing/lunardepth.git",
		"ARG LUNARDEPTH_REF=da06c33b2091546dc38c61b95452ce27a4ce32d5",
		"ARG LUNARDEPTH_COMPATIBILITY_REF=17aef23e05cc074c8621d38d159d77d25cb348ca",
		"ARG LUNARDEPTH_MODEL_ROUTING_REF=8c610180c84a2f4c27fb2958e6e7dab9e48d476b",
		"ARG LUNARDEPTH_PROFILE_SCHEMA_VERSION=unversioned",
		"ARG DEBIAN_SNAPSHOT=20261105T082236Z",
	}
	seen := make(map[string]int)
	for _, line := range lines {
		if line.name == "FROM" {
			break
		}
		for _, arg := range requiredArgs {
			if sameInstruction(line, arg) {
				seen[arg]++
			}
		}
	}
	for _, arg := range requiredArgs {
		if seen[arg] != 1 {
			errs = append(errs, fmt.Errorf("LunarDepth global ARG must appear exactly once before every stage: %s", arg))
		}
	}
	required := []string{
		"pnpm@11.7.0",
		"test \"$LUNARDEPTH_REPOSITORY\" = \"https://git.lunarwing.org/lunarwing/lunardepth.git\"",
		"test \"$(git -C /src rev-parse HEAD)\" = \"$LUNARDEPTH_REF\"",
		"git -C /src merge-base --is-ancestor \"$LUNARDEPTH_COMPATIBILITY_REF\" \"$LUNARDEPTH_REF\"",
		"git -C /src merge-base --is-ancestor \"$LUNARDEPTH_MODEL_ROUTING_REF\" \"$LUNARDEPTH_REF\"",
		"grep -q 'tensormoon' /src/deepseek-harness/packages/bundle/base/cordis.patch.yml",
		"pnpm exec tsx native/system/scripts/build.ts",
		"build-exe-for-python-sdk.ts --deploy-to=/opt/lunardepth",
		"sed -i '/^  \"prunedAt\":/d' /opt/lunardepth/node_modules/.modules.yaml",
		"profile_template.js /opt/lunardepth /opt/lunardepth-templates",
		"test -x /app/entrypoint-task-setup.sh",
		"test -z \"$(find -L /app /opt/lunardepth /opt/lunardepth-templates \\( ! -uid 0 -o -perm /222 \\) -print)\"",
	}
	for _, fragment := range required {
		if !strings.Contains(text, fragment) {
			errs = append(errs, fmt.Errorf("LunarDepth Dockerfile missing required input or assertion: %s", fragment))
		}
	}
	for _, forbidden := range []string{"LUNARDEPTH_API_KEY=", "LUNARDEPTH_LOCAL_AUTH_TOKEN=", "danger-full-access", "dsh-bash-local", "runnerCommand", "|| true"} {
		if strings.Contains(text, forbidden) {
			errs = append(errs, fmt.Errorf("LunarDepth Dockerfile contains forbidden value %s", forbidden))
		}
	}
	if strings.Contains(text, "19091") {
		errs = append(errs, fmt.Errorf("LunarDepth task port 19091 must remain unpublished"))
	}
	protected := map[string]bool{
		"LUNARDEPTH_REPOSITORY": true, "LUNARDEPTH_REF": true,
		"LUNARDEPTH_COMPATIBILITY_REF": true, "LUNARDEPTH_MODEL_ROUTING_REF": true,
		"LUNARDEPTH_PROFILE_SCHEMA_VERSION": true,
	}
	inStage := false
	for _, line := range lines {
		if line.name == "FROM" {
			inStage = true
			continue
		}
		if !inStage || line.name != "ARG" {
			continue
		}
		raw := strings.TrimSpace(line.text[len(line.name):])
		key := strings.SplitN(raw, "=", 2)[0]
		if protected[key] && raw != key {
			errs = append(errs, fmt.Errorf("LunarDepth stage ARG may only inherit the global pin: %s", line.text))
		}
	}
	return errors.Join(errs...)
}

func lunardepthFinalStagePolicy(lines []parsedLine) error {
	var errs []error
	stage, smokeStage, smokeIndex, smokeCount := -1, -1, -1, 0
	lastUser := ""
	for index, line := range lines {
		if line.name == "FROM" {
			stage++
			lastUser = ""
			if smokeIndex >= 0 {
				errs = append(errs, fmt.Errorf("LunarDepth must not add a stage after its final smoke: %s", line.text))
			}
		}
		if sameInstruction(line, lunardepthSmoke) {
			smokeStage, smokeIndex = stage, index
			smokeCount++
		}
		if smokeIndex >= 0 && stage == smokeStage && index > smokeIndex && !lunardepthMetadata(line.name) {
			errs = append(errs, fmt.Errorf("LunarDepth smoke must be the final executable step: %s", line.text))
		}
		if line.name == "USER" {
			lastUser = canonicalInstruction(line.text)
		}
	}
	if smokeCount != 1 || smokeStage != stage {
		errs = append(errs, fmt.Errorf("LunarDepth requires exactly one network-isolated smoke in the final stage"))
	}
	if lastUser != "USER 10005:10005" {
		errs = append(errs, fmt.Errorf("LunarDepth final USER must be exactly 10005:10005"))
	}
	return errors.Join(errs...)
}

func lunardepthMetadata(name string) bool {
	switch name {
	case "EXPOSE", "HEALTHCHECK", "USER", "ENTRYPOINT", "CMD", "LABEL", "STOPSIGNAL":
		return true
	default:
		return false
	}
}

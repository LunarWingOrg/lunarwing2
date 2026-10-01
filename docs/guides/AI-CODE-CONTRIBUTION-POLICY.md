# LW AI Code Contribution Policy

**Effective Date:** September 15th, 2026
**Last Updated:** August 19th, 2026 *(Added exceptions for documentation and non-generative cybersecurity analysis)*

**Revalidated 2026-09-17 (v2.1.2.0); re-checked 2026-09-27 (v2.1.3.0) and 2026-10-01 (v2.1.4.0).** Policy text reviewed; no changes were
needed. The companion human policy (`CODE-CONTRIBUTION-POLICY.md`) had its
stale `src/*/CLAUDE.md` references corrected to the `ic/` tree in this pass.

## 1. Core Rule: Closed-Weight Models Prohibited for Code Generation

Code produced by proprietary, closed-weight models — including but not limited to those accessed exclusively via vendor APIs where the model weights are not publicly available — will no longer be accepted. This includes code generated through services that do not provide downloadable, inspectable model weights.

## 2. Permitted Models

Code produced by open-weight models — such as Qwen (3.5, 3.6, 3.8), DeepSeek, GLM, GPT-OSS, Moonshot Kimi (2.5, 2.6, and 3), Llama variants, and similar models whose trained parameters are publicly released — will continue to be accepted. Contributions that disclose the model used are appreciated but not required.

## 3. Responsibility & Enforcement

Contributors are responsible for ensuring their submissions comply with this policy. Maintainers may reject pull requests that are known to originate from unauthorized closed-weight model output. When in doubt, ask before you submit. All other code contribution policies that exist remain in place, regardless of whether code is produced by a human or an LLM.

## 4. Authorized Exceptions

Proprietary, closed-weight models may be utilized only under the following narrow exceptions, provided they do not generate substantial blocks of repository source code:

- **Documentation:** Models may be used to generate repository documentation or analyze existing documentation to provide textual suggestions.

- **Cybersecurity Analysis & Threat Intelligence:** Specialized closed-weight security frameworks or orchestration models — such as the Sakana AI Fugu-Cyber platform — may be used to audit existing codebases, analyze threat logs, and evaluate vulnerabilities.

- **Non-Generative Output Constraints:** The output of these security tools is permitted for verification, security documentation, and logical analysis. They may not be used to bypass Section 1 by auto-generating expansive, functional software patches or new feature implementations for submission. Small, human-verified security rule adjustments (e.g., precise YARA, Sigma, or single-line fixes) stemming from security audits are acceptable.
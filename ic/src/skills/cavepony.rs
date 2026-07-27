//! Persistent Cavepony skill preference and turn-level overrides.

use serde_json::Value;

use crate::skills::LoadedSkill;
use crate::skills::selector::{
    prefilter_skills, prefilter_skills_excluding, skill_context_token_cost,
};

pub const CAVEPONY_MODE_SETTING_KEY: &str = "skills.cavepony_mode";
const CAVEPONY_SKILL_NAME: &str = "cavepony";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CaveponyMode {
    Lite,
    Full,
    Ultra,
    Pony,
    Canterlot,
    TonySoprano,
}

impl CaveponyMode {
    pub fn parse(value: &str) -> Option<Self> {
        match value.trim().to_ascii_lowercase().as_str() {
            "lite" => Some(Self::Lite),
            "full" => Some(Self::Full),
            "ultra" => Some(Self::Ultra),
            "pony" => Some(Self::Pony),
            "canterlot" => Some(Self::Canterlot),
            "tony-soprano" => Some(Self::TonySoprano),
            _ => None,
        }
    }

    pub fn from_setting_value(value: &Value) -> Option<Self> {
        value.as_str().and_then(Self::parse)
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Lite => "lite",
            Self::Full => "full",
            Self::Ultra => "ultra",
            Self::Pony => "pony",
            Self::Canterlot => "canterlot",
            Self::TonySoprano => "tony-soprano",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CaveponyDirective {
    Mode(CaveponyMode),
    Bypass,
}

impl CaveponyDirective {
    pub fn parse(content: &str) -> Option<Self> {
        let normalized = content.trim().to_ascii_lowercase();
        match normalized.as_str() {
            "/normal" | "stop cavepony" => Some(Self::Bypass),
            "/cavepony" => Some(Self::Mode(CaveponyMode::Full)),
            _ => normalized
                .strip_prefix("/cavepony ")
                .and_then(CaveponyMode::parse)
                .map(Self::Mode),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CaveponyActivation {
    Automatic,
    Persistent(CaveponyMode),
    Turn(CaveponyMode),
    Bypass,
}

impl CaveponyActivation {
    pub fn from_directive(directive: CaveponyDirective) -> Self {
        match directive {
            CaveponyDirective::Mode(mode) => Self::Turn(mode),
            CaveponyDirective::Bypass => Self::Bypass,
        }
    }

    pub fn prompt_instruction(self) -> Option<String> {
        match self {
            Self::Persistent(mode) => Some(format!(
                "<skill-preference name=\"cavepony\" scope=\"persistent\" mode=\"{}\">\n\
                 Apply this Cavepony mode to every response for this user across threads and sessions. \
                 An explicit Cavepony mode in the current request overrides this preference for that turn. \
                 /normal or an exact 'stop cavepony' request bypasses it for one turn.\n\
                 </skill-preference>",
                mode.as_str()
            )),
            Self::Turn(mode) => Some(format!(
                "<skill-preference name=\"cavepony\" scope=\"turn\" mode=\"{}\">\n\
                 Apply this Cavepony mode to the current response only.\n\
                 </skill-preference>",
                mode.as_str()
            )),
            Self::Bypass => Some(
                "<skill-preference name=\"cavepony\" scope=\"turn\" mode=\"off\">\n\
                 Respond normally for this turn. Keep any persistent Cavepony preference unchanged.\n\
                 </skill-preference>"
                    .to_string(),
            ),
            Self::Automatic => None,
        }
    }
}

pub fn select_with_activation<'a>(
    message: &str,
    available_skills: &'a [LoadedSkill],
    max_candidates: usize,
    max_context_tokens: usize,
    activation: CaveponyActivation,
) -> Vec<&'a LoadedSkill> {
    match activation {
        CaveponyActivation::Automatic => prefilter_skills(
            message,
            available_skills,
            max_candidates,
            max_context_tokens,
        ),
        CaveponyActivation::Bypass => prefilter_skills_excluding(
            message,
            available_skills,
            max_candidates,
            max_context_tokens,
            Some(CAVEPONY_SKILL_NAME),
        ),
        CaveponyActivation::Persistent(_) | CaveponyActivation::Turn(_) => {
            let Some(cavepony) = available_skills.iter().find(|skill| {
                skill.name() == CAVEPONY_SKILL_NAME
                    && matches!(&skill.source, crate::skills::SkillSource::Bundled(_))
            }) else {
                return prefilter_skills_excluding(
                    message,
                    available_skills,
                    max_candidates,
                    max_context_tokens,
                    Some(CAVEPONY_SKILL_NAME),
                );
            };

            if max_candidates == 0 {
                return Vec::new();
            }

            let cavepony_cost = skill_context_token_cost(cavepony);
            if cavepony_cost > max_context_tokens {
                return prefilter_skills_excluding(
                    message,
                    available_skills,
                    max_candidates,
                    max_context_tokens,
                    Some(CAVEPONY_SKILL_NAME),
                );
            }

            let mut selected = prefilter_skills_excluding(
                message,
                available_skills,
                max_candidates.saturating_sub(1),
                max_context_tokens - cavepony_cost,
                Some(CAVEPONY_SKILL_NAME),
            );
            selected.insert(0, cavepony);
            selected
        }
    }
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use crate::skills::{ActivationCriteria, LoadedSkill, SkillManifest, SkillSource, SkillTrust};

    use super::*;

    fn skill(name: &str, keywords: &[&str], context_tokens: usize) -> LoadedSkill {
        let keywords = keywords
            .iter()
            .map(|keyword| (*keyword).to_string())
            .collect::<Vec<_>>();
        LoadedSkill {
            manifest: SkillManifest {
                name: name.to_string(),
                version: "1.0.0".to_string(),
                description: String::new(),
                activation: ActivationCriteria {
                    keywords: keywords.clone(),
                    max_context_tokens: context_tokens,
                    ..ActivationCriteria::default()
                },
                metadata: None,
            },
            prompt_content: "prompt".to_string(),
            trust: SkillTrust::Trusted,
            source: SkillSource::Bundled(PathBuf::from(name)),
            content_hash: "sha256:test".to_string(),
            compiled_patterns: Vec::new(),
            lowercased_keywords: keywords
                .iter()
                .map(|keyword| keyword.to_ascii_lowercase())
                .collect(),
            lowercased_exclude_keywords: Vec::new(),
            lowercased_tags: Vec::new(),
        }
    }

    #[test]
    fn parses_modes_and_persisted_values() {
        assert_eq!(CaveponyMode::parse(" FULL "), Some(CaveponyMode::Full));
        assert_eq!(
            CaveponyMode::from_setting_value(&serde_json::json!("ultra")),
            Some(CaveponyMode::Ultra)
        );
        assert_eq!(
            CaveponyMode::from_setting_value(&serde_json::json!("tony-soprano")),
            Some(CaveponyMode::TonySoprano)
        );
        assert_eq!(CaveponyMode::parse("tokens"), None);
        assert_eq!(
            CaveponyMode::from_setting_value(&serde_json::json!(true)),
            None
        );
    }

    #[test]
    fn parses_exact_turn_directives() {
        assert_eq!(
            CaveponyDirective::parse("/cavepony"),
            Some(CaveponyDirective::Mode(CaveponyMode::Full))
        );
        assert_eq!(
            CaveponyDirective::parse(" /CAVEPONY ultra "),
            Some(CaveponyDirective::Mode(CaveponyMode::Ultra))
        );
        assert_eq!(
            CaveponyDirective::parse("/cavepony Tony-Soprano"),
            Some(CaveponyDirective::Mode(CaveponyMode::TonySoprano))
        );
        assert_eq!(
            CaveponyDirective::parse("/normal"),
            Some(CaveponyDirective::Bypass)
        );
        assert_eq!(CaveponyDirective::parse("/cavepony unknown"), None);
        assert_eq!(CaveponyDirective::parse("explain cavepony"), None);
    }

    #[test]
    fn persistent_mode_reserves_first_skill_slot() {
        let skills = vec![
            skill("writing", &["write"], 100),
            skill("cavepony", &["cavepony"], 200),
        ];
        let selected = select_with_activation(
            "write a note",
            &skills,
            2,
            400,
            CaveponyActivation::Persistent(CaveponyMode::Full),
        );

        assert_eq!(selected.len(), 2);
        assert_eq!(selected[0].name(), "cavepony");
        assert_eq!(selected[1].name(), "writing");
    }

    #[test]
    fn bypass_excludes_cavepony_for_one_turn() {
        let skills = vec![skill("cavepony", &["cavepony"], 100)];
        let selected =
            select_with_activation("/cavepony", &skills, 3, 400, CaveponyActivation::Bypass);

        assert!(selected.is_empty());
    }

    #[test]
    fn persistent_mode_respects_skill_limits() {
        let skills = vec![skill("cavepony", &["cavepony"], 200)];
        assert!(
            select_with_activation(
                "hello",
                &skills,
                0,
                400,
                CaveponyActivation::Persistent(CaveponyMode::Full),
            )
            .is_empty()
        );
        assert!(
            select_with_activation(
                "hello",
                &skills,
                1,
                100,
                CaveponyActivation::Persistent(CaveponyMode::Full),
            )
            .is_empty()
        );
    }

    #[test]
    fn persistent_mode_does_not_pin_shadowing_skill() {
        let mut shadow = skill("cavepony", &["cavepony"], 100);
        shadow.source = SkillSource::User(PathBuf::from("cavepony"));
        let skills = vec![shadow, skill("writing", &["write"], 100)];

        let selected = select_with_activation(
            "write a note",
            &skills,
            2,
            400,
            CaveponyActivation::Persistent(CaveponyMode::Full),
        );

        assert_eq!(selected.len(), 1);
        assert_eq!(selected[0].name(), "writing");

        let command_selection = select_with_activation(
            "/cavepony full",
            &skills,
            2,
            400,
            CaveponyActivation::Turn(CaveponyMode::Full),
        );
        assert!(command_selection.is_empty());
    }

    #[test]
    fn prompt_instruction_distinguishes_persistent_and_turn_modes() {
        let persistent = CaveponyActivation::Persistent(CaveponyMode::Pony)
            .prompt_instruction()
            .expect("persistent instruction");
        assert!(persistent.contains("scope=\"persistent\""));
        assert!(persistent.contains("mode=\"pony\""));

        let turn = CaveponyActivation::Turn(CaveponyMode::Lite)
            .prompt_instruction()
            .expect("turn instruction");
        assert!(turn.contains("scope=\"turn\""));
        assert!(turn.contains("mode=\"lite\""));

        let tony = CaveponyActivation::Turn(CaveponyMode::TonySoprano)
            .prompt_instruction()
            .expect("Tony Soprano instruction");
        assert!(tony.contains("mode=\"tony-soprano\""));

        let bypass = CaveponyActivation::Bypass
            .prompt_instruction()
            .expect("bypass instruction");
        assert!(bypass.contains("mode=\"off\""));
        assert!(bypass.contains("persistent Cavepony preference unchanged"));
    }
}

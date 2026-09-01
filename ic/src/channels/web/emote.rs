//! Emote parser for the web gateway mascot widget.
//!
//! The agent expresses personality through roleplay action phrases in its
//! replies (`*stamps hooves*`, `*ears perk up*`). This module parses those
//! italic action phrases into a canonical [`Emote`] enum and maps each emote
//! to a sprite animation in the pony/unicorn mascot pack served under
//! `/mascot/`. The web gateway's corner widget consumes the classified
//! emotes and drives its sprite state machine from them.
//!
//! Presentation layer only: parsing never alters message content and a
//! message with no recognized phrases simply produces no emotes.

/// A classified roleplay action phrase, mirroring one sprite in the mascot
/// pack. Sprite names follow the `lunar_<name>.gif` convention of
/// `static/mascot/`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Emote {
    /// Waves, greets, bows — `lunar_greet.gif`.
    Greet,
    /// Hugs, nuzzles, hearts — `lunar_love.gif`.
    Love,
    /// Glares, fumes, stomps in anger — `lunar_rage.gif`. Also the mascot's
    /// error-facing sprite.
    Rage,
    /// Stamps hooves, hops, bounces in excitement — `lunar_jump.gif`.
    Jump,
    /// Trots or paces along — `lunar_walk.gif`. Also the mascot's
    /// working/busy sprite.
    Walk,
    /// Yawns or dozes — `lunar_sleep.gif`. Also the mascot's idle sprite.
    Sleep,
    /// Ears perk up, listens attentively — `lunar_sup.gif`.
    Alert,
    /// Horn glows, flares with intensity — `lunar_fiery.gif`.
    Fiery,
}

/// Maximum emotes attached to a single outgoing message. Long roleplay
/// replies can carry many action phrases; the widget plays them in order,
/// so an unbounded list would keep the mascot busy for minutes.
const MAX_EMOTES_PER_MESSAGE: usize = 4;

/// Longest action phrase considered. Roleplay actions are short; a longer
/// `*...*` span is prose with stray asterisks, not an action.
const MAX_PHRASE_LEN: usize = 120;

/// Keyword table: first emote whose keywords substring-match the normalized
/// phrase wins. Keep more specific entries before broader ones (e.g. the
/// excitement `Jump` keywords before the anger keyword `stomps`).
const KEYWORDS: &[(Emote, &[&str])] = &[
    (
        Emote::Greet,
        &[
            "wave", "greet", "hello", "hi there", "bows", "bow ", "welcome", "good morning",
            "good evening", "salute",
        ],
    ),
    (
        Emote::Love,
        &[
            "love", "heart", "hug", "nuzzle", "cuddle", "adore", "blush", "snuggle", "kiss",
        ],
    ),
    (
        Emote::Jump,
        &[
            "stamp",
            "hop",
            "jump",
            "bounce",
            "prance",
            "dance",
            "squee",
            "excite",
            "yay",
            "twirl",
            "spin",
            "cheer",
            "clap",
            "pumps her hoof",
            "pumps his hoof",
            "pumps a hoof",
            "fist pump",
        ],
    ),
    (
        Emote::Fiery,
        &[
            "ignite",
            "blaze",
            "flare",
            "fiery",
            "charges up",
            "powers up",
            "determined",
            "intense",
            "fire",
            "horn glow",
            "magic glow",
        ],
    ),
    (
        Emote::Rage,
        &[
            "rage", "glare", "fume", "snarl", "growl", "angry", "furious", "stomp", "scowl",
            "grr",
        ],
    ),
    (
        Emote::Alert,
        &[
            "ears perk",
            "perks up",
            "perked",
            "ears up",
            "alert",
            "listen",
            "curious",
            "tilts her head",
            "tilts his head",
            "tilts their head",
            "head tilt",
            "looks up",
            "notice",
            "raises an eyebrow",
            "raises her eyebrow",
            "raises his eyebrow",
            "blink",
            "peers",
            "hmm",
        ],
    ),
    (
        Emote::Sleep,
        &[
            "sleep", "yawn", "doze", "nap", "snore", "sleepy", "tired", "rests her eyes",
            "rests his eyes", "curls up", "drowsy",
        ],
    ),
    (
        Emote::Walk,
        &[
            "trot", "walk", "pace", "stroll", "wander", "march", "gallop", "saunter",
        ],
    ),
];

impl Emote {
    /// Canonical wire name, sent to gateway clients in the `emotes` field of
    /// the assistant `response` event payload.
    pub fn as_str(self) -> &'static str {
        match self {
            Emote::Greet => "greet",
            Emote::Love => "love",
            Emote::Rage => "rage",
            Emote::Jump => "jump",
            Emote::Walk => "walk",
            Emote::Sleep => "sleep",
            Emote::Alert => "alert",
            Emote::Fiery => "fiery",
        }
    }

    /// Sprite file in `static/mascot/` (served at `/mascot/<file>`).
    pub fn sprite_file(self) -> &'static str {
        match self {
            Emote::Greet => "lunar_greet.gif",
            Emote::Love => "lunar_love.gif",
            Emote::Rage => "lunar_rage.gif",
            Emote::Jump => "lunar_jump.gif",
            Emote::Walk => "lunar_walk.gif",
            Emote::Sleep => "lunar_sleep.gif",
            Emote::Alert => "lunar_sup.gif",
            Emote::Fiery => "lunar_fiery.gif",
        }
    }

    /// All distinct sprite files the mascot pack must ship. Used by the
    /// sprite-serving handler's allowlist.
    pub const ALL_SPRITES: &'static [&'static str] = &[
        "lunar_greet.gif",
        "lunar_love.gif",
        "lunar_rage.gif",
        "lunar_jump.gif",
        "lunar_walk.gif",
        "lunar_sleep.gif",
        "lunar_sup.gif",
        "lunar_fiery.gif",
    ];
}

/// Parse the italic roleplay action phrases (`*stamps hooves*`) from an
/// outgoing assistant message and classify them, in order of appearance.
///
/// Returns at most `MAX_EMOTES_PER_MESSAGE` distinct emotes. Text without
/// action phrases — or with phrases that match no keyword — yields an empty
/// list, which the caller treats as "no emote change".
pub fn parse_emotes(message: &str) -> Vec<Emote> {
    let mut emotes: Vec<Emote> = Vec::new();
    for phrase in action_phrases(message) {
        if let Some(emote) = classify_phrase(phrase) {
            if !emotes.contains(&emote) {
                emotes.push(emote);
            }
        }
        if emotes.len() >= MAX_EMOTES_PER_MESSAGE {
            break;
        }
    }
    emotes
}

/// Extract the contents of single-asterisk italic spans (`*...*`) on a
/// single line. Bold (`**...**`) spans yield their inner text because the
/// second asterisk pairs with the opening one — harmless: classification is
/// keyword-driven, so bold prose without action keywords produces no emote.
fn action_phrases(message: &str) -> impl Iterator<Item = &str> {
    let mut rest = message;
    std::iter::from_fn(move || {
        loop {
            let open = rest.find('*')?;
            let after_open = &rest[open + 1..];
            let close = after_open.find('*')?;
            let phrase = &after_open[..close];
            rest = &after_open[close + 1..];
            let phrase = phrase.trim();
            // Skip empty spans, multi-line spans, phrases that still contain
            // an asterisk (nested emphasis), and overlong spans (prose).
            if !phrase.is_empty()
                && phrase.len() <= MAX_PHRASE_LEN
                && !phrase.contains('*')
                && !phrase.contains('\n')
            {
                return Some(phrase);
            }
        }
    })
}

/// Classify a single action phrase: normalize case, then take the first
/// emote whose keyword list substring-matches.
fn classify_phrase(phrase: &str) -> Option<Emote> {
    let normalized = phrase.to_lowercase();
    KEYWORDS
        .iter()
        .find(|(_, keywords)| keywords.iter().any(|kw| normalized.contains(kw)))
        .map(|(emote, _)| *emote)
}

/// Canonical emote names (see [`Emote::as_str`]) for an outgoing assistant
/// message, ready to attach to the gateway `response` event payload.
pub fn emote_names(message: &str) -> Vec<String> {
    parse_emotes(message)
        .into_iter()
        .map(Emote::as_str)
        .map(str::to_string)
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn kinds(message: &str) -> Vec<&'static str> {
        parse_emotes(message)
            .into_iter()
            .map(Emote::as_str)
            .collect()
    }

    #[test]
    fn parses_goal_example_phrases() {
        // The two phrases named in the feature goal.
        assert_eq!(kinds("Sure! *stamps hooves* Here you go."), vec!["jump"]);
        assert_eq!(kinds("Oh? *ears perk* Tell me more."), vec!["alert"]);
        assert_eq!(
            kinds("*stamps hooves* and then *ears perk up*"),
            vec!["jump", "alert"]
        );
    }

    #[test]
    fn maps_each_emote_family() {
        assert_eq!(kinds("*waves hello*"), vec!["greet"]);
        assert_eq!(kinds("*nuzzles you*"), vec!["love"]);
        assert_eq!(kinds("*glares at the broken build*"), vec!["rage"]);
        assert_eq!(kinds("*bounces excitedly*"), vec!["jump"]);
        assert_eq!(kinds("*trots over to the logs*"), vec!["walk"]);
        assert_eq!(kinds("*yawns widely*"), vec!["sleep"]);
        assert_eq!(kinds("*tilts her head, curious*"), vec!["alert"]);
        assert_eq!(kinds("*horn glows brightly*"), vec!["fiery"]);
    }

    #[test]
    fn no_phrase_no_emotes() {
        assert!(kinds("A plain answer with no actions.").is_empty());
        assert!(kinds("Code: `2 * 3 * 7`").is_empty());
        assert!(kinds("").is_empty());
    }

    #[test]
    fn unstarred_keywords_do_not_emote() {
        // Keywords only count inside italic action spans.
        assert!(kinds("I love walks and sleep.").is_empty());
    }

    #[test]
    fn unknown_phrases_produce_no_emotes() {
        assert!(kinds("*considers the problem carefully*").is_empty());
    }

    #[test]
    fn bold_inner_text_is_harmless() {
        // `**bold**` yields the inner span; without keywords it classifies
        // to nothing.
        assert!(kinds("**important notice**").is_empty());
    }

    #[test]
    fn non_action_asterisks_are_ignored() {
        assert!(kinds("a * b").is_empty());
        assert!(kinds("5 * 3 = 15").is_empty());
    }

    #[test]
    fn deduplicates_preserving_order() {
        assert_eq!(
            kinds("*stamps her hooves* then *stamps hooves again* and *hops*"),
            vec!["jump"]
        );
        assert_eq!(
            kinds("*waves* *hugs you* *waves again*"),
            vec!["greet", "love"]
        );
    }

    #[test]
    fn caps_at_four_distinct_emotes() {
        let msg = "*waves* *hugs you* *glares* *bounces* *horn glows* *trots away*";
        let parsed = kinds(msg);
        assert_eq!(parsed.len(), MAX_EMOTES_PER_MESSAGE);
        assert_eq!(parsed, vec!["greet", "love", "rage", "jump"]);
    }

    #[test]
    fn skips_overlong_and_multiline_spans() {
        let long = format!("*{}*", "waves ".repeat(40));
        assert!(kinds(&long).is_empty());
        assert!(kinds("*waves\nover here*").is_empty());
    }

    #[test]
    fn sprite_files_match_pack_and_allowlist() {
        let emotes = [
            Emote::Greet,
            Emote::Love,
            Emote::Rage,
            Emote::Jump,
            Emote::Walk,
            Emote::Sleep,
            Emote::Alert,
            Emote::Fiery,
        ];
        for emote in emotes {
            assert!(Emote::ALL_SPRITES.contains(&emote.sprite_file()));
            assert!(emote.sprite_file().starts_with("lunar_"));
            assert!(emote.sprite_file().ends_with(".gif"));
        }
        assert_eq!(Emote::ALL_SPRITES.len(), emotes.len());
    }

    #[test]
    fn wire_names_are_stable_lowercase() {
        for name in Emote::ALL_SPRITES {
            assert_eq!(name, &name.to_lowercase());
        }
        let parsed = emote_names("*stamps hooves* and *ears perk up*");
        assert_eq!(parsed, vec!["jump".to_string(), "alert".to_string()]);
    }

    #[test]
    fn case_insensitive_keywords() {
        assert_eq!(kinds("*STAMPS HOOVES EXCITEDLY*"), vec!["jump"]);
        assert_eq!(kinds("*Ears Perk Up*"), vec!["alert"]);
    }
}

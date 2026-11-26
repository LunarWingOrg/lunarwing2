use std::iter::Peekable;
use std::ops::Range;
use std::str::Chars;

use super::{Result, denied, is_command_prefix_word};

const MAX_BRACE_WORDS: usize = 256;
const MAX_BRACE_DEPTH: usize = 16;
const MAX_BRACE_BYTES: usize = 64 * 1024;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub(super) enum Expansion {
    #[default]
    None,
    Glob,
    GitGlob,
    Extglob,
    UnexpandedBrace,
}

#[derive(Debug, Clone, Default)]
pub(super) struct Word {
    pub(super) text: String,
    pub(super) dynamic: bool,
    pub(super) expansion: Expansion,
}

impl Word {
    pub(super) fn with_text(&self, text: impl Into<String>) -> Self {
        Self {
            text: text.into(),
            dynamic: self.dynamic,
            expansion: self.expansion,
        }
    }

    pub(super) fn needs_expansion(&self) -> bool {
        self.dynamic || self.expansion != Expansion::None
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Kind {
    Unquoted,
    Literal,
    Dynamic,
    Quote,
}

#[derive(Clone, Copy)]
struct Atom {
    ch: char,
    kind: Kind,
}

impl Atom {
    fn is(self, ch: char) -> bool {
        self.kind == Kind::Unquoted && self.ch == ch
    }
}

fn source_word(atoms: &[Atom], extglob: bool) -> Word {
    let text = atoms
        .iter()
        .filter(|atom| atom.kind != Kind::Quote)
        .map(|atom| atom.ch)
        .collect();
    let expansion = if extglob {
        Expansion::Extglob
    } else if atoms
        .iter()
        .any(|atom| atom.kind == Kind::Unquoted && "*?[".contains(atom.ch))
    {
        classify_glob(atoms)
    } else {
        Expansion::None
    };
    Word {
        text,
        dynamic: atoms.iter().any(|atom| atom.kind == Kind::Dynamic),
        expansion,
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum CasePhase {
    Subject,
    In,
    Pattern,
    Body,
}

#[derive(Default)]
struct Lexer {
    commands: Vec<Vec<Word>>,
    words: Vec<Word>,
    atoms: Vec<Atom>,
    case: Vec<CasePhase>,
    extglob_enabled: bool,
    extglob_word: bool,
}

impl Lexer {
    fn push(&mut self, ch: char, kind: Kind) {
        self.atoms.push(Atom { ch, kind });
    }

    fn push_text(&mut self, text: &str, kind: Kind) {
        self.atoms.extend(text.chars().map(|ch| Atom { ch, kind }));
    }

    fn keyword(&self, keyword: &str) -> bool {
        self.atoms.iter().all(|atom| atom.kind == Kind::Unquoted)
            && self.atoms.iter().map(|atom| atom.ch).eq(keyword.chars())
    }

    fn case_syntax(&mut self) -> bool {
        match self.case.last().copied() {
            Some(CasePhase::Subject) => {
                self.set_case_phase(CasePhase::In);
                true
            }
            Some(CasePhase::In) => {
                if self.keyword("in") {
                    self.set_case_phase(CasePhase::Pattern);
                }
                true
            }
            Some(CasePhase::Pattern) => {
                if self.keyword("esac") {
                    self.case.pop();
                }
                true
            }
            _ if self.keyword("case") && self.before_command() => {
                self.case.push(CasePhase::Subject);
                true
            }
            _ if self.keyword("esac") && self.before_command() && !self.case.is_empty() => {
                self.case.pop();
                true
            }
            _ => false,
        }
    }

    fn before_command(&self) -> bool {
        self.words
            .iter()
            .all(|word| is_command_prefix_word(&word.text))
    }

    fn set_case_phase(&mut self, phase: CasePhase) {
        if let Some(current) = self.case.last_mut() {
            *current = phase;
        }
    }

    fn finish_word(&mut self) {
        if !self.atoms.is_empty() && !self.case_syntax() {
            let first = self.words.len();
            let mut budget = BraceBudget::default();
            if expand_braces(
                &self.atoms,
                self.extglob_word,
                &mut self.words,
                &mut budget,
                0,
            )
            .is_err()
            {
                self.words.truncate(first);
                let mut word = source_word(&self.atoms, self.extglob_word);
                word.expansion = Expansion::UnexpandedBrace;
                self.words.push(word);
            }
        }
        self.atoms.clear();
        self.extglob_word = false;
    }

    fn finish_command(&mut self) {
        if self.words.is_empty() {
            return;
        }
        let command_index = self
            .words
            .iter()
            .position(|word| !is_command_prefix_word(&word.text));
        if let Some((command, options)) =
            command_index.and_then(|index| self.words[index..].split_first())
            && command.text == "shopt"
            && options.iter().any(|word| word.text == "extglob")
            && options.iter().any(|word| word.text == "-s")
        {
            self.extglob_enabled = true;
        }
        self.commands.push(std::mem::take(&mut self.words));
    }

    fn separator(&mut self, ch: char, chars: &mut Peekable<Chars<'_>>) {
        self.finish_word();
        if self.case.last() == Some(&CasePhase::Pattern) {
            if ch == ')' {
                self.set_case_phase(CasePhase::Body);
            }
            return;
        }
        self.finish_command();
        if ch == ';'
            && self.case.last() == Some(&CasePhase::Body)
            && (chars.next_if_eq(&';').is_some() || chars.peek() == Some(&'&'))
        {
            chars.next_if_eq(&'&');
            self.set_case_phase(CasePhase::Pattern);
        }
    }

    fn extglob_starts_here(&self) -> bool {
        self.extglob_enabled
            && self
                .atoms
                .last()
                .is_some_and(|atom| atom.kind == Kind::Unquoted && "@+?*!".contains(atom.ch))
    }

    fn quoted(&mut self, quote: char, chars: &mut Peekable<Chars<'_>>, depth: usize) -> Result<()> {
        self.push('\0', Kind::Quote);
        while let Some(ch) = chars.next() {
            match ch {
                ch if ch == quote => return Ok(()),
                '\\' if quote == '"' => match chars.next() {
                    Some('\n') | None => {}
                    Some(next @ ('\\' | '"' | '$' | '`')) => self.push(next, Kind::Literal),
                    Some(next) => {
                        self.push('\\', Kind::Literal);
                        self.push(next, Kind::Literal);
                    }
                },
                '$' | '`' if quote == '"' => self.dynamic(ch, chars, depth)?,
                _ => self.push(ch, Kind::Literal),
            }
        }
        Err(denied("unterminated shell quote"))
    }

    fn dynamic(&mut self, ch: char, chars: &mut Peekable<Chars<'_>>, depth: usize) -> Result<()> {
        self.push(ch, Kind::Dynamic);
        let group = if ch == '`' {
            Some(('`', '`'))
        } else if chars.next_if_eq(&'(').is_some() {
            self.push('(', Kind::Dynamic);
            Some(('(', ')'))
        } else if chars.next_if_eq(&'{').is_some() {
            self.push('{', Kind::Dynamic);
            Some(('{', '}'))
        } else {
            None
        };
        if let Some((open, close)) = group {
            let body = read_group(chars, open, close)?;
            if open != '{' {
                self.commands
                    .extend(tokenize(&body, self.extglob_enabled, depth + 1)?);
            }
            self.push_text(&body, Kind::Dynamic);
            self.push(close, Kind::Dynamic);
        }
        Ok(())
    }
}

pub(super) fn tokenize_simple_commands(input: &str, extglob: bool) -> Result<Vec<Vec<Word>>> {
    tokenize(input, extglob, 0)
}

fn tokenize(input: &str, extglob: bool, depth: usize) -> Result<Vec<Vec<Word>>> {
    if depth > 4 {
        return Err(denied("nested shell expansion is too deep to authorize"));
    }
    let mut lexer = Lexer {
        extglob_enabled: extglob,
        ..Lexer::default()
    };
    let mut chars = input.chars().peekable();
    while let Some(ch) = chars.next() {
        match ch {
            '\'' | '"' => lexer.quoted(ch, &mut chars, depth)?,
            '\\' => match chars.next() {
                Some('\n') => {}
                Some(next) => lexer.push(next, Kind::Literal),
                None => return Err(denied("incomplete shell escape")),
            },
            '$' if chars.next_if_eq(&'\'').is_some() => {
                lexer.push('\0', Kind::Quote);
                lexer.push_text(&ansi_c_quote(&mut chars)?, Kind::Literal);
            }
            '$' | '`' => lexer.dynamic(ch, &mut chars, depth)?,
            '(' if lexer.extglob_starts_here() => {
                lexer.extglob_word = true;
                lexer.push('(', Kind::Literal);
                lexer.push_text(&read_group(&mut chars, '(', ')')?, Kind::Literal);
                lexer.push(')', Kind::Literal);
            }
            '#' if lexer.atoms.is_empty() => {
                chars.by_ref().take_while(|&ch| ch != '\n').for_each(drop);
                lexer.separator('\n', &mut chars);
            }
            ';' | '\n' | '|' | '&' | '(' | ')' => lexer.separator(ch, &mut chars),
            ' ' | '\t' | '\r' => lexer.finish_word(),
            _ => lexer.push(ch, Kind::Unquoted),
        }
    }
    lexer.finish_word();
    lexer.finish_command();
    Ok(lexer.commands)
}

fn read_group(chars: &mut Peekable<Chars<'_>>, open: char, close: char) -> Result<String> {
    let mut body = String::new();
    let mut quote = None;
    let mut nesting = 1;
    while let Some(ch) = chars.next() {
        if ch == '\\' && quote != Some('\'') {
            body.push(ch);
            if let Some(next) = chars.next() {
                body.push(next);
            }
            continue;
        }
        match quote {
            Some(mark) if ch == mark => quote = None,
            Some(_) => {}
            None if ch == close => {
                nesting -= 1;
                if nesting == 0 {
                    return Ok(body);
                }
            }
            None if ch == open => nesting += 1,
            None if matches!(ch, '\'' | '"' | '`') => quote = Some(ch),
            None => {}
        }
        body.push(ch);
    }
    Err(denied("unterminated shell expansion"))
}

fn ansi_c_quote(chars: &mut Peekable<Chars<'_>>) -> Result<String> {
    let mut bytes = Vec::new();
    while let Some(ch) = chars.next() {
        match ch {
            '\'' => {
                bytes.truncate(
                    bytes
                        .iter()
                        .position(|&byte| byte == 0)
                        .unwrap_or(bytes.len()),
                );
                return Ok(String::from_utf8_lossy(&bytes).into_owned());
            }
            '\\' => ansi_c_escape(chars, &mut bytes)?,
            ch => bytes.extend_from_slice(ch.encode_utf8(&mut [0; 4]).as_bytes()),
        }
    }
    Err(denied("unterminated ANSI-C shell quote"))
}

fn ansi_c_escape(chars: &mut Peekable<Chars<'_>>, bytes: &mut Vec<u8>) -> Result<()> {
    let ch = chars
        .next()
        .ok_or_else(|| denied("incomplete ANSI-C shell escape"))?;
    let byte = match ch {
        'a' => 7,
        'b' => 8,
        'e' | 'E' => 27,
        'f' => 12,
        'n' => 10,
        'r' => 13,
        't' => 9,
        'v' => 11,
        '\\' | '\'' | '"' | '?' => ch as u8,
        '0'..='7' => read_digits(chars, 8, 2, ch.to_digit(8)).unwrap_or_default() as u8,
        'x' => match read_digits(chars, 16, 2, None) {
            Some(value) => value as u8,
            None => return Err(denied("incomplete hexadecimal shell escape")),
        },
        'u' | 'U' => {
            let digits = if ch == 'u' { 4 } else { 8 };
            let value = read_digits(chars, 16, digits, None)
                .and_then(char::from_u32)
                .ok_or_else(|| denied("invalid Unicode shell escape"))?;
            bytes.extend_from_slice(value.encode_utf8(&mut [0; 4]).as_bytes());
            return Ok(());
        }
        'c' => match chars.next() {
            Some('?') => 127,
            Some(value) if value.is_ascii() => (value.to_ascii_uppercase() as u8) & 31,
            _ => return Err(denied("invalid control shell escape")),
        },
        '\n' => return Ok(()),
        other => {
            bytes.push(b'\\');
            bytes.extend_from_slice(other.encode_utf8(&mut [0; 4]).as_bytes());
            return Ok(());
        }
    };
    bytes.push(byte);
    Ok(())
}

fn read_digits(
    chars: &mut Peekable<Chars<'_>>,
    radix: u32,
    limit: usize,
    mut value: Option<u32>,
) -> Option<u32> {
    for _ in 0..limit {
        let Some(digit) = chars.peek().and_then(|ch| ch.to_digit(radix)) else {
            break;
        };
        chars.next();
        value = Some(value.unwrap_or_default() * radix + digit);
    }
    value
}

#[derive(Default)]
struct BraceBudget {
    words: usize,
    bytes: usize,
}

struct BraceGroup {
    open: usize,
    close: usize,
    alternatives: BraceAlternatives,
}

enum BraceAlternatives {
    List(Vec<Range<usize>>),
    Sequence(Vec<String>),
}

fn expand_braces(
    atoms: &[Atom],
    extglob: bool,
    words: &mut Vec<Word>,
    budget: &mut BraceBudget,
    depth: usize,
) -> std::result::Result<(), ()> {
    let group = if atoms.iter().any(|atom| atom.is('{')) {
        brace_group(atoms)?
    } else {
        None
    };
    let Some(group) = group else {
        if depth != 0 {
            budget.words += 1;
            budget.bytes += atoms.len();
            if budget.words > MAX_BRACE_WORDS || budget.bytes > MAX_BRACE_BYTES {
                return Err(());
            }
        }
        if !atoms.is_empty() {
            words.push(source_word(atoms, extglob));
        }
        return Ok(());
    };
    if depth == MAX_BRACE_DEPTH || atoms.len() > MAX_BRACE_BYTES {
        return Err(());
    }
    let mut expanded = Vec::with_capacity(atoms.len());
    let mut expand = |middle: &[Atom]| {
        expanded.clear();
        expanded.extend_from_slice(&atoms[..group.open]);
        expanded.extend_from_slice(middle);
        expanded.extend_from_slice(&atoms[group.close + 1..]);
        expand_braces(&expanded, extglob, words, budget, depth + 1)
    };
    match group.alternatives {
        BraceAlternatives::List(ranges) => {
            for range in ranges {
                expand(&atoms[range])?;
            }
        }
        BraceAlternatives::Sequence(values) => {
            for value in values {
                let middle: Vec<_> = value
                    .chars()
                    .map(|ch| Atom {
                        ch,
                        kind: Kind::Unquoted,
                    })
                    .collect();
                expand(&middle)?;
            }
        }
    }
    Ok(())
}

fn brace_group(atoms: &[Atom]) -> std::result::Result<Option<BraceGroup>, ()> {
    let mut stack = Vec::<(usize, Vec<usize>)>::new();
    let mut first = None::<BraceGroup>;
    for (index, atom) in atoms
        .iter()
        .enumerate()
        .filter(|(_, atom)| atom.kind == Kind::Unquoted)
    {
        match atom.ch {
            '{' if stack.len() == MAX_BRACE_DEPTH => return Err(()),
            '{' => stack.push((index, Vec::new())),
            ',' => {
                if let Some((_, commas)) = stack.last_mut() {
                    commas.push(index);
                }
            }
            '}' => {
                let Some((open, commas)) = stack.pop() else {
                    continue;
                };
                if first.as_ref().is_none_or(|group| open < group.open)
                    && let Some(alternatives) = brace_alternatives(atoms, open, index, commas)?
                {
                    first = Some(BraceGroup {
                        open,
                        close: index,
                        alternatives,
                    });
                }
                if first.is_some() && stack.is_empty() {
                    break;
                }
            }
            _ => {}
        }
    }
    Ok(first)
}

fn brace_alternatives(
    atoms: &[Atom],
    open: usize,
    close: usize,
    commas: Vec<usize>,
) -> std::result::Result<Option<BraceAlternatives>, ()> {
    let body = &atoms[open + 1..close];
    if !commas.is_empty() {
        if commas.len() >= MAX_BRACE_WORDS || body.iter().any(|atom| atom.kind == Kind::Dynamic) {
            return Err(());
        }
        let starts = std::iter::once(open + 1).chain(commas.iter().map(|index| index + 1));
        let ends = commas.iter().copied().chain(std::iter::once(close));
        return Ok(Some(BraceAlternatives::List(
            starts.zip(ends).map(|(start, end)| start..end).collect(),
        )));
    }
    if !body
        .windows(2)
        .any(|pair| pair[0].is('.') && pair[1].is('.'))
    {
        return Ok(None);
    }
    if body.iter().any(|atom| atom.kind == Kind::Dynamic) {
        return Err(());
    }
    if body.iter().any(|atom| atom.kind != Kind::Unquoted) {
        return Ok(None);
    }
    let text: String = body.iter().map(|atom| atom.ch).collect();
    brace_sequence(&text).map(|sequence| sequence.map(BraceAlternatives::Sequence))
}

fn brace_sequence(text: &str) -> std::result::Result<Option<Vec<String>>, ()> {
    let mut parts = text.split("..");
    let (Some(start), Some(end)) = (parts.next(), parts.next()) else {
        return Ok(None);
    };
    let step = match parts.next() {
        Some(step) if is_integer(step) => {
            step.parse::<i64>().map_err(|_| ())?.unsigned_abs().max(1)
        }
        Some(_) => return Ok(None),
        None => 1,
    };
    if parts.next().is_some() {
        return Ok(None);
    }
    if is_integer(start) && is_integer(end) {
        let width = if [start, end].iter().any(|part| {
            let digits = part.trim_start_matches(['-', '+']);
            digits.len() > 1 && digits.starts_with('0')
        }) {
            start.len().max(end.len())
        } else {
            0
        };
        let start = start.parse::<i64>().map_err(|_| ())?;
        let end = end.parse::<i64>().map_err(|_| ())?;
        return sequence_values(start, end, step, |value| format!("{value:0width$}")).map(Some);
    }
    if let ([start], [end]) = (start.as_bytes(), end.as_bytes())
        && start.is_ascii_alphabetic()
        && end.is_ascii_alphabetic()
    {
        return sequence_values(i64::from(*start), i64::from(*end), step, |value| {
            let ch = char::from(value as u8);
            if ch == '\\' {
                String::new()
            } else {
                ch.to_string()
            }
        })
        .map(Some);
    }
    Ok(None)
}

fn is_integer(text: &str) -> bool {
    let digits = text.strip_prefix(['-', '+']).unwrap_or(text);
    !digits.is_empty() && digits.bytes().all(|byte| byte.is_ascii_digit())
}

fn sequence_values(
    start: i64,
    end: i64,
    step: u64,
    format: impl Fn(i64) -> String,
) -> std::result::Result<Vec<String>, ()> {
    let count = start.abs_diff(end) / step;
    if count >= MAX_BRACE_WORDS as u64 {
        return Err(());
    }
    let direction = if start <= end { 1 } else { -1 };
    let stride = i128::from(step) * direction;
    Ok((0..=count)
        .map(|index| format((i128::from(start) + i128::from(index) * stride) as i64))
        .collect())
}

const GIT_PROGRAMS: &[&str] = &[
    "git",
    "git-archive",
    "git-bisect",
    "git-clone",
    "git-config",
    "git-fetch",
    "git-fetch-pack",
    "git-for-each-repo",
    "git-http-fetch",
    "git-http-push",
    "git-log",
    "git-ls-remote",
    "git-maintenance",
    "git-pull",
    "git-push",
    "git-rebase",
    "git-remote",
    "git-remote-ext",
    "git-remote-fd",
    "git-remote-ftp",
    "git-remote-ftps",
    "git-remote-http",
    "git-remote-https",
    "git-send-pack",
    "git-status",
    "git-submodule",
    "git-submodule--helper",
    "git-version",
];

#[derive(Clone, Copy)]
enum GlobAtom {
    Literal(char),
    Star,
    Any,
    Class { mask: u128, singleton: bool },
}

impl GlobAtom {
    fn accepts(self, byte: u8) -> bool {
        match self {
            Self::Literal(ch) => ch == char::from(byte),
            Self::Class { mask, .. } => mask & (1u128 << byte) != 0,
            Self::Star | Self::Any => true,
        }
    }
}

fn classify_glob(atoms: &[Atom]) -> Expansion {
    let (directory, basename) = atoms
        .iter()
        .rposition(|atom| atom.ch == '/')
        .map_or((&[][..], atoms), |slash| {
            (&atoms[..slash], &atoms[slash + 1..])
        });
    let (pattern, has_glob) = glob_pattern(basename);
    let anchored = pattern.iter().any(|part| {
        matches!(
            part,
            GlobAtom::Literal(_)
                | GlobAtom::Class {
                    singleton: true,
                    ..
                }
        )
    });
    let directories = directory.split(|atom| atom.ch == '/');
    if !has_glob && !directories.clone().any(|part| glob_pattern(part).1) {
        return Expansion::None;
    }
    let git_name = anchored && GIT_PROGRAMS.iter().any(|name| glob_matches(&pattern, name));
    let git_core = anchored
        && !git_name
        && matches_dashed_prefix(&pattern)
        && directories
            .clone()
            .any(|part| glob_matches(&glob_pattern(part).0, "git-core"));
    if git_name || git_core {
        Expansion::GitGlob
    } else {
        Expansion::Glob
    }
}

fn glob_pattern(atoms: &[Atom]) -> (Vec<GlobAtom>, bool) {
    let unquoted_bracket = atoms
        .iter()
        .rposition(|atom| atom.is(']'))
        .unwrap_or_default();
    let mut pattern = Vec::new();
    let mut has_glob = false;
    let mut index = 0;
    while let Some(atom) = atoms.get(index) {
        index += 1;
        let part = match (atom.kind, atom.ch) {
            (Kind::Quote, _) => continue,
            (Kind::Unquoted, '*') => GlobAtom::Star,
            (Kind::Unquoted, '?') => GlobAtom::Any,
            (Kind::Unquoted, '[') if index <= unquoted_bracket => match glob_class(&atoms[index..])
            {
                Some((class, length)) => {
                    index += length;
                    class
                }
                None => GlobAtom::Literal('['),
            },
            (_, ch) => GlobAtom::Literal(ch),
        };
        has_glob |= !matches!(part, GlobAtom::Literal(_));
        pattern.push(part);
    }
    (pattern, has_glob)
}

fn glob_class(mut atoms: &[Atom]) -> Option<(GlobAtom, usize)> {
    let length = atoms.len();
    let negated = atoms
        .first()
        .is_some_and(|atom| atom.is('!') || atom.is('^'));
    atoms = &atoms[usize::from(negated)..];
    let mut mask = 0u128;
    let mut first = true;
    let mut uncertain = false;
    while let Some((atom, tail)) = atoms.split_first() {
        if atom.is(']') && !first {
            let singleton = !uncertain && !negated && mask.count_ones() == 1;
            let mask = if uncertain {
                u128::MAX
            } else if negated {
                !mask
            } else {
                mask
            };
            return Some((GlobAtom::Class { mask, singleton }, length - tail.len()));
        }
        first = false;
        if let Some((element, rest)) = posix_element(atoms) {
            uncertain |= element.is_none();
            mask |= element.unwrap_or_default();
            atoms = rest;
        } else if let [dash, end, rest @ ..] = tail
            && dash.is('-')
            && !end.is(']')
        {
            mask |= ascii_range(atom.ch, end.ch);
            atoms = rest;
        } else {
            mask |= ascii_range(atom.ch, atom.ch);
            atoms = tail;
        }
    }
    None
}

fn ascii_range(start: char, end: char) -> u128 {
    let start = start as u32;
    let end = (end as u32).min(127);
    if start > end {
        return 0;
    }
    (u128::MAX << start) & (u128::MAX >> (127 - end))
}

fn posix_element(atoms: &[Atom]) -> Option<(Option<u128>, &[Atom])> {
    let [open, marker, body @ ..] = atoms else {
        return None;
    };
    if !open.is('[') || marker.kind != Kind::Unquoted || !matches!(marker.ch, ':' | '=' | '.') {
        return None;
    }
    let end = body
        .windows(2)
        .position(|pair| pair[0].is(marker.ch) && pair[1].is(']'))?;
    let content = &body[..end];
    let mask = match (marker.ch, content) {
        (':', content) => ascii_class(&content.iter().map(|atom| atom.ch).collect::<String>()),
        (_, [atom]) if atom.ch.is_ascii() => Some(ascii_range(atom.ch, atom.ch)),
        _ => None,
    };
    Some((mask, &body[end + 2..]))
}

fn ascii_class(name: &str) -> Option<u128> {
    let letters = ascii_range('a', 'z') | ascii_range('A', 'Z');
    let digits = ascii_range('0', '9');
    Some(match name {
        "alnum" => letters | digits,
        "alpha" => letters,
        "blank" => ascii_range(' ', ' ') | ascii_range('\t', '\t'),
        "cntrl" => ascii_range('\0', '\x1f') | ascii_range('\x7f', '\x7f'),
        "digit" => digits,
        "graph" => ascii_range('!', '~'),
        "lower" => ascii_range('a', 'z'),
        "print" => ascii_range(' ', '~'),
        "punct" => ascii_range('!', '~') & !(letters | digits),
        "space" => ascii_range('\t', '\r') | ascii_range(' ', ' '),
        "upper" => ascii_range('A', 'Z'),
        "xdigit" => digits | ascii_range('a', 'f') | ascii_range('A', 'F'),
        _ => return None,
    })
}

fn glob_matches(pattern: &[GlobAtom], name: &str) -> bool {
    let mut remaining = pattern;
    let mut target = name.as_bytes();
    let mut retry = None::<(&[GlobAtom], &[u8])>;
    while let Some((&byte, tail)) = target.split_first() {
        match remaining.split_first() {
            Some((GlobAtom::Star, rest)) => {
                retry = Some((rest, target));
                remaining = rest;
            }
            Some((atom, rest)) if atom.accepts(byte) => {
                remaining = rest;
                target = tail;
            }
            _ => {
                let Some((rest, [_, next @ ..])) = retry else {
                    return false;
                };
                retry = Some((rest, next));
                remaining = rest;
                target = next;
            }
        }
    }
    remaining.iter().all(|atom| matches!(atom, GlobAtom::Star))
}

fn matches_dashed_prefix(pattern: &[GlobAtom]) -> bool {
    let prefix = b"git-";
    let suffix = ascii_range('a', 'z') | ascii_range('0', '9') | ascii_range('-', '-');
    let mut positions = 1u8;
    const AFTER_PREFIX: u8 = 1 << 4;
    const IN_SUFFIX: u8 = 1 << 5;
    for atom in pattern {
        if matches!(atom, GlobAtom::Star) {
            positions |= u8::MAX << positions.trailing_zeros().min(7);
            continue;
        }
        let accepts_suffix = match atom {
            GlobAtom::Literal(ch) => ch.is_ascii() && suffix & (1u128 << (*ch as u32)) != 0,
            GlobAtom::Class { mask, .. } => mask & suffix != 0,
            GlobAtom::Any => true,
            GlobAtom::Star => false,
        };
        let mut next = if accepts_suffix && positions & (AFTER_PREFIX | IN_SUFFIX) != 0 {
            IN_SUFFIX
        } else {
            0
        };
        for (index, byte) in prefix.iter().enumerate() {
            if positions & (1 << index) != 0 && atom.accepts(*byte) {
                next |= 1 << (index + 1);
            }
        }
        if next == 0 {
            return false;
        }
        positions = next;
    }
    positions & IN_SUFFIX != 0
}

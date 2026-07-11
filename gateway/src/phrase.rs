use std::fmt;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct PhraseConfig {
    /// Minimum size for a soft punctuation or whitespace split.
    pub min_chars: usize,
    /// Preferred size when no complete sentence is available.
    pub target_chars: usize,
    /// Hard bound for retained text and a single synthesized phrase.
    pub max_chars: usize,
}

impl Default for PhraseConfig {
    fn default() -> Self {
        Self {
            min_chars: 24,
            target_chars: 96,
            max_chars: 180,
        }
    }
}

impl PhraseConfig {
    pub fn validate(self) -> Result<Self, PhraseError> {
        if self.min_chars == 0
            || self.min_chars > self.target_chars
            || self.target_chars > self.max_chars
        {
            return Err(PhraseError::InvalidConfig);
        }
        Ok(self)
    }
}

#[derive(Debug)]
pub struct PhraseSegmenter {
    config: PhraseConfig,
    pending: String,
}

impl PhraseSegmenter {
    pub fn new(config: PhraseConfig) -> Result<Self, PhraseError> {
        Ok(Self {
            config: config.validate()?,
            pending: String::new(),
        })
    }

    /// Append a streaming text delta and return complete, phrase-safe chunks.
    ///
    /// Sentence punctuation wins. If the model does not punctuate promptly,
    /// the segmenter falls back to clause punctuation near `target_chars` and
    /// finally whitespace at `max_chars`, keeping memory and TTS request size
    /// bounded without cutting a UTF-8 code point.
    pub fn push(&mut self, delta: &str) -> Vec<String> {
        self.pending.push_str(delta);
        let mut phrases = Vec::new();

        while let Some(end) = find_boundary(&self.pending, self.config) {
            let remainder = self.pending.split_off(end);
            let phrase = std::mem::replace(&mut self.pending, remainder);
            self.pending = self.pending.trim_start().to_string();
            let phrase = phrase.trim();
            if !phrase.is_empty() {
                phrases.push(phrase.to_string());
            }
        }
        phrases
    }

    pub fn finish(&mut self) -> Option<String> {
        let phrase = std::mem::take(&mut self.pending);
        let phrase = phrase.trim();
        (!phrase.is_empty()).then(|| phrase.to_string())
    }

    #[allow(dead_code)]
    pub fn reset(&mut self) {
        self.pending.clear();
    }

    #[allow(dead_code)]
    pub fn pending(&self) -> &str {
        &self.pending
    }
}

fn find_boundary(text: &str, config: PhraseConfig) -> Option<usize> {
    if text.trim().is_empty() {
        return None;
    }

    let total_chars = text.chars().count();
    let mut chars_seen = 0_usize;
    let mut iterator = text.char_indices().peekable();

    while let Some((index, character)) = iterator.next() {
        chars_seen += 1;
        if chars_seen > config.max_chars {
            break;
        }
        if !is_terminal_punctuation(character) {
            continue;
        }
        if character == '.' && period_is_non_terminal(text, index) {
            continue;
        }

        let mut end = index + character.len_utf8();
        while let Some(&(next_index, next)) = iterator.peek() {
            if is_terminal_punctuation(next) || is_sentence_closer(next) {
                iterator.next();
                chars_seen += 1;
                end = next_index + next.len_utf8();
            } else {
                break;
            }
        }

        let following = text[end..].chars().next();
        let is_boundary = following.is_none_or(char::is_whitespace);
        let only_trailing_whitespace = text[end..].trim().is_empty();
        if is_boundary && (chars_seen >= config.min_chars || only_trailing_whitespace) {
            return Some(end);
        }
    }

    if total_chars >= config.target_chars {
        let mut before_target = None;
        let mut after_target = None;
        let mut count = 0_usize;
        for (index, character) in text.char_indices() {
            count += 1;
            if count > config.max_chars {
                break;
            }
            if count < config.min_chars
                || !is_clause_boundary(character)
                || is_numeric_separator(text, index, character)
            {
                continue;
            }
            let end = index + character.len_utf8();
            if count <= config.target_chars {
                before_target = Some(end);
            } else if after_target.is_none() {
                after_target = Some(end);
            }
        }
        if let Some(end) = before_target.or(after_target) {
            return Some(end);
        }
    }

    if total_chars >= config.max_chars {
        let mut last_whitespace = None;
        let mut count = 0_usize;
        let mut hard_end = text.len();
        for (index, character) in text.char_indices() {
            count += 1;
            if count > config.max_chars {
                hard_end = index;
                break;
            }
            if character.is_whitespace() && count >= config.min_chars {
                last_whitespace = Some(index);
            }
        }
        return Some(last_whitespace.unwrap_or(hard_end));
    }

    None
}

fn is_terminal_punctuation(character: char) -> bool {
    matches!(character, '.' | '!' | '?' | '。' | '！' | '？')
}

fn is_sentence_closer(character: char) -> bool {
    matches!(
        character,
        '"' | '\'' | ')' | ']' | '}' | '»' | '”' | '’' | '›'
    )
}

fn is_clause_boundary(character: char) -> bool {
    matches!(character, ',' | ';' | ':' | '\n' | '，' | '；' | '：' | '—')
}

fn is_numeric_separator(text: &str, index: usize, character: char) -> bool {
    if !matches!(character, ',' | ':' | '，' | '：') {
        return false;
    }
    text[..index]
        .chars()
        .next_back()
        .is_some_and(|previous| previous.is_ascii_digit())
        && text[index + character.len_utf8()..]
            .chars()
            .next()
            .is_some_and(|next| next.is_ascii_digit())
}

fn period_is_non_terminal(text: &str, period_index: usize) -> bool {
    let previous = text[..period_index].chars().next_back();
    let next = text[period_index + 1..].chars().next();
    if previous.is_some_and(|character| character.is_ascii_digit()) {
        // A streamed decimal may be split immediately after its period. Wait
        // for the next delta before deciding; `finish()` still releases a
        // sentence that legitimately ends in a number and a full stop.
        if next.is_none_or(|character| character.is_ascii_digit()) {
            return true;
        }
    }

    let token_start = text[..period_index]
        .char_indices()
        .rev()
        .find_map(|(index, character)| {
            character
                .is_whitespace()
                .then_some(index + character.len_utf8())
        })
        .unwrap_or(0);
    let token = text[token_start..period_index + 1]
        .trim_matches(|character: char| {
            is_sentence_closer(character) || matches!(character, '(' | '[' | '{')
        })
        .to_ascii_lowercase();

    const ABBREVIATIONS: &[&str] = &[
        "mr.", "mrs.", "ms.", "dr.", "prof.", "sr.", "jr.", "st.", "vs.", "etc.", "e.g.", "i.e.",
        "a.m.", "p.m.", "no.", "fig.",
    ];
    if ABBREVIATIONS.contains(&token.as_str()) {
        return true;
    }

    let without_period = token.strip_suffix('.').unwrap_or(&token);
    if without_period.len() == 1
        && without_period
            .bytes()
            .all(|character| character.is_ascii_alphabetic())
    {
        return true;
    }

    // Initialisms such as "U.S." should stay attached to the following
    // token. A final response is still released by `finish()`.
    let pieces: Vec<&str> = without_period.split('.').collect();
    pieces.len() > 1
        && pieces
            .iter()
            .all(|piece| piece.len() == 1 && piece.bytes().all(|byte| byte.is_ascii_alphabetic()))
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum PhraseError {
    InvalidConfig,
}

impl fmt::Display for PhraseError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("phrase segmentation bounds are invalid")
    }
}

impl std::error::Error for PhraseError {}

#[cfg(test)]
mod tests {
    use super::*;

    fn segmenter() -> PhraseSegmenter {
        PhraseSegmenter::new(PhraseConfig {
            min_chars: 12,
            target_chars: 40,
            max_chars: 64,
        })
        .unwrap()
    }

    #[test]
    fn emits_complete_sentences_when_deltas_arrive_one_character_at_a_time() {
        let mut segmenter = segmenter();
        let text = "The kitchen light is on. The back door is locked!";
        let mut phrases = Vec::new();
        for character in text.chars() {
            phrases.extend(segmenter.push(&character.to_string()));
        }
        assert_eq!(
            phrases,
            ["The kitchen light is on.", "The back door is locked!"]
        );
        assert_eq!(segmenter.finish(), None);
    }

    #[test]
    fn does_not_split_titles_initialisms_or_decimal_numbers() {
        let mut phrases_segmenter = segmenter();
        let phrases = phrases_segmenter.push("Dr. A. Smith measured 3.14 metres. Next sentence.");
        assert_eq!(
            phrases,
            ["Dr. A. Smith measured 3.14 metres.", "Next sentence."]
        );

        let mut initials = segmenter();
        assert!(initials.push("The U.S. forecast").is_empty());
        assert_eq!(initials.finish().as_deref(), Some("The U.S. forecast"));

        let mut fragmented_decimal = segmenter();
        assert!(fragmented_decimal.push("The reading is 3.").is_empty());
        assert_eq!(
            fragmented_decimal.push("14 metres. "),
            ["The reading is 3.14 metres."]
        );
    }

    #[test]
    fn keeps_quotes_and_unicode_sentence_closers_with_the_phrase() {
        let mut segmenter = segmenter();
        let phrases = segmenter.push("She said, “Ready?” 下一句很好。 ");
        assert_eq!(
            phrases,
            vec!["She said, “Ready?”".to_string(), "下一句很好。".to_string()]
        );
    }

    #[test]
    fn uses_clause_punctuation_near_the_target_when_sentences_run_long() {
        let mut segmenter = PhraseSegmenter::new(PhraseConfig {
            min_chars: 10,
            target_chars: 24,
            max_chars: 48,
        })
        .unwrap();
        let phrases = segmenter
            .push("This answer has a useful clause, and then it keeps going without a period");
        assert_eq!(
            phrases.first().map(String::as_str),
            Some("This answer has a useful clause,")
        );
        assert!(segmenter.pending().starts_with("and then"));

        let mut numeric = PhraseSegmenter::new(PhraseConfig {
            min_chars: 10,
            target_chars: 24,
            max_chars: 48,
        })
        .unwrap();
        let phrases = numeric.push("The long calculated total is 1,000 units and keeps growing");
        assert!(phrases.iter().all(|phrase| !phrase.ends_with("1,")));
    }

    #[test]
    fn hard_bound_splits_at_whitespace_without_breaking_utf8() {
        let mut segmenter = PhraseSegmenter::new(PhraseConfig {
            min_chars: 4,
            target_chars: 8,
            max_chars: 10,
        })
        .unwrap();
        let phrases = segmenter.push("éééé éééé éééé");
        assert!(!phrases.is_empty());
        assert!(phrases.iter().all(|phrase| phrase.chars().count() <= 10));
    }

    #[test]
    fn hard_bound_can_split_an_unbroken_token_at_a_character_boundary() {
        let mut segmenter = PhraseSegmenter::new(PhraseConfig {
            min_chars: 4,
            target_chars: 8,
            max_chars: 10,
        })
        .unwrap();
        let phrases = segmenter.push("abcdefghijklmnop");
        assert_eq!(phrases, ["abcdefghij"]);
        assert_eq!(segmenter.pending(), "klmnop");
    }

    #[test]
    fn a_short_finished_sentence_is_emitted_immediately() {
        let mut segmenter = segmenter();
        assert_eq!(segmenter.push("Yes."), ["Yes."]);
        assert_eq!(segmenter.finish(), None);
    }

    #[test]
    fn finish_flushes_unpunctuated_text_and_reset_drops_it() {
        let mut segmenter = segmenter();
        assert!(segmenter.push("Still working").is_empty());
        assert_eq!(segmenter.finish().as_deref(), Some("Still working"));
        segmenter.push("do not speak this");
        segmenter.reset();
        assert_eq!(segmenter.pending(), "");
        assert_eq!(segmenter.finish(), None);
    }

    #[test]
    fn rejects_incoherent_configuration() {
        for config in [
            PhraseConfig {
                min_chars: 0,
                target_chars: 10,
                max_chars: 20,
            },
            PhraseConfig {
                min_chars: 12,
                target_chars: 10,
                max_chars: 20,
            },
            PhraseConfig {
                min_chars: 10,
                target_chars: 30,
                max_chars: 20,
            },
        ] {
            assert!(matches!(
                PhraseSegmenter::new(config),
                Err(PhraseError::InvalidConfig)
            ));
        }
    }
}

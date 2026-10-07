use std::collections::{HashMap, HashSet};

use std::ops::Range;

use languagetool_rust::api::{
	check::{Data, DataAnnotation, Match, Request},
	server::ServerClient,
};

use crate::{LanguageToolBackend, Suggestion};

// Dictionary words go to the server as markup read as this, so it neither
// spell-checks them nor computes suggestions only to have them filtered out.
// Those suggestions were most of the server's time (Swedish Hunspell). A digit,
// like the raw placeholder in `convert`, so no word rule applies to it.
const DICTIONARY_PLACEHOLDER: &str = "0";
// Characters inside a word. `LIA:s` stays one word and is not mistaken for
// the dictionary word `LIA`.
const WORD_CONNECTORS: &[char] = &['-', '.', ':', '\'', '’', '_'];
// Besides whitespace and ASCII punctuation, these end a word. Every other
// character is part of one, so a word with a combining accent, a soft hyphen
// or a zero-width space in it is not taken for the dictionary word it begins
// with: the server would see something else than what was written.
const WORD_BOUNDARIES: &str = "–—…“”„«»‘‚‹›";

#[derive(Debug)]
pub struct LanguageToolRemote {
	server_client: ServerClient,
	disabled_categories: HashMap<String, Vec<String>>,
	allowed_words: HashMap<String, HashSet<String>>,
	username: Option<String>,
	api_key: Option<String>,
}

impl LanguageToolRemote {
	pub fn new(
		hostname: &str,
		port: &str,
		username: Option<String>,
		api_key: Option<String>,
	) -> anyhow::Result<Self> {
		let server_client = ServerClient::new(hostname, port);
		Ok(Self {
			server_client,
			disabled_categories: HashMap::new(),
			allowed_words: HashMap::new(),
			username,
			api_key,
		})
	}
}

impl LanguageToolBackend for LanguageToolRemote {
	async fn allow_words(&mut self, lang: String, words: &[String]) -> anyhow::Result<()> {
		self.allowed_words
			.entry(lang)
			.or_default()
			.extend(words.iter().map(Clone::clone));
		Ok(())
	}

	async fn disable_checks(&mut self, lang: String, checks: &[String]) -> anyhow::Result<()> {
		self.disabled_categories.insert(lang, checks.to_vec());
		Ok(())
	}

	async fn check_text(&self, lang: String, text: &str) -> anyhow::Result<Vec<crate::Suggestion>> {
		let disabled_rules = self.disabled_categories.get(&lang).cloned();
		let allowed = self.allowed_words.get(&lang);

		let req = match allowed {
			Some(allowed) => Request::default().with_data(annotate(text, allowed)),
			None => Request::default().with_text(text),
		};
		let mut req = req.with_language(lang);
		req.disabled_rules = disabled_rules;
		req.username = self.username.clone();
		req.api_key = self.api_key.clone();

		let response = self.server_client.check(&req).await?;

		let mut suggestions = Vec::with_capacity(response.matches.len());
		for m in response.matches {
			if let Some(allowed) = allowed
				&& filter_match(&m, allowed)
			{
				continue;
			}
			let suggestion = Suggestion {
				start: m.offset,
				end: m.offset + m.length,
				message: m.message,
				rule_description: m.rule.description,
				rule_id: m.rule.id,
				replacements: m.replacements.into_iter().map(|x| x.value).collect(),
			};
			suggestions.push(suggestion);
		}

		Ok(suggestions)
	}
}

/// The text with every word in `allowed` as markup. Offsets in the response
/// still count in `text`.
///
/// A word that repeats the one before or after it, in any case, stays text:
/// the server only reports "you repeated a word" when it sees the words.
fn annotate<'a>(text: &'a str, allowed: &HashSet<String>) -> Data<'a> {
	let all: Vec<Range<usize>> = words(text).collect();
	let repeats = |a: &Range<usize>, b: &Range<usize>| {
		text[a.clone()].to_lowercase() == text[b.clone()].to_lowercase()
	};
	let mut annotation = Vec::new();
	let mut plain = 0;
	for (i, word) in all.iter().enumerate() {
		let neighbours = [i.checked_sub(1), Some(i + 1)].into_iter().flatten();
		let repeated = neighbours
			.filter_map(|n| all.get(n))
			.any(|n| repeats(word, n));
		if !allowed.contains(&text[word.clone()]) || repeated {
			continue;
		}
		if plain < word.start {
			annotation.push(DataAnnotation::new_text(&text[plain..word.start]));
		}
		annotation.push(DataAnnotation::new_interpreted_markup(
			&text[word.clone()],
			DICTIONARY_PLACEHOLDER,
		));
		plain = word.end;
	}
	if plain < text.len() || annotation.is_empty() {
		annotation.push(DataAnnotation::new_text(&text[plain..]));
	}
	annotation.into_iter().collect()
}

/// Byte ranges of the words in `text`: runs of characters between boundaries,
/// without the `WORD_CONNECTORS` at either end.
fn words(text: &str) -> impl Iterator<Item = Range<usize>> + '_ {
	let in_word = |c: char| {
		WORD_CONNECTORS.contains(&c)
			|| !(c.is_whitespace() || c.is_ascii_punctuation() || WORD_BOUNDARIES.contains(c))
	};
	let mut run_start = None;
	text.char_indices()
		.chain([(text.len(), ' ')])
		.filter_map(move |(i, c)| match (run_start, in_word(c)) {
			(None, true) => {
				run_start = Some(i);
				None
			},
			(Some(start), false) => {
				run_start = None;
				let run = &text[start..i];
				let word = run.trim_start_matches(WORD_CONNECTORS);
				let start = start + run.len() - word.len();
				let word = word.trim_end_matches(WORD_CONNECTORS);
				(!word.is_empty()).then(|| start..start + word.len())
			},
			_ => None,
		})
}

fn filter_match(m: &Match, allowed: &HashSet<String>) -> bool {
	if m.context.length == 0 {
		return false;
	}
	let mut iter = m.context.text.char_indices();
	let Some((start, _)) = iter.nth(m.context.offset) else {
		return false;
	};
	let Some((end, _)) = iter.nth(m.context.length - 1) else {
		return false;
	};
	let text = &m.context.text[start..end];
	allowed.contains(text)
}

#[cfg(test)]
mod tests {
	use super::*;

	fn annotated(text: &str, allowed: &[&str]) -> Vec<(Option<String>, Option<String>)> {
		let allowed = allowed.iter().map(|word| word.to_string()).collect();
		annotate(text, &allowed)
			.into_iter()
			.map(|a| (a.text.map(String::from), a.markup.map(String::from)))
			.collect()
	}

	fn text(s: &str) -> (Option<String>, Option<String>) {
		(Some(s.into()), None)
	}

	fn markup(s: &str) -> (Option<String>, Option<String>) {
		(None, Some(s.into()))
	}

	#[test]
	fn test_words() {
		let text = "-LIA:s v2.1.0, kafka-servern (å'ä) _x_";
		let words: Vec<&str> = words(text).map(|word| &text[word]).collect();
		assert_eq!(words, ["LIA:s", "v2.1.0", "kafka-servern", "å'ä", "x"]);
	}

	#[test]
	fn test_dictionary_words_become_markup() {
		assert_eq!(
			annotated("LIA och Kafka, inte LIA:s.", &["LIA", "Kafka"]),
			[
				markup("LIA"),
				text(" och "),
				markup("Kafka"),
				text(", inte LIA:s."),
			]
		);
	}

	#[test]
	fn test_dictionary_word_must_match_exactly() {
		assert_eq!(annotated("lia Lia", &["LIA"]), [text("lia Lia")]);
	}

	#[test]
	fn test_annotation_keeps_the_text() {
		for sample in ["", "LIA", "räksmörgås LIA ärende", "LIA\n\nLIA"] {
			let joined: String = annotate(sample, &["LIA".to_string()].into())
				.into_iter()
				.map(|a| a.text.or(a.markup).unwrap().into_owned())
				.collect();
			assert_eq!(joined, sample);
		}
	}

	#[test]
	fn test_words_ends_at_boundaries() {
		let text = "“LIA” «Kafka» LIA–Kafka a—b a…b (x) x\u{a0}y\tz\r\nw";
		let words: Vec<&str> = words(text).map(|word| &text[word]).collect();
		assert_eq!(
			words,
			[
				"LIA", "Kafka", "LIA", "Kafka", "a", "b", "a", "b", "x", "x", "y", "z", "w"
			]
		);
	}

	#[test]
	fn test_words_keeps_invisible_and_combining_characters() {
		for text in [
			"cafe\u{301}",
			"LIA\u{ad}Kafka",
			"Kafka\u{200b}LIA",
			"LIA\u{200d}x",
			"LIA😀",
			"😀LIA",
			"LIA\u{fe0f}",
		] {
			let words: Vec<&str> = words(text).map(|word| &text[word]).collect();
			assert_eq!(words, [text], "{text:?} must stay one word");
		}
	}

	#[test]
	fn test_words_without_any() {
		for text in [
			"", " ", "\n\n", "---", "...", "-.:'_", " - ", "(),;!?", "“”",
		] {
			assert_eq!(words(text).count(), 0, "{text:?}");
		}
	}

	#[test]
	fn test_words_keeps_inner_connectors() {
		let text = "a--b a..b a:b:c x-.-y";
		let words: Vec<&str> = words(text).map(|word| &text[word]).collect();
		assert_eq!(words, ["a--b", "a..b", "a:b:c", "x-.-y"]);
	}

	#[test]
	fn test_repeated_dictionary_word_stays_text() {
		for sample in [
			"LIA LIA",
			"Kafka Kafka Kafka",
			"lia Lia LIA",
			"LIA lia",
			"LIA\n\nLIA",
			"LIA, LIA",
			"x LIA LIA y",
		] {
			let annotated = annotated(sample, &["LIA", "Kafka"]);
			assert!(
				annotated.iter().all(|(_, markup)| markup.is_none()),
				"{sample:?} must reach the server as written: {annotated:?}"
			);
		}
	}

	#[test]
	fn test_repeat_only_protects_the_repeated_word() {
		assert_eq!(
			annotated("LIA Kafka Kafka och LIA", &["LIA", "Kafka"]),
			[markup("LIA"), text(" Kafka Kafka och "), markup("LIA")]
		);
	}

	#[test]
	fn test_dictionary_word_next_to_a_different_word_is_markup() {
		assert_eq!(
			annotated("LIA Kafka", &["LIA", "Kafka"]),
			[markup("LIA"), text(" "), markup("Kafka")]
		);
		assert_eq!(
			annotated("LIAN LIA", &["LIA"]),
			[text("LIAN "), markup("LIA")]
		);
	}

	#[test]
	fn test_word_glued_to_invisible_character_stays_text() {
		for sample in [
			"cafe\u{301}",
			"LIA\u{ad}Kafka",
			"Kafka\u{200b}LIA",
			"😀LIA😀",
		] {
			assert_eq!(
				annotated(sample, &["cafe", "LIA", "Kafka"]),
				[text(sample)],
				"{sample:?}"
			);
		}
		assert_eq!(annotated("café", &["café"]), [markup("café")]);
	}

	#[test]
	fn test_word_next_to_typographic_punctuation_is_markup() {
		assert_eq!(
			annotated("“LIA” «Kafka»…", &["LIA", "Kafka"]),
			[
				text("“"),
				markup("LIA"),
				text("” «"),
				markup("Kafka"),
				text("»…"),
			]
		);
	}

	#[test]
	fn test_dictionary_entries_that_cannot_match_a_word() {
		// Trailing connector, spaces, and a prefix of the text's word.
		let allowed = ["NIST-", "Access Control Lists", "V2.1", ""];
		for sample in ["NIST-800 NIST-", "Access Control Lists", "V2.1.0", "  "] {
			assert_eq!(annotated(sample, &allowed), [text(sample)], "{sample:?}");
		}
	}

	#[test]
	fn test_dictionary_entry_with_inner_connectors_matches_whole() {
		assert_eq!(
			annotated("ARP-protokollet, V2.1.", &["ARP-protokollet", "V2.1"]),
			[
				markup("ARP-protokollet"),
				text(", "),
				markup("V2.1"),
				text(".")
			]
		);
	}

	#[test]
	fn test_dictionary_numbers_and_edges() {
		assert_eq!(
			annotated("2024 är 0.", &["2024", "0"]),
			[markup("2024"), text(" är "), markup("0"), text(".")]
		);
	}

	#[test]
	fn test_empty_text_is_one_empty_annotation() {
		assert_eq!(annotated("", &["LIA"]), [text("")]);
	}

	#[test]
	fn test_markup_is_read_as_placeholder() {
		let allowed = ["LIA".to_string()].into();
		let data = annotate("a LIA b", &allowed);
		let marked: Vec<_> = data
			.annotation
			.iter()
			.filter(|a| a.markup.is_some())
			.collect();
		assert_eq!(marked.len(), 1);
		assert_eq!(
			marked[0].interpret_as.as_deref(),
			Some(DICTIONARY_PLACEHOLDER)
		);
		assert!(
			data.annotation
				.iter()
				.filter(|a| a.markup.is_none())
				.all(|a| a.interpret_as.is_none())
		);
	}

	/// Generated texts: whatever the mix, the annotation must spell the text
	/// out again, mark only dictionary words, and never one that repeats its
	/// neighbour.
	#[test]
	fn test_annotation_invariants_on_generated_text() {
		const PIECES: &[&str] = &[
			"LIA",
			"lia",
			"Kafka",
			"ACL",
			"ARP-protokollet",
			"x",
			"0",
			"2024",
			"NIST-",
			"word",
			" ",
			" ",
			" ",
			"\n",
			"\r\n",
			"\t",
			"\u{a0}",
			"-",
			".",
			":",
			"'",
			"’",
			"_",
			",",
			"(",
			")",
			"“",
			"”",
			"–",
			"…",
			"$",
			"é",
			"e\u{301}",
			"\u{ad}",
			"\u{200b}",
			"😀",
			"𝒜",
			"日本",
			"م",
		];
		let allowed: HashSet<String> = ["LIA", "Kafka", "ACL", "ARP-protokollet", "x", "0", "2024"]
			.into_iter()
			.map(String::from)
			.collect();
		let mut state = 0x2545_f491_4f6c_dd1d_u64;
		let mut next = move || {
			state ^= state << 13;
			state ^= state >> 7;
			state ^= state << 17;
			state
		};
		for _ in 0..5000 {
			let len = (next() % 14) as usize;
			let sample: String = (0..len)
				.map(|_| PIECES[(next() % PIECES.len() as u64) as usize])
				.collect();

			let data = annotate(&sample, &allowed);
			let joined: String = data
				.annotation
				.iter()
				.map(|a| a.text.as_deref().or(a.markup.as_deref()).unwrap())
				.collect();
			assert_eq!(joined, sample, "annotation must keep the text");

			let all: Vec<&str> = words(&sample).map(|word| &sample[word]).collect();
			for a in &data.annotation {
				assert!(a.text.is_some() != a.markup.is_some(), "{sample:?}");
				if let Some(markup) = a.markup.as_deref() {
					assert!(allowed.contains(markup), "{markup:?} in {sample:?}");
					assert_eq!(a.interpret_as.as_deref(), Some(DICTIONARY_PLACEHOLDER));
				}
			}
			for (i, word) in all.iter().enumerate() {
				let neighbour_repeats = [i.checked_sub(1), Some(i + 1)]
					.into_iter()
					.flatten()
					.filter_map(|n| all.get(n))
					.any(|n| n.to_lowercase() == word.to_lowercase());
				if neighbour_repeats {
					let count = data
						.annotation
						.iter()
						.filter(|a| a.markup.as_deref() == Some(word))
						.count();
					assert_eq!(count, 0, "repeated {word:?} marked in {sample:?}");
				}
			}
		}
	}
}

use std::{
	collections::HashSet,
	ops::{Not, Range},
};

use typst::{
	World,
	foundations::{Content, SequenceElem, StyleChain, StyledElem, Value},
	introspection::TagElem,
	model::{HeadingElem, ParElem},
	syntax::{FileId, Source, Span, SyntaxKind},
	text::{Lang, Region, SmartQuoteElem, SpaceElem, TextElem},
};

use crate::Suggestion;

fn is_call_to_ignored_function(
	node: &typst::syntax::LinkedNode,
	ignore_functions: &HashSet<String>,
	ignore_emphasis: bool,
) -> bool {
	match node.kind() {
		SyntaxKind::FuncCall => node
			.leftmost_leaf()
			.map(|leaf| {
				let name = leaf.leaf_text();
				ignore_functions.contains(name.as_str()) || (ignore_emphasis && name == "emph")
			})
			.unwrap_or(false),
		SyntaxKind::Ref => ignore_functions.contains("cite"),
		_ => false,
	}
}

fn should_ignore(
	node: &typst::syntax::LinkedNode,
	ignore_functions: &HashSet<String>,
	ignore_emphasis: bool,
) -> bool {
	let mut current = Some(node);
	while let Some(node) = current {
		if ignore_emphasis && node.kind() == SyntaxKind::Emph {
			return true;
		}
		if is_call_to_ignored_function(node, ignore_functions, ignore_emphasis) {
			return true;
		}
		current = node.parent();
	}
	false
}

#[derive(Debug, Clone)]
struct MappedChar {
	span: Span,
	range: Range<u16>,
	emph: bool,
}

#[derive(Debug)]
pub struct Mapping {
	chars: Vec<MappedChar>,
	language: Lang,
	region: Option<Region>,
}

impl Mapping {
	pub fn location(
		&self,
		suggestion: &Suggestion,
		world: &impl World,
		source: Option<&Source>,
		ignore_functions: &HashSet<String>,
		ignore_emphasis: bool,
	) -> Vec<(FileId, Range<usize>)> {
		let Some(chars) = &self.chars.get(suggestion.start..suggestion.end) else {
			return Vec::new();
		};
		let mut locations = Vec::<(FileId, Range<usize>)>::new();
		for MappedChar { span, range, emph } in chars.iter().cloned() {
			if ignore_emphasis && emph {
				continue;
			}
			let Some(id) = span.id() else {
				continue;
			};
			let source = if let Some(source) = source {
				if source.id() != id {
					continue;
				}
				source.clone()
			} else {
				let Ok(source) = world.source(id) else {
					continue;
				};
				source
			};

			let Some(node) = source.find(span) else {
				continue;
			};

			if should_ignore(&node, ignore_functions, ignore_emphasis) {
				continue;
			}

			match node.kind() {
				SyntaxKind::Text => {
					let start = node.range().start;
					let range = (start + range.start as usize)..(start + range.end as usize);
					match locations.last_mut() {
						Some((last_id, last_range))
							if *last_id == id
								&& (last_range.start..=last_range.end).contains(&range.start) =>
						{
							last_range.end = range.end
						},
						_ => locations.push((id, range)),
					}
				},
				_ => {
					let range = node.range();
					match locations.last_mut() {
						Some((last_id, last_range)) if *last_id == id && *last_range == range => {},
						_ => locations.push((id, range)),
					}
				},
			}
		}
		locations
	}

	pub fn language(&self) -> String {
		match self.region {
			Some(region) => format!(
				"{}-{}",
				self.language.as_str(),
				region.as_str().to_uppercase()
			),
			None => self.language.as_str().into(),
		}
	}
}

const CONTEXT_MAX: usize = 200;

pub(crate) fn utf16_to_byte(text: &str, offset: usize) -> Option<usize> {
	let mut seen = 0;
	for (byte, c) in text.char_indices() {
		if seen == offset {
			return Some(byte);
		}
		seen += c.len_utf16();
		if seen > offset {
			return None;
		}
	}
	(seen == offset).then_some(text.len())
}

fn floor_char_boundary(text: &str, mut byte: usize) -> usize {
	while byte > 0 && !text.is_char_boundary(byte) {
		byte -= 1;
	}
	byte
}

fn ceil_char_boundary(text: &str, mut byte: usize) -> usize {
	while byte < text.len() && !text.is_char_boundary(byte) {
		byte += 1;
	}
	byte
}

pub(crate) fn context_excerpt(text: &str, start: usize, end: usize) -> Option<String> {
	sentence_excerpt(text, start, end).map(|(excerpt, _)| excerpt)
}

/// The sentence around the UTF-16 range `start..end` of `text`, with runs of
/// whitespace collapsed, and the byte range of the match within it.
pub(crate) fn sentence_excerpt(
	text: &str,
	start: usize,
	end: usize,
) -> Option<(String, Range<usize>)> {
	let start_byte = utf16_to_byte(text, start)?;
	let end_byte = utf16_to_byte(text, end)?;
	if start_byte > end_byte {
		return None;
	}

	let sentence_start = text[..start_byte]
		.char_indices()
		.rev()
		.find(|(_, c)| matches!(c, '.' | '!' | '?' | '\n'))
		.map_or(0, |(byte, c)| byte + c.len_utf8());
	let sentence_end = text[end_byte..]
		.char_indices()
		.find(|(_, c)| matches!(c, '.' | '!' | '?' | '\n'))
		.map_or(text.len(), |(byte, c)| end_byte + byte + c.len_utf8());

	let (excerpt_start, excerpt_end) = if sentence_end - sentence_start > CONTEXT_MAX {
		let center = (start_byte + end_byte) / 2;
		let start = ceil_char_boundary(
			text,
			center.saturating_sub(CONTEXT_MAX / 2).max(sentence_start),
		);
		let end = floor_char_boundary(text, (start + CONTEXT_MAX).min(sentence_end));
		// A match longer than the window is kept whole.
		(start.min(start_byte), end.max(start).max(end_byte))
	} else {
		(sentence_start, sentence_end)
	};

	let mut excerpt = String::new();
	if excerpt_start > sentence_start {
		excerpt.push('…');
	}
	push_collapsed(&mut excerpt, &text[excerpt_start..start_byte]);
	let match_start = excerpt.len();
	push_collapsed(&mut excerpt, &text[start_byte..end_byte]);
	let match_end = excerpt.len();
	push_collapsed(&mut excerpt, &text[end_byte..excerpt_end]);
	if excerpt_end < sentence_end {
		excerpt.push('…');
	}

	excerpt.truncate(excerpt.trim_end().len());
	let match_end = match_end.min(excerpt.len());
	Some((excerpt, match_start.min(match_end)..match_end))
}

/// Append `text`, with each run of whitespace as one space and none at the
/// start of `out`.
fn push_collapsed(out: &mut String, text: &str) {
	for c in text.chars() {
		if c.is_whitespace() {
			if out.is_empty().not() && out.ends_with(' ').not() {
				out.push(' ');
			}
		} else {
			out.push(c);
		}
	}
}

pub fn content(
	content: &Content,
	chunk_size: usize,
	file_id: Option<FileId>,
	default_language: Option<(Lang, Option<Region>)>,
) -> Vec<(String, Mapping)> {
	let (default_language, default_region) = default_language.unwrap_or((Lang::ENGLISH, None));
	let mut converter = Converter {
		text: String::new(),
		mapping: Mapping {
			chars: Vec::new(),
			language: default_language,
			region: default_region,
		},
		chunk_size,
		contains_file: false,
		file_id,
		prev: Vec::new(),
		default_language,
		default_region,
		separator_pending: false,
		footnotes: Vec::new(),
		language_set: false,
		emph_depth: 0,
	};
	converter.iter_content(content, StyleChain::default());
	converter.break_chunk();
	converter.prev
}

struct Converter {
	text: String,
	mapping: Mapping,
	chunk_size: usize,
	contains_file: bool,
	file_id: Option<FileId>,
	default_language: Lang,
	default_region: Option<Region>,

	separator_pending: bool,
	footnotes: Vec<Captured>,

	prev: Vec<(String, Mapping)>,
	language_set: bool,
	emph_depth: usize,
}

struct Captured {
	text: String,
	mapping: Mapping,
	contains_file: bool,
	language_set: bool,
}

// Text replacements
const SPACE: &str = " ";
const BREAK: &str = "\n\n";
const QUOTE: &str = "'";
const DOUBLE_QUOTE: &str = "\"";
// Digits keep LanguageTool from applying word rules to code adjacent to prose
// (e.g. `a 0` stays clean) and from treating the placeholder as a word.
const RAW: &str = "0";
// Mark `TextElem`s that replaced elements during realization. See the
// `raw_rule` and `footnote_rule` in lt-world.
const RAW_SENTINEL: &str = "\u{e000}";
const FOOTNOTE_START: &str = "\u{e001}";
const FOOTNOTE_END: &str = "\u{e002}";
// Mark emphasized content during realization. See the `emph_rule` in lt-world.
const EMPH_START: &str = "\u{e003}";
const EMPH_END: &str = "\u{e004}";

impl Converter {
	pub fn break_chunk(&mut self) {
		if self.text.is_empty() {
			return;
		}
		let text = std::mem::take(&mut self.text);
		self.separator_pending = false;
		self.language_set = true;
		let mapping = Mapping {
			chars: Vec::new(),
			language: self.mapping.language,
			region: self.mapping.region,
		};
		let mapping = std::mem::replace(&mut self.mapping, mapping);
		let contains_file = std::mem::take(&mut self.contains_file);

		if self.file_id.is_some() && contains_file.not() {
			return;
		}
		self.prev.push((text, mapping));
	}

	pub fn maybe_add_text(&mut self, text: &str, span: Span) {
		if self.text.ends_with(text) {
			return;
		}
		self.add_text(text, span);
	}

	pub fn add_text(&mut self, text: &str, span: Span) {
		if self.separator_pending {
			self.separator_pending = false;
			let ends_with_space =
				self.text.chars().next_back().is_some_and(char::is_whitespace);
			if !ends_with_space && text.chars().next().is_some_and(char::is_alphanumeric) {
				self.add_text(SPACE, Span::detached());
			}
		}
		if let Some(file) = self.file_id
			&& let Some(current) = span.id()
			&& file == current
		{
			self.contains_file = true;
		}
		self.text += text;
		let emph = self.emph_depth > 0;
		let mut buf = [0; 2];
		for (idx, c) in text.char_indices() {
			let n = c.encode_utf16(&mut buf).len();
			let range = (idx as u16)..((idx + c.len_utf8()) as u16);
			for _ in &buf[..n] {
				self.mapping.chars.push(MappedChar { span, range: range.clone(), emph });
			}
		}
	}

	pub fn add_raw_placeholder(&mut self) {
		if self.text.chars().next_back().is_some_and(char::is_alphanumeric) {
			self.add_text(SPACE, Span::detached());
		}
		self.add_text(RAW, Span::detached());
		self.separator_pending = true;
	}

	pub fn begin_footnote(&mut self) {
		if self.text.chars().next_back().is_some_and(char::is_alphanumeric) {
			self.add_text(SPACE, Span::detached());
		}
		let mapping = Mapping {
			chars: Vec::new(),
			language: self.mapping.language,
			region: self.mapping.region,
		};
		self.footnotes.push(Captured {
			text: std::mem::take(&mut self.text),
			mapping: std::mem::replace(&mut self.mapping, mapping),
			contains_file: std::mem::take(&mut self.contains_file),
			language_set: std::mem::replace(&mut self.language_set, false),
		});
	}

	pub fn end_footnote(&mut self) {
		self.break_chunk();
		if let Some(captured) = self.footnotes.pop() {
			self.text = captured.text;
			self.mapping = captured.mapping;
			self.contains_file = captured.contains_file;
			self.language_set = captured.language_set;
		}
		self.separator_pending = true;
	}

	pub fn iter_content(&mut self, content: &Content, style: StyleChain) {
		if let Some(styled) = content.to_packed::<StyledElem>() {
			let style = style.chain(&styled.styles);
			self.iter_content(&styled.child, style);
		} else if let Some(text) = content.to_packed::<TextElem>() {
			match text.text.as_str() {
				RAW_SENTINEL => {
					self.add_raw_placeholder();
					return;
				},
				FOOTNOTE_START => {
					self.begin_footnote();
					return;
				},
				FOOTNOTE_END => {
					self.end_footnote();
					return;
				},
				EMPH_START => {
					self.emph_depth += 1;
					return;
				},
				EMPH_END => {
					self.emph_depth = self.emph_depth.saturating_sub(1);
					return;
				},
				_ => {},
			}
			let has_lang = style.has(TextElem::lang);
			let has_region = style.has(TextElem::region);
			let lang = if has_lang {
				style.get(TextElem::lang)
			} else {
				self.default_language
			};
			let region = if has_region {
				style.get(TextElem::region)
			} else if has_lang {
				None
			} else {
				self.default_region
			};
			if (self.mapping.language != lang || self.mapping.region != region) && self.language_set
			{
				self.break_chunk();
			}
			self.mapping.language = lang;
			self.mapping.region = region;
			self.language_set = true;
			self.add_text(&text.text, text.span());
		} else if let Some(heading) = content.to_packed::<HeadingElem>() {
			let level = heading.resolve_level(style);
			if level.get() <= 2 {
				self.break_chunk();
			}
			self.iter_content(&heading.body, style);
			if self.text.len() > self.chunk_size {
				self.break_chunk();
			} else {
				self.maybe_add_text(BREAK, heading.span());
			}
		} else if let Some(sequence) = content.to_packed::<SequenceElem>() {
			for child in sequence.children.iter() {
				self.iter_content(child, style);
			}
		} else if let Some(space) = content.to_packed::<SpaceElem>() {
			self.maybe_add_text(SPACE, space.span());
		} else if let Some(smartquote) = content.to_packed::<SmartQuoteElem>() {
			if smartquote.double.get(style) {
				self.add_text(DOUBLE_QUOTE, smartquote.span());
			} else {
				self.add_text(QUOTE, smartquote.span());
			}
		} else if let Some(paragraph) = content.to_packed::<ParElem>() {
			self.iter_content(&paragraph.body, style);
			if self.text.len() > self.chunk_size {
				self.break_chunk();
			} else {
				self.maybe_add_text(BREAK, paragraph.span());
			}
		} else if content.is::<TagElem>() {
			// No text and no space for zero-width introspection tags.
		} else {
			for (_key, field) in content.fields() {
				self.iter_value(&field, style);
			}
			self.maybe_add_text(SPACE, content.span());
		}
	}

	pub fn iter_value(&mut self, value: &Value, style: StyleChain) {
		match value {
			Value::Content(content) => {
				self.iter_content(content, style);
			},
			Value::Array(array) => {
				for value in array.iter() {
					self.iter_value(value, style);
				}
			},
			_ => {},
		}
	}
}

#[cfg(test)]
mod tests {
	use super::*;
	use crate::Suggestion;
	use std::path::Path;

	struct TestHarness<'a> {
		world: lt_world::LtWorldRunning<'a>,
		text: String,
		mapping: Mapping,
		ignore_emphasis: bool,
	}

	impl<'a> TestHarness<'a> {
		fn new(world: &'a lt_world::LtWorld, main_file: &Path) -> Self {
			Self::new_with_options(world, main_file, None, false)
		}

		fn new_with_language(
			world: &'a lt_world::LtWorld,
			main_file: &Path,
			default_language: Option<(Lang, Option<Region>)>,
		) -> Self {
			Self::new_with_options(world, main_file, default_language, false)
		}

		fn new_with_options(
			world: &'a lt_world::LtWorld,
			main_file: &Path,
			default_language: Option<(Lang, Option<Region>)>,
			ignore_emphasis: bool,
		) -> Self {
			let world = world.with_main(main_file.to_path_buf()).unwrap();
			let compiled = world.compile();
			assert!(compiled.errors.is_empty(), "{:?}", compiled.errors);
			let doc = compiled.content.unwrap();
			let paragraphs = content(&doc, 1000, None, default_language);
			assert_eq!(paragraphs.len(), 1, "expected exactly one paragraph");
			let (text, mapping) = paragraphs.into_iter().next().unwrap();
			Self { world, text, mapping, ignore_emphasis }
		}

		fn suggestion_for(&self, needle: &str) -> Suggestion {
			let byte_start = self
				.text
				.find(needle)
				.unwrap_or_else(|| panic!("expected '{}' in text: {:?}", needle, self.text));
			let start = self.text[..byte_start].encode_utf16().count();
			Suggestion {
				start,
				end: start + needle.encode_utf16().count(),
				message: "test".into(),
				replacements: vec![],
				rule_description: "test".into(),
				rule_id: "test".into(),
			}
		}

		fn locations_with_ignore(
			&self,
			suggestion: &Suggestion,
			ignore_functions: &[&str],
		) -> Vec<(typst::syntax::FileId, std::ops::Range<usize>)> {
			let ignore_set: HashSet<String> =
				ignore_functions.iter().map(|s| s.to_string()).collect();
			self.mapping.location(
				suggestion,
				&self.world,
				None,
				&ignore_set,
				self.ignore_emphasis,
			)
		}

		fn is_ignored(&self, needle: &str, ignore_functions: &[&str]) -> bool {
			let suggestion = self.suggestion_for(needle);
			self.locations_with_ignore(&suggestion, ignore_functions)
				.is_empty()
		}
	}

	#[test]
	fn test_reference_keeps_surrounding_text_intact() {
		let world = lt_world::LtWorld::new("example".into(), true);
		let harness = TestHarness::new(&world, Path::new("example/reference.typ"));

		assert_eq!(
			harness.text.matches("I like X.").count(),
			1,
			"a reference must not add a space before the period: {:?}",
			harness.text
		);
		assert_eq!(
			harness.text.matches("I like X .").count(),
			1,
			"a space after a reference must be preserved: {:?}",
			harness.text
		);
	}

	#[test]
	fn test_inline_wrapper_keeps_paragraph() {
		let world = lt_world::LtWorld::new("example".into(), true);
		let harness = TestHarness::new(&world, Path::new("example/inline.typ"));

		assert_eq!(
			harness.text.matches("Utökad Testlicensen.").count(),
			1,
			"an inline wrapper must not split the paragraph or add a space: {:?}",
			harness.text
		);
	}

	#[test]
	fn test_heading_not_glued_to_following_text() {
		let world = lt_world::LtWorld::new("example".into(), true);
		let harness = TestHarness::new(&world, Path::new("example/heading.typ"));

		assert_eq!(
			harness.text.matches("Terminologi\n\nLIA").count(),
			1,
			"a heading must be separated from the following paragraph: {:?}",
			harness.text
		);
	}

	#[test]
	fn test_ignore_functions_filters_ancestors() {
		let world = lt_world::LtWorld::new("example".into(), true);
		let harness = TestHarness::new(&world, Path::new("example/ignore.typ"));

		// lambda is replaced by 0 because it is in an equation
		assert!(
			harness.is_ignored("0", &["ignorespelling"]),
			"lambda should be ignored when ignorespelling is in ignore_functions"
		);
		assert!(
			!harness.is_ignored("0", &[]),
			"lambda should not be ignored when ignorespelling is not in ignore_functions"
		);
	}

	#[test]
	fn test_ignore_functions_content_block_syntax() {
		let world = lt_world::LtWorld::new("example".into(), true);
		let harness = TestHarness::new(&world, Path::new("example/content_block.typ"));

		assert!(
			harness.is_ignored("mistaek", &["prog"]),
			"content in #prog[] should be ignored when prog is in ignore_functions"
		);
		assert!(
			!harness.is_ignored("mistaek", &[]),
			"content in #prog[] should not be ignored when prog is not in ignore_functions"
		);

		assert!(
			harness.is_ignored("anohter", &["prog"]),
			"content in #prog([]) should be ignored when prog is in ignore_functions"
		);
		assert!(
			!harness.is_ignored("anohter", &[]),
			"content in #prog([]) should not be ignored when prog is not in ignore_functions"
		);
	}

	#[test]
	fn test_raw_is_replaced_by_placeholder() {
		let world = lt_world::LtWorld::new("example".into(), true);
		let harness = TestHarness::new(&world, Path::new("example/raw.typ"));

		assert_eq!(
			RAW_SENTINEL,
			lt_world::RAW_PLACEHOLDER,
			"the converter must recognize the placeholder emitted by lt-world"
		);
		assert!(
			!harness.text.contains(RAW_SENTINEL),
			"the raw sentinel must not reach LanguageTool: {:?}",
			harness.text
		);
		assert_eq!(
			harness.text.matches("Använd 0 och 0.").count(),
			1,
			"raw must be replaced by a placeholder that keeps punctuation attached: {:?}",
			harness.text
		);
		assert!(
			!harness.text.contains("prechecks") && !harness.text.contains("certificates"),
			"raw content must not be spellchecked: {:?}",
			harness.text
		);
		assert_eq!(
			harness.mapping.language(),
			"sv",
			"raw must not switch the checked language: {:?}",
			harness.text
		);
		assert!(
			harness.is_ignored("0", &[]),
			"suggestions on the raw placeholder must map to no location"
		);
	}

	#[test]
	fn test_raw_is_kept_when_not_ignored() {
		let world = lt_world::LtWorld::new("example".into(), false);
		let world = world.with_main(Path::new("example/raw.typ").to_path_buf()).unwrap();
		let doc = world.compile().content.unwrap();
		let text: String = content(&doc, 1000, None, None)
			.into_iter()
			.map(|(text, _)| text)
			.collect();

		assert!(
			text.contains("prechecks"),
			"raw content must be checked when ignore_raw is disabled: {:?}",
			text
		);
	}

	#[test]
	fn test_raw_placeholder_keeps_adjacent_words_apart() {
		let world = lt_world::LtWorld::new("example".into(), true);
		let harness = TestHarness::new(&world, Path::new("example/raw_glued.typ"));

		assert_eq!(
			harness.text.matches("foo 0 baz").count(),
			1,
			"a placeholder glued to words must be separated: {:?}",
			harness.text
		);
	}

	#[test]
	fn test_raw_in_list_is_replaced_by_placeholder() {
		let world = lt_world::LtWorld::new("example".into(), true);
		let harness = TestHarness::new(&world, Path::new("example/raw_list.typ"));

		assert_eq!(
			harness.text.matches("0 är 1.\n\n0 är 3.").count(),
			1,
			"raw in list items must become placeholders: {:?}",
			harness.text
		);
		assert!(
			!harness.text.contains("adam") && !harness.text.contains("bertil"),
			"raw content must not be spellchecked: {:?}",
			harness.text
		);
	}

	#[test]
	fn test_raw_in_list_is_checked_when_not_ignored() {
		let world = lt_world::LtWorld::new("example".into(), false);
		let world = world.with_main(Path::new("example/raw_list.typ").to_path_buf()).unwrap();
		let doc = world.compile().content.unwrap();
		let text: String = content(&doc, 1000, None, None)
			.into_iter()
			.map(|(text, _)| text)
			.collect();

		assert_eq!(
			text.matches("adam är 1.\n\nbertil är 3.").count(),
			1,
			"raw in list items must be checked when ignore_raw is disabled: {:?}",
			text
		);
	}

	#[test]
	fn test_raw_line_in_show_rule_is_replaced_by_placeholder() {
		let world = lt_world::LtWorld::new("example".into(), true);
		let harness = TestHarness::new(&world, Path::new("example/raw_styled.typ"));

		assert!(
			!harness.text.contains("feeelstavad") && !harness.text.contains("felstavat"),
			"raw lines emitted by show rules must not be spellchecked: {:?}",
			harness.text
		);
		assert!(
			harness.text.contains('0'),
			"raw lines must become placeholders: {:?}",
			harness.text
		);
	}

	#[test]
	fn test_raw_line_in_show_rule_is_checked_when_not_ignored() {
		let world = lt_world::LtWorld::new("example".into(), false);
		let world = world.with_main(Path::new("example/raw_styled.typ").to_path_buf()).unwrap();
		let doc = world.compile().content.unwrap();
		let text: String = content(&doc, 1000, None, None)
			.into_iter()
			.map(|(text, _)| text)
			.collect();

		assert!(
			text.contains("feeelstavad") && text.contains("felstavat"),
			"raw lines must be checked when ignore_raw is disabled: {:?}",
			text
		);
	}

	#[test]
	fn test_footnote_in_list_is_checked_separately() {
		let world = lt_world::LtWorld::new("example".into(), true);
		let world = world.with_main(Path::new("example/footnote_list.typ").to_path_buf()).unwrap();
		let doc = world.compile().content.unwrap();
		let paragraphs: Vec<String> = content(&doc, 1000, None, None)
			.into_iter()
			.map(|(text, _)| text)
			.collect();

		assert!(
			paragraphs
				.iter()
				.any(|text| text.trim() == "En punkt med fotnot."),
			"the list item must not be split by the footnote: {:?}",
			paragraphs
		);
		assert!(
			paragraphs.iter().any(|text| text.trim() == "En fotnot."),
			"the footnote body must be checked as its own chunk: {:?}",
			paragraphs
		);
	}

	#[test]
	fn test_symbol_in_list_is_checked() {
		let world = lt_world::LtWorld::new("example".into(), true);
		let harness = TestHarness::new(&world, Path::new("example/symbol_list.typ"));

		assert_eq!(
			harness.text.matches("En pil → här.").count(),
			1,
			"symbols in list items must be realized: {:?}",
			harness.text
		);
	}

	#[test]
	fn test_table_cells_are_checked() {
		let world = lt_world::LtWorld::new("example".into(), true);
		let harness = TestHarness::new(&world, Path::new("example/table.typ"));

		assert_eq!(
			harness
				.text
				.matches("0\n\nen analys\n\nmer text\n\n0")
				.count(),
			1,
			"table cells must become separate paragraphs: {:?}",
			harness.text
		);
		assert!(
			!harness.text.contains("adam") && !harness.text.contains("bertil"),
			"raw in table cells must not be spellchecked: {:?}",
			harness.text
		);
	}

	#[test]
	fn test_language_change_splits_chunks() {
		let world = lt_world::LtWorld::new("example".into(), true);
		let world = world.with_main(Path::new("example/main.typ").to_path_buf()).unwrap();
		let doc = world.compile().content.unwrap();
		let languages: Vec<String> = content(&doc, 1000, None, None)
			.into_iter()
			.map(|(_, mapping)| mapping.language())
			.collect();

		assert!(
			languages.iter().any(|lang| lang == "de-DE"),
			"German text must be checked as German: {:?}",
			languages
		);
		assert!(
			languages.iter().any(|lang| lang == "en-GB"),
			"English text must be checked as English: {:?}",
			languages
		);
	}

	#[test]
	fn test_emphasis_is_ignored_when_enabled() {
		let world = lt_world::LtWorld::new("example".into(), true);

		let checked = TestHarness::new(&world, Path::new("example/emph.typ"));
		assert!(
			!checked.is_ignored("prechecks", &[]),
			"emphasis must be checked when ignore_emphasis is disabled"
		);

		let ignored = TestHarness::new_with_options(
			&world,
			Path::new("example/emph.typ"),
			None,
			true,
		);
		assert!(
			ignored.is_ignored("prechecks", &[]),
			"emphasis must be ignored when ignore_emphasis is enabled"
		);
		assert!(
			ignored.is_ignored("certificates", &[]),
			"#emph[..] must be ignored when ignore_emphasis is enabled"
		);
	}

	#[test]
	fn test_emphasis_in_eval_is_ignored_when_enabled() {
		assert_eq!(EMPH_START, lt_world::EMPH_START);
		assert_eq!(EMPH_END, lt_world::EMPH_END);

		let world = lt_world::LtWorld::new("example".into(), true);
		let file = Path::new("example/eval_emph.typ");

		let checked = TestHarness::new(&world, file);
		assert!(
			!checked.is_ignored("feeelstavad", &[]),
			"emphasis in eval'ed markup must be checked when ignore_emphasis is disabled"
		);
		assert!(
			!checked.is_ignored("felstavt", &[]),
			"eval'ed text outside emphasis must always be checked"
		);

		let ignored = TestHarness::new_with_options(&world, file, None, true);
		assert!(
			ignored.is_ignored("feeelstavad", &[]),
			"emphasis in eval'ed markup must be ignored when ignore_emphasis is enabled"
		);
		assert!(
			!ignored.is_ignored("felstavt", &[]),
			"eval'ed text outside emphasis must still be checked"
		);
	}

	#[test]
	fn test_inline_wrappers_in_caption_do_not_insert_space() {
		let world = lt_world::LtWorld::new("example".into(), true);
		let harness = TestHarness::new(&world, Path::new("example/emph_caption.typ"));

		assert_eq!(
			harness.text.matches("Modellen analys och viktig.").count(),
			1,
			"inline wrappers in a caption must not insert spaces: {:?}",
			harness.text
		);
	}

	#[test]
	fn test_footnote_is_checked_separately() {
		assert_eq!(FOOTNOTE_START, lt_world::FOOTNOTE_START);
		assert_eq!(FOOTNOTE_END, lt_world::FOOTNOTE_END);

		let world = lt_world::LtWorld::new("example".into(), true);
		let world = world.with_main(Path::new("example/footnote.typ").to_path_buf()).unwrap();
		let doc = world.compile().content.unwrap();
		let paragraphs: Vec<String> = content(&doc, 1000, None, None)
			.into_iter()
			.map(|(text, _)| text)
			.collect();

		assert!(
			paragraphs
				.iter()
				.any(|text| text.contains("En mening med fotnot.")),
			"the main text must not be split by the footnote: {:?}",
			paragraphs
		);
		assert!(
			paragraphs.iter().any(|text| text.trim() == "En fotnot."),
			"the footnote body must be checked as its own chunk: {:?}",
			paragraphs
		);
		assert!(
			!paragraphs.iter().any(|text| text.contains("fotnot. med")),
			"the footnote must not terminate the main sentence: {:?}",
			paragraphs
		);
	}

	#[test]
	fn test_footnote_keeps_adjacent_words_apart() {
		let world = lt_world::LtWorld::new("example".into(), true);
		let world = world
			.with_main(Path::new("example/footnote_glued.typ").to_path_buf())
			.unwrap();
		let doc = world.compile().content.unwrap();
		let paragraphs: Vec<String> = content(&doc, 1000, None, None)
			.into_iter()
			.map(|(text, _)| text)
			.collect();

		assert!(
			paragraphs.iter().any(|text| text.contains("Before after")),
			"words glued to the footnote must stay separate: {:?}",
			paragraphs
		);
		assert!(
			paragraphs.iter().any(|text| text.trim() == "Note text"),
			"the footnote body must be checked as its own chunk: {:?}",
			paragraphs
		);
	}

	#[test]
	fn test_compile_errors_are_reported() {
		let world = lt_world::LtWorld::new("example".into(), true);

		let fatal = world
			.with_main(Path::new("example/broken.typ").to_path_buf())
			.unwrap()
			.compile();
		assert!(fatal.content.is_none(), "a fatal evaluation error must not produce content");
		assert!(!fatal.errors.is_empty(), "a fatal evaluation error must be reported");

		let delayed = world
			.with_main(Path::new("example/broken_show.typ").to_path_buf())
			.unwrap()
			.compile();
		assert!(delayed.content.is_some(), "partial content must survive a show rule error");
		assert!(!delayed.errors.is_empty(), "a show rule error must be reported");

		let doc = delayed.content.unwrap();
		let text: String = content(&doc, 1000, None, None)
			.into_iter()
			.map(|(text, _)| text)
			.collect();
		assert!(
			text.contains("This is fine."),
			"the rest of the document must still be checked: {:?}",
			text
		);
	}

	#[test]
	fn test_default_language_when_document_sets_none() {
		let world = lt_world::LtWorld::new("example".into(), true);
		let default = crate::parse_language("sv-SE").unwrap();
		let harness = TestHarness::new_with_language(
			&world,
			Path::new("example/reference.typ"),
			Some(default),
		);

		assert_eq!(
			harness.mapping.language(),
			"sv-SE",
			"default language must be used when the document does not set one"
		);
	}

	#[test]
	fn test_default_language_is_overridden_by_document() {
		let world = lt_world::LtWorld::new("example".into(), true);
		let default = crate::parse_language("sv-SE").unwrap();
		let harness = TestHarness::new_with_language(
			&world,
			Path::new("example/other.typ"),
			Some(default),
		);

		assert_eq!(
			harness.mapping.language(),
			"en-GB",
			"the language set in the document must win over the default"
		);
	}

	#[test]
	fn test_utf16_to_byte() {
		let text = "aä😀b";
		assert_eq!(utf16_to_byte(text, 0), Some(0));
		assert_eq!(utf16_to_byte(text, 1), Some(1));
		assert_eq!(utf16_to_byte(text, 2), Some(3));
		assert_eq!(utf16_to_byte(text, 3), None);
		assert_eq!(utf16_to_byte(text, 4), Some(7));
		assert_eq!(utf16_to_byte(text, 5), Some(8));
		assert_eq!(utf16_to_byte(text, 6), None);
	}

	#[test]
	fn test_context_excerpt_sentence() {
		let text = "First sentence. This has a misspeled word in it. Third one.";
		let start = text[..text.find("misspeled").unwrap()].encode_utf16().count();
		let end = start + "misspeled".encode_utf16().count();
		assert_eq!(
			context_excerpt(text, start, end).as_deref(),
			Some("This has a misspeled word in it.")
		);
	}

	#[test]
	fn test_context_excerpt_newline() {
		let text = "Heading\nmisspeled word here\nTail";
		let start = text[..text.find("misspeled").unwrap()].encode_utf16().count();
		let end = start + "misspeled".encode_utf16().count();
		assert_eq!(
			context_excerpt(text, start, end).as_deref(),
			Some("misspeled word here")
		);
	}

	#[test]
	fn test_context_excerpt_truncates() {
		let padding = "word ".repeat(100);
		let text = format!("{padding}misspeled{padding}");
		let start = text[..text.find("misspeled").unwrap()].encode_utf16().count();
		let end = start + "misspeled".encode_utf16().count();
		let context = context_excerpt(&text, start, end).unwrap();
		assert!(context.contains("misspeled"), "{context:?}");
		assert!(context.starts_with('…') && context.ends_with('…'), "{context:?}");
	}

	#[test]
	fn test_sentence_excerpt_match_range() {
		let text = "First.\n  Här  är ett  felstavatt\tord. Last.";
		let start = text[..text.find("felstavatt").unwrap()]
			.encode_utf16()
			.count();
		let end = start + "felstavatt".encode_utf16().count();
		let (excerpt, range) = sentence_excerpt(text, start, end).unwrap();
		assert_eq!(excerpt, "Här är ett felstavatt ord.");
		assert_eq!(&excerpt[range], "felstavatt");
	}

	#[test]
	fn test_sentence_excerpt_truncated_match_range() {
		let padding = "ord ".repeat(100);
		let text = format!("{padding}felstavatt {padding}");
		let start = text[..text.find("felstavatt").unwrap()]
			.encode_utf16()
			.count();
		let end = start + "felstavatt".encode_utf16().count();
		let (excerpt, range) = sentence_excerpt(&text, start, end).unwrap();
		assert!(
			excerpt.starts_with('…') && excerpt.ends_with('…'),
			"{excerpt:?}"
		);
		assert_eq!(&excerpt[range], "felstavatt");
	}

	#[test]
	fn test_context_excerpt_invalid_offsets() {
		assert_eq!(context_excerpt("abc", 1, 5), None);
		assert_eq!(context_excerpt("😀", 1, 2), None);
	}

	fn add_diagnostic(harness: &TestHarness, needle: &str) -> Vec<crate::Diagnostic> {
		let suggestion = harness.suggestion_for(needle);
		let mut collector = crate::FileCollector::new(None, &harness.world);
		collector.add(
			&harness.world,
			&[suggestion],
			&harness.mapping,
			&harness.text,
			&HashSet::new(),
			false,
		);
		collector.finish()
	}

	#[test]
	fn test_context_absent_for_source_text() {
		let world = lt_world::LtWorld::new("example".into(), true);
		let harness = TestHarness::new(&world, Path::new("example/inline.typ"));

		let diagnostics = add_diagnostic(&harness, "Testlicensen");
		assert_eq!(diagnostics.len(), 1);
		assert!(diagnostics[0].context.is_none(), "{:?}", diagnostics[0].context);
	}

	#[test]
	fn test_context_for_eval_content() {
		let world = lt_world::LtWorld::new("example".into(), true);
		let harness = TestHarness::new(&world, Path::new("example/eval.typ"));

		let diagnostics = add_diagnostic(&harness, "felstavat");
		assert_eq!(diagnostics.len(), 1);
		let context = diagnostics[0].context.as_deref().unwrap();
		assert!(context.contains("felstavat"), "{context:?}");
		assert_eq!(diagnostics[0].matched(), "felstavat");
		assert!(diagnostics[0].sentence.contains("felstavat"));
	}

	#[test]
	fn test_sentence_for_source_text() {
		let world = lt_world::LtWorld::new("example".into(), true);
		let harness = TestHarness::new(&world, Path::new("example/inline.typ"));

		let diagnostics = add_diagnostic(&harness, "Testlicensen");
		assert_eq!(diagnostics[0].matched(), "Testlicensen");
	}
}

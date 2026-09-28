use std::{
	collections::HashSet,
	ops::{Not, Range},
};

use typst::{
	World,
	foundations::{Content, SequenceElem, StyleChain, StyledElem, Value},
	introspection::TagElem,
	math::EquationElem,
	model::{CiteElem, FigureElem, HeadingElem, ParElem, ParbreakElem, RefElem},
	syntax::{FileId, Source, Span, SyntaxKind},
	text::{Lang, Region, SpaceElem, SmartQuoteElem, TextElem},
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

#[derive(Debug)]
pub struct Mapping {
	chars: Vec<(Span, Range<u16>)>,
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
		for (span, range) in chars.iter().cloned() {
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
}

struct Captured {
	text: String,
	mapping: Mapping,
	contains_file: bool,
}

// Text replacements
const SPACE: &str = " ";
const BREAK: &str = "\n\n";
const EQUATION: &str = "0";
const REFERENCE: &str = "X";
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

impl Converter {
	pub fn break_chunk(&mut self) {
		if self.text.is_empty() {
			return;
		}
		let text = std::mem::take(&mut self.text);
		self.separator_pending = false;
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
		let mut buf = [0; 2];
		for (idx, c) in text.char_indices() {
			let n = c.encode_utf16(&mut buf).len();
			let range = (idx as u16)..((idx + c.len_utf8()) as u16);
			for _ in &buf[..n] {
				self.mapping.chars.push((span, range.clone()));
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
		});
	}

	pub fn end_footnote(&mut self) {
		self.break_chunk();
		if let Some(captured) = self.footnotes.pop() {
			self.text = captured.text;
			self.mapping = captured.mapping;
			self.contains_file = captured.contains_file;
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
			if self.mapping.language != lang || self.mapping.region != region {
				self.break_chunk();
			}
			self.mapping.language = lang;
			self.mapping.region = region;
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
		} else if let Some(parbreak) = content.to_packed::<ParbreakElem>() {
			if self.text.len() > self.chunk_size {
				self.break_chunk();
			} else {
				self.maybe_add_text(BREAK, parbreak.span());
			}
		} else if let Some(paragraph) = content.to_packed::<ParElem>() {
			self.iter_content(&paragraph.body, style);
			if self.text.len() > self.chunk_size {
				self.break_chunk();
			} else {
				self.maybe_add_text(BREAK, paragraph.span());
			}
		} else if let Some(figure) = content.to_packed::<FigureElem>() {
			if let Some(caption) = figure.caption.get_ref(style) {
				self.iter_content(&caption.body, style);
			}
			self.iter_content(&figure.body, style);
		} else if let Some(equation) = content.to_packed::<EquationElem>() {
			self.add_text(EQUATION, equation.span());
		} else if let Some(cite) = content.to_packed::<RefElem>() {
			self.add_text(REFERENCE, cite.span());
		} else if let Some(cite) = content.to_packed::<CiteElem>() {
			self.add_text(REFERENCE, cite.span());
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
}

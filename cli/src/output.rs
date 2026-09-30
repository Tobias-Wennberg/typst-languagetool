use std::{
	collections::{HashMap, HashSet},
	hash::Hasher,
	io::Write,
	io::stdout,
	ops::Not,
	ops::Range,
	path::Path,
};

use annotate_snippets::{AnnotationKind, Group, Level, Renderer, Snippet};
use serde_json::json;
use siphasher::sip128::{Hasher128, SipHasher13};
use typst::{
	World, WorldExt,
	diag::{Severity, SourceDiagnostic},
	syntax::Source,
};
use typst_languagetool::Diagnostic;

const MAX_SUGGESTIONS: usize = 20;

/// Suggestions in a Code Climate description; the rest are in its body.
const DESCRIPTION_SUGGESTIONS: usize = 3;

const COMPILE_ERROR_CHECK: &str = "typst/compile-error";

fn resolve(
	world: &impl World,
	diagnostic: &SourceDiagnostic,
) -> Option<(Source, String, Range<usize>)> {
	let id = diagnostic.span.id()?;
	let source = world.source(id).ok()?;
	let range = world.range(diagnostic.span)?;
	Some((source, id.vpath().get_without_slash().to_string(), range))
}

pub fn compile_plain(world: &impl World, diagnostic: &SourceDiagnostic) {
	let mut out = stdout().lock();

	match resolve(world, diagnostic) {
		Some((source, file, range)) => {
			let (start_line, start_column) = source
				.lines()
				.byte_to_line_column(range.start)
				.unwrap_or((0, 0));
			let (end_line, end_column) =
				source.lines().byte_to_line_column(range.end).unwrap_or((0, 0));
			write!(
				out,
				"{} {}:{}-{}:{} error {}",
				file,
				start_line + 1,
				start_column + 1,
				end_line + 1,
				end_column + 1,
				diagnostic.message,
			)
			.unwrap();
		},
		None => write!(out, "error {}", diagnostic.message).unwrap(),
	}
	for (idx, hint) in diagnostic.hints.iter().enumerate() {
		let separator = if idx == 0 { " (" } else { ", " };
		write!(out, "{}hint: {}", separator, hint.v).unwrap();
	}
	if diagnostic.hints.is_empty().not() {
		write!(out, ")").unwrap();
	}
	writeln!(out).unwrap();
}

pub fn compile_pretty(world: &impl World, diagnostic: &SourceDiagnostic) {
	let level = match diagnostic.severity {
		Severity::Error => Level::ERROR,
		Severity::Warning => Level::WARNING,
	};

	match resolve(world, diagnostic) {
		Some((source, file, range)) => {
			let start_line = source.lines().byte_to_line(range.start).unwrap_or(0);
			let end_line = source.lines().byte_to_line(range.end).unwrap_or(start_line);
			let text = source.text();
			let context = if start_line == end_line {
				source.lines().line_to_range(start_line).unwrap()
			} else {
				let start = source.lines().line_to_byte(start_line).unwrap();
				let end = source.lines().line_to_byte(end_line + 1).unwrap_or(text.len());
				start..end
			};

			let mut snippet = Snippet::source(&text[context.clone()])
				.line_start(start_line + 1)
				.path(&file)
				.fold(true);
			let start = range.start - context.start;
			let end = range.end - context.start;
			snippet = snippet.annotation(AnnotationKind::Primary.span(start..end));

			let message = level
				.primary_title(diagnostic.message.as_str())
				.element(snippet);
			println!("{}", Renderer::styled().render(&[message]));
		},
		None => {
			let message =
				Group::with_title(level.primary_title(diagnostic.message.as_str()));
			println!("{}", Renderer::styled().render(&[message]));
		},
	}

	for hint in &diagnostic.hints {
		println!("  hint: {}", hint.v);
	}
}

pub fn plain(file: &str, source: &Source, diagnostic: Diagnostic) {
	let mut out = stdout().lock();

	let (start_line, start_column) = source
		.lines()
		.byte_to_line_column(diagnostic.locations[0].1.start)
		.unwrap();
	let (end_line, end_column) = source
		.lines()
		.byte_to_line_column(diagnostic.locations[0].1.end)
		.unwrap();
	write!(
		out,
		"{} {}:{}-{}:{} info {}",
		file,
		start_line + 1,
		start_column + 1,
		end_line + 1,
		end_column + 1,
		diagnostic.message,
	)
	.unwrap();

	let mut suggestions = diagnostic
		.replacements
		.into_iter()
		.filter(|suggestion| suggestion.trim().is_empty().not())
		.take(MAX_SUGGESTIONS);
	if let Some(first) = suggestions.next() {
		write!(out, " ({}", first).unwrap();
		for suggestion in suggestions {
			write!(out, ", {}", suggestion).unwrap();
		}
		writeln!(out, ")").unwrap();
	} else {
		writeln!(out).unwrap();
	}
}

pub fn pretty(file: &str, source: &Source, diagnostic: Diagnostic) {
	let start_line = source
		.lines()
		.byte_to_line(diagnostic.locations[0].1.start)
		.unwrap();
	let end_line = source
		.lines()
		.byte_to_line(diagnostic.locations[0].1.end)
		.unwrap();
	let text = source.text();
	let context = if start_line == end_line {
		source.lines().line_to_range(start_line).unwrap()
	} else {
		let start = source.lines().line_to_byte(start_line).unwrap();
		let end = source
			.lines()
			.line_to_byte(end_line + 1)
			.unwrap_or(text.len());
		start..end
	};

	let mut snippet = Snippet::source(&text[context.clone()])
		.line_start(start_line + 1)
		.path(file)
		.fold(true);

	let start = diagnostic.locations[0].1.start - context.start;
	let end = diagnostic.locations[0].1.end - context.start;

	snippet = snippet.annotation(
		AnnotationKind::Primary
			.span(start..end)
			.label(&diagnostic.message),
	);

	for replacement in diagnostic
		.replacements
		.iter()
		.filter(|replacement| replacement.trim().is_empty().not())
		.take(MAX_SUGGESTIONS)
	{
		snippet = snippet.annotation(AnnotationKind::Context.span(start..end).label(replacement));
	}
	let mut message = Level::INFO
		.primary_title(&diagnostic.rule_description)
		.id(&diagnostic.rule_id)
		.element(snippet);
	if let Some(context) = &diagnostic.context {
		message = message.element(Level::NOTE.message(format!("checked text: {context}")));
	}

	let renderer = Renderer::styled();
	println!("{}", renderer.render(&[message]));
}

/// A Code Climate report, the format GitLab reads for code quality.
///
/// Issues are collected for the whole run and written at the end, as one
/// JSON array.
#[derive(Default)]
pub struct CodeClimate {
	issues: Vec<Issue>,
	seen: HashSet<SeenKey>,
}

/// Path, source range, rule, sentence, and the match in the sentence.
type SeenKey = (String, Range<usize>, String, String, Range<usize>);

struct Issue {
	/// What the fingerprint is made of: never the line, so that a finding
	/// keeps its fingerprint when text above it changes.
	identity: Vec<String>,
	check_name: String,
	description: String,
	body: String,
	categories: &'static [&'static str],
	severity: &'static str,
	path: String,
	begin: (usize, usize),
	end: (usize, usize),
}

impl CodeClimate {
	pub fn diagnostic(&mut self, file: &str, source: &Source, diagnostic: Diagnostic) {
		let range = diagnostic.locations[0].1.clone();

		// A file included twice yields each of its findings twice, at the
		// same location with the same sentence. Text from a variable reaches
		// the one location of the expression that inserts it with a different
		// sentence each time, and is kept.
		let key = (
			file.to_string(),
			range.clone(),
			diagnostic.rule_id.clone(),
			diagnostic.sentence.clone(),
			diagnostic.sentence_match.clone(),
		);
		if self.seen.insert(key).not() {
			return;
		}

		let matched = diagnostic.matched();
		let sentence = &diagnostic.sentence;
		let marked = format!(
			"{}»{}«{}",
			sentence
				.get(..diagnostic.sentence_match.start)
				.unwrap_or_default(),
			matched,
			sentence
				.get(diagnostic.sentence_match.end..)
				.unwrap_or_default(),
		);
		let replacements = diagnostic
			.replacements
			.iter()
			.filter(|replacement| replacement.trim().is_empty().not())
			.take(MAX_SUGGESTIONS)
			.collect::<Vec<_>>();
		// The sentence is left as it is so that it can be searched for; `|`
		// in a suggestion would make the last field ambiguous.
		let suggestions = replacements
			.iter()
			.take(DESCRIPTION_SUGGESTIONS)
			.map(|replacement| replacement.replace('|', "¦"))
			.collect::<Vec<_>>()
			.join(", ");

		let mut body = format!(
			"{}\n\n{}\n\nChecked text: {}",
			diagnostic.rule_description, diagnostic.message, marked
		);
		if replacements.is_empty().not() {
			let all = replacements
				.iter()
				.map(|replacement| replacement.as_str())
				.collect::<Vec<_>>()
				.join(", ");
			body.push_str(&format!("\n\nSuggestions: {all}"));
		}

		self.issues.push(Issue {
			identity: vec![file.to_string(), diagnostic.rule_id.clone(), marked.clone()],
			description: format!(
				"Typst-LT: {} | {} | {}",
				diagnostic.rule_id, marked, suggestions
			),
			check_name: diagnostic.rule_id,
			body,
			categories: &["Clarity"],
			severity: "minor",
			path: file.to_string(),
			begin: line_column(source, range.start),
			end: line_column(source, range.end),
		});
	}

	/// A compile error, at `fallback` when it has no location of its own.
	pub fn compile_error(
		&mut self,
		world: &impl World,
		diagnostic: &SourceDiagnostic,
		fallback: &str,
	) {
		let (path, begin, end) = match resolve(world, diagnostic) {
			Some((source, file, range)) => (
				file,
				line_column(&source, range.start),
				line_column(&source, range.end),
			),
			None => (fallback.to_string(), (1, 1), (1, 1)),
		};
		let hints = diagnostic
			.hints
			.iter()
			.map(|hint| hint.v.as_str().replace('|', "¦"))
			.collect::<Vec<_>>()
			.join(", ");

		self.issues.push(Issue {
			identity: vec![
				path.clone(),
				COMPILE_ERROR_CHECK.to_string(),
				diagnostic.message.to_string(),
			],
			check_name: COMPILE_ERROR_CHECK.to_string(),
			description: format!(
				"Typst-LT: {} | {} | {}",
				COMPILE_ERROR_CHECK, diagnostic.message, hints
			),
			body: diagnostic.message.to_string(),
			categories: &["Bug Risk"],
			severity: "major",
			path,
			begin,
			end,
		});
	}

	/// Write the report to `path`, or to stdout. With no issues the report
	/// is `[]`, never absent.
	pub fn write(self, path: Option<&Path>) -> anyhow::Result<()> {
		let report = serde_json::to_string_pretty(&self.into_json())?;
		match path {
			Some(path) => std::fs::write(path, report + "\n")?,
			None => println!("{report}"),
		}
		Ok(())
	}

	fn into_json(self) -> Vec<serde_json::Value> {
		let mut occurrences = HashMap::<Vec<String>, usize>::new();
		self.issues
			.into_iter()
			.map(|issue| {
				// The same finding more than once in a file, at different
				// locations, is told apart by its order.
				let occurrence = occurrences.entry(issue.identity.clone()).or_default();
				let fingerprint = fingerprint(&issue.identity, *occurrence);
				*occurrence += 1;
				json!({
					"type": "issue",
					"check_name": issue.check_name,
					"description": issue.description,
					"content": { "body": issue.body },
					"categories": issue.categories,
					"severity": issue.severity,
					"fingerprint": fingerprint,
					"location": {
						"path": issue.path,
						"positions": {
							"begin": { "line": issue.begin.0, "column": issue.begin.1 },
							"end": { "line": issue.end.0, "column": issue.end.1 },
						},
					},
				})
			})
			.collect()
	}
}

/// One-based line and column, the column in characters.
fn line_column(source: &Source, byte: usize) -> (usize, usize) {
	source
		.lines()
		.byte_to_line_column(byte)
		.map_or((1, 1), |(line, column)| (line + 1, column + 1))
}

/// SipHash-1-3 with fixed keys: the same input gives the same fingerprint on
/// every run and with every Rust version, unlike `DefaultHasher`.
fn fingerprint(identity: &[String], occurrence: usize) -> String {
	let mut hasher = SipHasher13::new();
	for part in identity {
		hasher.write(part.as_bytes());
		hasher.write(&[0]);
	}
	hasher.write(occurrence.to_string().as_bytes());
	format!("{:032x}", u128::from(hasher.finish128()))
}

#[cfg(test)]
mod tests {
	use super::*;

	#[test]
	fn test_fingerprint_is_stable() {
		let identity = vec!["docs/main.typ".to_string(), "RULE".to_string()];
		assert_eq!(fingerprint(&identity, 0), fingerprint(&identity, 0));
		assert_ne!(fingerprint(&identity, 0), fingerprint(&identity, 1));
		assert_eq!(fingerprint(&identity, 0).len(), 32);
	}

	/// A finding at `range` of `source`, where the checked text is `sentence`
	/// and the match is `word` in it.
	fn finding(source: &Source, range: Range<usize>, sentence: &str, word: &str) -> Diagnostic {
		let start = sentence.find(word).unwrap();
		Diagnostic {
			locations: vec![(source.id(), range)],
			message: "Möjligt stavfel".into(),
			replacements: vec!["a|b".into(), "två".into(), "tre".into(), "fyra".into()],
			rule_description: "Stavning".into(),
			rule_id: "MORFOLOGIK_RULE_SV".into(),
			context: None,
			sentence: sentence.into(),
			sentence_match: start..start + word.len(),
		}
	}

	#[test]
	fn test_code_climate_issue() {
		let source = Source::detached("Rad ett.\nDen här är felstavatt.\n");
		let start = source.text().find("felstavatt").unwrap();
		let range = start..start + "felstavatt".len();
		let mut report = CodeClimate::default();
		report.diagnostic(
			"docs/main.typ",
			&source,
			finding(&source, range, "Den här är felstavatt.", "felstavatt"),
		);
		let issues = report.into_json();
		assert_eq!(issues.len(), 1);
		let issue = &issues[0];
		assert_eq!(
			issue["description"],
			"Typst-LT: MORFOLOGIK_RULE_SV | Den här är »felstavatt«. | a¦b, två, tre"
		);
		assert_eq!(issue["check_name"], "MORFOLOGIK_RULE_SV");
		assert_eq!(issue["location"]["path"], "docs/main.typ");
		// `ä` is one column, not two bytes.
		assert_eq!(issue["location"]["positions"]["begin"]["line"], 2);
		assert_eq!(issue["location"]["positions"]["begin"]["column"], 12);
		assert_eq!(issue["location"]["positions"]["end"]["column"], 22);
	}

	#[test]
	fn test_code_climate_deduplicates_included_twice() {
		let source = Source::detached(r#"#include "snippet.typ""#);
		let mut report = CodeClimate::default();
		for _ in 0..2 {
			report.diagnostic(
				"docs/snippet.typ",
				&source,
				finding(&source, 0..5, "Ett felstavatt ord.", "felstavatt"),
			);
		}
		assert_eq!(report.into_json().len(), 1);
	}

	#[test]
	fn test_code_climate_keeps_variable_text() {
		// One expression inserting a different YAML value on each iteration:
		// same location, different sentences, and two matches in one sentence.
		let source = Source::detached("#for v in vars [#v.description]");
		let mut report = CodeClimate::default();
		for (sentence, word) in [
			("Första felstavatt värdet.", "felstavatt"),
			("Andra felstavatt värdet.", "felstavatt"),
			("Andra felstavatt värdet.", "värdet"),
		] {
			report.diagnostic(
				"docs/bilaga.typ",
				&source,
				finding(&source, 16..29, sentence, word),
			);
		}
		let issues = report.into_json();
		assert_eq!(issues.len(), 3);
		let fingerprints = issues
			.iter()
			.map(|issue| issue["fingerprint"].as_str().unwrap())
			.collect::<HashSet<_>>();
		assert_eq!(fingerprints.len(), 3);
	}

	#[test]
	fn test_code_climate_fingerprint_ignores_line() {
		let before = Source::detached("Ett felstavatt ord.");
		let after = Source::detached("Ny rad.\n\nEtt felstavatt ord.");
		let fingerprint_of = |source: &Source, range: Range<usize>| {
			let mut report = CodeClimate::default();
			report.diagnostic(
				"docs/main.typ",
				source,
				finding(source, range, "Ett felstavatt ord.", "felstavatt"),
			);
			report.into_json()[0]["fingerprint"].clone()
		};
		assert_eq!(
			fingerprint_of(&before, 4..14),
			fingerprint_of(&after, 13..23)
		);
	}

	#[test]
	fn test_fingerprint_separates_parts() {
		let a = vec!["ab".to_string(), "c".to_string()];
		let b = vec!["a".to_string(), "bc".to_string()];
		assert_ne!(fingerprint(&a, 0), fingerprint(&b, 0));
	}
}

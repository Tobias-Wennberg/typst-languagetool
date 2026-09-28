use std::{io::Write, io::stdout, ops::Not, ops::Range};

use annotate_snippets::{AnnotationKind, Group, Level, Renderer, Snippet};
use typst::{
	World, WorldExt,
	diag::{Severity, SourceDiagnostic},
	syntax::Source,
};
use typst_languagetool::Diagnostic;

const MAX_SUGGESTIONS: usize = 20;

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
	let message = Level::INFO
		.primary_title(&diagnostic.rule_description)
		.id(&diagnostic.rule_id)
		.element(snippet);

	let renderer = Renderer::styled();
	println!("{}", renderer.render(&[message]));
}

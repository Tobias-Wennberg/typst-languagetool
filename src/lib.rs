mod backends;
pub mod convert;

use std::{
	collections::{HashMap, HashSet},
	ops::Range,
	path::PathBuf,
};

#[allow(unused_imports)]
pub use backends::*;
use convert::{Mapping, context_excerpt, sentence_excerpt, utf16_to_byte};
use typst::{
	World,
	syntax::{FileId, Source},
	text::{Lang, Region},
};

#[allow(async_fn_in_trait)]
pub trait LanguageToolBackend {
	async fn allow_words(&mut self, lang: String, words: &[String]) -> anyhow::Result<()>;
	async fn disable_checks(&mut self, lang: String, checks: &[String]) -> anyhow::Result<()>;
	async fn check_text(&mut self, lang: String, text: &str) -> anyhow::Result<Vec<Suggestion>>;
}

#[derive(Debug)]
pub enum LanguageTool {
	#[cfg(any(feature = "bundle", feature = "jar"))]
	JNI(jni::LanguageToolJNI),
	#[cfg(feature = "server")]
	Remote(remote::LanguageToolRemote),
	#[cfg(test)]
	_TestOnly(std::convert::Infallible),
}

#[cfg(any(feature = "bundle", feature = "jar", feature = "server"))]
impl LanguageTool {
	pub async fn new(options: &LanguageToolOptions) -> anyhow::Result<Self> {
		let mut lt = match &options.backend {
			None => Err(anyhow::anyhow!(
				"No Languagetool Backend (bundle, jar or server) specified."
			))?,

			#[cfg(feature = "bundle")]
			Some(BackendOptions::Bundle) => Self::JNI(jni::LanguageToolJNI::new_bundled()?),

			#[cfg(not(feature = "bundle"))]
			Some(BackendOptions::Bundle) => Err(anyhow::anyhow!("Feature 'bundle' is disabled."))?,

			#[cfg(any(feature = "bundle", feature = "jar"))]
			Some(BackendOptions::Jar { jar_location }) => {
				Self::JNI(jni::LanguageToolJNI::new(jar_location)?)
			},
			#[cfg(all(not(feature = "bundle"), not(feature = "jar")))]
			Some(BackendOptions::Jar { jar_location: _ }) => {
				Err(anyhow::anyhow!("Features 'bundle' and 'jar' are disabled."))?
			},

			#[cfg(feature = "server")]
			Some(BackendOptions::Remote { host, port, username, api_key }) => Self::Remote(
				remote::LanguageToolRemote::new(host, port, username.clone(), api_key.clone())?,
			),

			#[cfg(not(feature = "server"))]
			Some(BackendOptions::Remote {
				host: _,
				port: _,
				username: _,
				api_key: _,
			}) => Err(anyhow::anyhow!("Feature 'server' is disabled."))?,
		};

		for (lang, dict) in &options.dictionary {
			lt.allow_words(lang.clone(), dict).await?;
		}
		for (lang, checks) in &options.disabled_checks {
			lt.disable_checks(lang.clone(), checks).await?;
		}

		Ok(lt)
	}
}

#[cfg(any(feature = "bundle", feature = "jar", feature = "server"))]
impl LanguageToolBackend for LanguageTool {
	async fn allow_words(&mut self, lang: String, words: &[String]) -> anyhow::Result<()> {
		match self {
			#[cfg(any(feature = "bundle", feature = "jar"))]
			Self::JNI(lt) => lt.allow_words(lang, words).await,
			#[cfg(feature = "server")]
			Self::Remote(lt) => lt.allow_words(lang, words).await,

			#[allow(unreachable_patterns)]
			_ => unreachable!("{:?} {:?}", lang, words),
		}
	}
	async fn disable_checks(&mut self, lang: String, checks: &[String]) -> anyhow::Result<()> {
		match self {
			#[cfg(any(feature = "bundle", feature = "jar"))]
			Self::JNI(lt) => lt.disable_checks(lang, checks).await,
			#[cfg(feature = "server")]
			Self::Remote(lt) => lt.disable_checks(lang, checks).await,

			#[allow(unreachable_patterns)]
			_ => unreachable!("{:?} {:?}", lang, checks),
		}
	}
	async fn check_text(&mut self, lang: String, text: &str) -> anyhow::Result<Vec<Suggestion>> {
		match self {
			#[cfg(any(feature = "bundle", feature = "jar"))]
			Self::JNI(lt) => lt.check_text(lang, text).await,
			#[cfg(feature = "server")]
			Self::Remote(lt) => lt.check_text(lang, text).await,

			#[allow(unreachable_patterns)]
			_ => unreachable!("{:?} {:?}", lang, text),
		}
	}
}

pub struct FileCollector {
	source: Option<Source>,
	diagnostics: Vec<Diagnostic>,
}

impl FileCollector {
	pub fn new(file_id: Option<FileId>, world: &impl World) -> Self {
		let source = file_id.map(|id| world.source(id).unwrap());
		Self { source, diagnostics: Vec::new() }
	}

	pub fn add(
		&mut self,
		world: &impl World,
		suggestions: &[Suggestion],
		mapping: &Mapping,
		text: &str,
		ignore_functions: &HashSet<String>,
		ignore_emphasis: bool,
	) {
		let diagnostics = suggestions.iter().filter_map(|suggestion| {
			let locations = mapping.location(
				suggestion,
				world,
				self.source.as_ref(),
				ignore_functions,
				ignore_emphasis,
			);
			if locations.is_empty() {
				return None;
			}
			let context = dynamic_context(world, suggestion, text, &locations);
			let (sentence, sentence_match) =
				sentence_excerpt(text, suggestion.start, suggestion.end).unwrap_or_default();
			let dia = Diagnostic {
				locations,
				message: suggestion.message.clone(),
				replacements: suggestion.replacements.clone(),
				rule_description: suggestion.rule_description.clone(),
				rule_id: suggestion.rule_id.clone(),
				context,
				sentence,
				sentence_match,
			};
			Some(dia)
		});
		self.diagnostics.extend(diagnostics)
	}

	/// The diagnostics added since the last call, for printing them as they
	/// come.
	pub fn take(&mut self) -> Vec<Diagnostic> {
		std::mem::take(&mut self.diagnostics)
	}

	pub fn finish(self) -> Vec<Diagnostic> {
		self.diagnostics
	}
}

fn dynamic_context(
	world: &impl World,
	suggestion: &Suggestion,
	text: &str,
	locations: &[(FileId, Range<usize>)],
) -> Option<String> {
	let start = utf16_to_byte(text, suggestion.start)?;
	let end = utf16_to_byte(text, suggestion.end)?;
	let word = text.get(start..end)?;
	let present = locations.iter().any(|(id, range)| {
		let Ok(source) = world.source(*id) else {
			return false;
		};
		source.text().get(range.clone()).is_some_and(|src| src.contains(word))
	});
	if present {
		return None;
	}
	context_excerpt(text, suggestion.start, suggestion.end)
}

#[derive(Debug, Clone)]
pub struct Diagnostic {
	pub locations: Vec<(FileId, Range<usize>)>,
	pub message: String,
	pub replacements: Vec<String>,
	pub rule_description: String,
	pub rule_id: String,
	/// Checked text around the match, only when the match is not in the
	/// source at its location (text from variables, `eval`, data files).
	pub context: Option<String>,
	/// Checked text of the sentence around the match, always set.
	pub sentence: String,
	/// Byte range of the match in `sentence`.
	pub sentence_match: Range<usize>,
}

impl Diagnostic {
	/// The checked text LanguageTool matched.
	pub fn matched(&self) -> &str {
		self.sentence
			.get(self.sentence_match.clone())
			.unwrap_or_default()
	}
}

#[derive(Debug, Clone)]
pub struct Suggestion {
	pub start: usize,
	pub end: usize,
	pub message: String,
	pub replacements: Vec<String>,
	pub rule_description: String,
	pub rule_id: String,
}

const DEFAULT_CHUNK_SIZE: usize = 1000;

#[derive(serde::Serialize, serde::Deserialize, Debug, Clone)]
#[serde(default)]
pub struct LanguageToolOptions {
	/// Project Root
	pub root: Option<PathBuf>,
	/// Project Main File
	pub main: Option<PathBuf>,
	/// Size for chunk send to LanguageTool
	pub chunk_size: usize,

	#[serde(flatten)]
	pub backend: Option<BackendOptions>,

	/// Additional allowed words
	pub dictionary: HashMap<String, Vec<String>>,
	/// Languagetool rules to ignore (WHITESPACE_RULE, ...)
	pub disabled_checks: HashMap<String, Vec<String>>,
	/// Functions calls to ignore (lorem, bibliography, ...)
	pub ignore_functions: HashSet<String>,

	/// Language used for text without `#set text(lang: ...)`
	/// (e.g. "sv" or "sv-SE")
	pub default_language: Option<String>,

	/// Ignore raw text (inline code and code blocks) when spellchecking
	/// (default: true)
	pub ignore_raw: Option<bool>,
	/// Ignore emphasis (`_..._` and `#emph[..]`) when spellchecking
	/// (default: false)
	pub ignore_emphasis: Option<bool>,
}

#[derive(serde::Serialize, serde::Deserialize, Debug, Clone, PartialEq, Eq)]
#[serde(tag = "backend")]
pub enum BackendOptions {
	#[serde(rename = "bundle")]
	Bundle,
	#[serde(rename = "jar")]
	Jar { jar_location: String },
	#[serde(rename = "server")]
	Remote {
		host: String,
		#[serde(deserialize_with = "string_or_number")]
		port: String,
		/// Username for LanguageTool Premium API
		username: Option<String>,
		/// API key for LanguageTool Premium API
		api_key: Option<String>,
	},
}

impl Default for LanguageToolOptions {
	fn default() -> Self {
		Self {
			root: None,
			main: None,
			chunk_size: DEFAULT_CHUNK_SIZE,

			backend: None,

			dictionary: HashMap::new(),
			disabled_checks: HashMap::new(),
			ignore_functions: ["lorem", "bibliography", "cite"]
				.into_iter()
				.map(String::from)
				.collect(),
			default_language: None,
			ignore_raw: None,
			ignore_emphasis: None,
		}
	}
}

impl LanguageToolOptions {
	pub fn overwrite(mut self, other: Self) -> Self {
		self.dictionary.extend(other.dictionary);
		self.disabled_checks.extend(other.disabled_checks);
		self.ignore_functions.extend(other.ignore_functions);

		Self {
			root: other.root.or(self.root),
			main: other.main.or(self.main),

			chunk_size: match other.chunk_size {
				DEFAULT_CHUNK_SIZE => self.chunk_size,
				_ => other.chunk_size,
			},

			backend: other.backend.or(self.backend),

			dictionary: self.dictionary,
			disabled_checks: self.disabled_checks,
			ignore_functions: self.ignore_functions,
			default_language: other.default_language.or(self.default_language),
			ignore_raw: other.ignore_raw.or(self.ignore_raw),
			ignore_emphasis: other.ignore_emphasis.or(self.ignore_emphasis),
		}
	}
}

/// Parse a language code like `sv` or `sv-SE` into a language and region.
pub fn parse_language(code: &str) -> anyhow::Result<(Lang, Option<Region>)> {
	let (lang, region) = match code.split_once('-') {
		Some((lang, region)) => {
			let lang = lang
				.parse::<Lang>()
				.map_err(|err| anyhow::anyhow!("invalid language '{lang}': {err}"))?;
			let region = region
				.parse::<Region>()
				.map_err(|err| anyhow::anyhow!("invalid region '{region}': {err}"))?;
			(lang, Some(region))
		},
		None => {
			let lang = code
				.parse::<Lang>()
				.map_err(|err| anyhow::anyhow!("invalid language '{code}': {err}"))?;
			(lang, None)
		},
	};
	Ok((lang, region))
}

fn string_or_number<'de, D>(deserializer: D) -> Result<String, D::Error>
where
	D: serde::Deserializer<'de>,
{
	struct StringOrNumberVisitor;

	impl<'de> serde::de::Visitor<'de> for StringOrNumberVisitor {
		type Value = String;

		fn expecting(&self, formatter: &mut std::fmt::Formatter) -> std::fmt::Result {
			formatter.write_str("a string or a number")
		}

		fn visit_str<E>(self, value: &str) -> Result<Self::Value, E>
		where
			E: serde::de::Error,
		{
			Ok(value.to_string())
		}

		fn visit_string<E>(self, value: String) -> Result<Self::Value, E>
		where
			E: serde::de::Error,
		{
			Ok(value)
		}

		fn visit_i64<E>(self, value: i64) -> Result<Self::Value, E>
		where
			E: serde::de::Error,
		{
			Ok(value.to_string())
		}

		fn visit_u64<E>(self, value: u64) -> Result<Self::Value, E>
		where
			E: serde::de::Error,
		{
			Ok(value.to_string())
		}

		fn visit_f64<E>(self, value: f64) -> Result<Self::Value, E>
		where
			E: serde::de::Error,
		{
			Ok(value.to_string())
		}
	}
	deserializer.deserialize_any(StringOrNumberVisitor)
}

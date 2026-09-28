use std::{
	collections::HashMap,
	ops::Deref,
	path::{Path, PathBuf},
};

use typst::{
	Library, LibraryExt, World,
	diag::{FileError, FileResult, SourceResult},
	engine::{Engine, Route, Sink, Traced},
	foundations::{Content, Duration, NativeRuleMap, Packed, StyleChain, Target, TargetElem},
	introspection::{EmptyIntrospector, Introspector, Locator},
	math::EquationElem,
	model::{CiteElem, DocumentInfo, EmphElem, LinkElem, QuoteElem, RefElem, StrongElem},
	routines::{Arenas, RealizationKind},
	syntax::{FileId, RootedPath, Source, VirtualPath, VirtualRoot},
	text::{
		Font, HighlightElem, OverlineElem, RawElem, SmallcapsElem, StrikeElem, SubElem, SuperElem,
		TextElem, UnderlineElem,
	},
	utils::{LazyHash, Protected},
};
use typst_kit::{
	datetime::Time, downloader::SystemDownloader, files::FsRoot, fonts::FontStore,
	packages::SystemPackages,
};

/// Replaces a reference with a placeholder that the converter expects.
///
/// Keeping references as raw elements would interrupt paragraph grouping
/// during realization, splitting the surrounding text into separate
/// paragraphs and discarding adjacent spaces.
fn reference_rule(elem: &Packed<RefElem>, _: &mut Engine, _: StyleChain) -> SourceResult<Content> {
	Ok(TextElem::packed("X").spanned(elem.span()))
}

/// See [`reference_rule`].
fn citation_rule(elem: &Packed<CiteElem>, _: &mut Engine, _: StyleChain) -> SourceResult<Content> {
	Ok(TextElem::packed("X").spanned(elem.span()))
}

/// See [`reference_rule`].
fn equation_rule(
	elem: &Packed<EquationElem>,
	_: &mut Engine,
	_: StyleChain,
) -> SourceResult<Content> {
	Ok(TextElem::packed("0").spanned(elem.span()))
}

/// Placeholder emitted for raw content, recognized by the converter.
///
/// The private use character cannot appear in normal text and, unlike the
/// placeholder itself, is not touched by `raw`'s show-set rules (especially
/// `TextElem::lang`), which would otherwise split the checked text into
/// separate language chunks.
pub const RAW_PLACEHOLDER: &str = "\u{e000}";

/// Replaces raw content with [`RAW_PLACEHOLDER`].
///
/// Like [`reference_rule`], this keeps paragraph grouping intact. Only
/// registered when raw content should not be spellchecked.
fn raw_rule(elem: &Packed<RawElem>, _: &mut Engine, _: StyleChain) -> SourceResult<Content> {
	Ok(TextElem::packed(RAW_PLACEHOLDER).spanned(elem.span()))
}

/// Defines rules that replace an inline wrapper with its body, and a function
/// that registers all of them.
macro_rules! body_rules {
	($($name:ident: $elem:ty),* $(,)?) => {
		$(
			fn $name(
				elem: &Packed<$elem>,
				_: &mut Engine,
				_: StyleChain,
			) -> SourceResult<Content> {
				Ok(Content::sequence([elem.body.clone()]))
			}
		)*

		fn register_body_rules(rules: &mut NativeRuleMap) {
			$(rules.register(Target::Paged, $name);)*
		}
	};
}

// Without these rules, the elements interrupt paragraph grouping during
// realization, splitting the surrounding text into separate paragraphs and
// adding a space after the body.
body_rules! {
	strong_rule: StrongElem,
	emph_rule: EmphElem,
	link_rule: LinkElem,
	quote_rule: QuoteElem,
	underline_rule: UnderlineElem,
	overline_rule: OverlineElem,
	strike_rule: StrikeElem,
	highlight_rule: HighlightElem,
	sub_rule: SubElem,
	super_rule: SuperElem,
	smallcaps_rule: SmallcapsElem,
}

pub struct LtWorld {
	library: LazyHash<Library>,
	realize_library: LazyHash<Library>,
	now: Time,

	packages: SystemPackages,
	root: FsRoot,

	fonts: FontStore,
	shadow_files: HashMap<FileId, Source>,
}

pub struct LtWorldRunning<'a> {
	world: &'a LtWorld,
	main: FileId,
}

impl LtWorld {
	pub fn new(root: PathBuf, ignore_raw: bool) -> Self {
		let root = root.canonicalize().unwrap();

		let mut fonts = FontStore::new();
		fonts.extend(typst_kit::fonts::embedded());
		fonts.extend(typst_kit::fonts::system());

		// Realization without layout rules, so that elements like equations
		// and smart quotes stay in the form the converter understands, plus
		// placeholder rules for the elements the converter maps to text.
		let realize_library = {
			let mut library = Library::builder().build();
			library.rules = NativeRuleMap::new();
			library.rules.register(Target::Paged, reference_rule);
			library.rules.register(Target::Paged, citation_rule);
			library.rules.register(Target::Paged, equation_rule);
			if ignore_raw {
				library.rules.register(Target::Paged, raw_rule);
			}
			register_body_rules(&mut library.rules);
			LazyHash::new(library)
		};

		Self {
			library: LazyHash::new(Library::builder().build()),
			realize_library,
			now: Time::system(),

			packages: SystemPackages::new(SystemDownloader::new("typst-languagetool")),

			fonts,
			root: FsRoot::new(root),
			shadow_files: HashMap::new(),
		}
	}

	pub fn root(&self) -> &Path {
		self.root.path()
	}

	pub fn file_id(&self, path: &Path) -> Option<FileId> {
		let path = path.canonicalize().unwrap();
		let path = VirtualPath::virtualize(self.root.path(), &path).ok()?;
		let id = RootedPath::new(VirtualRoot::Project, path).intern();
		Some(id)
	}

	pub fn use_shadow_file(&mut self, path: &Path, text: String) {
		let Some(file_id) = self.file_id(path) else {
			return;
		};
		self.shadow_files
			.insert(file_id, Source::new(file_id, text));
	}

	pub fn shadow_file(&mut self, path: &Path) -> Option<&mut Source> {
		let file_id = self.file_id(path)?;
		self.shadow_files.get_mut(&file_id)
	}

	pub fn use_original_file(&mut self, path: &Path) {
		let Some(file_id) = self.file_id(path) else {
			return;
		};
		self.shadow_files.remove(&file_id);
	}

	pub fn path(&self, file_id: FileId) -> typst::diag::FileResult<PathBuf> {
		match file_id.root() {
			VirtualRoot::Package(spec) => self.packages.obtain(spec)?.resolve(file_id.vpath()),
			VirtualRoot::Project => self.root.resolve(file_id.vpath()),
		}
	}

	pub fn with_main(&self, main: PathBuf) -> LtWorldRunning<'_> {
		let main = self.file_id(&main).unwrap();
		LtWorldRunning { world: self, main }
	}
}

impl Deref for LtWorldRunning<'_> {
	type Target = LtWorld;

	fn deref(&self) -> &Self::Target {
		self.world
	}
}

impl LtWorldRunning<'_> {
	pub fn compile(&self) -> SourceResult<Content> {
		use typst::comemo::{Constraint, Track};

		let mut sink = Sink::new();
		let world = (self as &dyn World).track();

		let main = world.main();
		let main = world.source(main).expect("source exist");

		let content = typst_eval::eval(
			world,
			&self.library,
			Traced::default().track(),
			sink.track_mut(),
			Route::default().track(),
			&main,
		)?
		.content();

		// Realize the content to apply show rules and expand `context`
		// expressions, which evaluation leaves opaque.
		let empty_introspector = EmptyIntrospector;
		let introspector: &dyn Introspector = &empty_introspector;
		let constraint = Constraint::new();
		let traced = Traced::default();
		let mut engine = Engine {
			library: &self.realize_library,
			world,
			introspector: Protected::new(introspector.track_with(&constraint)),
			traced: traced.track(),
			sink: sink.track_mut(),
			route: Route::default(),
		};

		let base = StyleChain::new(&self.realize_library.styles);
		let target = TargetElem::target.set(Target::Paged).wrap();
		let styles = base.chain(&target);

		let mut locator = Locator::root().split();
		let arenas = Arenas::default();
		let mut info = DocumentInfo::default();
		let children = (self.realize_library.routines.realize)(
			RealizationKind::Document { info: &mut info },
			&mut engine,
			&mut locator,
			&arenas,
			&content,
			styles,
		)?;

		let content = Content::sequence(
			children
				.into_iter()
				.map(|(content, styles)| content.clone().styled_with_map(styles.to_map())),
		);

		Ok(content)
	}
}

impl World for LtWorldRunning<'_> {
	fn library(&self) -> &LazyHash<Library> {
		&self.library
	}

	fn today(&self, offset: Option<Duration>) -> Option<typst::foundations::Datetime> {
		self.now.today(offset)
	}

	fn book(&self) -> &LazyHash<typst::text::FontBook> {
		self.fonts.book()
	}

	fn main(&self) -> FileId {
		self.main
	}

	fn source(&self, id: FileId) -> typst::diag::FileResult<typst::syntax::Source> {
		if let Some(source) = self.shadow_files.get(&id) {
			return Ok(source.clone());
		}

		let path = self.path(id)?;

		let Ok(text) = std::fs::read_to_string(&path) else {
			return Err(FileError::NotFound(path));
		};
		Ok(Source::new(id, text))
	}

	fn file(&self, id: FileId) -> FileResult<typst::foundations::Bytes> {
		let path = self.path(id)?;

		let Ok(bytes) = std::fs::read(&path) else {
			return Err(FileError::NotFound(path));
		};
		Ok(typst::foundations::Bytes::new(bytes))
	}

	fn font(&self, index: usize) -> Option<Font> {
		self.fonts.font(index)
	}
}

use std::{
	collections::HashMap,
	ops::Deref,
	path::{Path, PathBuf},
};

use typst::{
	Library, LibraryExt, World,
	diag::{FileError, FileResult, SourceDiagnostic, SourceResult},
	engine::{Engine, Route, Sink, Traced},
	foundations::{Content, Duration, NativeRuleMap, Packed, StyleChain, Target, TargetElem},
	introspection::{EmptyIntrospector, Introspector, Locator},
	layout::{
		AlignElem, BlockBody, BlockElem, BoxElem, ColumnsElem, GridCell, GridChild, GridElem,
		GridItem, HideElem, MoveElem, PadElem, PlaceElem, RepeatElem, RotateElem, ScaleElem,
		SkewElem, StackChild, StackElem,
	},
	math::EquationElem,
	model::{
		CiteElem, DocumentInfo, EmphElem, EnumElem, FigureCaption, FigureElem, FootnoteElem,
		LinkElem, ListElem, ParbreakElem, QuoteElem, RefElem, StrongElem, TableCell, TableChild,
		TableElem, TableItem, TermsElem, TitleElem,
	},
	routines::{Arenas, RealizationKind},
	syntax::{FileId, RootedPath, Source, Span, VirtualPath, VirtualRoot},
	text::{
		Font, HighlightElem, OverlineElem, RawElem, RawLine, SmallcapsElem, StrikeElem, SubElem,
		SuperElem, TextElem, UnderlineElem,
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

/// Keeps the raw content when it should be spellchecked.
///
/// The lines are synthesized before rules run, so they are available at any
/// nesting depth.
fn raw_text_rule(elem: &Packed<RawElem>, _: &mut Engine, _: StyleChain) -> SourceResult<Content> {
	let lines = elem.lines.as_deref().unwrap_or_default();
	Ok(Content::sequence(
		lines.iter().map(|line| line.body.clone()),
	))
}

/// See [`raw_rule`]. Raw show rules emit `raw.line` elements instead of the
/// (guarded) `raw` element, so the lines need the same treatment.
fn raw_line_rule(elem: &Packed<RawLine>, _: &mut Engine, _: StyleChain) -> SourceResult<Content> {
	Ok(TextElem::packed(RAW_PLACEHOLDER).spanned(elem.span()))
}

/// See [`raw_text_rule`].
fn raw_text_line_rule(
	elem: &Packed<RawLine>,
	_: &mut Engine,
	_: StyleChain,
) -> SourceResult<Content> {
	Ok(elem.body.clone())
}

/// Sentinels around a footnote body, recognized by the converter.
///
/// The converter captures the text between them as a separate chunk so a
/// footnote does not become part of the surrounding sentence.
pub const FOOTNOTE_START: &str = "\u{e001}";
/// See [`FOOTNOTE_START`].
pub const FOOTNOTE_END: &str = "\u{e002}";

/// Marks a footnote body with [`FOOTNOTE_START`] and [`FOOTNOTE_END`].
///
/// Like [`reference_rule`], this keeps paragraph grouping intact, which the
/// missing footnote rule would otherwise interrupt.
fn footnote_rule(
	elem: &Packed<FootnoteElem>,
	_: &mut Engine,
	_: StyleChain,
) -> SourceResult<Content> {
	let Some(body) = elem.body_content() else {
		return Ok(Content::empty());
	};
	Ok(Content::sequence([
		TextElem::packed(FOOTNOTE_START).spanned(elem.span()),
		body.clone(),
		TextElem::packed(FOOTNOTE_END).spanned(elem.span()),
	]))
}

/// A paragraph break used to separate block-level content.
///
/// Paragraph breaks are consumed by the paragraph grouping and do not reach the
/// converter, but they end the current paragraph so that the converter sees
/// separate [`ParElem`](typst::model::ParElem)s.
fn paragraph_break(span: Span) -> Content {
	ParbreakElem::shared().clone().spanned(span)
}

/// Joins content with paragraph breaks in between.
fn join_paragraphs(contents: impl IntoIterator<Item = Content>, span: Span) -> Content {
	let mut children = Vec::new();
	for content in contents {
		if !children.is_empty() {
			children.push(paragraph_break(span));
		}
		children.push(content);
	}
	Content::sequence(children)
}

/// Isolates block-level content from the surrounding paragraphs.
fn block(content: Content, span: Span) -> Content {
	Content::sequence([paragraph_break(span), content, paragraph_break(span)])
}

/// Defines rules that replace an element with its body, and a function that
/// registers all of them.
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

/// Like [`body_rules!`], but isolates the body as a block.
macro_rules! block_rules {
	($($name:ident: $elem:ty),* $(,)?) => {
		$(
			fn $name(
				elem: &Packed<$elem>,
				_: &mut Engine,
				_: StyleChain,
			) -> SourceResult<Content> {
				Ok(block(elem.body.clone(), elem.span()))
			}
		)*

		fn register_block_rules(rules: &mut NativeRuleMap) {
			$(rules.register(Target::Paged, $name);)*
		}
	};
}

// Without these rules, the elements interrupt paragraph grouping during
// realization, splitting the surrounding text into separate paragraphs and
// adding a space after the body. They also make the wrappers available inside
// containers that the converter does not descend into itself.
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
	hide_rule: HideElem,
	table_cell_rule: TableCell,
	grid_cell_rule: GridCell,
	figure_caption_rule: FigureCaption,
}

block_rules! {
	pad_rule: PadElem,
	align_rule: AlignElem,
	columns_rule: ColumnsElem,
	place_rule: PlaceElem,
	repeat_rule: RepeatElem,
	move_rule: MoveElem,
	scale_rule: ScaleElem,
	rotate_rule: RotateElem,
	skew_rule: SkewElem,
}

/// See [`body_rules!`]. The body is optional and the box is inline.
fn box_rule(elem: &Packed<BoxElem>, _: &mut Engine, styles: StyleChain) -> SourceResult<Content> {
	Ok(elem.body.get_cloned(styles).unwrap_or_default())
}

/// See [`block_rules!`]. Layout callbacks are ignored, they only exist after
/// layout.
fn block_rule(
	elem: &Packed<BlockElem>,
	_: &mut Engine,
	styles: StyleChain,
) -> SourceResult<Content> {
	let body = match elem.body.get_ref(styles) {
		Some(BlockBody::Content(body)) => body.clone(),
		_ => Content::empty(),
	};
	Ok(block(body, elem.span()))
}

/// See [`block_rules!`]. An automatic title resolves to nothing because the
/// document fields are not available here.
fn title_rule(
	elem: &Packed<TitleElem>,
	_: &mut Engine,
	styles: StyleChain,
) -> SourceResult<Content> {
	Ok(block(
		elem.resolve_body(styles).unwrap_or_default(),
		elem.span(),
	))
}

/// Realizes the items of a bullet list as separate paragraphs.
///
/// The items are flattened here instead of being returned as elements: the
/// list grouping only builds the [`ListElem`] if the items are not replaced by
/// a show rule first.
fn list_rule(elem: &Packed<ListElem>, _: &mut Engine, _: StyleChain) -> SourceResult<Content> {
	let span = elem.span();
	let items = elem.children.iter().map(|item| item.body.clone());
	Ok(block(join_paragraphs(items, span), span))
}

/// Realizes the items of an enumeration as separate paragraphs.
///
/// See [`list_rule`] for why the items are flattened.
fn enum_rule(elem: &Packed<EnumElem>, _: &mut Engine, _: StyleChain) -> SourceResult<Content> {
	let span = elem.span();
	let items = elem.children.iter().map(|item| item.body.clone());
	Ok(block(join_paragraphs(items, span), span))
}

/// Realizes a term and its description as one paragraph per item.
fn terms_rule(
	elem: &Packed<TermsElem>,
	_: &mut Engine,
	styles: StyleChain,
) -> SourceResult<Content> {
	let span = elem.span();
	let separator = elem.separator.get_ref(styles);
	let items = elem.children.iter().map(|item| {
		Content::sequence([
			item.term.clone(),
			separator.clone(),
			item.description.clone(),
		])
	});
	Ok(block(join_paragraphs(items, span), span))
}

/// Realizes the caption before the body, skipping numbering and supplement.
fn figure_rule(
	elem: &Packed<FigureElem>,
	_: &mut Engine,
	styles: StyleChain,
) -> SourceResult<Content> {
	let span = elem.span();
	let mut children = Vec::new();
	if let Some(caption) = elem.caption.get_ref(styles) {
		children.push(caption.pack_ref().clone());
	}
	let body = elem.body.clone();
	if !body.is_empty() {
		children.push(body);
	}
	Ok(block(join_paragraphs(children, span), span))
}

/// Realizes every table cell as its own paragraph, skipping lines.
fn table_rule(elem: &Packed<TableElem>, _: &mut Engine, _: StyleChain) -> SourceResult<Content> {
	let span = elem.span();
	let mut cells = Vec::new();
	for child in &elem.children {
		match child {
			TableChild::Header(header) => {
				cells.extend(header.children.iter().filter_map(table_item));
			},
			TableChild::Footer(footer) => {
				cells.extend(footer.children.iter().filter_map(table_item));
			},
			TableChild::Item(item) => cells.extend(table_item(item)),
		}
	}
	Ok(block(join_paragraphs(cells, span), span))
}

fn table_item(item: &TableItem) -> Option<Content> {
	match item {
		TableItem::Cell(cell) => Some(cell.pack_ref().clone()),
		_ => None,
	}
}

/// Realizes every grid cell as its own paragraph, skipping lines.
fn grid_rule(elem: &Packed<GridElem>, _: &mut Engine, _: StyleChain) -> SourceResult<Content> {
	let span = elem.span();
	let mut cells = Vec::new();
	for child in &elem.children {
		match child {
			GridChild::Header(header) => {
				cells.extend(header.children.iter().filter_map(grid_item));
			},
			GridChild::Footer(footer) => {
				cells.extend(footer.children.iter().filter_map(grid_item));
			},
			GridChild::Item(item) => cells.extend(grid_item(item)),
		}
	}
	Ok(block(join_paragraphs(cells, span), span))
}

fn grid_item(item: &GridItem) -> Option<Content> {
	match item {
		GridItem::Cell(cell) => Some(cell.pack_ref().clone()),
		_ => None,
	}
}

/// Realizes every stack child as its own paragraph, skipping spacing.
fn stack_rule(elem: &Packed<StackElem>, _: &mut Engine, _: StyleChain) -> SourceResult<Content> {
	let span = elem.span();
	let children = elem.children.iter().filter_map(|child| match child {
		StackChild::Block(content) => Some(content.clone()),
		StackChild::Spacing(_) => None,
	});
	Ok(block(join_paragraphs(children, span), span))
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
		// rules that realize the content of containers the converter does not
		// descend into itself.
		let realize_library = {
			let mut library = Library::builder().build();
			library.rules = NativeRuleMap::new();
			library.rules.register(Target::Paged, reference_rule);
			library.rules.register(Target::Paged, citation_rule);
			library.rules.register(Target::Paged, equation_rule);
			library.rules.register(Target::Paged, footnote_rule);
			if ignore_raw {
				library.rules.register(Target::Paged, raw_rule);
				library.rules.register(Target::Paged, raw_line_rule);
			} else {
				library.rules.register(Target::Paged, raw_text_rule);
				library.rules.register(Target::Paged, raw_text_line_rule);
			}
			library.rules.register(Target::Paged, box_rule);
			library.rules.register(Target::Paged, block_rule);
			library.rules.register(Target::Paged, title_rule);
			library.rules.register(Target::Paged, list_rule);
			library.rules.register(Target::Paged, enum_rule);
			library.rules.register(Target::Paged, terms_rule);
			library.rules.register(Target::Paged, figure_rule);
			library.rules.register(Target::Paged, table_rule);
			library.rules.register(Target::Paged, grid_rule);
			library.rules.register(Target::Paged, stack_rule);
			register_body_rules(&mut library.rules);
			register_block_rules(&mut library.rules);
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
		let path = path.canonicalize().ok()?;
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

	pub fn with_main(&self, main: PathBuf) -> Option<LtWorldRunning<'_>> {
		let main = self.file_id(&main)?;
		Some(LtWorldRunning { world: self, main })
	}
}

impl Deref for LtWorldRunning<'_> {
	type Target = LtWorld;

	fn deref(&self) -> &Self::Target {
		self.world
	}
}

/// The outcome of [`LtWorldRunning::compile`].
pub struct Compiled {
	/// The realized content, if evaluation got far enough to produce any.
	pub content: Option<Content>,
	/// Errors encountered while evaluating and realizing the document.
	pub errors: Vec<SourceDiagnostic>,
}

impl LtWorldRunning<'_> {
	pub fn compile(&self) -> Compiled {
		use typst::comemo::{Constraint, Track};

		let mut sink = Sink::new();
		let world = (self as &dyn World).track();

		let main = world.main();
		let main = match world.source(main) {
			Ok(source) => source,
			Err(err) => {
				return Compiled {
					content: None,
					errors: vec![SourceDiagnostic::error(Span::detached(), err.to_string())],
				};
			},
		};

		let content = match typst_eval::eval(
			world,
			&self.library,
			Traced::default().track(),
			sink.track_mut(),
			Route::default().track(),
			&main,
		) {
			Ok(content) => content.content(),
			Err(errors) => {
				return Compiled { content: None, errors: errors.into_iter().collect() };
			},
		};

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
		let children = match (self.realize_library.routines.realize)(
			RealizationKind::Document { info: &mut info },
			&mut engine,
			&mut locator,
			&arenas,
			&content,
			styles,
		) {
			Ok(children) => children,
			Err(errors) => {
				return Compiled { content: None, errors: errors.into_iter().collect() };
			},
		};

		let content = Content::sequence(
			children
				.into_iter()
				.map(|(content, styles)| content.clone().styled_with_map(styles.to_map())),
		);

		// Show rule errors are delayed until the end of realization, see
		// `Engine::delay`.
		Compiled { content: Some(content), errors: sink.delayed().into_iter().collect() }
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

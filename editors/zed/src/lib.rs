use zed_extension_api::{self as zed, settings::LspSettings, Result};

const BINARY_NAME: &str = "typst-languagetool-lsp";

struct TypstLanguageTool;

impl TypstLanguageTool {
	fn binary_path(worktree: &zed::Worktree) -> Result<String> {
		worktree.which(BINARY_NAME).ok_or_else(|| {
			format!(
				"`{BINARY_NAME}` is not in PATH. Install it in the environment where the language \
				 server runs, e.g. `cargo install --path lsp --features=server` from a checkout or \
				 `cargo install --git=https://github.com/antonWetzel/typst-languagetool lsp \
				 --features=server`."
			)
		})
	}
}

impl zed::Extension for TypstLanguageTool {
	fn new() -> Self {
		Self
	}

	fn language_server_command(
		&mut self,
		_language_server_id: &zed::LanguageServerId,
		worktree: &zed::Worktree,
	) -> Result<zed::Command> {
		Ok(zed::Command {
			command: Self::binary_path(worktree)?,
			args: Vec::new(),
			env: Vec::new(),
		})
	}

	fn language_server_initialization_options(
		&mut self,
		language_server_id: &zed::LanguageServerId,
		worktree: &zed::Worktree,
	) -> Result<Option<zed::serde_json::Value>> {
		let user_options = LspSettings::for_worktree(language_server_id.as_ref(), worktree)
			.ok()
			.and_then(|settings| settings.initialization_options);

		let mut options = user_options
			.filter(|options| options.is_object())
			.unwrap_or_else(|| zed::serde_json::json!({}));
		if let Some(options) = options.as_object_mut() {
			options
				.entry("root")
				.or_insert_with(|| zed::serde_json::Value::String(worktree.root_path()));
		}
		Ok(Some(options))
	}

	fn language_server_workspace_configuration(
		&mut self,
		language_server_id: &zed::LanguageServerId,
		worktree: &zed::Worktree,
	) -> Result<Option<zed::serde_json::Value>> {
		Ok(LspSettings::for_worktree(language_server_id.as_ref(), worktree)
			.ok()
			.and_then(|settings| settings.settings))
	}
}

zed::register_extension!(TypstLanguageTool);

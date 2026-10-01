# typst-languagetool

Spellcheck typst files with LanguageTool.

## Overview

1. compile the document
1. extract text content
1. check text with languagetool
1. map results back to the source 

## LanguageTool Backend

- different LanguageTool backends can be used to check the text
- atleast one backend must be enabled for `cargo install ...` with `--features=<backend>`
- one backend must be selected for `typst-languagetool ...` with the required flags

### Bundle

- typst-languagetool starts a LanguageTool instance with JNI
- requires maven and the executable is not portable
- add feature `bundle`
- specify flag `--bundle` for cli or `"backend: "bundle"` for LSP

### JAR

- typst-languagetool starts a LanguageTool instance with JNI
- requires JAR with languagetool
- add feature  `jar`
- specify flag `jar_location=<path>` for cli or `"backend: "jar"` and `"jar_location": <path>` for LSP

### Server

- typst-languagetool connects to a running LanguageTool server
- add feature `server`
- specify flags `host=<host>` and `port=<port>` for cli or `"backend": "server"`, `"host": <host>` and `"port": <port>` for LSP
- public API endpoint: `https://api.languagetool.org`
- premium API endpoint: `https://api.languagetoolplus.com`
- premium API also requires `username` and `api_key`

```json
{
  "backend": "server",
  "host": "https://api.languagetoolplus.com",
  "port": 443,
  "username": "user@example.com",
  "api_key": "secret"
}
```

## Usage

- prebuilt binaries
	- download `typst-languagetool-<tag>-x86_64-unknown-linux-musl.tar.gz` from the GitHub releases
	- contains `typst-languagetool` (CLI) and `typst-languagetool-lsp` (LSP)
	- static Linux x86_64 binaries (musl, no system libraries needed) with the `server` backend only, other platforms and backends need `cargo install`
- terminal
	- install command line interface (CLI)
		- `cargo install --git=https://github.com/antonWetzel/typst-languagetool cli --features=...`
	- Check on time or watch for changes
		- `typst-languagetool check ...`
		- `typst-languagetool watch ...`
	- Path to check
		- `typst-languagetool watch --path=<directory or file>`
		- `typst-languagetool check --path=<file>`
	- Main file of the document
		- defaults to path if not specified
		- check the complete document if a path is not specified
		- `--main=<file>`
	- Project root can be changed
		- defaults to main parent folder
		- `--root=<path>`
	- Compile errors
		- are printed with their source location
		- `check` exits with a non-zero status, `watch` keeps watching
		- text that could still be realized is checked anyway
	- Output format
		- `--format=pretty` (default), `--format=plain` (same as `--plain`)
		- `--format=code-climate` writes a Code Climate report for GitLab code quality, only with `check`
			- `--output=<file>` writes it to a file instead of stdout
			- one issue per result, described as `Typst-LT: <rule> | <sentence with »match«> | <suggestions>`
			- the sentence is the checked text, so text from variables and data files can be searched for
			- compile errors are issues with the check `typst/compile-error`
			- results only count as a failure when the document does not compile
- vs-codium/vs-code
	- install language server protocol (LSP)
		- `cargo install --git=https://github.com/antonWetzel/typst-languagetool lsp --features=...`
	- install generic lsp (`editors/vscodium/generic-lsp/generic-lsp-0.0.1.vsix`)
	- configure options (see below)
	- hints should appear
		- first check takes longer
- neovim
	- install language server protocol (LSP)
		- `cargo install --git=https://github.com/antonWetzel/typst-languagetool lsp --features=...`
    - copy the `editors/nvim/typst.lua` file in the `ftplugin/` folder (should be in the nvim config path)
	- configure options in `init_option` (see below)
    - create a `main.typst` file and include your typst files inside if needed
	- hints should appear (if not use `set filetype=typst` to force the type)
		- first check takes longer
- zed
	- install language server protocol (LSP)
		- `cargo install --git=https://github.com/antonWetzel/typst-languagetool lsp --features=...`
	- install the extension as a dev extension
		- extensions -> `Install Dev Extension` -> `editors/zed`
	- configure options (see below)
	- hints should appear
		- first check takes longer


## Options

```rust
/// Additional allowed words for language codes
dictionary: HashMap<String, Vec<String>>,
/// Languagetool rules to ignore (WHITESPACE_RULE, ...) for language codes
disabled_checks: HashMap<String, Vec<String>>,
/// Functions calls to ignore (lorem, bibliography, cite, ...)
ignore_functions: HashSet<String>,
/// Language (and optional region) for text without `#set text(lang: ...)`
/// e.g. "sv" or "sv-SE"
default_language: Option<String>,
/// Ignore raw text (inline code and code blocks) when checking (default: true)
ignore_raw: Option<bool>,
/// Ignore emphasis (`_..._`, `#emph[..]`) when checking (default: false)
ignore_emphasis: Option<bool>,

/// specify used backend
backend: "bundle" | "jar" | "server",
/// path for jar backend
jar_location: Option<String>,
/// host for server backend
host: Option<String>,
/// port for server backend
port: Option<String>,
/// username for LanguageTool Premium API
username: Option<String>,
/// API key for LanguageTool Premium API
api_key: Option<String>,

/// Size for a text chunk to send to LanguageTool
chunk_size: usize,


/// Project Root
root: Option<PathBuf>,
/// Project Main File
main: Option<PathBuf>,
```

### For CLI

```rust
/// Path to check a different file as the main file
path: Option<PathBuf>,
/// Delay to wait after a file change
delay: f64,
/// Output the diagnostic plain without color
plain: bool,
/// Path to a JSON file to load common options
options: Option<PathBuf>,
```

### For LSP

```rust
/// Duration to wait for additional changes before checking the file
/// Leave empty to only check on open and save
on_change: Option<std::time::Duration>,
/// Path to a JSON file to load common options
options: Option<PathBuf>,
```

### For Zed

Options are set in the Zed settings under `lsp.typst-languagetool.initialization_options`.
`root` defaults to the worktree root, `main` defaults to the checked file.
`on_change` is only used as initialization option and must be a humantime string.

```json
{
  "lsp": {
    "typst-languagetool": {
      "initialization_options": {
        "backend": "server",
        "host": "http://localhost",
        "port": 8081,
        "on_change": "500ms"
      }
    }
  }
}
```

## Releasing

1. bump `version` under `[workspace.package]` in `Cargo.toml`, run `cargo update --workspace` to update `Cargo.lock` and commit both to `main`
	- the release build uses `--locked` and fails if `Cargo.lock` still has the old version
1. tag the commit with the same version and push it together with `main`
	- `git tag v<version>` (`v` or `V`, e.g. `v1.0.0`)
	- `git push --atomic origin main v<version>`
1. the release workflow (`.github/workflows/release.yml`) builds the binaries and publishes the release
	- fails if the tag is not on `main` or does not match the version in `Cargo.toml`

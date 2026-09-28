list:
    @just --list

check-example:
    cargo run \
        --package=cli \
        --features=server \
        -- \
        check  \
        --host="http://localhost" \
        --port="8081" \
        --main="example/main.typ"

lint:
    cargo clippy --workspace --features=server --features=jar
    taplo check
    cargo fmt --check

# Build the Zed extension wasm; installs into $ZED_EXTENSIONS_DIR when set.
zed-build:
    cargo build --manifest-path editors/zed/Cargo.toml --release --target wasm32-wasip2
    if [ -n "${ZED_EXTENSIONS_DIR:-}" ]; then \
        dest="$ZED_EXTENSIONS_DIR/installed/typst-languagetool-lsp"; \
        mkdir -p "$dest"; \
        cp editors/zed/extension.toml "$dest/extension.toml"; \
        cp editors/zed/target/wasm32-wasip2/release/zed_typst_languagetool.wasm "$dest/extension.wasm"; \
    fi

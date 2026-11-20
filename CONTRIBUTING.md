# Contributing to Shortcut Hub

Thank you for your interest in contributing to **Shortcut Hub**!

## Development Setup

### Prerequisites
- Windows 10 or Windows 11 (`x86_64-pc-windows-msvc`)
- [Rust toolchain](https://rustup.rs/) (`stable`) with `rustfmt` and `clippy`
- Python 3.10+ (for `store.py`, `discovery.py`, `powertoys_yaml.py`, and integration tests)
- Optional: [Microsoft PowerToys](https://github.com/microsoft/PowerToys) for testing Shortcut Guide sync

### Building & Testing

```powershell
# Rust formatting, lints, unit tests, and release build
cd native
cargo fmt --check
cargo clippy --all-targets -- -D warnings
cargo test
cargo build --release

# Copy release binary to project root and run runtime checks
Copy-Item target\release\shortcut-hub.exe ..\shortcut-hub.exe -Force
cd ..
.\shortcut-hub.exe --bench
.\shortcut-hub.exe --smoke-test

# Python auto-discovery & PowerToys YAML sync tests
python test_suite.py -v
```

### Local Overrides Without Polluting Git

Never commit personal directories, custom shortcut state, or built binaries. Put machine-specific `extra_watch_dirs` or `category_hints` in `config.local.json` (gitignored alongside `state/` and `shortcut-hub.exe`).

## Pull Request Guidelines

1. Keep changes focused and modular.
2. Add or update unit tests in `native/src/*.rs` and/or `test_suite.py` for new behavior.
3. Ensure `cargo fmt --check`, `cargo clippy --all-targets -- -D warnings`, `cargo test`, and `python test_suite.py -v` all pass before opening a PR.

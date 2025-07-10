# Rust Core for Shogi

This package contains the WebAssembly (WASM) implementation for advanced Shogi features including WebRTC communication, mate search, and opening book functionality.

## Features

- 🌐 WebRTC peer-to-peer communication for online play
- 🔍 Mate search algorithm implementation
- 📚 Opening book with binary format support
- 🎯 High-performance position hashing and move encoding

## Prerequisites

- Rust toolchain (install from https://rustup.rs/)
- wasm-pack (`cargo install wasm-pack`)
- Make (optional, for convenience commands)

## Project Structure

```
src/
├── lib.rs                    # Main library entry point
├── simple_webrtc.rs         # WebRTC implementation
├── mate_search.rs           # Mate search algorithm
├── opening_book/            # Opening book module
│   ├── mod.rs              # Module exports
│   ├── binary_converter.rs  # Binary format conversion
│   ├── data_structures.rs   # Core data types
│   ├── move_encoder.rs      # Move encoding/decoding
│   ├── position_filter.rs   # Position filtering logic
│   ├── position_hasher.rs   # Position hashing
│   └── sfen_parser.rs       # SFEN format parsing
└── opening_book_reader.rs   # Opening book reader interface
```

## Building

### From project root
```bash
npm run build:wasm      # Production build (optimized)
npm run build:wasm:dev  # Development build (faster)
```

### From this directory
```bash
make build      # Production build
make build-dev  # Development build
make clean      # Clean build artifacts
```

## Important Notes

⚠️ **WASM files must be built before running the web application!**

The build process:
1. Compiles Rust code to WebAssembly
2. Generates JavaScript bindings and TypeScript definitions
3. Copies the generated files to `packages/web/src/wasm/`

The generated files in `packages/web/src/wasm/` are:
- Excluded from git (in .gitignore)
- Required for the web application to run
- Must be regenerated when Rust code changes

## Development Workflow

1. Make changes to Rust code
2. Run quality checks: `cargo fmt`, `cargo clippy`, `cargo test`
3. Build WASM: `npm run build:wasm` (from root) or `make build`
4. Test changes in the web application

## Testing

```bash
# Run standard Rust tests
cargo test

# Run WASM tests in browser (requires Chrome)
wasm-pack test --chrome --headless

# Or use Make commands
make test       # Standard tests
make test-wasm  # Browser tests
```

## Code Quality

### Required Checks (run automatically on pre-commit)
```bash
cargo fmt                    # Format code
cargo clippy -- -D warnings  # Lint with warnings as errors
cargo check                  # Fast type checking
```

### Additional Tools
```bash
cargo audit      # Security vulnerability scan
cargo outdated   # Check for outdated dependencies
cargo machete    # Find unused dependencies (requires installation)
```

### Quick Commands
```bash
make check-all        # Run all quality checks
make format-check     # Check formatting without modifying
make install-dev-tools # Install additional development tools
```

## API Documentation

### WebRTC Module
Provides simple WebRTC functionality for peer-to-peer connections:
- Connection establishment
- Message passing
- Error handling

### Mate Search Module
Implements efficient mate search algorithms:
- Depth-limited search
- Move ordering optimization
- Performance-oriented design

### Opening Book Module
Handles opening book data in binary format:
- **Binary Format**: Compact storage of positions and moves
- **Position Hashing**: Fast lookup using FNV-1a algorithm
- **Move Encoding**: Efficient 16-bit move representation
- **SFEN Support**: Parse and convert SFEN notation
- **Database**: Currently supports 100,000+ opening positions

## Performance Considerations

- Use `--release` flag for production builds
- Opening book uses memory-mapped files for efficiency
- Position hashing optimized for fast lookups
- Move encoding reduces memory footprint

## License

MIT
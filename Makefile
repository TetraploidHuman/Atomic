# Atomic Language Compiler - Makefile
# Works on any Linux distribution with Rust + LLVM 18 installed

.PHONY: build test examples clean install help
.PHONY: linux-x64-static linux-x64-dynamic
.PHONY: windows-x64-static windows-x64-dynamic
.PHONY: linux-arm64-static linux-arm64-dynamic
.PHONY: dist dist-linux-x64 dist-windows-x64 dist-linux-arm64
.PHONY: lsp lsp-debug install-lsp

# Default target
all: build

# Build distribution package (all platforms)
dist: dist-linux-x64
	@echo "Distribution packages built in dist/"

# Build release (default: dynamic linking, small binary)
build: linux-x64-dynamic
	@echo "Build complete: target/release/atomic (dynamic)"

# Build debug mode (faster compile, slower runtime)
debug:
	cargo build
	@echo "Debug build complete: target/debug/atomic"

# ============================================================
# Linux x86_64 — native host builds
# ============================================================

# Static: self-contained, ~174MB, no external LLVM needed
linux-x64-static:
	cargo build --release --no-default-features --features codegen-static
	@echo "Build complete: target/release/atomic (linux-x64, static, $$(du -h target/release/atomic | cut -f1))"

# Dynamic: small binary ~5MB, needs libLLVM.so.20.1 at runtime
linux-x64-dynamic:
	cargo build --release --features codegen-dynamic
	@echo "Build complete: target/release/atomic (linux-x64, dynamic, $$(du -h target/release/atomic | cut -f1))"

# ============================================================
# Windows x86_64 — cross-compiled from Linux
# Requires: mingw-w64, Windows-targeting LLVM, Rust target x86_64-pc-windows-gnu
# ============================================================

# Windows static: self-contained .exe, ready to distribute
windows-x64-static:
	cargo build --release --no-default-features --features codegen-static --target x86_64-pc-windows-gnu
	@echo "Build complete: target/x86_64-pc-windows-gnu/release/atomic.exe"

# Windows dynamic: needs LLVM-C.dll on target system
windows-x64-dynamic:
	cargo build --release --no-default-features --features codegen-dynamic --target x86_64-pc-windows-gnu
	@echo "Build complete: target/x86_64-pc-windows-gnu/release/atomic.exe"

# ============================================================
# Linux ARM64 — native or cross-compiled
# Requires: aarch64-linux-gnu toolchain (cross) or build on ARM64 host (native)
# ============================================================

linux-arm64-static:
	cargo build --release --no-default-features --features codegen-static --target aarch64-unknown-linux-gnu
	@echo "Build complete: target/aarch64-unknown-linux-gnu/release/atomic"

linux-arm64-dynamic:
	cargo build --release --no-default-features --features codegen-dynamic --target aarch64-unknown-linux-gnu
	@echo "Build complete: target/aarch64-unknown-linux-gnu/release/atomic"

# ============================================================
# Distribution packaging
# ============================================================

dist-linux-x64:
	@# Build static variant
	cargo build --release --no-default-features --features codegen-static
	cp target/release/atomic /tmp/atomic-linux-x64-static
	@# Build dynamic variant
	cargo build --release --features codegen-dynamic
	cp target/release/atomic /tmp/atomic-linux-x64-dynamic
	@# Package both
	@bash scripts/build-dist.sh $(or $(VERSION),0.1.0) linux-x64

dist-windows-x64: windows-x64-static
	@bash scripts/build-dist.sh 0.1.0 windows-x64

dist-linux-arm64: linux-arm64-static
	@bash scripts/build-dist.sh 0.1.0 linux-arm64

# ============================================================
# LSP Server
# ============================================================

# Build the LSP server (no LLVM required)
lsp:
	cargo build -p atomic-lsp --release
	@echo "LSP build complete: target/release/atomic-lsp"

# Build LSP in debug mode
lsp-debug:
	cargo build -p atomic-lsp
	@echo "LSP debug build complete: target/debug/atomic-lsp"

# ============================================================
# Testing
# ============================================================

# Run all tests
test:
	cargo test

# Compile all examples for native target (smoke-test compilation only, no linking)
examples:
	@echo "Compiling all examples..."
	@passed=0; failed=0; \
	for f in examples/*.at examples/*.atom; do \
		[ -f "$$f" ] || continue; \
		if ./target/release/atomic build --target native "$$f" --emit obj -o /dev/null 2>/dev/null; then \
			passed=$$((passed+1)); \
		else \
			echo "  FAIL: $$f"; \
			failed=$$((failed+1)); \
		fi; \
	done; \
	echo "Results: $$passed passed, $$failed failed"

# ============================================================
# Cleanup & Installation
# ============================================================

# Clean build artifacts
clean:
	cargo clean
	rm -f *.o *.wasm a.out

# Install atomic compiler to /usr/local/bin (dynamic build)
install: build
	install -m 755 target/release/atomic /usr/local/bin/atomic
	@echo "Installed atomic to /usr/local/bin/atomic"

# Install LSP server
install-lsp: lsp
	install -m 755 target/release/atomic-lsp /usr/local/bin/atomic-lsp
	@echo "Installed atomic-lsp to /usr/local/bin/atomic-lsp"

# Uninstall
uninstall:
	rm -f /usr/local/bin/atomic /usr/local/bin/atomic-lsp
	@echo "Uninstalled atomic"

# ============================================================
# Help
# ============================================================

help:
	@echo "Atomic Language Compiler"
	@echo ""
	@echo "Build Targets (Host: Linux x86_64):"
	@echo "  make build              - Dynamic build (default, ~5MB)"
	@echo "  make linux-x64-static   - Static Linux x86_64 build (~174MB)"
	@echo "  make linux-x64-dynamic  - Dynamic Linux x86_64 build (~5MB)"
	@echo ""
	@echo "Build Targets (Cross-compile):"
	@echo "  make windows-x64-static - Static Windows x86_64 build"
	@echo "  make windows-x64-dynamic- Dynamic Windows x86_64 build"
	@echo "  make linux-arm64-static - Static Linux ARM64 build"
	@echo "  make linux-arm64-dynamic- Dynamic Linux ARM64 build"
	@echo ""
	@echo "Distribution:"
	@echo "  make dist               - Build all distribution packages"
	@echo "  make dist-linux-x64     - Linux x86_64 (static + dynamic)"
	@echo "  make dist-windows-x64   - Windows x86_64"
	@echo "  make dist-linux-arm64   - Linux ARM64"
	@echo ""
	@echo "Other:"
	@echo "  make lsp                - Build LSP server"
	@echo "  make test               - Run test suite"
	@echo "  make examples           - Compile all example files"
	@echo "  make clean              - Remove build artifacts"
	@echo "  make install            - Install compiler"
	@echo ""
	@echo "Static vs Dynamic:"
	@echo "  Static:  Self-contained binary, no LLVM needed (~174MB)"
	@echo "  Dynamic: Small binary, needs libLLVM.so.20.1 at runtime (~5MB)"

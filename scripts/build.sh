#!/usr/bin/env bash
# Atomic Language Compiler - Build Script
# Detects platform and dependencies, then builds the compiler.
# Works on any Linux distribution.
set -euo pipefail

RED='\033[0;31m'
GREEN='\033[0;32m'
YELLOW='\033[1;33m'
NC='\033[0m' # No Color

echo "============================================"
echo "  Atomic Language Compiler - Build Script"
echo "============================================"
echo ""

# ---- Detect platform ----
ARCH=$(uname -m)
OS=$(uname -s)
echo "Platform: ${OS} ${ARCH}"

# ---- Check dependencies ----
check_cmd() {
    if command -v "$1" &>/dev/null; then
        echo -e "  ${GREEN}[OK]${NC} $1 ($(command -v $1))"
        return 0
    else
        echo -e "  ${RED}[MISSING]${NC} $1"
        return 1
    fi
}

MISSING=0

echo ""
echo "Checking dependencies..."
check_cmd rustc || MISSING=1
check_cmd cargo || MISSING=1
check_cmd llvm-config || MISSING=1
check_cmd cc || MISSING=1

# Check LLVM version
if command -v llvm-config &>/dev/null; then
    LLVM_VER=$(llvm-config --version | cut -d. -f1)
    echo "  LLVM major version: ${LLVM_VER}"
    if [ "${LLVM_VER}" != "18" ]; then
        echo -e "  ${YELLOW}[WARN]${NC} Atomic requires LLVM 18.x, found LLVM ${LLVM_VER}"
    fi
fi

if [ $MISSING -eq 1 ]; then
    echo ""
    echo -e "${YELLOW}Missing dependencies. Install them:${NC}"
    echo ""
    echo "  NixOS:"
    echo "    nix-shell shell.nix"
    echo ""
    echo "  Ubuntu/Debian:"
    echo "    sudo apt install llvm-18 llvm-18-dev libcurl4-openssl-dev build-essential"
    echo ""
    echo "  Fedora:"
    echo "    sudo dnf install llvm18 llvm18-devel libcurl-devel gcc"
    echo ""
    echo "  Arch Linux:"
    echo "    sudo pacman -S llvm18 curl gcc"
    echo ""
    echo "  See https://rustup.rs/ for Rust installation."
    exit 1
fi

echo ""
echo -e "${GREEN}All dependencies found.${NC}"
echo ""

# ---- Build ----
echo "Building compiler (release mode)..."
cargo build --release

echo ""
echo -e "${GREEN}Build successful!${NC}"
echo "Binary: target/release/atomic"
echo ""
echo "Try: ./target/release/atomic run examples/hello.at"
echo ""
echo "Supported targets:"
echo "  native       - build for current host"
echo "  linux-x64    - x86_64 Linux"
echo "  linux-arm64  - ARM64 Linux"
echo "  windows-x64  - x86_64 Windows (requires mingw-w64)"
echo "  wasm         - WebAssembly"

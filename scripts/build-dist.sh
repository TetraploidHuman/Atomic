#!/usr/bin/env bash
# Build Atomic compiler distribution packages
# Creates self-contained tarballs for each platform, both static and dynamic.
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
PROJECT_DIR="$(dirname "$SCRIPT_DIR")"
DIST_DIR="$PROJECT_DIR/dist"
VERSION="${1:-0.1.0}"
PLATFORM="${2:-linux-x64}"

echo "=== Atomic Compiler Distribution Builder ==="
echo "  Version:  $VERSION"
echo "  Platform: $PLATFORM"
echo ""

# Determine binary path and package extension
case "$PLATFORM" in
    linux-x64)
        STATIC_BIN="/tmp/atomic-linux-x64-static"
        DYNAMIC_BIN="/tmp/atomic-linux-x64-dynamic"
        BINARY_NAME="atomic"
        ;;
    linux-arm64)
        STATIC_BIN="/tmp/atomic-linux-arm64-static"
        DYNAMIC_BIN="/tmp/atomic-linux-arm64-dynamic"
        BINARY_NAME="atomic"
        ;;
    windows-x64)
        STATIC_BIN="/tmp/atomic-windows-x64-static"
        DYNAMIC_BIN="/tmp/atomic-windows-x64-dynamic"
        BINARY_NAME="atomic.exe"
        ;;
    *)
        echo "Error: Unknown platform '$PLATFORM'"
        echo "  Supported: linux-x64, linux-arm64, windows-x64"
        exit 1
        ;;
esac

# ============================================================
# Helper: build a package for a given link mode
# ============================================================
build_package() {
    local LINKMODE="$1"  # static or dynamic
    local BINARY_SRC="$2"
    local PKG_NAME="atomic-${VERSION}-${PLATFORM}-${LINKMODE}"
    local PKG_DIR="$DIST_DIR/$PKG_NAME"

    echo "--- Packaging: $PKG_NAME ---"

    # Check binary
    if [ ! -f "$BINARY_SRC" ]; then
        echo "  [SKIP] Binary not found: $BINARY_SRC"
        echo "         Build first: make ${PLATFORM}-${LINKMODE}"
        return
    fi

    rm -rf "$PKG_DIR"
    mkdir -p "$PKG_DIR/bin"

    # Copy binary
    cp "$BINARY_SRC" "$PKG_DIR/bin/$BINARY_NAME"
    chmod 755 "$PKG_DIR/bin/$BINARY_NAME"
    echo "  [OK] Binary: $(du -h "$BINARY_SRC" | cut -f1)"

    # On Linux dynamic builds, bundle libLLVM and other shared libs
    if [ "$PLATFORM" != "windows-x64" ] && [ "$LINKMODE" = "dynamic" ]; then
        mkdir -p "$PKG_DIR/lib"
        echo "  Bundling shared libraries..."

        # First, find and bundle libLLVM from the build environment
        local llvm_found=0
        # Search env var paths first
        for search_path in "${LD_LIBRARY_PATH:-}" "${NIX_LD_RUN_PATH:-}" "${LIBRARY_PATH:-}"; do
            [ -z "$search_path" ] && continue
            IFS=':' read -ra LIBDIRS <<< "$search_path"
            for dir in "${LIBDIRS[@]}"; do
                for lib in "$dir"/libLLVM*.so*; do
                    [ -f "$lib" ] || continue
                    cp -f "$lib" "$PKG_DIR/lib/"
                    echo "    [OK] $(basename "$lib")"
                    llvm_found=1
                done
            done
        done
        # Fallback: search Nix store for matching LLVM version
        if [ "$llvm_found" -eq 0 ]; then
            # llvm-config returns "20.1.8", soname is "libLLVM.so.20.1"
            local llvm_soname=$(llvm-config --version 2>/dev/null | sed 's/\.[0-9]\+$//' || echo "20.1")
            # Search with exact soname, then with wildcard
            for lib in $(find /nix/store -name "libLLVM.so.${llvm_soname}" -type f,l 2>/dev/null | head -1); do
                cp -f "$lib" "$PKG_DIR/lib/"
                echo "    [OK] $(basename "$lib") (from nix store)"
                llvm_found=1
                break
            done
        fi
        if [ "$llvm_found" -eq 0 ]; then
            echo "    [WARN] libLLVM not found — ensure target system has it"
        fi

        # Bundle other non-system dependencies (but NOT ld-linux, libc, etc.)
        ldd "$BINARY_SRC" 2>/dev/null | while IFS= read -r line; do
            local libname=$(echo "$line" | awk '{print $1}')
            local libpath=$(echo "$line" | awk '{print $3}')

            case "$libname" in
                linux-vdso*|ld-linux*|libc.so*|libm.so*|libpthread*|libdl.so*|librt.so*|libresolv*|libutil*|libLLVM*)
                    # libLLVM already bundled above; system libs should not be bundled
                    continue
                    ;;
            esac

            if [ -n "$libpath" ] && [ -f "$libpath" ]; then
                # Check it's not in a glibc directory
                case "$libpath" in
                    */glibc-*|*/ld-linux*)
                        continue
                        ;;
                esac
                cp "$libpath" "$PKG_DIR/lib/"
                echo "    [OK] $libname"
            fi
        done

        # Set RPATH: bundled lib dir first, then system paths as fallback
        if command -v patchelf &>/dev/null; then
            patchelf --set-rpath '$ORIGIN/../lib:/usr/lib/x86_64-linux-gnu:/usr/lib:/lib' "$PKG_DIR/bin/$BINARY_NAME"
            echo "  [OK] RPATH configured"
        fi

        # Strip debug symbols
        strip "$PKG_DIR/bin/$BINARY_NAME" 2>/dev/null || true
    fi

    # On Linux static builds, just strip
    if [ "$PLATFORM" != "windows-x64" ] && [ "$LINKMODE" = "static" ]; then
        strip "$PKG_DIR/bin/$BINARY_NAME" 2>/dev/null || true
    fi

    # Copy examples
    mkdir -p "$PKG_DIR/examples"
    cp "$PROJECT_DIR"/examples/*.at "$PKG_DIR/examples/" 2>/dev/null || true

    # Copy stdlib
    cp -r "$PROJECT_DIR/stdlib" "$PKG_DIR/"

    # Create install script
    local install_note=""
    if [ "$LINKMODE" = "static" ]; then
        install_note="Self-contained static build. No external LLVM needed."
    else
        install_note="Dynamic build with bundled LLVM libraries."
    fi

    cat > "$PKG_DIR/install.sh" << INSTALL_EOF
#!/usr/bin/env bash
# Atomic Language Compiler - Install Script
# $PKG_NAME
# $install_note
set -euo pipefail

PREFIX="\${PREFIX:-/usr/local}"
BIN_DIR="\${PREFIX}/bin"
LIB_DIR="\${PREFIX}/lib"
SHARE_DIR="\${PREFIX}/share/atomic"

echo "Installing Atomic Compiler ($PKG_NAME)..."

mkdir -p "\$BIN_DIR" "\$LIB_DIR" "\$SHARE_DIR"

# Install binary
cp "\$(dirname "\$0")/bin/$BINARY_NAME" "\$BIN_DIR/$BINARY_NAME"
chmod 755 "\$BIN_DIR/$BINARY_NAME"

# Install bundled libraries (dynamic builds only)
if ls "\$(dirname "\$0")/lib/"*.so* >/dev/null 2>&1; then
    cp "\$(dirname "\$0")/lib/"*.so* "\$LIB_DIR/"
fi

# Install stdlib and examples
cp -r "\$(dirname "\$0")/stdlib" "\$SHARE_DIR/"
cp -r "\$(dirname "\$0")/examples" "\$SHARE_DIR/"

echo ""
echo "Installation complete!"
echo "  Binary:   \$BIN_DIR/$BINARY_NAME"
echo "  Stdlib:   \$SHARE_DIR/stdlib"
echo "  Examples: \$SHARE_DIR/examples"
echo ""
echo "Try: atomic run \$SHARE_DIR/examples/hello.at"
INSTALL_EOF

    # Dynamic builds: add LLVM note
    if [ "$LINKMODE" = "dynamic" ]; then
        cat >> "$PKG_DIR/install.sh" << INSTALL_EOF
echo ""
echo "Note: This is a dynamic build. If you see 'libLLVM.so not found',"
echo "the bundled LLVM libraries should be in \$LIB_DIR."
echo "You may need to run: ldconfig"
INSTALL_EOF
    fi

    chmod 755 "$PKG_DIR/install.sh"

    # Create uninstall script
    cat > "$PKG_DIR/uninstall.sh" << UNINSTALL_EOF
#!/usr/bin/env bash
set -euo pipefail
PREFIX="\${PREFIX:-/usr/local}"
rm -f "\$PREFIX/bin/$BINARY_NAME"
rm -f "\$PREFIX/lib"/lib{LLVM,clang,lldb}*.so*
rm -rf "\$PREFIX/share/atomic"
echo "Atomic compiler uninstalled."
UNINSTALL_EOF
    chmod 755 "$PKG_DIR/uninstall.sh"

    # Create README
    local size_mb="$(du -h "$PKG_DIR/bin/$BINARY_NAME" | cut -f1)"
    local reqs=""
    if [ "$LINKMODE" = "static" ]; then
        reqs="- Linux x86_64 (glibc 2.28+)\n- No LLVM or Rust installation needed"
    else
        reqs="- Linux x86_64 (glibc 2.28+)\n- Bundled LLVM libraries included"
    fi

    cat > "$PKG_DIR/README.txt" << README_EOF
Atomic Language Compiler - Distribution Package
================================================

Package:  $PKG_NAME
Linking:  $LINKMODE
Size:     $size_mb

Requirements
------------
$reqs

Installation
------------
  sudo ./install.sh

Or locally:
  PREFIX=\$HOME/.local ./install.sh

Usage
-----
  atomic run <file.at>        Compile and run an Atomic program
  atomic build <file.at>      Compile to an executable
  atomic check <file.at>      Type-check without compiling

Examples
--------
  atomic run examples/hello.at
  atomic run examples/fizzbuzz.at

Target Platforms (for compiled output)
--------------------------------------
  --target native              Current host
  --target linux-x64           x86_64 Linux
  --target linux-arm64         ARM64 Linux
  --target windows-x64         x86_64 Windows (needs mingw-w64)
  --target wasm                WebAssembly
README_EOF

    # Create tarball
    cd "$DIST_DIR"
    tar czf "${PKG_NAME}.tar.gz" "$PKG_NAME"
    echo "  [OK] Package: dist/${PKG_NAME}.tar.gz ($(du -h "${PKG_NAME}.tar.gz" | cut -f1))"
    echo ""
}

# ============================================================
# Build packages for each link mode
# ============================================================

mkdir -p "$DIST_DIR"

# Static package
build_package "static" "$STATIC_BIN"

# Dynamic package (Linux only — Windows doesn't have a standard LLVM shared lib)
if [ "$PLATFORM" != "windows-x64" ]; then
    build_package "dynamic" "$DYNAMIC_BIN"
fi

echo "=== Distribution packages created in dist/ ==="
ls -lh "$DIST_DIR"/*.tar.gz 2>/dev/null || true

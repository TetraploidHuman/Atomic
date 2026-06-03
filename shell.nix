{ pkgs ? import <nixpkgs> {} }:

pkgs.mkShell {
  buildInputs = with pkgs; [
    llvmPackages_20.llvm
    llvmPackages_20.libllvm
    llvmPackages_20.libclang
    pkg-config
    rustc
    cargo

    # Required for linking LLVM
    libffi
    zlib
    ncurses
    libxml2

    # HTTP / networking support
    curl.dev
    curl
    gcc
    binutils
  ];

  LLVM_SYS_201_PREFIX = "${pkgs.llvmPackages_20.llvm.dev}";
  LLVM_CONFIG_PATH = "${pkgs.llvmPackages_20.llvm.dev}/bin/llvm-config";

  shellHook = ''
    export LIBRARY_PATH="${pkgs.llvmPackages_20.libllvm}/lib''${LIBRARY_PATH:+:}$LIBRARY_PATH"
    export LD_LIBRARY_PATH="${pkgs.llvmPackages_20.libllvm}/lib''${LD_LIBRARY_PATH:+:}$LD_LIBRARY_PATH"
    export NIX_LD_RUN_PATH="${pkgs.llvmPackages_20.libllvm}/lib''${NIX_LD_RUN_PATH:+:}$NIX_LD_RUN_PATH"
    echo "Atomic Language Development Environment"
    echo "LLVM version: $(llvm-config --version)"
    echo "Rust version: $(rustc --version)"
  '';
}

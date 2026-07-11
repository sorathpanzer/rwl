{
  description = "rwl — dynamic Wayland window manager, rewritten in Rust with Smithay.";

  inputs = {
    nixpkgs.url = "github:NixOS/nixpkgs/nixos-26.05";
    flake-utils.url = "github:numtide/flake-utils";
    rust-overlay = {
      url = "github:oxalica/rust-overlay";
      inputs.nixpkgs.follows = "nixpkgs";
    };
  };

  outputs = { self, nixpkgs, flake-utils, rust-overlay }:
    flake-utils.lib.eachDefaultSystem (system:
      let
        overlays = [ (import rust-overlay) ];
        pkgs = import nixpkgs { inherit system overlays; };

        # Stable Rust toolchain with clippy + rustfmt
        rustToolchain = pkgs.rust-bin.stable.latest.default.override {
          extensions = [ "rust-src" "clippy" "rustfmt" ];
        };

        # Runtime libraries
        libs = with pkgs; [
          fcft
          pixman
          wayland
          seatd
          udev               # libudev runtime
          libdrm
          libgbm             # GBM (mesa-libgbm)
          libglvnd           # EGL / OpenGL
          libinput
          libxkbcommon
          libxcb
          pam                # libpam / libpam_misc for the `lock` feature
                             # (no pam.pc, so it must be a buildInput, not via pkg-config)
          cargo-bloat
          cargo-llvm-lines
        ];

        # Dev headers (pkg-config .pc files live here)
        devLibs = with pkgs; [
          wayland.dev
          seatd.dev
          udev.dev           # libudev.pc
          libdrm.dev
          libgbm             # gbm.pc is in the main output
          libglvnd.dev       # egl.pc
          libinput.dev
          libxkbcommon.dev
          libxcb.dev
        ];

        nativeBuildInputs = with pkgs; [
          rustToolchain
          pkg-config
        ];

        # Build PKG_CONFIG_PATH from known .pc locations
        pcPath = pkgs.lib.concatStringsSep ":" (
          pkgs.lib.concatMap (p: [
            "${p}/lib/pkgconfig"
            "${p}/share/pkgconfig"
          ]) devLibs
        );

        ldPath = pkgs.lib.concatStringsSep ":" (
          map (p: "${p}/lib") (libs ++ devLibs)
        );

        # Embed Nix store rpaths into the binary at link time so it works
        # outside the dev shell (e.g. running ./target/release/dwl directly).
        rustflags = pkgs.lib.concatStringsSep " " (
          map (p: "-C link-arg=-Wl,-rpath,${p}/lib") libs
        );

      in {
        devShells.default = pkgs.mkShell {
          name = "rwl-rust-dev";
          buildInputs = libs ++ devLibs;
          inherit nativeBuildInputs;

          shellHook = ''
            export PKG_CONFIG_PATH="${pcPath}:''${PKG_CONFIG_PATH:-}"
            export LD_LIBRARY_PATH="${ldPath}:''${LD_LIBRARY_PATH:-}"
            export RUSTFLAGS="${rustflags} ''${RUSTFLAGS:-}"
            echo "rwl Rust/Smithay dev shell — run: cargo build --release"
          '';
        };
      }
    );
}

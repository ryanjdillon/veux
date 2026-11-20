{
  description = "veux - terminal UI to configure ReSpeaker audio devices";

  inputs = {
    nixpkgs.url = "github:NixOS/nixpkgs/nixos-unstable";
    flake-utils.url = "github:numtide/flake-utils";
  };

  outputs =
    {
      nixpkgs,
      flake-utils,
      ...
    }:
    flake-utils.lib.eachDefaultSystem (
      system:
      let
        pkgs = import nixpkgs { inherit system; };
      in
      {
        # Toolchains only. rust-toolchain.toml pins the Rust version, and
        # Cargo.toml declares the dependencies.
        devShells.default = pkgs.mkShell {
          packages = with pkgs; [
            rustup
            just
            prek
            cargo-deny
            # Static helpers for remote hosts: `just build-helpers`.
            cargo-zigbuild
            zig
            pkg-config
            libusb1
          ];
          RUST_BACKTRACE = "1";
        };

        formatter = pkgs.nixfmt-rfc-style;
      }
    );
}

{
  description = "A very opinionated Matrix TUI";

  inputs = {
    nixpkgs.url = "github:nixos/nixpkgs/nixos-unstable";
    flake-utils.url = "github:numtide/flake-utils";
    rust-overlay = {
      url = "github:oxalica/rust-overlay";
      inputs.nixpkgs.follows = "nixpkgs";
    };
  };

  outputs =
    {
      nixpkgs,
      rust-overlay,
      flake-utils,
      ...
    }:
    flake-utils.lib.eachDefaultSystem (
      system:
      let
        pkgs = import nixpkgs {
          inherit system;
          overlays = [ rust-overlay.overlays.default ];
        };
        rustToolchain = pkgs.rust-bin.fromRustupToolchainFile ./rust-toolchain.toml;
        cargoToml = builtins.fromTOML (builtins.readFile ./Cargo.toml);
        sharedDeps = [
          rustToolchain
          pkgs.pkg-config
        ]
        ++ pkgs.lib.optionals pkgs.stdenv.isDarwin [ pkgs.darwin.apple_sdk.frameworks.Security ];
      in
      {
        packages = rec {

          matui = pkgs.rustPlatform.buildRustPackage {
            pname = cargoToml.package.name;
            inherit (cargoToml.package) version;
            src = ./.;
            cargoLock.lockFile = ./Cargo.lock;
            # Won't be found by `openssl-sys` if it's in `nativeBuildInputs`.
            buildInputs = with pkgs; [ openssl ];
            nativeBuildInputs =
              sharedDeps
              ++ (with pkgs; [
                ffmpeg
                sqlite
              ]);
          };

          default = matui;
        };

        devShells.default = pkgs.mkShell {
          buildInputs =
            sharedDeps
            ++ (with pkgs; [
              bacon
              openssl
              sqlite
            ]);
        };
      }
    );
}

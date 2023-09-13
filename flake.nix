{
  description = "A very opinionated Matrix TUI";

  inputs = {
    nixpkgs.url = "github:nixos/nixpkgs/nixos-unstable";
    rust-overlay.url = "github:oxalica/rust-overlay";
    flake-utils.url = "github:numtide/flake-utils";
  };

  outputs = { self, nixpkgs, rust-overlay, flake-utils, ... }:
    flake-utils.lib.eachDefaultSystem (system:
      let
        overlays = [ (import rust-overlay) ];
        pkgs = import nixpkgs { inherit system overlays; };
        rusttoolchain =
          pkgs.rust-bin.fromRustupToolchainFile ./rust-toolchain.toml;
        cargoToml = (builtins.fromTOML (builtins.readFile ./Cargo.toml));
        sharedDeps = with pkgs; [ rusttoolchain pkg-config ]
          ++ pkgs.lib.optionals pkgs.stdenv.isDarwin
          [ pkgs.darwin.apple_sdk.frameworks.Security ];
      in
      rec {
        # `nix build`
        packages = {
          matui = pkgs.rustPlatform.buildRustPackage {
            pname = cargoToml.package.name;
            version = cargoToml.package.version;
            src = ./.;
            cargoLock = {
              outputHashes = {
                "matrix-sdk-0.6.2" = "sha256-uV+pKHrAvApNrynyzQgxDCPZ3RKMO2FviBL4n+RDPzc=";
                "ruma-0.8.2" = "sha256-tgqUqiN6LNUyz5I6797J0YFsiFyYWfexa7n2jwUoHWA=";
                "vodozemac-0.3.0" = "sha256-tAimsVD8SZmlVybb7HvRffwlNsfb7gLWGCplmwbLIVE=";
              };
              lockFile = ./Cargo.lock;
            };
            # won't be found by `openssl-sys` if it's in `nativeBuildInputs`
            buildInputs = with pkgs;
              [ openssl ];
            nativeBuildInputs = with pkgs;
              sharedDeps ++ [ ];
          };
          default = packages.matui;
        };

        # `nix develop`
        devShells.default = pkgs.mkShell {
          buildInputs = with pkgs;
            sharedDeps ++ [ bacon openssl ];
        };

      });
}

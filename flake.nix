{
  description = "A very opinionated Matrix TUI";

  inputs = {
    nixpkgs.url = "github:nixos/nixpkgs/nixos-25.11";
    flake-utils.url = "github:numtide/flake-utils";
    rust-overlay = {
      url = "github:oxalica/rust-overlay";
      inputs.nixpkgs.follows = "nixpkgs";
    };
  };

  outputs = { nixpkgs, rust-overlay, flake-utils, ... }:
    flake-utils.lib.eachDefaultSystem (system:
      let
        overlays = [ (import rust-overlay) ];
        pkgs = import nixpkgs { inherit system overlays; };
        rusttoolchain =
          pkgs.rust-bin.fromRustupToolchainFile ./rust-toolchain.toml;
        cargoToml = builtins.fromTOML (builtins.readFile ./Cargo.toml);
        sharedDeps = [ rusttoolchain pkgs.pkg-config ]
          ++ pkgs.lib.optionals pkgs.stdenv.isDarwin
          [ pkgs.darwin.apple_sdk.frameworks.Security ];
      in rec {
        # `nix build`
        packages = {
          matui = pkgs.rustPlatform.buildRustPackage {
            pname = cargoToml.package.name;
            inherit (cargoToml.package) version;
            src = ./.;
            cargoLock.lockFile = ./Cargo.lock;
            # won't be found by `openssl-sys` if it's in `nativeBuildInputs`
            buildInputs = with pkgs; [ openssl ];
            nativeBuildInputs = sharedDeps ++ (with pkgs; [ ffmpeg sqlite ]);
          };
          default = packages.matui;
        };

        # `nix develop`
        devShells.default = pkgs.mkShell {
          buildInputs = sharedDeps ++ (with pkgs; [ bacon openssl sqlite ]);
        };
      });
}

let
  # put this on stable once Cargo 1.7 hits
  pkgs = import (fetchTarball("channel:nixpkgs-unstable")) {};
in pkgs.mkShell {
  buildInputs = with pkgs; [ cargo rustc rust-analyzer openssl pkg-config ];
}

{ pkgs ? import <nixpkgs> {} }:

pkgs.mkShell {
  buildInputs = with pkgs; [ cargo ffmpeg rustc rust-analyzer bacon clippy openssl pkg-config ];
}

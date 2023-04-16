{ pkgs ? import <nixpkgs> { } }:

pkgs.mkShell {
  nativeBuildInputs = with pkgs; [ ];
  buildInputs = with pkgs; [ libxkbcommon ];
  inputsFrom = with pkgs; [ ];
  hardeningDisable = [ "all" ];
}

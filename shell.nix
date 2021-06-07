let pkgs = import <nixpkgs> {}; in
pkgs.mkShell {
  buildInputs = with pkgs; [ cargo rustfmt pkg-config openssl ];
}

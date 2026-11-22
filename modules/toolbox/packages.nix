# The toolbox's tools by what they're for: the toolbox installs both
# (../toolbox.nix), and an engine plugin's sandbox gets each only with its
# grant (../agents/plugins.nix): `system` the first, `toolchains` the second.
pkgs: {
  system = with pkgs; [
    jujutsu
    mergiraf
    # search and files
    ripgrep
    fd
    tree
    file
    unzip
    zip
    zstd
  ];
  # languages and builds
  toolchains = with pkgs; [
    gcc
    gnumake
    pkg-config
    python3
    uv
    nodejs_22
    pnpm
    rustup
    go
    sqlite
  ];
}

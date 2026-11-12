# The house fonts (fieldmouseworks.com's), as the dashboard serves them:
# WOFF2, under the names crates/burrow-dashboard/src/assets.rs's FONTS
# reads. Served from the machine, so the dashboard never asks Google for
# them, and the service worker can keep them for the offline shell.
{ pkgs }:
let
  # Not in nixpkgs on its own: from Google Fonts' repository, at the commit
  # nixpkgs' google-fonts builds from.
  bigShoulders = pkgs.fetchurl {
    name = "big-shoulders-display.ttf";
    url = "https://raw.githubusercontent.com/google/fonts/5174b3333331c966c38f4355d50b03ca1c1df2f9/ofl/bigshouldersdisplay/BigShouldersDisplay%5Bwght%5D.ttf";
    hash = "sha256-YOII3CdqHDX8W2LpT5+5WcQMEXg6nrdUgXXBSx++tyA=";
  };
  serif = "${pkgs.source-serif}/share/fonts/truetype";
  mono = "${pkgs.ibm-plex}/share/fonts/truetype";
in
pkgs.runCommand "burrow-dashboard-fonts" { nativeBuildInputs = [ pkgs.woff2 ]; } ''
  mkdir $out
  font() {
    cp "$1" "$out/$2.ttf"
    woff2_compress "$out/$2.ttf"
    rm "$out/$2.ttf"
  }
  font ${bigShoulders} big-shoulders-display
  font ${serif}/SourceSerif4-Regular.ttf source-serif-4-regular
  font ${serif}/SourceSerif4-It.ttf source-serif-4-italic
  font ${serif}/SourceSerif4-Semibold.ttf source-serif-4-semibold
  font ${mono}/IBMPlexMono-Regular.ttf ibm-plex-mono-regular
  font ${mono}/IBMPlexMono-Medium.ttf ibm-plex-mono-medium
''

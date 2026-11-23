# The one Conary: the rev every conary-* crate in crates/Cargo.lock comes
# from. burrow's crates link it, and the ledger's package (burrow.ledger
# .package) is built from it, or held to it when a machine gives its own, so
# what both compute alike (the flow's judging context, which the judge checks
# a request's against) can't drift apart. Moving Conary is moving this pin:
# the crates and the machine's conaryd and `cn` go together.
{ lib }:
let
  lock = builtins.fromTOML (builtins.readFile ../crates/Cargo.lock);
  sources = lib.unique (map (p: p.source)
    (builtins.filter (p: lib.hasPrefix "conary-" p.name && p ? source) lock.package));
  # git+URL?rev=REV#REV
  parsed = builtins.match "git\\+([^?#]+)(\\?[^#]*)?#([0-9a-f]{40})" (builtins.head sources);
in
if builtins.length sources != 1 then
  throw "crates/Cargo.lock takes Conary from ${toString (builtins.length sources)} sources (${lib.concatStringsSep ", " sources}): pin every conary-* crate at one rev"
else if parsed == null then
  throw "crates/Cargo.lock takes Conary from ${builtins.head sources}, not a git rev"
else rec {
  url = builtins.elemAt parsed 0;
  rev = builtins.elemAt parsed 2;
  # The source at that rev, fetched as crane fetches it for the crates.
  src = builtins.fetchGit { inherit url rev; allRefs = true; submodules = true; };
  # Its packages, from its own flake.nix with burrow as the burrow it gets:
  # what a machine with Conary as an input that follows its burrow builds.
  flake = burrow: inputs:
    let
      self = src // outputs // { inherit rev; outPath = src.outPath; };
      outputs = (import (src + "/flake.nix")).outputs ({ inherit self burrow; } // inputs);
    in
    self;
}

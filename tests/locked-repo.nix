{ lib }:
let
  repo = import ../lib/locked-repo.nix { inherit lib; };
in
assert repo { type = "github"; owner = "sam"; repo = "tool"; } == "sam/tool";
assert repo { type = "git"; url = "https://github.com/sam/tool.git"; } == "sam/tool";
assert repo { type = "git"; url = "https://github.com/sam/tool"; } == "sam/tool";
assert repo { type = "git"; url = "https://other.invalid/sam/tool.git"; } == null;
assert repo { type = "path"; path = "/unknown"; } == null;
true

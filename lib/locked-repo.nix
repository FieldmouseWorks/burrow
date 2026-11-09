# The canonical GitHub repository of a flake lock source, if identified.
{ lib }:
locked:
if (locked.type or "") == "github" then "${locked.owner}/${locked.repo}"
else if (locked.type or "") == "git" then
  let m = builtins.match "https://github.com/([^/]+)/([^/?#]+).*" (locked.url or "");
  in if m == null then null else "${lib.elemAt m 0}/${lib.removeSuffix ".git" (lib.elemAt m 1)}"
else null

# lib/project.nix's secrets check over all of burrow's tracked files;
# the projects below each see only their own directory.
{ self, system, ... }:
(self.lib.project { inherit self; systems = [ system ]; }).checks.${system}.secrets

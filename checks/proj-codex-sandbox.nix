# Real Codex sandbox, git metadata grants and no_new_privs, in a VM.
{ self, pkgs, ... }:
import ../tests/proj-codex-sandbox.nix { inherit self pkgs; }

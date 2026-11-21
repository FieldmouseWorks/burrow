# Standing-authority commands, in Claude Bash-pattern syntax: the owner's
# advance approval (burrow.claude.standingAuthority) for things a session
# does routinely, so Claude Code's auto-mode check, which can't tell the
# owner's words from a relay, isn't the one deciding them. Each is a
# `permissions.allow` rule as written, never with sudo. Only these exact
# commands: no bare gh, gh api, other proj or settings verbs, or merges.
#
# A rule's wildcard stands for any text in that place, so a label edit that
# also changes another field of the same issue or PR matches too; every one
# of these is reversible. The R2 ask rules outrank them (ask beats allow),
# and the guard hook (claude/guard.nix) still runs before every command.
# checks/claude-permissions.nix holds the list to these forms.
[
  # Pass news (a sign-in, a ruling) to a parked job.
  "proj reply *"
  # Hand one PR in a paused repo to the steward, which merges it on its own
  # checks (docs/settings.md, "Handing one PR to the steward").
  "settings land *"
  # Take a label off an issue or PR.
  "gh issue edit * --remove-label *"
  "gh pr edit * --remove-label *"
  # Run a workflow run's jobs again.
  "gh run rerun *"
]

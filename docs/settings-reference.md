# Settings reference

<!-- Written by `burrow-settings reference` from
crates/burrow-settings/src/schema.rs: change the schema, not this file. -->

Every pipeline option, declared once in burrow-settings' schema
([crates/burrow-settings/src/schema.rs](../crates/burrow-settings/src/schema.rs)).
This is the one place the guidance gives an option's value or where it's
kept: anything else links here. `proj settings show` prints the value in force
on the machine and where it came from; [settings.md](settings.md) says how
they're read and changed.

An option that isn't live is set in the machine's flake, under `burrow.`
and its name: a change is a PR to the flake and a deploy. A live one is kept
in a file on the machine (the settings overlay under `burrow.stateDir`;
proj's in the job user's `~/.config/proj`, setups in `roles.d/` beside the
roles file, and the usage hold in proj's jobs directory), and its readers
read it again at their next decision.

| Option | Type | Allowed | Default | Live | Owner | Kept in | What it is |
|---|---|---|---|---|---|---|---|
| `proj.maxJobs` | integer | 1 or more | `4` | no | proj | `burrow.proj.maxJobs` in the flake | How many turns run Claude at once; the rest wait in line. |
| `proj.pipelineSlots` | integer | 0 or more | 1 when maxJobs is more than 1, else 0 | no | proj | `burrow.proj.pipelineSlots` in the flake | Of maxJobs, how many only the pipeline's own repos' jobs may take. |
| `proj.maxJobsPerProject` | integer | 1 or more | max 1 (maxJobs - pipelineSlots - 1) | no | proj | `burrow.proj.maxJobsPerProject` in the flake | The most slots one project's jobs hold at once. |
| `proj.codex.slots` | integer | 1 or more | `2` | no | proj | `burrow.proj.codex.slots` in the flake | How many Codex turns run at once, in a pool of their own. |
| `proj.usageTrickle` | integer | 1 or more | `2` | no | proj | `burrow.proj.usageTrickle` in the flake | How many Claude turns run at once while the line from a usage hold drains after its reset. |
| `proj.usage.paid` | boolean | `true`, `false` | `false` | no | proj | `burrow.proj.usage.paid` in the flake | Whether paid extra usage is on; otherwise a usage limit means waiting for its reset. |
| `authors` | list of {login: text, id: integer} | each login: any; id: 1 or more | `[]` | no | proj | `burrow.authors` in the flake | The GitHub accounts, by login and immutable numeric ID, whose issues and replies the inbox, engines door, probe, undo door and settings door act on. |
| `settings.allowed` | list of repository | each `OWNER/NAME` | `[]` | no | proj | `burrow.settings.allowed` in the flake | Tracked repositories whose automated work may move; the live repository overlay overrides these defaults. |
| `settings.repositoryOverrides` | map of boolean | each `true`, `false` | `{}` | yes | proj | `settings/overlay.json` | Runtime repository permissions overriding settings.allowed; false vacates every role immediately. |
| `settings.pipelineUntil` | integer or null | 0 or more, or `null` | `null` | yes | proj | `settings/overlay.json` | Until this timestamp only pipeline repositories may move; null means no scoped hold. |
| `proj.inbox.triage.drain` | word or null | `claude`, `codex`, or `null` | `null` | no | proj-inbox | `burrow.proj.inbox.triage.drain` in the flake | The engine `ready` issues are handed to on its idle slots; null hands none on. |
| `proj.inbox.triage.drainJobs` | integer | 1 or more | `2` | no | proj-inbox | `burrow.proj.inbox.triage.drainJobs` in the flake | Handed-on issues with a job at once, across every repo. |
| `proj.inbox.triage.atOnce` | integer | 1 or more | `2` | no | proj-inbox | `burrow.proj.inbox.triage.atOnce` in the flake | Sorting jobs at once, across every repo. |
| `autoland.rounds` | integer | 1 or more | `5` | no | autoland | `burrow.autoland.rounds` in the flake | Review rounds with findings before the reviewers stop and a judge decides. |
| `autoland.handoffAfterMinutes` | integer | 1 or more | `10` | no | autoland | `burrow.autoland.handoffAfterMinutes` in the flake | How long findings or red CI sit untouched before a Claude job takes them. |
| `autoland.sessionHandoffAfterMinutes` | integer | 1 or more | `30` | no | autoland | `burrow.autoland.sessionHandoffAfterMinutes` in the flake | The same wait for a PR whose own session is linked in its body. |
| `autoland.settleDelayMinutes` | integer | 1 or more | `1` | no | autoland | `burrow.autoland.settleDelayMinutes` in the flake | How long a new head sits still before the reviewers are asked. |
| `autoland.stallAfterMinutes` | integer | 1 or more | `120` | no | autoland | `burrow.autoland.stallAfterMinutes` in the flake | How long a PR may wait on one thing before the stall watchdog takes it. |
| `autoland.trainStallAfterMinutes` | integer | 1 or more | `240` | no | autoland | `burrow.autoland.trainStallAfterMinutes` in the flake | The same for a PR in its repo's merge train. |
| `autoland.holdStallAfterMinutes` | integer | 1 or more | `480` | no | autoland | `burrow.autoland.holdStallAfterMinutes` in the flake | The same for a PR on hold. |
| `autoland.renovateMergeAfterMinutes` | integer | 1 or more | `60` | no | autoland | `burrow.autoland.renovateMergeAfterMinutes` in the flake | How long a settled, green Renovate PR with automerge may sit before the watchdog takes it. |
| `autoland.jobTimeoutMinutes` | integer | 1 or more | `60` | no | autoland | `burrow.autoland.jobTimeoutMinutes` in the flake | How long each turn of a steward's Claude job may run. |
| `autoland.jobIdleMinutes` | integer | 1 or more | `20` | no | autoland | `burrow.autoland.jobIdleMinutes` in the flake | How long such a turn may say nothing before it's stuck and stopped. |
| `autoland.proofTimeoutMinutes` | integer | 1 or more | `180` | no | autoland | `burrow.autoland.proofTimeoutMinutes` in the flake | How long a proof build or run the steward does itself may take. |
| `autoland.baselinePerPass` | integer | 1 or more | `2` | no | autoland | `burrow.autoland.baselinePerPass` in the flake | The most baseline jobs one pass starts. |
| `autoland.repos` | map of object | any | `{}` | no | autoland | `burrow.autoland.repos` in the flake | Per-repo overrides (OWNER/NAME) of the steward's rounds, waits and checks. |
| `autoland.reviewers.codex.enable` | boolean | `true`, `false` | `false` | no | autoland-review | `burrow.autoland.reviewers.codex.enable` in the flake | Whether Codex reviews every head. |
| `autoland.reviewers.codex.effort` | word | `low`, `medium`, `high`, `xhigh` | `"high"` | no | autoland-review | `burrow.autoland.reviewers.codex.effort` in the flake | How hard Codex thinks on a review. |
| `autoland.reviewers.codex.parallel` | integer | 1 or more | `2` | no | autoland-review | `burrow.autoland.reviewers.codex.parallel` in the flake | The most Codex reviews at once. |
| `autoland.reviewers.timeoutMinutes` | integer | 1 or more | `30` | no | autoland-review | `burrow.autoland.reviewers.timeoutMinutes` in the flake | How long one review may run before it's a failed try. |
| `autoland.reviewers.skipPaths` | list of text | any | `["(^\|/)secrets?/","(^\|/)\\.sops\\.yaml$","\\.sops\\.(ya?ml\|json\|env)$","(^\|/)\\.env(\\..*)?$","\\.(pem\|key\|p12\|pfx)$"]` | no | autoland-review | `burrow.autoland.reviewers.skipPaths` in the flake | Path regexes left out of what the reviewers read. |
| `proj.roles` | map of {engine: word, over: {pct: number, engine: word} or null} | each engine: `claude`, `codex`; over: pct: 0 to 100; engine: `claude`, `codex`, or `null` | `{"*":{"engine":"claude","over":null},"planner":{"engine":"codex","over":null}}` | yes | proj | `settings/overlay.roles.json` | Which engine runs each role (ROLE, ROLE@OWNER/REPO, * for every role, *@OWNER/REPO), and the engine it moves to when its own is short (over PCT ENGINE). The closest key wins: a named role over *, a repo's over the bare one. |
| `proj.models` | map of {model: text or null, effort: text or null} | each model: any, or `null`; effort: any, or `null` | `{"* claude":{"effort":null,"model":null},"* codex":{"effort":null,"model":null}}` | yes | proj | `settings/overlay.roles.json` | Each role's model and effort on each engine (ROLE[@OWNER/REPO] ENGINE), keyed as proj.roles is; null is the engine's own setting. Each must be one proj.engineOffers lists. |
| `proj.engineOffers` | map of {models: list of text, efforts: list of text} | each models: any; efforts: any | `{"claude":{"efforts":["low","medium","high","xhigh","max"],"models":[]},"codex":{"efforts":["minimal","low","medium","high","xhigh"],"models":[]}}` | yes | proj | `settings/overlay.roles.json` | The models and efforts each engine offers on the machine: the only ones proj.models may name. Model ids live here, never in a tracked file. |
| `proj.engineSetups` | {active: text or null, saved: map of {roles: map of {engine: word, over: {pct: number, engine: word} or null}, models: map of {model: text or null, effort: text or null}}} | active: any, or `null`; saved: each roles: each engine: `claude`, `codex`; over: pct: 0 to 100; engine: `claude`, `codex`, or `null`; models: each model: any, or `null`; effort: any, or `null` | `{"active":null,"saved":{}}` | yes | proj | `settings/overlay.roles.json` | Saved engine setups (proj engines save/use), each a named set of proj.roles and proj.models values, and the one in force. |
| `proj.usageHold` | {until: integer, since: integer, owner: boolean, reason: text} or null | until: 0 or more; since: 0 or more; owner: `true`, `false`; reason: any, or `null` | `null` | yes | proj | `.usage.json` | The usage hold in force: no Claude turn takes a slot until it ends. Null for none. |

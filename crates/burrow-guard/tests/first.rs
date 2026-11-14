//! A proj job never puts the `first` label on: a job for a person's issue
//! acts on GitHub as them, where GitHub can't tell its label from theirs.

use std::path::PathBuf;
use std::time::SystemTime;

use burrow_guard::config::Config;
use burrow_guard::{Guard, Hook, Input, Namespace};

/// What the guard says about `cmd`, keeping `label`, in a job (`job`) or
/// not, Codex's (`codex`) or Claude's.
fn says(cmd: &str, label: Option<&str>, job: bool, codex: bool) -> Option<String> {
    let dir = tempfile::tempdir().unwrap();
    let config = Config {
        git: PathBuf::from("git"),
        zfs: PathBuf::from("/no-such-zfs"),
        systemd_run: PathBuf::from("/no-such-systemd-run"),
        snapshot_datasets: Vec::new(),
        ask_after_seconds: 8,
        first_label: label.map(str::to_owned),
    };
    let guard = Guard {
        config: &config,
        codex,
        job,
        mark: dir.path().join("mark"),
        namespace: Namespace::Machine,
    };
    let hook = Hook {
        tool_name: Some("Bash".to_owned()),
        tool_input: Some(Input {
            command: Some(cmd.to_owned()),
        }),
        cwd: Some(dir.path().to_string_lossy().into_owned()),
    };
    guard.check(&hook, SystemTime::now())
}

fn refused(cmd: &str) -> bool {
    says(cmd, Some("first"), true, false)
        .is_some_and(|r| r.starts_with("Puts the `first` label on"))
}

const PUTS_FIRST_ON: &[&str] = &[
    "gh issue edit 5 --add-label first",
    "gh pr edit 5 --add-label bug,first",
    "gh pr edit 5 --add-label=First",
    "gh issue edit 5 --add-label ' first '",
    "gh -R o/r issue edit 5 --add-label first",
    "gh issue edit 5 -R o/r --add-label \"$label\"",
    "gh issue edit 5 --add-label fir*",
    "gh issue create --title x --label first",
    "gh pr create --fill -l first",
    "gh pr create --fill -lfirst",
    "gh pr create --fill --label=bug,first",
    "gh api repos/o/r/issues/5/labels -f 'labels[]=first'",
    "gh api -X PUT repos/o/r/issues/5/labels -f labels[]=first",
    "gh api --method PATCH /repos/o/r/issues/5 -f 'labels[]=first'",
    "gh api repos/o/r/issues -f title=x -f 'labels[]=first'",
    "gh api https://api.github.com/repos/o/r/issues/5/labels/ -F 'labels[]=FIRST'",
    "gh api 'repos/{owner}/{repo}/issues/5/labels' --input labels.json",
    "gh api repos/o/r/issues/5/labels -F labels[]=@labels.txt",
    "gh api repos/o/r/issues/5/labels -f \"labels[]=$l\"",
    "gh api repos/o/r/issues/5/labels -f \"$field\"",
    "gh api \"$endpoint\" -f 'labels[]=first'",
    "gh api \"repos/o/r/issues/$n\" -X PATCH -f 'labels[]=first'",
    "gh api graphql -f query='mutation { addLabelsToLabelable(input: {labelableId: \"I\", labelIds: [\"L\"]}) { clientMutationId } }'",
    "gh api graphql -f query='mutation($i: UpdateIssueInput!) { updateIssue(input: $i) { issue { id } } }' -f 'labelIds[]=L'",
    "gh api graphql -F query=@mutation.graphql",
    "gh api graphql --input body.json",
    "curl -X POST -H 'Authorization: token t' https://api.github.com/repos/o/r/issues/5/labels -d '{\"labels\":[\"first\"]}'",
    "curl https://api.github.com/repos/o/r/issues/5/labels -d '[\"first\"]'",
    "curl -X PATCH https://api.github.com/repos/o/r/issues/5 --json '{\"labels\":[{\"name\":\"First\"}]}'",
    "curl -X PATCH https://api.github.com/repos/o/r/issues/5 -d \"$body\"",
    "curl https://api.github.com/repos/o/r/issues/5/labels -d @labels.json",
    "curl -X POST \"$url\" -d '{\"labels\":[\"first\"]}'",
    "curl https://api.github.com/graphql -d @mutation.json",
    "sh -c 'gh issue edit 5 --add-label first'",
    "cd /work && gh issue edit 5 --add-label first",
    "gh issue view 5; /run/current-system/sw/bin/gh pr edit 5 --add-label first",
];

const LEAVES_IT_ALONE: &[&str] = &[
    "gh issue edit 5 --add-label bug",
    "gh issue edit 5 --remove-label first",
    "gh issue edit 5 --add-label firsts,first-time",
    "gh pr create --title 'do this first' --body first",
    "gh issue create --title first --label bug",
    "gh issue view 5 --json labels",
    "gh issue list --label first",
    "gh api repos/o/r/issues/5/labels",
    "gh api repos/o/r/issues/5/comments -f body=\"$msg\"",
    "gh api repos/o/r/pulls/5/comments/7/replies -f body=first",
    "gh api -X PATCH repos/o/r/issues/5 -f body=\"$text\"",
    "gh api repos/o/r/issues -f title='first things' -f body=first",
    "gh api repos/o/r/issues/5/labels -f 'labels[]=bug'",
    "gh api -X DELETE repos/o/r/issues/5/labels/first",
    "gh api graphql -f query='mutation($id: ID!) { resolveReviewThread(input: {threadId: $id}) { thread { id } } }' -f id=\"$id\"",
    "gh api graphql -f query='query { repository(owner: \"o\", name: \"r\") { labels(first: 5) { nodes { name } } } }'",
    "curl https://api.github.com/repos/o/r/issues/5/labels",
    "curl -X POST https://api.github.com/repos/o/r/issues/5/comments -d '{\"body\":\"first\"}'",
    "curl -X POST https://api.github.com/repos/o/r/issues/5/labels -d '{\"labels\":[\"bug\"]}'",
    "curl -X POST \"$url\" -d '{\"text\":\"hi\"}'",
    "echo first",
];

/// What the shell gives at run time, where gh or curl would take it as
/// options, a command or a label: an unquoted expansion or substitution
/// (any number of words), a glob, a quoted one where an option may be, a
/// program or a gh command only an expansion names, and words xargs adds.
const RUNTIME_WORDS: &[&str] = &[
    "opts='--add-label first'; gh issue edit 5 $opts",
    "gh issue edit 5 $opts",
    "gh issue edit 5 \"$opt\"",
    "gh issue edit \"$n\" --add-label bug",
    "gh pr create --title x --body $(cat body.md)",
    "gh pr create --title x --body `cat body.md`",
    "gh issue edit 5 --add-label \"$(cat labels)\"",
    "gh issue edit 5 ${opts:+--add-label first}",
    "gh issue edit 5 --add-label=$label",
    "gh $sub 5 --add-label first",
    "gh issue \"$verb\" 5 --add-label first",
    "\"$GH\" issue edit 5 --add-label first",
    "$GH issue edit 5 --add-label first",
    "$cmd --add-label first",
    "$(command -v gh) pr edit 5 --add-label first",
    "printf '%s\\n' first | xargs gh issue edit 5 --add-label",
    "xargs gh pr create --title x",
    "echo 5 | xargs -I{} gh pr edit {} --add-label bug",
    "sh -c \"gh issue edit 5 $opts\"",
    "sh -c 'gh pr edit 5 --add-label \"$1\"' _ first",
    "gh alias set fl 'issue edit --add-label first'",
    "gh alias import aliases.yml",
    "gh api repos/o/r/issues/5/labels $fields",
    "gh api $endpoint -f 'labels[]=bug'",
    "gh api \"$opt\" repos/o/r/issues/5/comments -f 'labels[]=first'",
    "echo 'labels[]=first' | xargs gh api repos/o/r/issues/5/labels -f",
    "curl https://api.github.com/repos/o/r/issues/5/labels $opts",
    "curl $url -d '{\"labels\":[\"bug\"]}'",
    "echo first | xargs curl -X POST https://api.github.com/repos/o/r/issues/5/labels -d",
    "\"$curl\" -X POST https://api.github.com/repos/o/r/issues/5/labels -d '[\"first\"]'",
];

/// Where a call goes or what it sends, the guard can't read: an endpoint
/// only an expansion or gh's `{branch}` gives (REST or GraphQL, either may
/// be it), a GraphQL query built from one, or a typed field gh fills in.
const UNREAD_TARGETS: &[&str] = &[
    "gh api graphql -f query=\"$query\"",
    "gh api graphql -f query=\"mutation { $m }\"",
    "gh api graphql -F query='{branch}'",
    "gh api \"$endpoint\" -f query='mutation { addLabelsToLabelable(input: {labelableId: \"I\", labelIds: [\"L\"]}) { clientMutationId } }'",
    "gh api \"repos/o/r/issues/$n/comments\" -f 'labels[]=first'",
    "gh api 'repos/o/r/issues/5/{branch}' -f 'labels[]=first'",
    "gh api repos/o/r/issues/5/labels -F 'labels[]={branch}'",
    "gh api -X \"$method\" repos/o/r/issues/5/labels -f 'labels[]=first'",
    "curl -X POST \"$url\" -d '{\"query\":\"mutation { addLabelsToLabelable(input: {}) { clientMutationId } }\"}'",
    "curl -X POST \"https://api.github.com/repos/o/r/issues/$n/comments\" -d '{\"labels\":[\"first\"]}'",
];

/// gh's own grammar: pflag's `-l=value`, short options run together, the
/// `new` alias, CSV-quoted label lists, an issue template's labels and a
/// recovered run's; and a GitHub path spelled another way.
const GH_GRAMMAR: &[&str] = &[
    "gh pr create --fill -l=first",
    "gh pr create -wl first",
    "gh pr create -dlfirst",
    "gh pr create -fl=bug,first",
    "gh issue new --title x --label first",
    "gh pr new -l first",
    "gh issue edit 5 --add-label '\"first\"'",
    "gh issue edit 5 --add-label 'bug,\"First\"'",
    "gh issue edit 5 --add-label '\"bug\",\"first\"'",
    "gh issue create --title x --template bug_report.md",
    "gh issue create -T 'Bug report' -t x",
    "gh pr create --recover state.json",
    "gh label edit bug --name first",
    "gh label edit bug -n First -R o/r",
    "gh api repos/o/r/issues/5/labels '-f=labels[]=first'",
    "gh api -iXPOST repos/o/r/issues/5/labels -f 'labels[]=first'",
    "gh api repos/o/r/issues/5/labels --raw-field='labels[]=first'",
    "gh api repos/o/r/Issues/5/Labels -f 'labels[]=first'",
    "gh api repos/o/r/issues/5/x/../labels -f 'labels[]=first'",
    "gh api repos/o/r/issues/5/label%73 -f 'labels[]=first'",
    "gh api -X PATCH repos/o/r/labels/bug -f new_name=first",
    "gh api graphql -f query='mutation { createIssue(input: {repositoryId: \"R\", title: \"x\", issueTemplate: \"bug\"}) { issue { id } } }'",
    "gh api graphql -f query='mutation { updateLabel(input: {id: \"L\", name: \"first\"}) { label { id } } }'",
];

/// curl's body as it's sent: its pieces put together (`--json`'s with
/// nothing between), and JSON's escapes decoded.
const CURL_BODIES: &[&str] = &[
    "curl https://api.github.com/repos/o/r/issues/5/labels --json '{\"labels\":[\"fir' --json 'st\"]}'",
    "curl https://api.github.com/repos/o/r/issues/5/labels -d '{\"labels\":' --json '[\"first\"]}'",
    "curl https://api.github.com/repos/o/r/issues/5/labels -d '{\"lab\\u0065ls\":[\"fir\\u0073t\"]}'",
    "curl https://api.github.com/graphql -d '{\"query\":\"mutation { addLabelsToLabel\\u0061ble(input: {}) { clientMutationId } }\"}'",
    "curl https://api.github.com/graphql --json '{\"query\":\"mutation($i: UpdateIssueInput!) { updateIssue(input: $i) { issue { id } } }\",\"variables\":{\"i\":{\"id\":\"I\",\"label\\u0049ds\":[\"L\"]}}}'",
    "curl https://api.github.com/graphql --json '{\"query\":\"mutation { addLabelsTo' --json 'Labelable(input: {}) { clientMutationId } }\"}'",
    "curl https://api.github.com/graphql -d '{\"query\":\"x\"}' -d @more.json",
    "curl -X PATCH https://api.github.com/repos/o/r/labels/bug -d '{\"new_name\":\"first\"}'",
    "CURL_HOME=/tmp/c curl https://api.github.com/repos/o/r/issues/5/labels",
    "XDG_CONFIG_HOME=/tmp/c curl https://api.github.com/repos/o/r/issues/5",
    "HOME=/tmp/c curl https://api.github.com/user",
];

/// What a job still does: expansions quoted where gh or curl takes a
/// value that isn't a label, readable GraphQL with its own variables, a
/// title or body that only looks like a label option, and curl kept off
/// its default config.
const STILL_RUNS: &[&str] = &[
    "gh pr create --title \"$title\" --body \"$(cat <<'EOF'\nFixes the first label.\nEOF\n)\"",
    "gh issue comment 5 --body \"$msg\"",
    "gh pr edit 5 --body-file \"$f\"",
    "gh pr edit 5 --title \"$(git log -1 --format=%s)\" --add-label bug",
    "gh issue create --title x --body '--label=first'",
    "gh pr create --title '-l first' --body x",
    "gh pr create --title x --body x --template .github/pull_request_template.md",
    "gh issue edit 5 --remove-label first",
    "gh pr view \"$n\" --json labels",
    "gh api graphql -f query='query($n: Int!) { repository(owner: \"o\", name: \"r\") { pullRequest(number: $n) { labels(first: 5) { nodes { name } } } } }' -F n=\"$n\"",
    "gh api \"repos/o/r/pulls/$n\"",
    "gh api repos/o/r/issues/5/comments -f body=\"$(cat note.md)\"",
    "curl -s \"$url\"",
    "curl https://api.github.com/repos/o/r/issues/5/labels -d '{\"labels\":[\"fir' -d 'st\"]}'",
    "CURL_HOME=/tmp/c curl -q https://api.github.com/repos/o/r/issues/5",
    "cp \"$a\" \"$b\" && cd $dir && ls $dir",
    "for f in *.rs; do echo $f; done",
    "git commit -m \"$msg\"",
    "sh -c 'gh pr edit \"$1\" --add-label bug' _ 5",
];

#[test]
fn what_puts_first_on_is_refused_in_a_job() {
    for cmd in PUTS_FIRST_ON
        .iter()
        .chain(RUNTIME_WORDS)
        .chain(UNREAD_TARGETS)
        .chain(GH_GRAMMAR)
        .chain(CURL_BODIES)
    {
        assert!(refused(cmd), "{cmd}");
        // Codex's guard, every Codex it guards a job's, refuses it too.
        assert!(says(cmd, Some("first"), true, true).is_some(), "{cmd}");
    }
}

#[test]
fn what_leaves_first_alone_runs() {
    for cmd in LEAVES_IT_ALONE.iter().chain(STILL_RUNS) {
        assert_eq!(says(cmd, Some("first"), true, false), None, "{cmd}");
    }
}

/// The owner's own sessions put it on as theirs, and a machine whose
/// steward reads no such label keeps none.
#[test]
fn outside_a_job_or_with_no_label_nothing_is_asked() {
    for cmd in PUTS_FIRST_ON
        .iter()
        .chain(RUNTIME_WORDS)
        .chain(UNREAD_TARGETS)
        .chain(GH_GRAMMAR)
    {
        assert_eq!(says(cmd, Some("first"), false, false), None, "{cmd}");
        assert_eq!(says(cmd, None, true, false), None, "{cmd}");
    }
}

/// The label is the machine's to name, spaces and all.
#[test]
fn the_label_is_the_one_the_machine_names() {
    let says_as = |cmd, label| says(cmd, Some(label), true, false);
    let says = |cmd| says_as(cmd, "go now");
    assert!(says("gh issue edit 5 --add-label 'Go Now'").is_some());
    assert!(says("gh api repos/o/r/issues/5/labels -f 'labels[]=go now'").is_some());
    assert!(says("gh issue edit 5 --add-label first").is_none());
    // A comma in it, which gh's label lists take quoted, as CSV.
    let says_comma = |cmd| says_as(cmd, "now, please");
    assert!(says_comma("gh pr create -t x -l 'bug,\"Now, please\"'").is_some());
    assert!(says_comma("gh pr create -t x -l 'now,please'").is_none());
}

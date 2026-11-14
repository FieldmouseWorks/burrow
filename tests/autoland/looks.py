"""A repo's PRs labelled `first` mustn't keep the rest from their look.

Every kicked pass starts the repo's PRs over (Batch.pending), and a
pass's slice ends after a few slow looks. With enough of them labelled
first, each slice went to them again, and a PR without the label (the
next in the proof line, say) waited for hours. Each open PR gets a look
before any gets its second (a round, Batch.due), the first-labelled
ahead of the rest within one.
"""
import copy
from contextlib import ExitStack
import json
import os
from pathlib import Path
import sys
import tempfile
from unittest.mock import patch

sys.path.insert(0, sys.argv[1] if len(sys.argv) > 1 else
                str(Path(__file__).resolve().parents[2] / 'modules/autoland'))
with tempfile.TemporaryDirectory() as tmp:
    cfg = Path(tmp) / 'config.json'
    cfg.write_text(json.dumps(dict(
        context='test', stateDir=tmp, snapDir=tmp,
        conary={'cn': 'cn', 'env': {}}, trainContext='train', baselineJobs=False,
        labels={'first': 'first', 'waiting': {'ci': {'name': 'waiting: CI'}}},
        watch={'kick': 'watchdog', 'escalated': f'{tmp}/escalated.json'},
    )))
    os.environ['AUTOLAND_CONFIG'] = str(cfg)
    import common as C
    import steward as S

    repos = [{'repo': 'sam/d', 'default': 'main'}]
    info = {'defaultBranchRef': {'name': 'main', 'target': {'oid': 'base'}}}
    FIRST = range(1, 5)

    def pr(n):
        labels = [{'name': 'first'}] if n in FIRST else []
        return dict(number=n, title=str(n), headRefOid=f'head-{n}', headRefName=f'branch-{n}',
                    baseRefName='main', labels={'nodes': labels}, url=f'https://github.com/sam/d/pull/{n}')

    prs = [pr(n) for n in range(1, 13)]
    clock, handled = [0], []

    def handle(repo, pr, *args):
        handled.append(pr['number'])
        clock[0] += 200  # a slow look: a slice has room for three

    with ExitStack() as stack:
        for obj, name, value in (
            (S.R, 'discover', lambda: repos), (S.R, 'restart', lambda *a: None),
            (S.R, 'settle_repo', lambda *a: False), (S.R, 'watch_default', lambda *a: {}),
            (S.R, 'publish', lambda *a: None), (S.R, 'hooks', lambda *a: None),
            (S, 'open_prs', lambda owner, name: (info, copy.deepcopy(prs))),
            (S.P, 'begin', lambda *a: None), (S.P, 'place', lambda repo, n: (n not in FIRST, n)),
            (S.T, 'begin', lambda repo: None), (S.T, 'on', lambda *a: False),
            (S.step, 'handle', handle),
            (S, 'branches', lambda *a, **k: None),
            (S, 'upkeep', lambda *a: None), (S.V, 'sweep', lambda *a: None),
            (C, 'follow_renames', lambda names: None), (C, 'taken', lambda *a: None),
            (C, 'poke', lambda *a: None),
        ):
            stack.enter_context(patch.object(obj, name, value))
        stack.enter_context(patch('time.monotonic', side_effect=lambda: clock[0]))
        C.NOW = 1000
        # A webhook about the repo between every slice: each pass is a
        # new kick, never a continuation, as on a busy repo.
        for turn in range(4):
            C.WAKE.clear()
            S.run_pass(['sam/d'])
            C.NOW += 60
        assert len(handled) == 12, handled
        assert sorted(handled) == list(range(1, 13)), ('a PR went without its look', handled)
        assert handled[:4] == list(FIRST), ('the first-labelled go first in a round', handled)
        # The round over, the next one starts with the first-labelled again.
        S.run_pass(['sam/d'])
        assert handled[12:] == [1, 2, 3], handled
print('rounds of looks: passed')

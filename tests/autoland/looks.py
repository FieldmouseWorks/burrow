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

    info = {'defaultBranchRef': {'name': 'main', 'target': {'oid': 'base'}}}
    clock = [0]

    def scenario(repo, count, first, costs, kinds, held=()):
        """The looks each pass of `kinds` (True a kicked pass, False a
        continuation) gives `count` PRs of `repo`, each look taking the
        next of `costs` seconds, `held` PRs held by a red-branch fix. A
        webhook comes before every kicked pass, as on a busy repo."""
        repos = [{'repo': repo, 'default': 'main'}]

        def pr(n):
            labels = [{'name': 'first'}] if n in first else []
            return dict(number=n, title=str(n), headRefOid=f'head-{n}', headRefName=f'branch-{n}',
                        baseRefName='main', labels={'nodes': labels}, url=f'https://github.com/{repo}/pull/{n}')

        prs = [pr(n) for n in range(1, count + 1)]
        passes, cost = [], iter(costs * 100)

        def handle(repo, pr, *args):
            passes[-1].append(pr['number'])
            clock[0] += next(cost)

        with ExitStack() as stack:
            for obj, name, value in (
                (S.R, 'discover', lambda: repos), (S.R, 'restart', lambda *a: None),
                (S.R, 'settle_repo', lambda *a: False),
                (S.R, 'watch_default', lambda *a: dict.fromkeys(held, {'fix': 1})),
                (S.R, 'publish', lambda *a: None), (S.R, 'hooks', lambda *a: None),
                (S, 'open_prs', lambda owner, name: (info, copy.deepcopy(prs))),
                (S.P, 'begin', lambda *a: None), (S.P, 'place', lambda repo, n: (n not in first, n)),
                (S.T, 'begin', lambda repo: None), (S.T, 'on', lambda *a: False),
                (S.step, 'handle', handle),
                (S, 'branches', lambda *a, **k: None),
                (S, 'upkeep', lambda *a: None), (S.V, 'sweep', lambda *a: None),
                (C, 'follow_renames', lambda names: None), (C, 'taken', lambda *a: None),
                (C, 'poke', lambda *a: None),
            ):
                stack.enter_context(patch.object(obj, name, value))
            stack.enter_context(patch('time.monotonic', side_effect=lambda: clock[0]))
            for kicked in kinds:
                C.WAKE.clear()
                passes.append([])
                S.run_pass([repo], continuing=not kicked)
                # A completed traversal clears the cursor: a round of its own.
                if repo.lower() not in C.load('pass-progress.json', {}).get('repos', {}):
                    passes[-1].append(None)
                C.NOW += 60
        return passes

    def whole(passes, count, skip=()):
        """The looks cut into rounds, one ending when a PR comes round again
        or a traversal completes: every round but the last must be whole."""
        done = [[]]
        for n in (n for p in passes for n in p if n not in skip):
            if n is None or n in done[-1]:
                done.append([])
            if n is not None:
                done[-1].append(n)
        done = [r for r in done if r]
        for r in done[:-1]:
            assert sorted(r) == sorted(set(range(1, count + 1)) - set(skip)), ('a PR went without its look', passes)
        return done

    C.NOW = 1000
    # 4 first-labelled among 12, three slow looks per slice.
    looks = scenario('sam/d', 12, range(1, 5), [200], [True] * 5)
    assert looks[:4] == [[1, 2, 3], [4, 5, 6], [7, 8, 9], [10, 11, 12]], ('a PR went without its look', looks)
    # The round over, the next one starts with the first-labelled again.
    assert looks[4] == [1, 2, 3], looks

    # A round ending partway through a slice: the look after it, there, is
    # the next round's, and doesn't make its PR due a third time.
    looks = scenario('sam/e', 11, {1}, [200], [True] * 9)
    assert looks == [[1, 2, 3], [4, 5, 6], [7, 8, 9], [10, 11, 1], [2, 3, 4], [5, 6, 7], [8, 9, 10],
                     [11, 1, 2], [3, 4, 5]], looks
    whole(looks, 11)

    # Slices of every length, kicked passes and continuations mixed: every
    # round whole, and each led by the first-labelled.
    kinds = [True, False, False, True, False, True, True, False, True, False, False, True] * 3
    looks = scenario('sam/f', 13, {1, 2, 3}, [50, 350, 120, 500, 30, 200, 90], kinds)
    done = whole(looks, 13)
    assert len(done) > 3, looks
    for r in done[1:-1]:
        assert r[:3] == [1, 2, 3], ('the first-labelled go first in a round', looks)

    # A PR a red-branch fix holds goes first in every pass, round or not;
    # its extra looks don't take the others' turn.
    looks = scenario('sam/g', 11, {1}, [200], [True] * 9, held={7})
    assert all(p[0] == 7 for p in looks), looks
    whole(looks, 11, skip={7})
print('rounds of looks: passed')

"""A PR fixture as GitHub would answer the steward's query: only the fields
PRS selects, by path and by each node's type. The CI owner reads the PR
through this in the harness, so a field a scenario sets but the query no
longer fetches is gone there too, as it would be in production, and the
scenario that needs it fails instead of passing on a field never asked for."""

import re

from prs import PRS


def _tokens(query):
    query = re.sub(r"\([^()]*\)", "", query)  # arguments select nothing
    return re.findall(r"\.\.\.|[{}]|[_A-Za-z][_0-9A-Za-z]*", query)


def _merge(a, b):
    if a is None or b is None:
        return a or b
    fields, on = dict(a["fields"]), dict(a["on"])
    for f, s in b["fields"].items():
        fields[f] = _merge(fields.get(f), s)
    for t, s in b["on"].items():
        on[t] = _merge(on.get(t), s)
    return {"fields": fields, "on": on}


def _block(toks, i):
    """The selection opening at toks[i] ('{'), and the index after it: its
    fields (each to its own selection, None for a scalar) and its inline
    fragments (each type to its selection)."""
    assert toks[i] == "{", toks[i:i + 5]
    sel, i = {"fields": {}, "on": {}}, i + 1
    while toks[i] != "}":
        if toks[i] == "...":
            assert toks[i + 1] == "on", toks[i:i + 5]
            kind = toks[i + 2]
            sub, i = _block(toks, i + 3)
            sel["on"][kind] = _merge(sel["on"].get(kind), sub)
            continue
        name, i, sub = toks[i], i + 1, None
        if toks[i] == "{":
            sub, i = _block(toks, i)
        sel["fields"][name] = _merge(sel["fields"].get(name), sub)
    return sel, i + 1


def _find(sel, *path):
    for name in path:
        sel = sel["fields"][name]
    return sel


_toks = _tokens(PRS)
# Each node of the repository's pullRequests connection.
PR = _find(_block(_toks, _toks.index("{"))[0], "repository", "pullRequests", "nodes")


def shape(value, sel=PR):
    """value with only what sel asks for: a node's fields, plus its own
    type's fragment (every fragment's, for a fixture that names no type)."""
    if sel is None or value is None:
        return value
    if isinstance(value, list):
        return [shape(v, sel) for v in value]
    assert isinstance(value, dict), value
    kind = value.get("__typename")
    fields = sel["on"].get(kind) if kind else None
    for frag in ([fields] if kind else sel["on"].values()):
        sel = _merge(sel, frag)
    return {k: shape(v, sel["fields"][k]) for k, v in value.items() if k in sel["fields"]}

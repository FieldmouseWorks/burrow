"""Unbuilt non-VM prebuild inputs for proof derivations.

Descends through any derivation whose unbuilt closure includes a `kvm` or
`nixos-test` derivation (such as a `linkFarm` of VM tests) so prebuild never
runs VM tests outside proofSlots."""

import json
import os

VM_FEATURES = frozenset(("kvm", "nixos-test"))  # requiredSystemFeatures that mark a VM derivation
SHOW_BATCH = 500  # derivations per `nix derivation show` call (stays well under ARG_MAX)


def _feature_words(value):
    if isinstance(value, str):
        return value.split()
    if isinstance(value, list):
        return [v for v in value if isinstance(v, str)]
    return []


def _is_vm_drv(entry):
    env = entry.get("env")
    if not isinstance(env, dict):
        raise RuntimeError("nix derivation show: entry has no env")
    features = set(_feature_words(env.get("requiredSystemFeatures")))
    sa = entry.get("structuredAttrs")
    if isinstance(sa, dict):
        features.update(_feature_words(sa.get("requiredSystemFeatures")))
    if raw := env.get("__json"):
        try:
            parsed = json.loads(raw) or {}
        except ValueError as e:
            raise RuntimeError(f"nix derivation show: bad __json: {e}") from None
        if not isinstance(parsed, dict):
            raise RuntimeError("nix derivation show: __json is not an object")
        features.update(_feature_words(parsed.get("requiredSystemFeatures")))
    return bool(features & VM_FEATURES)


def _drv_deps(entry, store_dir):
    raw = entry.get("inputDrvs")
    if raw is None:
        raw = (entry.get("inputs") or {}).get("drvs")
    if not isinstance(raw, dict):
        raise RuntimeError("nix derivation show: entry has no inputDrvs")
    return [k if "/" in k else os.path.join(store_dir, k) for k in raw]


def _pending_drvs(drvs, run_nix):
    """Unbuilt .drv paths in drvs' closure, from a local dry-run (no substituter)."""
    dry = run_nix("nix-store", "--realise", "--dry-run", "--option", "substitute", "false", *drvs, stderr=True)
    pending, building_section = set(), False
    for line in dry.splitlines():
        s = line.strip()
        if "will be built:" in s:
            building_section = True
        elif "will be fetched" in s or "don't know how to build" in s:
            building_section = False
        elif building_section and s.startswith("/") and s.endswith(".drv"):
            pending.add(s)
    return pending


def inputs(drvs, run_nix):
    """The unbuilt non-VM derivations drvs are built from, and every output
    of them: ([drv, ...], [path, ...]). Descends through any derivation
    whose unbuilt closure includes a kvm/nixos-test derivation (such as a
    linkFarm of VM tests) so prebuild never runs VM tests outside proofSlots.
    Raises when the store can't say."""
    if not drvs:
        return [], []
    pending = _pending_drvs(drvs, run_nix)
    if not pending:
        return [], []
    store_dir = os.path.dirname(drvs[0])
    pending_list = sorted(pending)
    shown = {}
    for i in range(0, len(pending_list), SHOW_BATCH):
        raw = run_nix("nix", "--extra-experimental-features", "nix-command", "derivation", "show",
                      *pending_list[i:i + SHOW_BATCH])
        try:
            body = json.loads(raw)
        except ValueError as e:
            raise RuntimeError(f"nix derivation show: {e}") from None
        if not isinstance(body, dict):
            raise RuntimeError("nix derivation show: expected JSON object")
        entries = body["derivations"] if isinstance(body.get("derivations"), dict) else body
        for k, v in entries.items():
            shown[k if "/" in k else os.path.join(store_dir, k)] = v
    vm_self, deps = {}, {}
    for d in pending_list:
        entry = shown.get(d)
        if not isinstance(entry, dict):
            raise RuntimeError(f"nix derivation show: missing {d}")
        vm_self[d] = _is_vm_drv(entry)
        deps[d] = [c for c in _drv_deps(entry, store_dir) if c in pending]
    has_vm = {}
    for start in pending_list:
        if start in has_vm:
            continue
        stack = [(start, False)]
        while stack:
            node, done = stack.pop()
            if node in has_vm:
                continue
            if done:
                has_vm[node] = vm_self[node] or any(has_vm.get(c, False) for c in deps[node])
            else:
                stack.append((node, True))
                for c in deps[node]:
                    if c not in has_vm:
                        stack.append((c, False))
    roots = set(drvs)
    ins, seen, queue = set(), set(), [d for d in drvs if d in pending]
    while queue:
        node = queue.pop()
        if node in seen:
            continue
        seen.add(node)
        for c in deps[node]:
            if c in roots or has_vm[c]:
                queue.append(c)
            else:
                ins.add(c)
    ins = sorted(ins)
    return ins, sorted(set(run_nix("nix-store", "--query", "--outputs", *ins).split())) if ins else []

"""Tool handlers for the triad plugin. Contract: (args: dict, **kwargs) -> JSON str,
never raise, always return a JSON string (errors included)."""

from __future__ import annotations

import json
import os

from . import cairn, strix


def _client() -> cairn.Cairn:
    return cairn.Cairn(os.environ.get("CAIRN_BASE_URL", cairn.DEFAULT_BASE))


def _workdir(args: dict) -> str:
    return os.path.expanduser(args.get("workdir") or os.environ.get("TRIAD_WORKDIR")
                              or strix.DEFAULT_WORKDIR)


def _ok(**kw) -> str:
    return json.dumps({"ok": True, **kw})


def _err(msg) -> str:
    return json.dumps({"ok": False, "error": str(msg)})


def cairn_list(args: dict, **kwargs) -> str:
    try:
        return _ok(projects=_client().list_projects())
    except Exception as e:
        return _err(e)


def cairn_create(args: dict, **kwargs) -> str:
    try:
        res = _client().create_project(args["title"], args["origin"], args["goal"],
                                       hints=args.get("hints"),
                                       bootstrap_enabled=args.get("bootstrap", True))
        return _ok(project=res.get("project"), facts=res.get("facts"))
    except Exception as e:
        return _err(e)


def cairn_graph(args: dict, **kwargs) -> str:
    try:
        c = _client()
        fmt = args.get("format", "summary")
        if fmt in ("yaml", "timeline"):
            return _ok(format=fmt, content=c.export(args["project_id"], fmt))
        g = c.get_project(args["project_id"])
        if fmt == "path":
            return _ok(status=g.get("project", {}).get("status"),
                       path=c.goal_path(g))
        return _ok(**c.summarize(g))
    except Exception as e:
        return _err(e)


def cairn_hint(args: dict, **kwargs) -> str:
    try:
        return _ok(hint=_client().add_hint(args["project_id"], args["content"],
                                           args.get("creator", "hermes")))
    except Exception as e:
        return _err(e)


def cairn_intent(args: dict, **kwargs) -> str:
    try:
        return _ok(intent=_client().add_intent(args["project_id"], args["from_facts"],
                                               args["description"],
                                               args.get("creator", "hermes"),
                                               args.get("claim", False)))
    except Exception as e:
        return _err(e)


def cairn_complete(args: dict, **kwargs) -> str:
    try:
        return _ok(completion=_client().complete(args["project_id"], args["from_facts"],
                                                 args["description"],
                                                 args.get("worker", "hermes")))
    except Exception as e:
        return _err(e)


def cairn_reopen(args: dict, **kwargs) -> str:
    try:
        return _ok(**(_client().reopen(args["project_id"], args["description"],
                                       args.get("creator", "validator")) or {}))
    except Exception as e:
        return _err(e)


def cairn_status(args: dict, **kwargs) -> str:
    try:
        return _ok(project=_client().set_status(args["project_id"], args["status"]))
    except Exception as e:
        return _err(e)


def strix_scan(args: dict, **kwargs) -> str:
    try:
        wd = _workdir(args)
        res = strix.run_scan(args["target"], wd,
                             instruction_file=args.get("instruction_file"),
                             instruction=args.get("instruction"),
                             scan_mode=args.get("scan_mode", "quick"),
                             max_turns=int(args.get("max_turns", 60)))
        return _ok(**res)
    except Exception as e:
        return _err(e)


def strix_findings(args: dict, **kwargs) -> str:
    try:
        return _ok(**strix.read_run(_workdir(args), args.get("run_name")))
    except Exception as e:
        return _err(e)


def strix_progress(args: dict, **kwargs) -> str:
    try:
        return _ok(**strix.run_progress(_workdir(args), args.get("run_name")))
    except Exception as e:
        return _err(e)


def triad_feed(args: dict, **kwargs) -> str:
    try:
        run = strix.read_run(_workdir(args), args.get("run_name"))
        posted_h, posted_i = strix.post_leads(_client(), args["project_id"], run,
                                              args.get("anchor_fact", "origin"))
        return _ok(run=run.get("run"), findings=len(run.get("findings", [])),
                   coverage_gaps=len(run.get("coverage_gaps", [])),
                   hints_posted=posted_h, intents_posted=posted_i)
    except Exception as e:
        return _err(e)

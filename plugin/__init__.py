"""Triad: one control plane over Strix (discovery), Cairn (exploitation) and
Hermes (orchestration). Registration entry point."""

from __future__ import annotations

import logging

from . import schemas, tools

logger = logging.getLogger(__name__)

_TOOLS = [
    ("cairn_list",    schemas.CAIRN_LIST,    tools.cairn_list),
    ("cairn_create",  schemas.CAIRN_CREATE,  tools.cairn_create),
    ("cairn_graph",   schemas.CAIRN_GRAPH,   tools.cairn_graph),
    ("cairn_hint",    schemas.CAIRN_HINT,    tools.cairn_hint),
    ("cairn_intent",  schemas.CAIRN_INTENT,  tools.cairn_intent),
    ("cairn_complete", schemas.CAIRN_COMPLETE, tools.cairn_complete),
    ("cairn_reopen",  schemas.CAIRN_REOPEN,  tools.cairn_reopen),
    ("cairn_status",  schemas.CAIRN_STATUS,  tools.cairn_status),
    ("strix_scan",    schemas.STRIX_SCAN,    tools.strix_scan),
    ("strix_findings", schemas.STRIX_FINDINGS, tools.strix_findings),
    ("strix_progress", schemas.STRIX_PROGRESS, tools.strix_progress),
    ("triad_feed",    schemas.TRIAD_FEED,    tools.triad_feed),
]

_AUDIT = []


def _on_post_tool_call(tool_name, args, result, task_id=None, **kwargs):
    """Append-only audit trail: every triad tool call, who asked, what came back."""
    if not str(tool_name).startswith(("cairn_", "strix_", "triad_")):
        return
    _AUDIT.append({"tool": tool_name, "args": args, "session": task_id})
    if len(_AUDIT) > 2000:
        del _AUDIT[:1000]
    try:
        logger.info("triad: %s %s", tool_name, json_snippet(args))
    except Exception:
        pass


def json_snippet(obj, n=200):
    import json
    try:
        s = json.dumps(obj, ensure_ascii=False)
    except Exception:
        s = str(obj)
    return s[:n]


def register(ctx):
    for name, schema, handler in _TOOLS:
        ctx.register_tool(name=name, toolset="triad", schema=schema, handler=handler)
    ctx.register_hook("post_tool_call", _on_post_tool_call)
    logger.info("triad: registered %d tools", len(_TOOLS))

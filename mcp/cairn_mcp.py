#!/usr/bin/env python3
"""Cairn MCP bridge.

One integration point, two consumers:
  * Hermes  -> add to ~/.hermes/config.yaml under mcp_servers (see hermes/config-snippets.yaml)
  * Strix   -> add to ~/.strix/mcp-servers.json so the scanning agent can read
               and write the blackboard mid-scan

Exposes the Cairn Fact/Intent graph as MCP tools. Read tools are safe; the
write tools are what "read authority vs write authority" means in practice,
so point Strix at this server with allowed_tools restricting it to the read
set (cairn_get_project, cairn_export, cairn_list_projects).

Run:  uv run --with 'mcp<2' cairn_mcp.py        (stdio transport)
Env:  CAIRN_BASE_URL (default http://127.0.0.1:8000)
"""

from __future__ import annotations

import os
import sys
from pathlib import Path

# reuse the plugin's stdlib-only Cairn client
_PLUGIN = Path(__file__).resolve().parent.parent / "hermes" / "plugin-triad"
sys.path.insert(0, str(_PLUGIN))
from cairn import Cairn, CairnError  # noqa: E402

try:
    from mcp.server.fastmcp import FastMCP
except ImportError:  # mcp >= 2.0 renamed FastMCP -> MCPServer
    try:
        from mcp.server.mcpserver import MCPServer as FastMCP
    except ImportError:
        FastMCP = None

if FastMCP is None:  # pragma: no cover
    sys.stderr.write(
        "This bridge needs the MCP Python SDK.\n"
        "Recommended (v1 API, what this file is written against):\n"
        "  uv run --with 'mcp<2' cairn_mcp.py\n"
        "MCP 2.x renamed FastMCP to MCPServer and changed several signatures.\n"
    )
    raise SystemExit(2)

mcp = FastMCP("cairn")

READ_TOOLS = ["cairn_list_projects", "cairn_get_project", "cairn_export"]


def _c() -> Cairn:
    return Cairn(os.environ.get("CAIRN_BASE_URL", "http://127.0.0.1:8000"))


def _guard(fn):
    try:
        return fn()
    except CairnError as e:
        return {"error": str(e)}


# Read-only tools.
@mcp.tool()
def cairn_list_projects() -> list:
    """List Cairn projects with status and fact/intent counts."""
    return _guard(lambda: _c().list_projects())


@mcp.tool()
def cairn_get_project(project_id: str) -> dict:
    """Full graph for one project: project meta, facts, intents, hints."""
    return _guard(lambda: _c().get_project(project_id))


@mcp.tool()
def cairn_export(project_id: str, format: str = "yaml") -> str:
    """Project export. format='yaml' (graph snapshot) or 'timeline' (event history)."""
    return _guard(lambda: _c().export(project_id, format))


# Graph-changing tools, so a caller can filter them out.
@mcp.tool()
def cairn_create_project(title: str, origin: str, goal: str, hints: list = None,
                         bootstrap_enabled: bool = True) -> dict:
    """Create a project: origin = starting fact, goal = success condition."""
    return _guard(lambda: _c().create_project(title, origin, goal, hints, bootstrap_enabled))


@mcp.tool()
def cairn_add_hint(project_id: str, content: str, creator: str = "mcp") -> dict:
    """Add out-of-graph guidance (works even when the project is stopped/completed)."""
    return _guard(lambda: _c().add_hint(project_id, content, creator))


@mcp.tool()
def cairn_add_intent(project_id: str, from_facts: list, description: str,
                     creator: str = "mcp") -> dict:
    """Declare an exploration direction from known fact ids."""
    return _guard(lambda: _c().add_intent(project_id, from_facts, description, creator))


@mcp.tool()
def cairn_conclude_intent(project_id: str, intent_id: str, fact_description: str,
                          worker: str = "mcp") -> dict:
    """Close an intent and write its finding as a new fact."""
    return _guard(lambda: _c().conclude_intent(project_id, intent_id, fact_description, worker))


@mcp.tool()
def cairn_complete(project_id: str, from_facts: list, description: str,
                   worker: str = "mcp") -> dict:
    """Declare the goal reached from specific facts."""
    return _guard(lambda: _c().complete(project_id, from_facts, description, worker))


@mcp.tool()
def cairn_reopen(project_id: str, description: str, creator: str = "mcp") -> dict:
    """Undo a completion that external validation rejected."""
    return _guard(lambda: _c().reopen(project_id, description, creator))


@mcp.tool()
def cairn_set_status(project_id: str, status: str) -> dict:
    """Hard stop ('stopped') or resume ('active') a project. The kill switch."""
    return _guard(lambda: _c().set_status(project_id, status))


@mcp.tool()
def cairn_goal_path(project_id: str) -> list:
    """Ordered origin -> goal fact chain of a completed project (the narrative)."""
    return _guard(lambda: _c().goal_path(_c().get_project(project_id)))


if __name__ == "__main__":
    mcp.run()

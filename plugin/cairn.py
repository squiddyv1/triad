"""Minimal Cairn REST client (stdlib only).

Targets the oritera/Cairn blackboard API (Fact / Intent / Hint graph).
Verified against cairn v0.2.1 (`cairn serve`, port 8000).

API shapes as observed on a live server:
  POST /projects                    -> {project, facts, intents, hints}
  GET  /projects                    -> [ {id,title,status,fact_count,...}, ... ]
  GET  /projects/{id}               -> {project, facts, intents, hints}
  POST /projects/{id}/hints         -> hint
  POST /projects/{id}/intents       -> intent      (worker must be null or == creator)
  POST /projects/{id}/intents/{iid}/conclude -> {fact, intent}
  POST /projects/{id}/intents/{iid}/heartbeat|release -> intent
  POST /projects/{id}/reason/claim|heartbeat|release
  POST /projects/{id}/complete      -> the completion intent (to == "goal")
  POST /projects/{id}/reopen        -> {project, fact, intent}
  PUT  /projects/{id}/status        -> project     ("stopped" is the hard kill switch)
  PUT  /projects/{id}/title         -> project
  GET  /projects/{id}/export?format=yaml|timeline
  DELETE /projects/{id}             -> 204
  GET  /settings, PUT /settings     -> {intent_timeout, reason_timeout}
"""

from __future__ import annotations

import json
import urllib.error
import urllib.request

DEFAULT_BASE = "http://127.0.0.1:8000"


class CairnError(RuntimeError):
    """Raised for any Cairn call failure.

    `status` is the HTTP status when the server answered, or None when the
    request never reached it. Callers use that to tell "Cairn is down" apart
    from "Cairn said no"."""

    def __init__(self, message: str, status: int | None = None):
        super().__init__(message)
        self.status = status


class Cairn:
    def __init__(self, base_url: str = DEFAULT_BASE, timeout: int = 30):
        self.base = base_url.rstrip("/")
        self.timeout = timeout

    def _call(self, method: str, path: str, body: dict | None = None):
        data = json.dumps(body).encode() if body is not None else None
        req = urllib.request.Request(
            f"{self.base}{path}", data=data, method=method,
            headers={"Content-Type": "application/json", "Accept": "application/json"},
        )
        try:
            with urllib.request.urlopen(req, timeout=self.timeout) as r:
                raw = r.read().decode()
                return json.loads(raw) if raw.strip() else None
        except urllib.error.HTTPError as e:
            detail = e.read().decode(errors="replace").strip()
            try:
                detail = json.loads(detail).get("detail", detail)
            except (json.JSONDecodeError, AttributeError):
                pass
            raise CairnError(f"{method} {path} -> HTTP {e.code}: {detail}", e.code) from None
        except urllib.error.URLError as e:
            raise CairnError(f"{method} {path} unreachable: {e.reason}") from None

    def _text(self, path: str) -> str:
        req = urllib.request.Request(f"{self.base}{path}", method="GET")
        try:
            with urllib.request.urlopen(req, timeout=self.timeout) as r:
                return r.read().decode(errors="replace")
        except urllib.error.HTTPError as e:
            raise CairnError(f"GET {path} -> HTTP {e.code}") from None

    def health(self) -> list:
        return self._call("GET", "/projects")

    def list_projects(self) -> list:
        return self._call("GET", "/projects") or []

    def create_project(self, title, origin, goal, hints=None, bootstrap_enabled=True) -> dict:
        body = {"title": title, "origin": origin, "goal": goal,
                "bootstrap_enabled": bool(bootstrap_enabled)}
        if hints:
            body["hints"] = [h if isinstance(h, dict) else {"content": str(h), "creator": "hermes"}
                             for h in hints]
        return self._call("POST", "/projects", body)

    def get_project(self, project_id) -> dict:
        return self._call("GET", f"/projects/{project_id}")

    def export(self, project_id, fmt: str = "yaml") -> str:
        return self._text(f"/projects/{project_id}/export?format={fmt}")

    def set_status(self, project_id, status: str) -> dict:
        """status: 'active' | 'stopped'. 'stopped' is the emergency kill switch:
        the server rejects all exploration writes and the dispatcher cancels
        running work for that project."""
        if status not in ("active", "stopped"):
            raise CairnError("status must be 'active' or 'stopped' (completed is set by /complete)")
        return self._call("PUT", f"/projects/{project_id}/status", {"status": status})

    def delete_project(self, project_id):
        return self._call("DELETE", f"/projects/{project_id}")

    def add_hint(self, project_id, content, creator="hermes") -> dict:
        return self._call("POST", f"/projects/{project_id}/hints",
                          {"content": content, "creator": creator})

    def add_intent(self, project_id, from_facts, description, creator="hermes",
                   claim=False) -> dict:
        if isinstance(from_facts, str):
            from_facts = [from_facts]
        return self._call("POST", f"/projects/{project_id}/intents", {
            "from": from_facts, "description": description, "creator": creator,
            "worker": creator if claim else None,
        })

    def conclude_intent(self, project_id, intent_id, fact_description, worker) -> dict:
        return self._call("POST",
                          f"/projects/{project_id}/intents/{intent_id}/conclude",
                          {"worker": worker, "description": fact_description})

    def heartbeat_intent(self, project_id, intent_id, worker) -> dict:
        return self._call("POST",
                          f"/projects/{project_id}/intents/{intent_id}/heartbeat",
                          {"worker": worker})

    def release_intent(self, project_id, intent_id, worker) -> dict:
        return self._call("POST",
                          f"/projects/{project_id}/intents/{intent_id}/release",
                          {"worker": worker})

    def complete(self, project_id, from_facts, description, worker) -> dict:
        """Declare goal reached. Creates an edge from_facts -> goal."""
        if isinstance(from_facts, str):
            from_facts = [from_facts]
        return self._call("POST", f"/projects/{project_id}/complete",
                          {"from": from_facts, "description": description, "worker": worker})

    def reopen(self, project_id, description, creator) -> dict:
        """Undo a completion that external validation rejected."""
        return self._call("POST", f"/projects/{project_id}/reopen",
                          {"description": description, "creator": creator})

    @staticmethod
    def goal_path(graph: dict) -> list:
        """Walk the completed graph backwards from `goal` and return the ordered
        fact chain origin -> ... -> goal. Empty list if not complete."""
        facts = {f["id"]: f.get("description", "") for f in graph.get("facts", [])}
        by_to = {i["to"]: i for i in graph.get("intents", []) if i.get("to")}
        if "goal" not in by_to:
            return []
        chain, node, seen = [], "goal", set()
        while node and node not in seen:
            seen.add(node)
            intent = by_to.get(node)
            chain.append({"fact": node, "description": facts.get(node, ""),
                          "via": (intent or {}).get("description"),
                          "worker": (intent or {}).get("worker")})
            if not intent:
                break  # terminal node with no producing edge (normally `origin`)
            src = intent.get("from") or []
            node = src[0] if src else None
        chain.reverse()
        return chain

    @staticmethod
    def summarize(graph: dict) -> dict:
        p = graph.get("project", {})
        intents = graph.get("intents", [])
        return {
            "id": p.get("id"), "title": p.get("title"), "status": p.get("status"),
            "facts": [f"{f['id']}: {f.get('description','')}" for f in graph.get("facts", [])],
            "open_intents": [f"{i['id']}: {i.get('description','')} (worker={i.get('worker')})"
                             for i in intents if not i.get("to")],
            "done_intents": len([i for i in intents if i.get("to")]),
            "hints": [h.get("content") for h in graph.get("hints", [])],
        }

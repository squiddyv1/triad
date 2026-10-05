"""Tool schemas for the triad plugin (Hermes plugins expect an
{name, description, parameters} dict per tool)."""

CAIRN_LIST = {
    "name": "cairn_list",
    "description": ("List Cairn exploration projects (id, title, status, fact/intent counts). "
                    "Cairn is the exploitation state-space engine; each project is one "
                    "target+objective with a Fact/Intent graph."),
    "parameters": {"type": "object", "properties": {}, "required": []},
}

CAIRN_CREATE = {
    "name": "cairn_create",
    "description": ("Create a Cairn project: a target (origin) plus an objective (goal). "
                    "Cairn workers then explore/exploit toward the goal autonomously. "
                    "Pass the engagement rules of engagement as hints."),
    "parameters": {
        "type": "object",
        "properties": {
            "title": {"type": "string", "description": "Engagement/项目 title"},
            "origin": {"type": "string", "description": "Starting fact, e.g. 'target https://x.example'"},
            "goal": {"type": "string", "description": "Success condition, e.g. 'obtain admin access'"},
            "hints": {"type": "array", "items": {"type": "string"},
                      "description": "Rules of engagement, scope limits, credentials, focus areas"},
            "bootstrap": {"type": "boolean", "default": True,
                          "description": "Allow an initial direct-solve attempt"},
        },
        "required": ["title", "origin", "goal"],
    },
}

CAIRN_GRAPH = {
    "name": "cairn_graph",
    "description": ("Read a Cairn project's live graph: facts, open/concluded intents, hints. "
                    "Use format='path' to get the ordered origin->goal chain of a completed "
                    "project (the attack narrative), 'yaml'/'timeline' for the raw export."),
    "parameters": {
        "type": "object",
        "properties": {
            "project_id": {"type": "string"},
            "format": {"type": "string", "enum": ["summary", "path", "yaml", "timeline"],
                       "default": "summary"},
        },
        "required": ["project_id"],
    },
}

CAIRN_HINT = {
    "name": "cairn_hint",
    "description": ("Inject out-of-graph guidance into a Cairn project (works even while the "
                    "project is stopped or completed). Use for situational awareness, dead-end "
                    "notes, or new leads from an external scanner."),
    "parameters": {
        "type": "object",
        "properties": {"project_id": {"type": "string"},
                       "content": {"type": "string"},
                       "creator": {"type": "string", "default": "hermes"}},
        "required": ["project_id", "content"],
    },
}

CAIRN_INTENT = {
    "name": "cairn_intent",
    "description": ("Declare an exploration direction in a Cairn project from one or more known "
                    "fact ids. Leave claim=false to let Cairn workers pick it up."),
    "parameters": {
        "type": "object",
        "properties": {"project_id": {"type": "string"},
                       "from_facts": {"type": "array", "items": {"type": "string"}},
                       "description": {"type": "string"},
                       "creator": {"type": "string", "default": "hermes"},
                       "claim": {"type": "boolean", "default": False}},
        "required": ["project_id", "from_facts", "description"],
    },
}

CAIRN_COMPLETE = {
    "name": "cairn_complete",
    "description": ("Declare a Cairn goal reached from specific fact ids. Creates the fact->goal "
                    "edge and stops exploration."),
    "parameters": {
        "type": "object",
        "properties": {"project_id": {"type": "string"},
                       "from_facts": {"type": "array", "items": {"type": "string"}},
                       "description": {"type": "string"},
                       "worker": {"type": "string", "default": "hermes"}},
        "required": ["project_id", "from_facts", "description"],
    },
}

CAIRN_REOPEN = {
    "name": "cairn_reopen",
    "description": ("Undo a Cairn completion after external validation rejected it (e.g. a bogus "
                    "proof). Records the correction as a new fact and resumes exploration."),
    "parameters": {
        "type": "object",
        "properties": {"project_id": {"type": "string"},
                       "description": {"type": "string"},
                       "creator": {"type": "string", "default": "validator"}},
        "required": ["project_id", "description"],
    },
}

CAIRN_STATUS = {
    "name": "cairn_status",
    "description": ("Hard stop/resume a Cairn project. status='stopped' immediately voids all "
                    "intent claims and makes the dispatcher cancel running work -- this is the "
                    "emergency kill switch. Writes are rejected until set back to 'active'."),
    "parameters": {
        "type": "object",
        "properties": {"project_id": {"type": "string"},
                       "status": {"type": "string", "enum": ["active", "stopped"]}},
        "required": ["project_id", "status"],
    },
}

STRIX_SCAN = {
    "name": "strix_scan",
    "description": ("Launch a headless Strix discovery scan against one target (URL, IP, repo, "
                    "dir, or API spec). Non-blocking: returns the run dir; read it later with "
                    "strix_findings. Requires Docker and a running scan is tens of minutes."),
    "parameters": {
        "type": "object",
        "properties": {
            "target": {"type": "string"},
            "instruction_file": {"type": "string",
                                 "description": "Path to the rules-of-engagement file"},
            "scan_mode": {"type": "string", "enum": ["quick", "standard", "deep"],
                          "default": "quick"},
            "max_turns": {"type": "integer", "default": 60,
                          "description": "Hard guardrail -- the real budget control"},
            "workdir": {"type": "string",
                        "description": "Engagement directory (strix_runs/ is created inside it)"},
            "instruction": {"type": "string", "description": "Inline instruction (short cases only)"},
        },
        "required": ["target"],
    },
}

STRIX_FINDINGS = {
    "name": "strix_findings",
    "description": ("Read a Strix run's results: validated findings, coverage gaps (NOT "
                    "vulnerabilities -- treat as unexamined), status and LLM cost."),
    "parameters": {
        "type": "object",
        "properties": {"workdir": {"type": "string"},
                       "run_name": {"type": "string",
                                    "description": "Defaults to the most recent run"}},
        "required": ["workdir"],
    },
}

TRIAD_FEED = {
    "name": "triad_feed",
    "description": ("Bridge discovery into exploitation: take a Strix run and post its findings "
                    "into a Cairn project as hints plus exploitation intents. This is the "
                    "Strix -> Cairn handoff."),
    "parameters": {
        "type": "object",
        "properties": {"project_id": {"type": "string"},
                       "workdir": {"type": "string"},
                       "run_name": {"type": "string"},
                       "anchor_fact": {"type": "string", "default": "origin"}},
        "required": ["project_id", "workdir"],
    },
}

"""Revision-checked unfinished engineering drafts, separate from runnable studies."""
import copy
import json
from pathlib import Path
import sqlite3
import uuid

from research_workbench import canonical


def execute(root, operation, payload):
    from research import now, safe_id
    path = Path(root) / "research-drafts.sqlite3"
    with sqlite3.connect(path, timeout=30) as db:
        db.execute("CREATE TABLE IF NOT EXISTS drafts(id TEXT NOT NULL, revision INTEGER NOT NULL, content TEXT NOT NULL, PRIMARY KEY(id,revision))")
        if operation == "draft-list":
            return {"drafts": [json.loads(row[0]) for row in db.execute(
                "SELECT d.content FROM drafts d JOIN (SELECT id,max(revision) revision FROM drafts GROUP BY id) latest USING(id,revision) ORDER BY d.rowid DESC LIMIT 1000")]}
        if operation != "draft-save":
            raise ValueError("Unsupported engineering draft operation")
        allowed = {"id", "expected_revision", "name", "dataset_id", "model_id", "config", "research", "updated_by"}
        if not isinstance(payload, dict) or set(payload) - allowed:
            raise ValueError("Unsupported engineering draft fields")
        if not isinstance(payload.get("name"), str) or not 1 <= len(payload["name"].strip()) <= 120:
            raise ValueError("Draft name must contain 1–120 characters")
        if not all(isinstance(payload.get(key), dict) for key in ("config", "research")) or len(canonical(payload)) > 1024 * 1024:
            raise ValueError("Draft requires bounded engineering and research objects (at most 1 MiB)")
        for field in ("dataset_id", "model_id"):
            if payload.get(field):
                safe_id(payload[field])
        db.execute("BEGIN IMMEDIATE")
        identifier = payload.get("id")
        if identifier:
            safe_id(identifier)
            current = db.execute("SELECT max(revision) FROM drafts WHERE id=?", (identifier,)).fetchone()[0]
            if current is None or type(payload.get("expected_revision")) is not int or payload["expected_revision"] != current:
                raise ValueError("Draft revision changed or is missing; reopen before saving")
            revision = current + 1
        else:
            if db.execute("SELECT count(DISTINCT id) FROM drafts").fetchone()[0] >= 1000:
                raise ValueError("The registry supports at most 1000 distinct engineering drafts; update an existing draft instead")
            identifier, revision = "draft_" + uuid.uuid4().hex, 1
        result = {key: copy.deepcopy(value) for key, value in payload.items() if key != "expected_revision"}
        result.update(id=identifier, revision=revision, draft=True, updated_at=now(), runnable=False)
        db.execute("INSERT INTO drafts VALUES(?,?,?)", (identifier, revision, canonical(result).decode("utf-8")))
        return result

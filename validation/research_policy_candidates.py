"""Automatically seal preselected policy winners as review-only immutable artifacts.

A publication template is a content-addressed reference, not an approval. Runtime
owns independent delegated review and every field handoff. This module never
installs, starts, approves, arms or writes a controller.
"""
import json
from pathlib import Path
import sqlite3

from common import load_json
from research_workbench import digest, load_model

BINDING_FIELDS = ("deployment_profile_hash", "target", "equipment_instance_ids", "operating_envelope")


def publication_template(root, policy, study, model):
    from control_versions import get_version, get_version_model
    from research import make_controller
    identifier = policy["publication_template_version_id"]
    template = get_version(root, identifier)
    creator = policy.get("created_by", {})
    if (type(creator.get("user_id")) is not int or not isinstance(creator.get("identity_realm"), str)
            or template.get("author_user_id") != creator["user_id"]
            or template.get("author_identity_realm") != creator["identity_realm"]):
        raise ValueError("Automatic candidate template requires the same server-owned author identity and authentication realm")
    if any(field not in template for field in BINDING_FIELDS):
        raise ValueError("Publication template lacks exact profile/target/equipment/envelope binding; it cannot be guessed")
    if template.get("method") not in study["research"]["methods"]:
        raise ValueError("Template control method is not in the fixed study")
    baseline = json.loads(get_version_model(root, identifier))
    expected = make_controller(model["model"], template["method"], study["research"]["initial"], study["research"]["bands"])
    if baseline != expected:
        raise ValueError("Policy initial controller must exactly reproduce template dynamics, Ts, nodes, safety and tuning; select matching frozen model and initial parameters")
    return template


def policy_lineage(root, record):
    study = record["study"]
    identifier = study.get("policy_id")
    if not identifier:
        return {}
    path = Path(root) / "research-policy.sqlite3"
    if not path.is_file():
        raise ValueError("Policy run lacks its durable source claim")
    with sqlite3.connect(path, timeout=30) as db:
        claimed = db.execute("SELECT 1 FROM claims WHERE policy_id=? AND run_id=?", (identifier, record["id"])).fetchone()
        row = db.execute("SELECT definition FROM policies WHERE id=?", (identifier,)).fetchone()
    if not claimed or row is None:
        raise ValueError("This run was not accepted by the claimed policy; create a separately reviewed manual study")
    definition = json.loads(row[0])
    if (digest(definition) != study.get("policy_definition_sha256")
            or definition["study_id"] != study.get("policy_origin_study_id")
            or type(study.get("policy_revision")) is not int or study["policy_revision"] < 1):
        raise ValueError("Policy run lineage differs from the immutable accepted policy revision")
    result = {key: study[key] for key in ("policy_id", "policy_revision", "policy_definition_sha256", "policy_origin_study_id")}
    if definition.get("publication_template_version_id"):
        from control_versions import get_version
        baseline = get_version(root, definition["publication_template_version_id"])
        result.update(baseline_version_id=baseline["id"], baseline_model_sha256=baseline["model_sha256"])
    return result


def _seals(root):
    db = sqlite3.connect(Path(root) / "research-policy-seals.sqlite3", timeout=30)
    db.execute("CREATE TABLE IF NOT EXISTS seals(run_id TEXT PRIMARY KEY, policy_id TEXT NOT NULL, result TEXT NOT NULL)")
    return db


def seal_status(root, run_id):
    path = Path(root) / "research-policy-seals.sqlite3"
    if not path.is_file():
        return None
    with sqlite3.connect(path, timeout=30) as db:
        row = db.execute("SELECT result FROM seals WHERE run_id=?", (run_id,)).fetchone()
    return json.loads(row[0]) if row else None


def finalize_run(root, run_id):
    from control_versions import create_version, list_versions
    from research import load_run, now
    root = Path(root)
    record = load_run(root, run_id)
    study = record["study"]
    identifier = study.get("policy_id")
    if not identifier:
        return {"status": "not_policy_run", "run_id": run_id}
    if record["status"] not in ("completed", "failed", "cancelled"):
        return {"status": "pending", "run_id": run_id}
    # A separate sealing DB serializes only artifact writers; pausing new policy
    # work does not wait on a large archive requalification transaction.
    with _seals(root) as db:
        db.execute("BEGIN IMMEDIATE")
        prior = db.execute("SELECT result FROM seals WHERE run_id=?", (run_id,)).fetchone()
        if prior:
            return json.loads(prior[0])
        result = {"status": "run_not_completed", "run_id": run_id, "policy_id": identifier, "completed_at": now(), "authority": "review_only"}
        try:
            lineage = policy_lineage(root, record)
            with sqlite3.connect(root / "research-policy.sqlite3", timeout=30) as policy_db:
                row = policy_db.execute("SELECT definition FROM policies WHERE id=?", (identifier,)).fetchone()
            definition = json.loads(row[0])
            if not definition.get("publication_template_version_id"):
                result.update(status="manual_only_unbound", reason="No predeclared publication template; no target or safety mapping is invented")
            elif definition["action"] != "frozen_model_tuning":
                result.update(status="manual_commissioning_required", reason="Dynamic reidentification cannot automatically produce a field-switch candidate")
            elif record["status"] == "completed":
                model = load_model(root, definition["model_id"])
                origin = load_json(root / "studies" / (definition["study_id"] + ".json"))
                template = publication_template(root, definition, origin, model)
                method = template["method"]
                selected = next(item for item in record["results"]["methods"] if item["method"] == method)
                if selected["recommendation"] != "candidate_for_review":
                    result.update(status="retain_baseline", reason="Frozen development winner or final qualification does not justify a new candidate")
                else:
                    creator = definition["created_by"]
                    payload = {"run_id": run_id, "method": method, **{key: template[key] for key in BINDING_FIELDS},
                               "author_user_id": creator["user_id"], "author_identity_realm": creator["identity_realm"]}
                    # Recover a crash after immutable publication but before the
                    # sealing transaction committed; never create another ID.
                    existing = [version for version in list_versions(root) if version["run_id"] == run_id and version["method"] == method
                                and all(version.get(key) == value for key, value in lineage.items())
                                and all(version.get(key) == payload[key] for key in (*BINDING_FIELDS, "author_user_id", "author_identity_realm"))]
                    if len(existing) > 1:
                        raise ValueError("Multiple policy candidates already exist; explicit review required rather than choosing by held-out score")
                    version = existing[0] if existing else create_version(root, payload, "bounded_policy_candidate_sealing")
                    result.update(status="sealed", candidate_id=version["id"], method=method, baseline_version_id=template["id"],
                                  reason="Previously frozen development winner passed full archived qualification; Runtime approval is separate")
        except (ValueError, KeyError, OSError, StopIteration) as error:
            result.update(status="failed_qualification", reason=str(error)[:2000])
        db.execute("INSERT INTO seals VALUES(?,?,?)", (run_id, identifier, json.dumps(result)))
        return result


def reconcile_seals(root):
    """One persisted terminal job per tick; restart recovery never reruns research."""
    from research import load_run
    path = Path(root) / "research-policy.sqlite3"
    if not path.is_file():
        return {"status": "idle"}
    with sqlite3.connect(path, timeout=30) as db:
        claims = db.execute("SELECT run_id FROM claims ORDER BY created_at").fetchall()
    with _seals(root) as db:
        finished = {row[0] for row in db.execute("SELECT run_id FROM seals")}
    for (run_id,) in claims:
        if run_id in finished:
            continue
        try:
            record = load_run(root, run_id)
        except FileNotFoundError:
            continue  # Reserved-but-unknown launch is never replayed or sealed.
        if record["status"] in ("completed", "failed", "cancelled"):
            return finalize_run(root, run_id)
    return {"status": "idle"}


def publisher_policies(root):
    path = Path(root) / "research-policy.sqlite3"
    if not path.is_file():
        return []
    with sqlite3.connect(path, timeout=30) as db:
        rows = db.execute("SELECT definition,state FROM policies ORDER BY rowid DESC").fetchall()
    result = []
    for definition_json, state_json in rows:
        definition, state = json.loads(definition_json), json.loads(state_json)
        row = {key: definition.get(key) for key in ("id", "study_id", "publication_template_version_id", "name", "created_by", "action", "candidate_mode")}
        row.update(revision=state["revision"], enabled=state["enabled"], definition_sha256=digest(definition))
        result.append(row)
    return result


def publisher_candidates(root, policy_id, study_id):
    from control_versions import list_versions
    from research import safe_id
    safe_id(policy_id)
    safe_id(study_id)
    return [version["id"] for version in list_versions(root)
            if version.get("policy_id") == policy_id and version.get("policy_origin_study_id") == study_id
            and version.get("baseline_version_id") and version.get("author_identity_realm")]

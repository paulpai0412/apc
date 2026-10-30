"""Publication reference/identity boundaries; fixtures are not native run proof."""
import copy
import hashlib
import json
from pathlib import Path
import tempfile
import unittest

from campaign import controller_template
from control_versions import _canonical
from research import atomic_json, defaults, make_controller
from research_policy import database
from research_policy_candidates import publication_template, policy_lineage, publisher_policies, finalize_run
from research_workbench import digest


class PolicyCandidateBoundaryTests(unittest.TestCase):
    def setUp(self):
        self.temporary = tempfile.TemporaryDirectory()
        self.addCleanup(self.temporary.cleanup)
        self.root = Path(self.temporary.name)
        settings = defaults()
        self.study = {"id": "origin-study", "config": settings["campaign"], "research": settings["research"]}
        self.model = {"model": controller_template(self.study["config"])}
        self.creator = {"user_id": 7, "username": "research-author", "identity_realm": "c" * 64}
        controller = make_controller(self.model["model"], "standard_dmc", self.study["research"]["initial"], self.study["research"]["bands"])
        encoded = _canonical(controller)
        manifest = {"schema_version": 1, "authority": "review_only", "created_at": "2026-01-01T00:00:00Z",
                    "method": "standard_dmc", "model_sha256": hashlib.sha256(encoded).hexdigest(),
                    "author_user_id": self.creator["user_id"], "author_identity_realm": self.creator["identity_realm"],
                    "deployment_profile_hash": "d" * 64, "target": {"plant": "lab", "line": "1", "area": "cement", "group": "mill"},
                    "equipment_instance_ids": ["isolated-mill"], "operating_envelope": "local-only"}
        manifest["id"] = hashlib.sha256(_canonical(manifest)).hexdigest()
        target = self.root / "versions" / manifest["id"]
        target.mkdir(parents=True)
        (target / "model.json").write_bytes(encoded)
        (target / "version.json").write_bytes(_canonical(manifest))
        self.template = manifest
        self.policy = {"id": "policy-reference", "study_id": self.study["id"], "created_by": self.creator,
                       "action": "frozen_model_tuning", "publication_template_version_id": manifest["id"]}

    def test_template_is_reference_only_and_rejects_cross_realm_or_changed_dynamics(self):
        template = publication_template(self.root, self.policy, self.study, self.model)
        self.assertEqual(template["authority"], "review_only")
        cross_realm = dict(self.policy, created_by=dict(self.creator, identity_realm="e" * 64))
        with self.assertRaisesRegex(ValueError, "identity"):
            publication_template(self.root, cross_realm, self.study, self.model)
        changed = copy.deepcopy(self.model)
        changed["model"]["tuning"]["sample_time"] *= 2
        with self.assertRaisesRegex(ValueError, "exactly reproduce"):
            publication_template(self.root, self.policy, self.study, changed)
        changed = copy.deepcopy(self.model)
        changed["model"]["variables"]["mvs"][0]["node_ids"]["sp"] = "different-physical-actuator"
        with self.assertRaisesRegex(ValueError, "exactly reproduce"):
            publication_template(self.root, self.policy, self.study, changed)

    def test_policy_lineage_requires_durable_claim_and_exact_definition_hash(self):
        state = {"revision": 3, "enabled": True}
        with database(self.root) as db:
            db.execute("INSERT INTO policies VALUES(?,?,?)", (self.policy["id"], json.dumps(self.policy), json.dumps(state)))
        study = dict(self.study, policy_id=self.policy["id"], policy_revision=3, policy_origin_study_id=self.study["id"],
                     policy_definition_sha256=digest(self.policy))
        record = {"id": "accepted-run", "study": study}
        with self.assertRaisesRegex(ValueError, "not accepted"):
            policy_lineage(self.root, record)
        with database(self.root) as db:
            db.execute("INSERT INTO claims VALUES(?,?,?,?)", ("claim", self.policy["id"], record["id"], 1))
        lineage = policy_lineage(self.root, record)
        self.assertEqual(lineage["baseline_version_id"], self.template["id"])
        self.assertEqual(lineage["policy_definition_sha256"], publisher_policies(self.root)[0]["definition_sha256"])
        altered = copy.deepcopy(record)
        altered["study"]["policy_definition_sha256"] = "f" * 64
        with self.assertRaisesRegex(ValueError, "lineage"):
            policy_lineage(self.root, altered)
        # A failed run is persisted as non-candidate. Reconciliation is idempotent
        # and cannot fabricate an approval or change the only template artifact.
        record.update(status="failed")
        atomic_json(self.root / "runs" / record["id"] / "run.json", record)
        result = finalize_run(self.root, record["id"])
        self.assertEqual(result["status"], "run_not_completed")
        self.assertNotIn("candidate_id", result)
        self.assertEqual(finalize_run(self.root, record["id"]), result)
        self.assertEqual(len(list((self.root / "versions").iterdir())), 1)

    def test_readonly_policy_listing_does_not_create_a_registry(self):
        other = self.root / "unconfigured"
        other.mkdir()
        self.assertEqual(publisher_policies(other), [])
        self.assertEqual(list(other.iterdir()), [])

    def test_completed_claim_seals_actual_archive_and_reconciliation_is_idempotent(self):
        # Reuse the complete archived trajectory/receipt boundary fixture. This
        # checks the real version qualifier/writer, not a mocked qualification,
        # and does not assert that a native process ran inside this unit test.
        from test_control_versions import ControlVersionTests
        from common import fingerprint, load_json
        from control_versions import get_version
        fixture = ControlVersionTests(methodName="runTest")
        fixture.setUp()
        self.addCleanup(fixture.doCleanups)
        root = fixture.root
        directory = root / "runs" / "run-1"
        record = load_json(directory / "run.json")
        origin = copy.deepcopy(record["study"])
        model = load_json(directory / "baseline-controller.json")
        plant = load_json(directory / "plant.json")
        artifact = {"id": "model_policy", "model": model, "model_sha256": digest(model),
                    "evaluation_plant": plant, "evaluation_plant_sha256": digest(plant)}
        artifact["artifact_sha256"] = digest(artifact)
        atomic_json(root / "models" / (artifact["id"] + ".json"), artifact)
        atomic_json(root / "studies" / (origin["id"] + ".json"), origin)
        baseline = make_controller(model, "standard_dmc", origin["research"]["initial"], origin["research"]["bands"])
        encoded = _canonical(baseline)
        template = {key: value for key, value in self.template.items() if key != "id"}
        template.update(model_sha256=hashlib.sha256(encoded).hexdigest(), run_id="earlier-baseline", study_id="earlier-study")
        template["id"] = hashlib.sha256(_canonical(template)).hexdigest()
        version_dir = root / "versions" / template["id"]
        version_dir.mkdir(parents=True)
        (version_dir / "model.json").write_bytes(encoded)
        (version_dir / "version.json").write_bytes(_canonical(template))
        policy = dict(self.policy, study_id=origin["id"], model_id=artifact["id"], publication_template_version_id=template["id"])
        record["study"].update(model_id=artifact["id"], policy_id=policy["id"], policy_revision=1,
                               policy_origin_study_id=origin["id"], policy_definition_sha256=digest(policy))
        record["study"]["research"].update(policy_action="frozen_model_tuning", selection_mode="automatic")
        atomic_json(directory / "study.json", record["study"])
        atomic_json(directory / "frozen-model.json", artifact)
        record["provenance"].update(study=fingerprint(directory / "study.json"),
                                    frozen_model=fingerprint(directory / "frozen-model.json"))
        atomic_json(directory / "run.json", record)
        frozen_run_sha = fingerprint(directory / "run.json")
        with database(root) as db:
            db.execute("INSERT INTO policies VALUES(?,?,?)", (policy["id"], json.dumps(policy), json.dumps({"revision": 1, "enabled": True})))
            db.execute("INSERT INTO claims VALUES(?,?,?,?)", ("accepted-archive", policy["id"], record["id"], 1))
        result = finalize_run(root, record["id"])
        self.assertEqual(result["status"], "sealed", result)
        candidate = get_version(root, result["candidate_id"])
        self.assertEqual(candidate["baseline_version_id"], template["id"])
        self.assertEqual(candidate["policy_definition_sha256"], digest(policy))
        self.assertEqual(candidate["author_identity_realm"], self.creator["identity_realm"])
        self.assertEqual(candidate["authority"], "review_only")
        self.assertEqual(finalize_run(root, record["id"]), result)
        self.assertEqual(fingerprint(directory / "run.json"), frozen_run_sha)
        self.assertEqual(len(list((root / "versions").iterdir())), 2)


if __name__ == "__main__":
    unittest.main()

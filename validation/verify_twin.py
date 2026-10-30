"""Full-scenario batch/session equivalence using real native MPC and optional COM DA.

Consumes an existing frozen research run, creates new-only artifacts, and never
changes its controller, plant or scenario. This is not a shortened smoke test.
Run with --binary, --run-directory and a new --output directory.
"""
import argparse
import hashlib
import json
from pathlib import Path
import subprocess

import numpy as np


FIELDS = ("time_seconds", "cv", "mv", "dv", "targets", "command_mv", "predicted_cv", "quality", "solver_status")


def load(path):
    return json.loads(Path(path).read_text())


def sha(path):
    with Path(path).open("rb") as stream:
        return hashlib.file_digest(stream, "sha256").hexdigest()


def save(path, value):
    with Path(path).open("x") as stream:
        json.dump(value, stream, ensure_ascii=False, allow_nan=False, indent=2)
        stream.write("\n")


def require(condition, message):
    if not condition:
        raise ValueError(message)


def compare(left, right):
    require(left["status"] == right["status"] == "valid", "Both complete outputs must be valid")
    require(left["samples"] == right["samples"] == left["requested_samples"] == right["requested_samples"], "Full scenario was not executed")
    differences = {}
    for field in FIELDS:
        a, b = left["trace"][field], right["trace"][field]
        require(len(a) == len(b) == left["samples"], f"Missing {field} samples")
        if field == "solver_status":
            require(a == b, "Solver status differs")
            differences[field] = 0
        else:
            x, y = np.asarray(a), np.asarray(b)
            require(x.shape == y.shape, f"{field} dimensions differ")
            delta = float(np.max(np.abs(x - y))) if x.size else 0.0
            require(delta == 0.0, f"{field} differs: maximum absolute delta {delta}")
            differences[field] = delta
    require(right["solver"]["calls"] == right["samples"] and right["solver"]["failures"] == 0, "Not one successful MPC solve per scan")
    return differences


class Session:
    def __init__(self, command, stderr_path):
        self.stderr = stderr_path.open("x")
        self.child = subprocess.Popen(command, stdin=subprocess.PIPE, stdout=subprocess.PIPE,
                                      stderr=self.stderr, text=True, bufsize=1)
        assert self.child.stdin is not None and self.child.stdout is not None
        self.input, self.output = self.child.stdin, self.child.stdout
        self.sequence = 0

    def request(self, command, **values):
        self.sequence += 1
        request_id = str(self.sequence)
        self.input.write(json.dumps({"request_id": request_id, "command": command, **values}) + "\n")
        self.input.flush()
        line = self.output.readline()
        require(bool(line), f"Native session exited before response {request_id}")
        response = json.loads(line)
        require(response["request_id"] == request_id, "Response request identity differs")
        return response

    def close(self):
        if self.child.poll() is None:
            self.input.close()
            try:
                self.child.wait(timeout=60)
            except subprocess.TimeoutExpired:
                self.child.kill()
                self.child.wait()
        self.output.close()
        self.stderr.close()


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--binary", required=True)
    parser.add_argument("--run-directory", required=True)
    parser.add_argument("--output", required=True)
    parser.add_argument("--bridge")
    parser.add_argument("--bridge-arg", action="append", default=[])
    args = parser.parse_args()
    source, output = Path(args.run_directory).resolve(), Path(args.output).resolve()
    output.mkdir(parents=True, exist_ok=False)
    run, selection, scenario = load(source / "run.json"), load(source / "selection.json"), load(source / "scenario.json")
    require(run["status"] == "completed", "Requires a completed frozen research run")
    total = len(scenario["time_seconds"])
    evidence = {"source_run": run["id"], "binary_sha256": sha(args.binary), "samples": total,
                "sample_time": scenario["sample_time"], "checks": [], "scope": "software_equivalence_not_factory_validation"}
    source_files = [source / "run.json", source / "selection.json", source / "plant.json", source / "scenario.json"]
    try:
        for method, winner in selection["winners"].items():
            trial = winner["best"]
            controller = source / trial["controller"]
            require(controller.resolve().parent == source, "Trial controller escapes frozen directory")
            source_files.append(controller)
            inputs = {p.name: sha(p) for p in source_files}
            stem = method + "-" + trial["id"]
            common = ["--plant", str(source / "plant.json"), "--controller", str(controller), "--scenario", str(source / "scenario.json")]
            batch_path = output / (stem + "-batch.json")
            batch = subprocess.run([args.binary, "closed-loop", *common, "--output", str(batch_path)], text=True, capture_output=True)
            save(output / (stem + "-batch-process.json"), {"returncode": batch.returncode, "stdout": batch.stdout, "stderr": batch.stderr})
            require(batch.returncode == 0, "Real batch MPC failed: " + batch.stderr)
            baseline = load(batch_path)
            require(baseline["samples"] == total, "Batch scenario was shortened")
            archived = source / (method + "-optimized.json")
            if archived.is_file():
                evidence["checks"].append({"name": stem + "-archived-numerical-equivalence", "maximum_differences": compare(load(archived), baseline)})
            for transport in (["direct", "opc-da"] if args.bridge else ["direct"]):
                result_path = output / (stem + "-session-" + transport + ".json")
                command = [args.binary, "session", *common, "--output", str(result_path), "--transport", transport]
                if transport == "opc-da":
                    command += ["--bridge", args.bridge]
                    for value in args.bridge_arg:
                        command += ["--bridge-arg=" + value]
                session = Session(command, output / (stem + "-" + transport + "-stderr.txt"))
                try:
                    first = session.request("snapshot")
                    require(first["samples"] == 0 and first["status"] == "paused", "Session did not start paused at initial state")
                    require(not first.get("frame"), "Initial state was presented as an executed scan")
                    stale = session.request("advance", expected_samples=1, count=1)
                    require(stale.get("error") and stale["samples"] == 0, "Stale sequence advanced the simulator")
                    for count in (0, 4097):
                        rejected = session.request("advance", expected_samples=0, count=count)
                        require(rejected.get("error") and rejected["samples"] == 0, "Invalid batch count was executed")
                    observed = session.request("advance", expected_samples=0, count=1)
                    require(observed["samples"] == 1, "Single step must execute exactly one scan")
                    paused = session.request("snapshot")
                    require(paused["samples"] == 1 and paused["frame"] == observed["frame"], "Snapshot changed state")
                    duplicate = session.request("advance", expected_samples=0, count=1)
                    require(duplicate.get("error") and duplicate["samples"] == 1, "Duplicate advancement was reapplied")
                    chunks, completed = [7, 97, 1024, 4096], 1
                    index = 0
                    while completed < total:
                        count = min(chunks[min(index, len(chunks) - 1)], total - completed)
                        observed = session.request("advance", expected_samples=completed, count=count)
                        require(not observed.get("error") and observed["samples"] == completed + count, "Session skipped or failed scans: " + str(observed.get("reason", observed.get("error"))))
                        require(len(observed.get("recent_trace", {}).get("time_seconds", [])) <= 256, "Unbounded recent history response")
                        completed += count
                        index += 1
                    require(observed["status"] == "completed", "Complete timeline not marked completed")
                    after = session.request("snapshot")
                    require(after["samples"] == total and after["status"] == "completed", "Completed session state not stable")
                finally:
                    session.close()
                result = load(result_path)
                row = {"name": stem + "-" + transport, "samples": total,
                       "maximum_differences": compare(baseline, result), "transport_stats": result.get("transport_stats"),
                       "state_checks": ["initial-paused", "single-step", "read-only-snapshot", "stale-sequence-rejected", "duplicate-rejected", "invalid-count-rejected", "bounded-recent-history", "complete-archive"]}
                evidence["checks"].append(row)
                print(json.dumps({"stage": "complete", "name": row["name"], "samples": total}), flush=True)
            # EOF after one real scan must persist incomplete evidence, not valid success.
            partial_path = output / (stem + "-interrupted.json")
            partial = Session([args.binary, "session", *common, "--output", str(partial_path)], output / (stem + "-interrupted-stderr.txt"))
            try:
                one = partial.request("advance", expected_samples=0, count=1)
                require(one["samples"] == 1, "Interruption case did not execute real MPC")
            finally:
                partial.close()
            interrupted = load(partial_path)
            require(interrupted["status"] != "valid" and interrupted["samples"] == 1, "EOF incorrectly published complete success")
            require(all(sha(p) == inputs[p.name] for p in source_files), "Frozen source artifacts changed")
            evidence["checks"].append({"name": stem + "-interruption-and-immutability", "passed": True, "source_sha256": inputs})
        evidence["status"] = "passed"
    except Exception as error:
        evidence.update(status="failed", error=str(error))
        raise
    finally:
        save(output / "verification.json", evidence)
    print(json.dumps({"status": evidence["status"], "checks": len(evidence["checks"]), "evidence": str(output / "verification.json")}), flush=True)


if __name__ == "__main__":
    main()

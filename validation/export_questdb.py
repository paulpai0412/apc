#!/usr/bin/env python3
"""Export an isolated QuestDB copy or saved /exec response into MPC history JSON.

No credentials, production discovery, interpolation or future-value filling.
Historian timestamps are receipt times, not PLC source timestamps.
"""
import argparse
from datetime import datetime, timedelta, timezone
import hashlib
import ipaddress
import json
import math
from pathlib import Path
import re
from urllib.parse import urlencode, urlsplit
from urllib.request import Request


def timestamp(value):
    if not isinstance(value, str):
        raise ValueError("timestamps must be ISO-8601 strings with timezone")
    result = datetime.fromisoformat(value.replace("Z", "+00:00"))
    if result.tzinfo is None:
        raise ValueError("timestamp has no timezone")
    return result.astimezone(timezone.utc)


def number(value, name):
    if isinstance(value, bool) or not isinstance(value, (int, float)) or not math.isfinite(value):
        raise ValueError(f"{name} must be a finite number")
    return float(value)


def mapped_nodes(mapping):
    nodes = []
    for group in ("cv", "mv", "dv"):
        entries = mapping.get(group)
        if not isinstance(entries, list) or (group != "dv" and not entries):
            raise ValueError(f"mapping.{group} must be a list (CV/MV cannot be empty)")
        for node in entries:
            if not isinstance(node, str) or not node.strip() or "\0" in node:
                raise ValueError("mapping nodes must be nonblank exact OPC string identifiers")
        nodes.extend(entries)
    if len(set(nodes)) != len(nodes):
        raise ValueError("each mapped node must have one explicit CV/MV/DV role")
    return nodes


def convert(response, mapping, start, end, max_age, max_rows=1000000):
    """Causal zero-order hold with explicit freshness and quality rejection."""
    nodes = mapped_nodes(mapping)
    dt = number(mapping.get("sample_time"), "sample_time")
    max_age = number(max_age, "max_age")
    if dt <= 0 or max_age < 0 or end <= start or max_rows < 1:
        raise ValueError("require positive sample_time/window/max_rows and nonnegative max_age")
    span = (end - start).total_seconds() / dt
    if not math.isclose(span, round(span), rel_tol=0, abs_tol=1e-7):
        raise ValueError("end-start must be an integer number of sample intervals")
    count = round(span)  # [start, end)
    if count < 2 or count > max_rows:
        raise ValueError("grid must contain between 2 and max_rows samples")
    columns = [column["name"] for column in response.get("columns", [])]
    required = ("timestamp", "node_id", "value", "status")
    if any(columns.count(key) != 1 for key in required):
        raise ValueError("response needs unique timestamp, node_id, value, status columns; legacy split identities cannot be inferred")
    rows = response.get("dataset")
    if not isinstance(rows, list) or len(rows) > max_rows:
        raise ValueError("response missing dataset or exceeds max_rows; split the export window")
    if response.get("count", len(rows)) != len(rows):
        raise ValueError("QuestDB response count differs from returned rows; export may be truncated")
    indexes = [columns.index(key) for key in required]
    series = {node: [] for node in nodes}
    earliest = start - timedelta(seconds=max_age)
    for row in rows:
        if len(row) != len(columns):
            raise ValueError("ragged QuestDB response row")
        ts, node, value, status = [row[index] for index in indexes]
        if node not in series:
            continue
        instant = timestamp(ts)
        if not earliest <= instant < end:
            continue
        if isinstance(status, bool) or not isinstance(status, int) or status != 0:
            raise ValueError(f"non-Good OPC status for {node} at {ts}; do not hide bad-quality intervals")
        value = number(value, f"{node} value")
        series[node].append((instant, value))
    for node, records in series.items():
        records.sort(key=lambda item: item[0])
        if any(a[0] == b[0] for a, b in zip(records, records[1:])):
            raise ValueError(f"duplicate timestamp for {node}")
    positions = dict.fromkeys(nodes, -1)
    history = {"sample_time": dt, "time_seconds": [], "cv": [], "mv": [], "dv": []}
    for i in range(count):
        instant = start + timedelta(seconds=i * dt)
        values = {}
        for node, records in series.items():
            pos = positions[node]
            while pos + 1 < len(records) and records[pos + 1][0] <= instant:
                pos += 1
            positions[node] = pos
            if pos < 0 or (instant - records[pos][0]).total_seconds() > max_age:
                raise ValueError(f"missing/stale {node} at {instant.isoformat()}")
            values[node] = records[pos][1]
        history["time_seconds"].append(i * dt)
        for group in ("cv", "mv", "dv"):
            history[group].append([values[node] for node in mapping[group]])
    history["provenance"] = {
        "timestamp_basis": "historian receipt time, not PLC source time",
        "start_utc": start.isoformat(), "end_exclusive_utc": end.isoformat(),
        "resampling": "causal zero-order hold; no backfill or interpolation",
        "max_age_seconds": max_age, "mapping": mapping,
        "quality_policy": "reject every selected nonzero status in window plus pre-roll",
    }
    source_metadata = response.get("provenance", {})
    if not isinstance(source_metadata, dict):
        raise ValueError("source provenance must be an object")
    history["provenance"]["source_metadata"] = source_metadata
    if source_metadata.get("synthetic") is True:
        history["provenance"]["synthetic"] = True
    return history


def query(mapping, table, start, end, max_age, max_rows):
    if not re.fullmatch(r"[A-Za-z_][A-Za-z0-9_]*", table):
        raise ValueError("table must be a simple SQL identifier")
    quote = lambda text: "'" + text.replace("'", "''") + "'"
    clauses = [quote(node) for node in mapped_nodes(mapping)]
    earliest = start - timedelta(seconds=max_age)
    return (
        f"SELECT timestamp, node_id, value, status FROM {table} "
        f"WHERE timestamp >= {quote(earliest.isoformat())} "
        f"AND timestamp < {quote(end.isoformat())} AND node_id IN ({', '.join(clauses)}) "
        f"ORDER BY timestamp LIMIT {max_rows + 1}"
    )


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    source = parser.add_mutually_exclusive_group(required=True)
    source.add_argument("--response", type=Path, help="saved QuestDB /exec JSON including status")
    source.add_argument("--url", help="explicit loopback QuestDB HTTP root; never a production endpoint")
    parser.add_argument("--mapping", type=Path, required=True)
    parser.add_argument("--start", required=True, help="inclusive ISO-8601 UTC time")
    parser.add_argument("--end", required=True, help="exclusive ISO-8601 UTC time")
    parser.add_argument("--max-age", type=float, required=True, help="maximum causal hold age in seconds")
    parser.add_argument("--max-rows", type=int, default=1000000)
    parser.add_argument("--table", default="process_data")
    parser.add_argument("--output", type=Path, required=True)
    args = parser.parse_args()
    try:
        start, end = timestamp(args.start), timestamp(args.end)
        mapping = json.loads(args.mapping.read_text(encoding="utf-8"))
        number(args.max_age, "max_age")
        if args.max_age < 0 or args.max_rows < 1 or end <= start:
            raise ValueError("invalid max-age, max-rows or time interval")
        if args.response:
            raw = args.response.read_bytes()
            source_name = str(args.response)
        else:
            url = urlsplit(args.url)
            if url.scheme != "http" or not url.hostname or url.username or url.password or url.query or url.fragment or url.path not in ("", "/"):
                raise ValueError("--url must be a credential-free loopback HTTP root")
            try:
                local = ipaddress.ip_address(url.hostname).is_loopback
            except ValueError:
                local = False
            if not local:
                raise ValueError("only literal loopback IP endpoints allowed; use a saved response for external history")
            sql = query(mapping, args.table, start, end, args.max_age, args.max_rows)
            endpoint = args.url.rstrip("/") + "/exec?" + urlencode({"query": sql, "limit": args.max_rows + 1, "count": "true"})
            # No redirect following: a local endpoint must not redirect to production.
            from urllib.request import HTTPRedirectHandler, ProxyHandler, build_opener
            class NoRedirect(HTTPRedirectHandler):
                def redirect_request(self, req, fp, code, msg, headers, newurl):
                    raise ValueError("redirects are not allowed")
            opener = build_opener(ProxyHandler({}), NoRedirect())
            with opener.open(Request(endpoint), timeout=30) as reply:
                raw = reply.read()
            source_name = args.url
        result = convert(json.loads(raw), mapping, start, end, args.max_age, args.max_rows)
        result["provenance"].update({"source": source_name, "sha256": hashlib.sha256(raw).hexdigest()})
        with args.output.open("x", encoding="utf-8") as output:
            json.dump(result, output, allow_nan=False, indent=2)
            output.write("\n")
    except (ValueError, KeyError, TypeError, OSError) as error:
        parser.exit(2, f"export rejected: {error}\n")


if __name__ == "__main__":
    main()

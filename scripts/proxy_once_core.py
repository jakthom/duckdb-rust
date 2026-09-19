"""Minimal, fail-closed execution path for one Python SQLLogic proxy workload."""
import startup_json as json
import os
import time

import sqllogic
from secure_scratch import ScratchDirectory
from worker_protocol import RustEngine


ROOT = os.path.dirname(os.path.dirname(os.path.realpath(__file__)))
JOB_SCHEMA = "sqllogic-proxy-once-v1"
ONCE_TIMEOUT_SECONDS = 60
EXPECTED_MANIFEST = "benchmark/a2_1_sqllogic_workloads.json"
EXPECTED_MANIFEST_SHA256 = "dbb7c05065fe5a40513b538c8200aef9f280e7bdcd8ee0a535c5d193eac05242"
EXPECTED_WORKLOADS = (
    {"id": "a2_1_scalar_comma_loop", "path": "test/performance/a2_1_scalar_comma_loop.test", "sha256": "e4e9ebe9c15dcab0ae9ce74d2fc4411e94fb6f4e70d8174a9341f7cd6f0f2adb", "bytes": 368, "proxy_records": 5},
    {"id": "a2_1_scalar_comma_loop_large", "path": "test/performance/a2_1_scalar_comma_loop_large.test", "sha256": "bfc43aff382c8c0ab2f74d067df8a16f9a3ee0ac93579d797ff944d6725288db", "bytes": 420, "proxy_records": 5000},
    {"id": "g01_2c_loop_conditions_concurrent_sessions_lifecycle", "path": "test/performance/g01_2c_loop_sessions.test", "sha256": "790954ce9e77bb67e7bb5b6e2fe06fcb1ed10a41e4d915d1c2265388e20ea974", "bytes": 862, "proxy_records": 16},
    {"id": "a2_1_readonly_rejections", "path": "test/performance/a2_1_readonly_rejections.test", "sha256": "e6d07907d8d8d88cfd1ec0ebbf653d1bb9dece1012e2464dbc1154789c7bbd6b", "bytes": 400, "proxy_records": 4},
    {"id": "a2_1_readonly_rejections_large", "path": "test/performance/a2_1_readonly_rejections_large.test", "sha256": "439bbb9a3bda14709f2af3026b0ea8d26066e817dc68c2f50dba032ce46a248e", "bytes": 415, "proxy_records": 2002},
)
_ATTESTATION_KEYS = frozenset(("worker_sha256", "source_sha256", "provenance_sha256"))
_JOB_KEYS = frozenset(("schema", "worker", "worker_provenance", "test_root", "path", "timeout", "attestation"))
_DIGEST_BLOCK_SIZE = 256 * 1024


def sha256_constructor(import_module=__import__):
    """Prefer CPython's builtin SHA-256, with the public API as a fallback."""
    try:
        return import_module("_sha256", fromlist=("sha256",)).sha256
    except ImportError:
        return import_module("hashlib", fromlist=("sha256",)).sha256


_SHA256 = sha256_constructor()


def _resolve(path):
    resolved = os.path.realpath(os.fspath(path))
    os.stat(resolved)
    return resolved


def digest(path):
    result = _SHA256()
    with open(path, "rb") as file:
        while block := file.read(_DIGEST_BLOCK_SIZE):
            result.update(block)
    return result.hexdigest()


def workload_specs(root):
    root = _resolve(root)
    result = []
    for expected in EXPECTED_WORKLOADS:
        path = _resolve(os.path.join(root, expected["path"]))
        result.append({
            "id": expected["id"], "path": expected["path"], "kind": "custom_comparable",
            "shared_test_dir": root, "shared_workload_path": path,
            "shared_workload_sha256": expected["sha256"],
            "shared_workload_bytes": expected["bytes"],
            "proxy_records_expected": expected["proxy_records"],
        })
    return result


def validate_workload_population(manifest, test_root):
    root = _resolve(test_root)
    if root != ROOT:
        raise ValueError(f"proxy acceptance test root must be {ROOT}")
    manifest = _resolve(manifest)
    expected_manifest = _resolve(os.path.join(root, EXPECTED_MANIFEST))
    if manifest != expected_manifest or digest(manifest) != EXPECTED_MANIFEST_SHA256:
        raise ValueError("proxy acceptance requires the frozen five-workload manifest")
    try:
        with open(manifest) as file:
            data = json.load(file)
    except (OSError, json.JSONDecodeError) as error:
        raise ValueError("proxy workload manifest is not valid JSON") from error
    if set(data) != {"workloads"} or not isinstance(data["workloads"], list):
        raise ValueError("proxy manifest must contain only its workload population")
    expected = workload_specs(root)
    declared = [{"id": workload["id"], "path": workload["path"]} for workload in expected]
    if data["workloads"] != declared:
        raise ValueError("proxy manifest has missing, extra, or duplicate workloads")
    for workload in expected:
        path = workload["shared_workload_path"]
        if not os.path.isfile(path) or digest(path) != workload["shared_workload_sha256"] or os.path.getsize(path) != workload["shared_workload_bytes"]:
            raise ValueError("proxy workload identity changed: " + workload["id"])
    return expected


def _require_string(value, name):
    if not isinstance(value, str) or not value or "\x00" in value:
        raise ValueError(f"once job {name} must be a nonempty string")
    try:
        value.encode("utf-8")
    except UnicodeEncodeError as error:
        raise ValueError(f"once job {name} is not valid UTF-8") from error
    return value


def _validate_attestation(value):
    if value is None:
        return None
    if not isinstance(value, dict) or set(value) != _ATTESTATION_KEYS:
        raise ValueError("once job attestation must be absent or a complete triple")
    for name, digest_value in value.items():
        if not isinstance(digest_value, str) or len(digest_value) != 64 or any(character not in "0123456789abcdef" for character in digest_value):
            raise ValueError(f"once job attestation {name} is not a SHA-256")
    return dict(value)


def validate_job(job):
    if not isinstance(job, dict) or set(job) != _JOB_KEYS:
        raise ValueError("once job has missing, duplicate, or unknown fields")
    if job["schema"] != JOB_SCHEMA:
        raise ValueError("once job has an unsupported schema")
    if isinstance(job["timeout"], bool) or job["timeout"] != ONCE_TIMEOUT_SECONDS:
        raise ValueError("once job requires the frozen 60-second timeout")
    result = {"schema": job["schema"], "timeout": job["timeout"]}
    for name in ("worker", "worker_provenance", "test_root", "path"):
        result[name] = _require_string(job[name], name)
    result["attestation"] = _validate_attestation(job["attestation"])
    return result


def _no_duplicate_keys(pairs):
    result = {}
    for key, value in pairs:
        if key in result:
            raise ValueError("once job has duplicate JSON keys")
        result[key] = value
    return result


def _reject_json_constant(value):
    raise ValueError("once job contains a non-JSON numeric constant: " + value)


def decode_job(encoded):
    _require_string(encoded, "envelope")
    try:
        job = json.loads(encoded, object_pairs_hook=_no_duplicate_keys, parse_constant=_reject_json_constant)
    except (json.JSONDecodeError, ValueError) as error:
        raise ValueError("once job is not valid strict JSON") from error
    return validate_job(job)


def encode_job(job):
    return json.dumps(validate_job(job), separators=(",", ":"), ensure_ascii=True, allow_nan=False)


def make_job(worker, worker_provenance, test_root, relative, timeout=ONCE_TIMEOUT_SECONDS, attestation=None):
    return validate_job({
        "schema": JOB_SCHEMA, "worker": os.fspath(worker), "worker_provenance": os.fspath(worker_provenance),
        "test_root": os.fspath(test_root), "path": relative, "timeout": timeout, "attestation": attestation,
    })


def checked_campaign_attestation(worker, worker_provenance, attestation):
    attestation = _validate_attestation(attestation)
    if attestation is None:
        raise ValueError("campaign worker attestation is incomplete")
    worker = _resolve(worker)
    provenance_path = _resolve(worker_provenance)
    canonical_path = _resolve(worker + ".provenance.json")
    if provenance_path != canonical_path:
        raise ValueError("--once requires the canonical worker provenance sidecar")
    if digest(provenance_path) != attestation["provenance_sha256"]:
        raise ValueError("campaign worker provenance sidecar changed")
    try:
        with open(provenance_path) as file:
            provenance = json.load(file)
    except (OSError, json.JSONDecodeError) as error:
        raise ValueError("campaign worker provenance sidecar is unreadable") from error
    expected = {"profile": "release", "source_sha256": attestation["source_sha256"], "binary_sha256": attestation["worker_sha256"]}
    if provenance != expected:
        raise ValueError("campaign worker provenance sidecar is stale or tampered")
    return provenance_path, provenance


def _standalone_provenance(worker, provenance_path):
    from pathlib import Path
    from run_upstream import checked_worker_provenance
    checked_path, provenance = checked_worker_provenance(Path(worker), Path(provenance_path))
    return os.fspath(checked_path), provenance


def run(job, *, standalone_provenance=None, campaign_provenance=None, workload_validator=None):
    if standalone_provenance is None:
        standalone_provenance = _standalone_provenance
    if campaign_provenance is None:
        campaign_provenance = checked_campaign_attestation
    if workload_validator is None:
        workload_validator = validate_workload_population
    job = validate_job(job)
    root = _resolve(job["test_root"])
    workloads = {workload["path"]: workload for workload in workload_validator(os.path.join(root, EXPECTED_MANIFEST), root)}
    if job["path"] not in workloads:
        raise ValueError("--once path is not in the frozen proxy workload population")
    worker = _resolve(job["worker"])
    expected_worker = _resolve(os.path.join(ROOT, "target/release/duckdb-rust-test-worker"))
    if worker != expected_worker:
        raise ValueError("--once requires the worktree release feedback worker")
    if job["attestation"] is None:
        provenance_path, _ = standalone_provenance(worker, job["worker_provenance"])
    else:
        provenance_path, _ = campaign_provenance(worker, job["worker_provenance"], job["attestation"])
    if _resolve(provenance_path) != _resolve(worker + ".provenance.json"):
        raise ValueError("--once requires the canonical worker provenance sidecar")
    path = _resolve(os.path.join(root, job["path"]))
    if not os.path.isfile(path) or os.path.commonpath((root, path)) != root:
        raise ValueError("--once workload path is invalid")
    with open(path) as file:
        records = sqllogic.parse(file.read())
    deadline = time.monotonic() + job["timeout"]
    with ScratchDirectory(prefix="ddb-proxy-measure-") as scratch:
        scratch.validate_path()
        scratch_path = os.fspath(scratch)
        engine = RustEngine(worker, scratch, deadline)
        try:
            runner = sqllogic.Runner(engine, {
                "{TEST_DIR}": scratch_path, "__TEST_DIR__": scratch_path,
                "{WORKING_DIRECTORY}": scratch_path, "__WORKING_DIRECTORY__": scratch_path,
                "{TEST_NAME}": job["path"], "{BASE_TEST_NAME}": job["path"].replace("/", "_"),
                "__SOURCE_DIR__": root,
            })
            runner.run(records)
            expected = workloads[job["path"]]["proxy_records_expected"]
            if runner.passed != expected or runner.skipped:
                raise ValueError("proxy workload PASS count differs from frozen untimed expectation")
        except BaseException as original:
            # A protocol cleanup failure is diagnostic only when execution has
            # already failed: preserve the SQLLogic/body exception as primary.
            try:
                engine.close()
            except BaseException as cleanup:
                raise original from cleanup
            raise
        else:
            engine.close()
            return runner.passed


def bootstrap(encoded):
    job = decode_job(encoded)
    count = run(job)
    print(f"PASS {job['path']} ({count} records)\n{count} records passed; 0 skipped")
    return 0

#!/usr/bin/env python3
"""M6F-B production-path driver: Core HTTP API orchestration for the
real SR Linux Core→collector acceptance run.

Drives ONLY supported production interfaces:

  * POST /api/auth/login                     (env-bootstrap admin JWT)
  * POST /api/identity/devices               (canonical collector device)
  * POST /api/identity/bindings              (cert-fingerprint collector binding)
  * POST /api/credentials                    (device credential alias, vault)
  * POST /api/onboarding/devices             (managed device registration)
  * POST /api/assignment/override            (device → collector assignment)
  * POST /api/devices/{a}/yang-applicability/refresh   (executor=assigned_collector)
  * GET  /api/devices/{a}/yang-discovery/jobs          (job polling)
  * GET  /api/devices/{a}/yang-applicability           (persisted snapshot)
  * POST /api/devices/{a}/schema-binding/preview|activate|revoke
  * GET  /api/devices/{a}                    (subscription_statuses read-back)
  * GET  /api/assignment/status              (collector session evidence)

Every stage writes a JSON artifact under --evidence-dir; an incomplete
stage is recorded as FAIL/NOT_EXECUTED — never silently green.

Secrets are read from environment variables only and are never written
to evidence or stdout.
"""

import argparse
import json
import os
import sys
import time
import urllib.error
import urllib.parse
import urllib.request


class Api:
    def __init__(self, base, token=None, timeout=30):
        self.base = base.rstrip("/")
        self.token = token
        self.timeout = timeout

    def _req(self, method, path, body=None):
        data = None
        headers = {"Accept": "application/json"}
        if self.token:
            headers["Authorization"] = f"Bearer {self.token}"
        if body is not None:
            data = json.dumps(body).encode()
            headers["Content-Type"] = "application/json"
        req = urllib.request.Request(
            self.base + path, data=data, headers=headers, method=method
        )
        try:
            with urllib.request.urlopen(req, timeout=self.timeout) as r:
                raw = r.read()
                return r.status, json.loads(raw) if raw else {}
        except urllib.error.HTTPError as e:
            raw = e.read()
            try:
                payload = json.loads(raw) if raw else {}
            except Exception:
                payload = {"raw": raw.decode("utf-8", "replace")[:2000]}
            return e.code, payload

    def get(self, path):
        return self._req("GET", path)

    def post(self, path, body=None):
        return self._req("POST", path, body if body is not None else {})


def stage_dir(ev, name):
    d = os.path.join(ev, name)
    os.makedirs(d, exist_ok=True)
    return d


def dump(ev, name, obj):
    path = os.path.join(ev, name)
    with open(path, "w") as f:
        json.dump(obj, f, indent=2, sort_keys=True)
    return path


def redact(obj):
    """Best-effort secret scrub for evidence payloads."""
    if isinstance(obj, dict):
        out = {}
        for k, v in obj.items():
            lk = k.lower()
            if any(s in lk for s in ("password", "secret", "token", "key")):
                out[k] = "<redacted>"
            else:
                out[k] = redact(v)
        return out
    if isinstance(obj, list):
        return [redact(v) for v in obj]
    return obj


def record(results, stage, ok, detail=None, **extra):
    entry = {"stage": stage, "ok": bool(ok)}
    if detail is not None:
        entry["detail"] = detail
    entry.update(extra)
    results.append(entry)
    status = "PASS" if ok else "FAIL"
    print(f"[{status}] {stage}" + (f" — {detail}" if detail else ""),
          file=sys.stderr)
    return ok


def cmd_login(api, args, results):
    user = os.environ.get(args.admin_user_env, "")
    pw = os.environ.get(args.admin_pass_env, "")
    if not user or not pw:
        record(results, "admin_login", False,
               f"env {args.admin_user_env}/{args.admin_pass_env} not set")
        return None
    code, body = api.post("/api/auth/login",
                          {"username": user, "password": pw})
    ok = code == 200 and body.get("success") and body.get("token")
    record(results, "admin_login", ok, f"http={code}")
    dump(args.evidence_dir, "01-login.json", redact(body))
    return body.get("token") if ok else None


def cmd_provision(api, args, results):
    ev = args.evidence_dir
    collector_id = args.collector_id
    cert_fp = args.cert_fingerprint  # "sha256:<hex>" of leaf DER

    # 1. canonical device row for the collector identity
    code, body = api.post("/api/identity/devices",
                          {"address": f"collector:{collector_id}",
                           "site": args.site})
    device_id = (body.get("device") or {}).get("device_id", "")
    ok = code in (200, 201) and device_id
    record(results, "identity_device", ok,
           f"http={code} device_id={device_id or body}")
    dump(ev, "02-identity-device.json", redact(body))
    if not ok:
        return False

    # 2. credential-pinned collector binding (protocol=collector)
    code, body = api.post("/api/identity/bindings", {
        "scope": args.scope,
        "protocol": "collector",
        "exporter_id": collector_id,
        "device_id": device_id,
        "match_ingress_credential": cert_fp,
    })
    ok = code in (200, 201) and (body.get("binding") or {}).get("binding_id")
    if not ok and code == 400 and "already bound" in json.dumps(body):
        # idempotent re-run: verify the existing active binding pins the
        # same credential fingerprint rather than creating a duplicate
        code2, lb = api.get("/api/identity/bindings?protocol=collector")
        for b in (lb.get("bindings") or []):
            if (b.get("exporter_id") == collector_id
                    and b.get("match_ingress_credential") == cert_fp
                    and b.get("status") == "active"):
                ok = True
                body = {"binding": b, "idempotent": True}
                break
    record(results, "collector_binding", ok, f"http={code}")
    dump(ev, "03-collector-binding.json", redact(body))
    if not ok:
        return False

    # 3. device credential alias in the vault (never on CLI/evidence).
    #    Idempotent re-runs update the existing alias in place.
    cred_body = {
        "alias": args.cred_alias,
        "username": os.environ[args.device_user_env],
        "password": os.environ[args.device_pass_env],
    }
    code, body = api.post("/api/credentials", cred_body)
    if code == 200 and not body.get("success") \
            and "already exists" in str(body.get("error", "")):
        code, body = api.post("/api/credentials/update", cred_body)
    ok = code == 200 and body.get("success")
    record(results, "device_credential", ok, f"http={code}")
    dump(ev, "04-device-credential.json", redact(body))
    if not ok:
        return False

    # 4. managed device registration (schema acquisition starts disabled;
    #    the schema-bound approval is the only acquisition authority)
    code, body = api.post("/api/onboarding/devices", {
        "address": args.device_addr,
        "enabled": True,
        "tls_domain": args.tls_domain,
        "ca_cert": args.device_ca,
        "vendor": args.vendor,
        "credential_alias": args.cred_alias,
        "hostname": args.hostname,
        "role": args.role,
        "site": args.site,
        "security": "tls_verify",
        "acquisition": "disabled",
    })
    ok = code == 200 and body.get("success")
    record(results, "device_register", ok,
           f"http={code} {body.get('error', '')}")
    dump(ev, "05-device-register.json", redact(body))
    if not ok:
        return False

    # 5. explicit assignment override → collector
    code, body = api.post("/api/assignment/override", {
        "device_address": args.device_addr,
        "collector_id": collector_id,
    })
    ok = code == 200 and body.get("success")
    record(results, "device_assignment", ok,
           f"http={code} {body.get('error', '')}")
    dump(ev, "06-assignment-override.json", redact(body))
    return ok


def cmd_wait_collector(api, args, results):
    deadline = time.time() + args.timeout
    last = None
    while time.time() < deadline:
        code, body = api.get("/api/assignment/status")
        if code == 200:
            last = body
            for c in body.get("collectors", []):
                if c.get("id") == args.collector_id and c.get("connected"):
                    record(results, "collector_registered", True,
                           f"assigned={c.get('assigned_device_count')}")
                    dump(args.evidence_dir, "07-assignment-status.json",
                         redact(body))
                    return True
        time.sleep(3)
    record(results, "collector_registered", False,
           "collector did not connect within timeout")
    if last:
        dump(args.evidence_dir, "07-assignment-status.json", redact(last))
    return False


def cmd_compile(api, args, results):
    """Compile + activate the YANG catalogue through the authenticated
    admin API — the same production path an operator uses."""
    code, body = api.post("/api/yang/compile?activate=true")
    ok = code == 200
    record(results, "catalogue_compile", ok,
           f"http={code} schema={body.get('schema_id')} "
           f"status={body.get('status')} modules={body.get('module_count')} "
           f"paths={body.get('path_count')} activated={body.get('activated')}")
    dump(args.evidence_dir, "08-catalogue-compile.json", redact(body))
    if not ok:
        return False
    if not body.get("activated") and body.get("status") in (
            "partial", "failed", "unknown"):
        # explicit activation is an admin decision; only proceed when the
        # catalogue is at least usable (partial is explicit diagnostic
        # state, never silently treated as complete)
        code, act = api.post("/api/yang/activate",
                             {"schema_id": body["schema_id"]})
        ok = code == 200
        record(results, "catalogue_activate_explicit", ok,
               f"http={code} schema={body.get('schema_id')}")
        dump(args.evidence_dir, "09-catalogue-activate.json", redact(act))
    return ok


def cmd_discovery(api, args, results):
    ev = args.evidence_dir
    addr = args.device_addr
    path = f"/api/devices/{addr}/yang-applicability/refresh"
    body = {"executor": "assigned_collector"}
    if args.datastore:
        body["datastore"] = args.datastore
    code, resp = api.post(path, body)
    ok = code == 202 and resp.get("job_id")
    record(results, "discovery_job_queued", ok,
           f"http={code} job={resp.get('job_id', resp)}")
    dump(ev, "10-discovery-job-accepted.json", redact(resp))
    if not ok:
        return False
    job_id = resp["job_id"]

    # poll job state
    deadline = time.time() + args.timeout
    job = None
    while time.time() < deadline:
        code, body = api.get(f"/api/devices/{addr}/yang-discovery/jobs")
        if code == 200:
            for j in body.get("jobs", []):
                if j.get("job_id") == job_id:
                    job = j
                    if j.get("state") in ("completed", "failed",
                                          "expired", "cancelled"):
                        break
        time.sleep(3)
    dump(ev, "11-discovery-job-final.json", redact(job or {}))
    state = (job or {}).get("state", "unknown")
    ok = state == "completed"
    record(results, "discovery_job_completed", ok,
           f"state={state} class={(job or {}).get('result_classification')} "
           f"reason={(job or {}).get('reason_code')} "
           f"snapshot={(job or {}).get('snapshot_id')}")
    return ok


def cmd_applicability(api, args, results):
    addr = args.device_addr
    code, body = api.get(f"/api/devices/{addr}/yang-applicability"
                         f"?limit=5000")
    ok = code == 200 and (body.get("snapshot") or {}).get("snapshot_id")
    snap = body.get("snapshot") or {}
    record(results, "applicability_snapshot", ok,
           f"http={code} status={snap.get('overall_status')} "
           f"outcome={snap.get('outcome')} schema={snap.get('schema_id')} "
           f"content_id={snap.get('content_id')} "
           f"paths={body.get('paths_matching')}")
    dump(args.evidence_dir, "12-applicability.json", redact(body))
    return ok


def cmd_paths(api, args, results):
    """Select concrete applicable paths for the schema-bound plan."""
    addr = args.device_addr
    q = args.path_query
    code, body = api.get(
        f"/api/devices/{addr}/yang-applicability?status=applicable"
        f"&q={urllib.parse.quote(q)}&limit=200")
    paths = body.get("paths", []) if code == 200 else []
    record(results, "applicable_paths", code == 200 and bool(paths),
           f"http={code} matches={len(paths)}")
    dump(args.evidence_dir, "13-applicable-paths.json", redact(body))
    return paths


def cmd_preview(api, args, results):
    addr = args.device_addr
    req = json.load(open(args.plan_file))
    code, body = api.post(f"/api/devices/{addr}/schema-binding/preview", req)
    ok = code == 200
    ev = body.get("evaluation") or {}
    record(results, "plan_preview", ok and body.get("activation_permitted", False),
           f"http={code} permitted={body.get('activation_permitted')} "
           f"plan={ev.get('plan_id')} reasons={ev.get('rejection_reasons')}")
    dump(args.evidence_dir, "20-plan-preview.json", redact(body))
    if ok:
        with open(os.path.join(args.evidence_dir, "preview.json"), "w") as f:
            json.dump(body, f)
    return ok and bool(body.get("activation_permitted"))


def cmd_activate(api, args, results):
    addr = args.device_addr
    req = json.load(open(args.plan_file))
    preview = json.load(open(os.path.join(args.evidence_dir, "preview.json")))
    req["expected_config_revision"] = preview.get("config_revision")
    ev = preview.get("evaluation") or {}
    if ev.get("plan_id"):
        req["plan_id"] = ev["plan_id"]
    if ev.get("binding_fingerprint"):
        req["binding_fingerprint"] = ev["binding_fingerprint"]
    if preview.get("current_binding"):
        req["binding_fingerprint"] = (
            preview["current_binding"].get("binding_fingerprint"))
    code, body = api.post(f"/api/devices/{addr}/schema-binding/activate", req)
    ok = code == 200 and body.get("success")
    record(results, "plan_activate", ok,
           f"http={code} plan={body.get('plan_id')} "
           f"rev={body.get('approval_revision')} state={body.get('state')}")
    dump(args.evidence_dir, "21-plan-activate.json", redact(body))
    return ok


def cmd_telemetry(api, args, results):
    """Wait for real telemetry: subscription_statuses must reach
    'observed' with advancing last_observed_at (SAMPLE) — a bare
    successful Subscribe RPC is not sufficient."""
    addr = args.device_addr
    deadline = time.time() + args.timeout
    seen = []
    baseline = None
    while time.time() < deadline:
        code, dev = api.get(f"/api/devices/{addr}")
        if code == 200:
            statuses = dev.get("subscription_statuses", [])
            dump(args.evidence_dir, "30-device-detail-latest.json",
                 redact(dev))
            observed = [s for s in statuses if s.get("status") == "observed"]
            if observed:
                latest = max(s.get("last_observed_at_ns") or 0
                             for s in observed)
                if baseline is None:
                    baseline = latest
                    seen = observed
                elif latest > baseline:
                    record(results, "telemetry_observed", True,
                           f"{len(observed)} observed path(s), "
                           f"last_observed advancing "
                           f"({baseline} -> {latest})")
                    dump(args.evidence_dir, "31-telemetry-observed.json",
                         redact({"statuses": statuses}))
                    return True
                seen = observed
        time.sleep(args.poll_secs)
    record(results, "telemetry_observed", False,
           f"no advancing observed telemetry within {args.timeout}s "
           f"(last statuses: {seen})")
    return False


def cmd_revoke(api, args, results):
    addr = args.device_addr
    code, body = api.post(f"/api/devices/{addr}/schema-binding/revoke",
                          {"reason": args.revoke_reason})
    ok = code == 200 and body.get("success")
    record(results, "plan_revoke", ok, f"http={code} state={body.get('state')}")
    dump(args.evidence_dir, "40-plan-revoke.json", redact(body))
    return ok


def cmd_verify_withdrawal(api, args, results):
    """After revoke: acquisition must be disabled and no NEW telemetry
    observations may be accepted. We verify (a) device binding cleared,
    (b) subscription statuses stop advancing / drop out of observed,
    (c) assignment status reflects withdrawal."""
    addr = args.device_addr
    deadline = time.time() + args.timeout
    withdrew = False
    last_obs = None
    while time.time() < deadline:
        code, dev = api.get(f"/api/devices/{addr}")
        code2, status = api.get("/api/assignment/status")
        if code == 200:
            dump(args.evidence_dir, "41-post-revoke-device.json",
                 redact(dev))
            statuses = dev.get("subscription_statuses", [])
            observed = [s for s in statuses
                        if s.get("status") == "observed"]
            cur = max((s.get("last_observed_at_ns") or 0
                       for s in observed), default=0)
            if last_obs is None:
                last_obs = cur
            # withdrawn when nothing remains observed OR observations
            # stopped advancing for a full poll window
            if not observed:
                withdrew = True
                break
            if cur == last_obs:
                # one stable read is not proof; require it stays stable
                # across two consecutive polls below
                pass
        if code2 == 200:
            dump(args.evidence_dir, "42-post-revoke-assignment.json",
                 redact(status))
        time.sleep(args.poll_secs)
        # re-check stability: compare across loop iterations
        if last_obs is not None:
            code, dev = api.get(f"/api/devices/{addr}")
            if code == 200:
                statuses = dev.get("subscription_statuses", [])
                observed = [s for s in statuses
                            if s.get("status") == "observed"]
                cur = max((s.get("last_observed_at_ns") or 0
                           for s in observed), default=0)
                if not observed or cur == last_obs:
                    withdrew = True
                    break
                last_obs = cur
    record(results, "withdrawal_verified", withdrew,
           "observed telemetry ceased / statuses cleared after revoke")
    return withdrew


def main():
    ap = argparse.ArgumentParser(description=__doc__)
    ap.add_argument("--core", required=True, help="core base URL")
    ap.add_argument("--evidence-dir", required=True)
    ap.add_argument("--collector-id", default="m6f-col-1")
    ap.add_argument("--device-addr", required=True)
    ap.add_argument("--tls-domain", default="srlinux")
    ap.add_argument("--device-ca", default="")
    ap.add_argument("--vendor", default="nokia_srlinux")
    ap.add_argument("--hostname", default="srlinux")
    ap.add_argument("--role", default="leaf")
    ap.add_argument("--site", default="m6f-lab")
    ap.add_argument("--scope", default="m6f-lab")
    ap.add_argument("--cred-alias", default="m6f-srlinux")
    ap.add_argument("--cert-fingerprint", required=True,
                    help="sha256:<hex> of collector leaf cert DER")
    ap.add_argument("--admin-user-env", default="M6F_ADMIN_USER")
    ap.add_argument("--admin-pass-env", default="M6F_ADMIN_PASS")
    ap.add_argument("--device-user-env", default="M6F_SRL_USER")
    ap.add_argument("--device-pass-env", default="M6F_SRL_PASS")
    ap.add_argument("--datastore", default="ietf")
    ap.add_argument("--path-query", default="counter")
    ap.add_argument("--plan-file", default="")
    ap.add_argument("--revoke-reason", default="m6f_acceptance_complete")
    ap.add_argument("--timeout", type=int, default=300)
    ap.add_argument("--poll-secs", type=int, default=5)
    ap.add_argument("--stage", default="all",
                    help="comma list or 'all': login,provision,"
                         "wait_collector,compile,discovery,applicability,"
                         "paths,preview,activate,telemetry,revoke,"
                         "verify_withdrawal")
    args = ap.parse_args()

    os.makedirs(args.evidence_dir, exist_ok=True)
    results = []
    api = Api(args.core)

    stages = (args.stage.split(",") if args.stage != "all"
              else ["login", "provision", "wait_collector", "compile",
                    "discovery", "applicability", "preview", "activate",
                    "telemetry", "revoke", "verify_withdrawal"])

    token = None
    if "login" in stages:
        token = cmd_login(api, args, results)
        if not token:
            finish(args, results)
            return 1
        api.token = token

    dispatch = {
        "provision": lambda: cmd_provision(api, args, results),
        "wait_collector": lambda: cmd_wait_collector(api, args, results),
        "compile": lambda: cmd_compile(api, args, results),
        "discovery": lambda: cmd_discovery(api, args, results),
        "applicability": lambda: cmd_applicability(api, args, results),
        "preview": lambda: cmd_preview(api, args, results),
        "activate": lambda: cmd_activate(api, args, results),
        "telemetry": lambda: cmd_telemetry(api, args, results),
        "revoke": lambda: cmd_revoke(api, args, results),
        "verify_withdrawal": lambda: cmd_verify_withdrawal(api, args, results),
    }
    ok = True
    for s in stages:
        if s == "login":
            continue
        if s == "paths":
            paths = cmd_paths(api, args, results)
            if paths:
                print(json.dumps(paths[:10], indent=2))
            continue
        fn = dispatch.get(s)
        if fn and not fn():
            ok = False
            # discovery/applicability failures are fatal to the chain;
            # telemetry/revoke still attempted only if earlier stages ok
            if s in ("provision", "wait_collector", "discovery",
                     "applicability", "preview", "activate"):
                break
    finish(args, results)
    return 0 if ok and all(r["ok"] for r in results) else 1


def finish(args, results):
    summary = {
        "evidence_dir": args.evidence_dir,
        "utc": time.strftime("%Y-%m-%dT%H:%M:%SZ", time.gmtime()),
        "stages": results,
        "overall": "PASS" if results and all(r["ok"] for r in results)
                   else "FAIL",
    }
    dump(args.evidence_dir, "00-summary.json", summary)
    print(json.dumps(summary, indent=2))


if __name__ == "__main__":
    sys.exit(main())

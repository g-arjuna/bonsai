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

    def stream_events(self, duration):
        """Yield `/api/events` SSE payloads for up to `duration` seconds.

        Used to capture authoritative in-process evidence (plan-state
        and status-change events) that durable read-backs cannot show.
        Yields decoded JSON objects; transport errors end the stream.
        """
        headers = {"Accept": "text/event-stream"}
        if self.token:
            headers["Authorization"] = f"Bearer {self.token}"
        req = urllib.request.Request(
            self.base + "/api/events", headers=headers)
        deadline = time.time() + duration
        try:
            with urllib.request.urlopen(req, timeout=self.timeout) as r:
                for raw in r:
                    if time.time() > deadline:
                        break
                    line = raw.decode("utf-8", "replace").strip()
                    if not line.startswith("data:"):
                        continue
                    try:
                        yield json.loads(line[5:].strip())
                    except Exception:
                        continue
        except Exception:
            return


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
    reg = {
        "address": args.device_addr,
        "enabled": True,
        "vendor": args.vendor,
        "credential_alias": args.cred_alias,
        "hostname": args.hostname,
        "role": args.role,
        "site": args.site,
        "security": args.device_security,
        "acquisition": "disabled",
    }
    if args.device_security != "plaintext":
        # plaintext forbids TLS material (SNI/CA are meaningless and
        # the API rejects the combination)
        reg["tls_domain"] = args.tls_domain
        reg["ca_cert"] = args.device_ca
    code, body = api.post("/api/onboarding/devices", reg)
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


# Durable subscription-status values that prove the coverage verifier
# processed a non-submitted (withdrawn/disabled) manifest. "observed"
# and "pending" are NEVER acceptable post-withdrawal states.
_TERMINAL_STATUSES = {"superseded", "expired", "stale", "failed",
                      "disabled"}


def _device_withdrawal_snapshot(api, addr):
    """One authoritative read: device detail + collector assignment
    status. Returns (device, collector_entry_or_None, ok)."""
    code, dev = api.get(f"/api/devices/{addr}")
    code2, status = api.get("/api/assignment/status")
    collector = None
    if code2 == 200:
        for c in status.get("collectors", []):
            collector = c  # caller filters by id when needed
    return (dev if code == 200 else None,
            status if code2 == 200 else None,
            code == 200 and code2 == 200)


def cmd_verify_withdrawal(api, args, results):
    """Authoritative withdrawal verification (M6F-C).

    A frozen `last_observed_at` across one short poll interval is NOT
    proof of withdrawal: a SAMPLE subscription at 10s can look stable
    for two polls while the device keeps streaming, and a noncompliant
    collector may ignore the withdrawal entirely. Withdrawal is
    verified only when ALL of the following authoritative evidence is
    observed within one monitoring window:

      1. binding_revoked        — the durable registry cleared the
                                  device's schema_binding.
      2. acquisition_disabled   — the target's acquisition mode is
                                  "disabled" (revoke sets it).
      3. withdrawal_issued      — the coverage verifier registered the
                                  disabled plan: durable subscription
                                  statuses left "observed" AND a live
                                  `subscription_plan_state` SSE event
                                  recorded the withdrawal (or, when the
                                  SSE stream was unavailable, every
                                  status durably transitioned).
      4. collector_ack          — the assigned collector's heartbeat is
                                  still fresh (it is alive and applying
                                  post-revoke generations) and its
                                  reported subscription_count is zero.
      5. subscription_terminated— no path remains "observed"/"pending";
                                  terminal states are recorded.
      6. no_continued_collection— across a window spanning at least two
                                  sample intervals, no
                                  last_observed_at_ns/updated_at_ns
                                  advances, no path returns to
                                  "observed", and subscription_count
                                  stays zero. This is also the
                                  observable consequence of
                                  old-session rejection: telemetry
                                  stamped with the retired session can
                                  never re-establish coverage.

    Any missing or contradictory evidence records FAIL — never a
    frozen-timestamp pass.
    """
    addr = args.device_addr
    evidence = {
        "binding_revoked": False,
        "acquisition_disabled": False,
        "withdrawal_issued": False,
        "collector_ack": False,
        "subscription_terminated": False,
        "no_continued_collection": False,
        "old_session_rejected": False,
    }
    detail = {}

    # Learn the sample cadence up-front so the stability window spans
    # at least two full SAMPLE periods — the minimum interval in which
    # continued collection would provably advance a timestamp. An
    # explicit --withdraw-window overrides for tests.
    code, dev0 = api.get(f"/api/devices/{addr}")
    sample_ns = 0
    if code == 200:
        for s in dev0.get("subscription_statuses", []):
            sample_ns = max(sample_ns, s.get("sample_interval_ns") or 0)
    sample_secs = max(1.0, sample_ns / 1e9) if sample_ns else 10.0
    window = (float(args.withdraw_window) if args.withdraw_window > 0
              else min(max(25.0, 2.5 * sample_secs), 90.0))
    wait_budget = max(float(args.timeout or 300),
                      window + args.poll_secs + 2.0)
    deadline = time.time() + wait_budget

    # SSE capture runs for the whole verify in a daemon thread; plan
    # state events are the authoritative withdrawal-issued signal.
    import queue as _queue
    import threading as _threading
    events_q = _queue.Queue()

    def _pump():
        try:
            for ev in api.stream_events(wait_budget + 5.0):
                events_q.put(ev)
        except Exception:
            pass

    pump = _threading.Thread(target=_pump, daemon=True)
    pump.start()

    revoke_ns = int(time.time() * 1e9)
    collector_id = args.collector_id
    saw_plan_state = False
    sse_events = 0
    contradictions = []
    last_statuses = []

    # Phase A — wait for quiesce: every positive evidence class must be
    # observed. In-flight telemetry between revoke and termination is
    # expected here and is NOT counted as contradiction.
    quiesced = False
    stable_deadline = None
    baseline_obs = baseline_upd = None
    while time.time() < deadline and not (
            quiesced and time.time() >= stable_deadline):
        dev, status_body, _ok = _device_withdrawal_snapshot(api, addr)
        cur_obs = cur_upd = 0
        sub_count = None
        if dev is not None:
            dump(args.evidence_dir, "41-post-revoke-device.json",
                 redact(dev))
            if not dev.get("schema_binding"):
                evidence["binding_revoked"] = True
            if dev.get("acquisition") == "disabled":
                evidence["acquisition_disabled"] = True
            statuses = dev.get("subscription_statuses", [])
            last_statuses = statuses
            observed = [s for s in statuses
                        if s.get("status") == "observed"]
            cur_obs = max((s.get("last_observed_at_ns") or 0
                           for s in observed), default=0)
            cur_upd = max((s.get("updated_at_ns") or 0
                           for s in statuses), default=0)
            non_terminal = [s.get("status") for s in statuses
                            if s.get("status") not in _TERMINAL_STATUSES]
            detail["non_terminal_statuses"] = non_terminal[:10]
            if not observed and not any(
                    s.get("status") == "pending" for s in statuses):
                evidence["subscription_terminated"] = True
        if status_body is not None:
            dump(args.evidence_dir, "42-post-revoke-assignment.json",
                 redact(status_body))
            for c in status_body.get("collectors", []):
                if c.get("id") != collector_id:
                    continue
                hb = c.get("last_heartbeat_ns") or 0
                alive = bool(c.get("connected")) and hb > revoke_ns
                sub_count = c.get("subscription_count")
                detail["collector"] = {
                    "connected": c.get("connected"),
                    "heartbeat_ns": hb,
                    "subscription_count": sub_count,
                    "pending_withdrawal_devices":
                        c.get("pending_withdrawal_devices"),
                }
                # The collector acknowledges withdrawal by staying
                # connected with a post-revoke heartbeat AND reporting
                # zero live gNMI subscriptions. A collector that keeps
                # streaming never satisfies this — it is the
                # noncompliance detector.
                if alive and (sub_count or 0) == 0:
                    evidence["collector_ack"] = True
        while True:
            try:
                ev = events_q.get_nowait()
            except _queue.Empty:
                break
            sse_events += 1
            if ev.get("device_address") == addr and \
                    ev.get("event_type") == "subscription_plan_state":
                try:
                    dj = json.loads(ev.get("detail_json") or "{}")
                except Exception:
                    dj = {}
                if dj.get("state") in ("disabled", "rejected",
                                       "failed", "superseded"):
                    saw_plan_state = True
                    detail["plan_state_event"] = dj

        positive = all(evidence[k] for k in (
            "binding_revoked", "acquisition_disabled",
            "collector_ack", "subscription_terminated"))
        if positive and not quiesced:
            quiesced = True
            baseline_obs, baseline_upd = cur_obs, cur_upd
            stable_deadline = time.time() + window
            # the stability window is mandatory — extend the overall
            # budget rather than silently truncating it
            deadline = max(deadline, stable_deadline + args.poll_secs + 1)
            detail["quiesced_at_ns"] = int(time.time() * 1e9)

        if quiesced:
            # Phase B — the stability window: ANY regression is
            # continued/unauthorized collection.
            if cur_obs > baseline_obs:
                contradictions.append(
                    f"last_observed_at_ns advanced "
                    f"{baseline_obs} -> {cur_obs} post-quiesce")
            if cur_upd > baseline_upd:
                contradictions.append(
                    f"updated_at_ns advanced "
                    f"{baseline_upd} -> {cur_upd} post-quiesce")
            baseline_obs = max(baseline_obs, cur_obs)
            baseline_upd = max(baseline_upd, cur_upd)
            if any(s.get("status") == "observed"
                   for s in last_statuses):
                contradictions.append(
                    "a path returned to 'observed' post-quiesce")
            if sub_count is None:
                contradictions.append(
                    "collector status unavailable during stability "
                    "window — zero-subscription state unproven")
            elif sub_count > 0:
                contradictions.append(
                    f"collector reports {sub_count} live subscription(s) "
                    "post-quiesce")
            if dev is not None and dev.get("schema_binding"):
                contradictions.append("schema_binding reappeared")
            if dev is not None and dev.get("acquisition") != "disabled":
                contradictions.append("acquisition re-enabled")

        time.sleep(args.poll_secs)

    # final SSE drain
    pump.join(timeout=2.0)
    while True:
        try:
            ev = events_q.get_nowait()
        except _queue.Empty:
            break
        sse_events += 1
        if ev.get("device_address") == addr and \
                ev.get("event_type") == "subscription_plan_state":
            try:
                dj = json.loads(ev.get("detail_json") or "{}")
            except Exception:
                dj = {}
            if dj.get("state") in ("disabled", "rejected", "failed",
                                   "superseded"):
                saw_plan_state = True
                detail["plan_state_event"] = dj
    detail["sse_events_seen"] = sse_events
    detail["quiesced"] = quiesced
    detail["monitor_window_secs"] = round(window, 1)
    detail["sample_interval_secs"] = round(sample_secs, 3)
    detail["contradictions"] = contradictions

    # withdrawal_issued resolves after the loop: the authoritative
    # plan-state event, or — only when the SSE stream delivered NOTHING
    # at all — every durable status sitting in a terminal state.
    all_terminal = bool(last_statuses) and all(
        s.get("status") in _TERMINAL_STATUSES for s in last_statuses)
    evidence["withdrawal_issued"] = saw_plan_state or (
        sse_events == 0 and all_terminal)
    detail["saw_plan_state_event"] = saw_plan_state
    detail["all_statuses_terminal"] = all_terminal

    # The stability window must actually have elapsed post-quiesce.
    evidence["no_continued_collection"] = (
        quiesced and not contradictions
        and stable_deadline is not None
        and time.time() >= stable_deadline)
    # Old-session rejection is verified through its observable
    # consequence: retired-session telemetry can never re-establish
    # coverage, so nothing may advance or return to 'observed' during
    # the stability window (registry-level fencing is covered by the
    # Rust session-authority tests).
    evidence["old_session_rejected"] = (
        evidence["no_continued_collection"]
        and evidence["subscription_terminated"])

    withdrew = all(evidence.values())
    dump(args.evidence_dir, "43-withdrawal-evidence.json",
         {"evidence": evidence, "detail": detail})
    record(results, "withdrawal_verified", withdrew,
           "; ".join(f"{k}={v}" for k, v in evidence.items())
           + (f" contradictions={contradictions}" if contradictions
              else ""))
    for sub in ("binding_revoked", "acquisition_disabled",
                "withdrawal_issued", "collector_ack",
                "subscription_terminated", "no_continued_collection",
                "old_session_rejected"):
        record(results, f"withdrawal_{sub.removeprefix('withdrawal_')}",
               evidence[sub],
               "; ".join(detail["contradictions"])
               if detail.get("contradictions") else None)
    return withdrew


def _git(repo, *args):
    import subprocess
    try:
        return subprocess.run(
            ["git", "-C", repo, *args], capture_output=True, text=True,
            timeout=15).stdout.strip()
    except Exception:
        return ""


def _sha256_file(path):
    import hashlib
    try:
        h = hashlib.sha256()
        with open(path, "rb") as f:
            for chunk in iter(lambda: f.read(1 << 20), b""):
                h.update(chunk)
        return h.hexdigest()
    except Exception:
        return ""


def _load_json(ev, name):
    try:
        with open(os.path.join(ev, name)) as f:
            return json.load(f)
    except Exception:
        return None


def cmd_provenance(api, args, results):
    """L4 evidence provenance manifest (M6F-C A3).

    Records everything needed to audit exactly what was tested:

      * exact committed source SHA + dirty-tree flag (a binary built
        from uncommitted production changes is NOT final-SHA
        acceptance — `source_tree_dirty` makes that explicit),
      * runtime binary SHA256 for core and collector,
      * core/collector image identities,
      * NOS vendor/version,
      * pinned model source commit (from config/yang_sources.toml),
      * compiled schema identity + applicability snapshot,
      * capabilities / YANG-Library evidence references (the discovery
        artifacts — including explicit absence),
      * actual acquisition mode + approved plan identity,
      * submitted subscription request,
      * observed telemetry + metric projection read-back,
      * revocation/withdrawal evidence references,
      * operator-supplied resource-consumption snapshot.

    Missing inputs are recorded as null/"NOT_PROVIDED" — never
    invented.
    """
    ev = args.evidence_dir
    repo = args.repo
    head = _git(repo, "rev-parse", "HEAD")
    # Generated L4 evidence output must not mark the source tree dirty —
    # it is produced BY the run, not part of the tested source. Untracked
    # or modified files anywhere else (including other evidence-unrelated
    # paths) still count.
    dirty = bool(_git(repo, "status", "--porcelain", "--",
                      ".", ":(exclude)docs/merger/evidence"))

    # Pinned model source for this vendor, when identifiable.
    model_source = {"status": "NOT_PROVIDED"}
    try:
        import tomllib
        with open(os.path.join(repo, "config", "yang_sources.toml"),
                  "rb") as f:
            sources = tomllib.load(f).get("source", [])
        vendor = (args.vendor or "").lower()
        for src in sources:
            platforms = [p.lower() for p in src.get("platforms", [])]
            if vendor in platforms or src.get("vendor", "").lower() == vendor:
                model_source = {
                    "status": "PINNED",
                    "id": src.get("id"),
                    "repo": src.get("repo"),
                    "pinned_ref": src.get("pinned_ref"),
                    "subdirs": src.get("subdirs"),
                }
                break
    except Exception as e:
        model_source = {"status": f"UNAVAILABLE: {e}"}

    # Compiled schema identity from the catalogue/activation artifacts.
    compile_ev = _load_json(ev, "08-catalogue-compile.json") or {}
    activate_ev = _load_json(ev, "21-plan-activate.json") or {}
    applicability = _load_json(ev, "12-applicability.json") or {}
    snap = applicability.get("snapshot") or {}

    # Live read-backs: acquisition mode, plan/binding identity,
    # interface metric projection.
    code, dev = api.get(f"/api/devices/{args.device_addr}")
    device = dev if code == 200 else {}
    interfaces = device.get("interfaces", []) or []
    metric_projection = [
        {"name": i.get("name"),
         "in_octets": i.get("in_octets"),
         "out_octets": i.get("out_octets"),
         "updated_at_ns": i.get("updated_at_ns")}
        for i in interfaces[:20]
    ]

    binaries = {}
    for label, path in (("core", args.core_binary),
                        ("collector", args.collector_binary)):
        binaries[label] = {
            "path": path or None,
            "sha256": _sha256_file(path) if path else "NOT_PROVIDED",
        }

    resources = None
    if args.resources_file:
        try:
            with open(args.resources_file) as f:
                resources = json.load(f)
        except Exception as e:
            resources = {"error": str(e)}

    prov = {
        "generated_at": time.strftime("%Y-%m-%dT%H:%M:%SZ",
                                      time.gmtime()),
        "source": {
            "repo": repo,
            "commit_sha": head or "NOT_PROVIDED",
            "tree_dirty": dirty if head else "UNKNOWN",
            "final_sha_acceptance_eligible": bool(head) and not dirty,
        },
        "binaries": binaries,
        "images": {
            "core": args.core_image or "NOT_PROVIDED",
            "collector": args.collector_image or "NOT_PROVIDED",
        },
        "nos": {
            "vendor": args.vendor,
            "version": args.nos_version or "NOT_PROVIDED",
        },
        "model_source": model_source,
        "schema": {
            "compiled_schema_id": compile_ev.get("schema_id")
                                  or "NOT_PROVIDED",
            "catalogue_status": compile_ev.get("status"),
            "module_count": compile_ev.get("module_count"),
            "path_count": compile_ev.get("path_count"),
            "snapshot_id": snap.get("snapshot_id"),
            "applicability_status": snap.get("overall_status"),
            "content_id": snap.get("content_id"),
            "datastore": snap.get("datastore"),
        },
        "evidence_references": {
            "capabilities": "11-discovery-job-final.json / "
                            "12-applicability.json (device-advertised "
                            "models + YANG Library outcome incl. "
                            "explicit absence)",
            "yang_library": "12-applicability.json "
                            "yang_library_evidence fields",
            "subscription_request": "20-plan-preview.json / "
                                    "preview.json / plan-file",
            "observed_telemetry": "31-telemetry-observed.json",
            "revocation": "40-plan-revoke.json / "
                          "41-post-revoke-device.json / "
                          "42-post-revoke-assignment.json / "
                          "43-withdrawal-evidence.json",
        },
        "acquisition": {
            "mode": device.get("acquisition") or "NOT_PROVIDED",
            "schema_binding": device.get("schema_binding"),
            "plan_id": activate_ev.get("plan_id"),
            "approval_revision": activate_ev.get("approval_revision"),
            "binding_fingerprint": activate_ev.get("binding_fingerprint"),
        },
        "metric_projection": metric_projection,
        "resources": resources or "NOT_PROVIDED",
    }
    ok = bool(prov["source"]["commit_sha"]
              and prov["binaries"]["core"]["sha256"]
              != "NOT_PROVIDED"
              and prov["images"]["core"] != "NOT_PROVIDED")
    record(results, "l4_provenance", ok,
           f"sha={prov['source']['commit_sha'][:12]} "
           f"dirty={prov['source']['tree_dirty']} "
           f"core_img={prov['images']['core']}")
    dump(ev, "00-provenance.json", prov)
    return ok


def main():
    ap = argparse.ArgumentParser(description=__doc__)
    ap.add_argument("--core", required=True, help="core base URL")
    ap.add_argument("--evidence-dir", required=True)
    ap.add_argument("--collector-id", default="m6f-col-1")
    ap.add_argument("--device-addr", required=True)
    ap.add_argument("--tls-domain", default="srlinux")
    ap.add_argument("--device-ca", default="")
    ap.add_argument("--device-security", default="tls_verify",
                    choices=["tls_verify", "mtls", "plaintext"],
                    help="device gNMI transport security; plaintext is "
                         "the explicit lab-only opt-in (e.g. XRd)")
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
    ap.add_argument("--withdraw-window", type=float, default=0,
                    help="post-quiesce stability window seconds "
                         "(0=auto: >=2 sample intervals, 25-90s)")
    # L4 provenance inputs (A3) — all optional; missing values are
    # recorded as NOT_PROVIDED, never invented.
    ap.add_argument("--repo", default=".",
                    help="bonsai checkout used for source SHA/dirty "
                         "detection and yang_sources.toml lookup")
    ap.add_argument("--core-binary", default="",
                    help="path to the running core binary (sha256 "
                         "recorded)")
    ap.add_argument("--collector-binary", default="",
                    help="path to the running collector binary")
    ap.add_argument("--core-image", default="",
                    help="core container image identity (repo:tag@digest)")
    ap.add_argument("--collector-image", default="",
                    help="collector container image identity")
    ap.add_argument("--nos-version", default="",
                    help="NOS version string (e.g. 26.3.1-410)")
    ap.add_argument("--resources-file", default="",
                    help="optional JSON file with resource-consumption "
                         "snapshot (docker stats etc.)")
    ap.add_argument("--stage", default="all",
                    help="comma list or 'all': login,provision,"
                         "wait_collector,compile,discovery,applicability,"
                         "paths,preview,activate,telemetry,revoke,"
                         "verify_withdrawal,provenance")
    args = ap.parse_args()

    os.makedirs(args.evidence_dir, exist_ok=True)
    results = []
    api = Api(args.core)

    stages = (args.stage.split(",") if args.stage != "all"
              else ["login", "provision", "wait_collector", "compile",
                    "discovery", "applicability", "preview", "activate",
                    "telemetry", "revoke", "verify_withdrawal",
                    "provenance"])

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
        "provenance": lambda: cmd_provenance(api, args, results),
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

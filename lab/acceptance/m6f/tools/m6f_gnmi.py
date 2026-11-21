#!/usr/bin/env python3
"""m6f_gnmi.py — minimal read-only gNMI probe for M6F multi-vendor
interoperability acceptance.

Generates gNMI stubs from proto/gnmi.proto at run time (into a temp dir),
then performs Capabilities, YANG Library Get (RFC 8525 and legacy) and a
single-shot Subscribe. Emits JSON evidence on stdout; diagnostics on
stderr. Never writes device state (no Set RPC is implemented).

Auth: gNMI has no credential field in the request; lab NOS devices
commonly accept HTTP/2 `username`/`password` metadata headers. We send
those plus an optional `authorization: Bearer` when configured. TLS can
use a CA file + server name, or be fully insecure for lab images.

Usage:
  m6f_gnmi.py ensure-stubs --proto-dir DIR --out DIR
  m6f_gnmi.py caps --addr HOST:PORT [tls opts] [auth opts]
  m6f_gnmi.py yanglib --addr ... --variant ietf|legacy
  m6f_gnmi.py subscribe-once --addr ... --path X [--mode on_change|sample|once]
  m6f_gnmi.py get --addr ... --path X

Common options:
  --ca FILE            PEM CA bundle for TLS
  --server-name NAME   TLS SNI / cert name override
  --insecure           plaintext or skip verification (lab only)
  --user-env NAME      env var holding username (never pass secrets on CLI)
  --pass-env NAME      env var holding password
  --token-env NAME     env var holding bearer token
"""
import argparse
import datetime
import json
import os
import subprocess
import sys
import tempfile


def log(*a):
    print(*a, file=sys.stderr)


def ensure_stubs(proto_dir, out_dir):
    os.makedirs(out_dir, exist_ok=True)
    cmd = [
        sys.executable, "-m", "grpc_tools.protoc",
        f"-I{proto_dir}",
        f"--python_out={out_dir}",
        f"--grpc_python_out={out_dir}",
        "gnmi.proto",
    ]
    r = subprocess.run(cmd, capture_output=True, text=True)
    if r.returncode != 0:
        log("protoc failed:", r.stderr)
        sys.exit(2)
    # gnmi_ext is a different proto package; gnmi.proto imports it, so
    # compile it too (needed for annotations/extend resolution at runtime
    # only if referenced — generate anyway for completeness).
    subprocess.run([
        sys.executable, "-m", "grpc_tools.protoc",
        f"-I{proto_dir}",
        f"--python_out={out_dir}",
        f"--grpc_python_out={out_dir}",
        "gnmi_ext/gnmi_ext.proto",
    ], capture_output=True, text=True)
    init = os.path.join(out_dir, "__init__.py")
    if not os.path.exists(init):
        open(init, "w").close()
    ext_dir = os.path.join(out_dir, "gnmi_ext")
    ext_init = os.path.join(ext_dir, "__init__.py")
    if os.path.isdir(ext_dir) and not os.path.exists(ext_init):
        open(ext_init, "w").close()
    # Fix grpc_tools absolute-import quirk inside package dirs
    for dirpath, _dirs, files in os.walk(out_dir):
        for fn in files:
            if fn.endswith("_pb2_grpc.py"):
                p = os.path.join(dirpath, fn)
                t = open(p).read()
                t = t.replace("import gnmi_pb2", "from . import gnmi_pb2")
                t = t.replace("import gnmi_ext_pb2", "from . import gnmi_ext_pb2")
                open(p, "w").write(t)
    return out_dir


def load_stubs(out_dir):
    import importlib
    parent = os.path.dirname(os.path.abspath(out_dir))
    # generated code mixes package-relative and top-level imports
    # (`from gnmi_ext import gnmi_ext_pb2`), so expose both levels
    for p in (parent, os.path.abspath(out_dir)):
        if p not in sys.path:
            sys.path.insert(0, p)
    pkg = os.path.basename(os.path.abspath(out_dir))
    importlib.import_module(pkg)
    pb = importlib.import_module(f"{pkg}.gnmi_pb2")
    grpc_mod = importlib.import_module(f"{pkg}.gnmi_pb2_grpc")
    return pb, grpc_mod


def build_channel(addr, args):
    import grpc
    opts = [("grpc.max_receive_message_length", 64 * 1024 * 1024),
            ("grpc.max_send_message_length", 64 * 1024 * 1024)]
    if args.insecure:
        return grpc.insecure_channel(addr, options=opts)
    creds = grpc.ssl_channel_credentials(
        root_certificates=_read(args.ca) if args.ca else None)
    return grpc.secure_channel(addr, creds, options=opts)


def _read(p):
    with open(p, "rb") as f:
        return f.read()


def metadata_headers(args):
    md = []
    user = os.environ.get(args.user_env) if args.user_env else None
    pw = os.environ.get(args.pass_env) if args.pass_env else None
    tok = os.environ.get(args.token_env) if args.token_env else None
    if user is not None:
        md.append(("username", str(user)))
    if pw is not None:
        md.append(("password", str(pw)))
    if tok is not None:
        md.append(("authorization", f"Bearer {tok}"))
    return md


def gnmi_meta():
    return {"timestamp": int(datetime.datetime.now(
        datetime.timezone.utc).timestamp() * 1e9)}


def encode_path(path):
    """Parse 'a/b[key=v]/c' into (origin, elem list)."""
    origin = ""
    if ":" in path.split("/", 1)[0]:
        origin, path = path.split(":", 1)
    elems = []
    for seg in path.strip("/").split("/"):
        if not seg:
            continue
        name, _, rest = seg.partition("[")
        e = {"name": name}
        if rest:
            e["key"] = {}
            for kv in rest.rstrip("]").split(","):
                k, _, v = kv.partition("=")
                e["key"][k.strip()] = v.strip().strip("'\"")
        elems.append(e)
    return origin, elems


def make_path(path):
    g = _gnmi_pb()
    origin, elems = encode_path(path)
    return g.Path(origin=origin, elem=elems)


def _gnmi_pb():
    return _STUBS[0]


_STUBS = None


def cmd_caps(args):
    global _STUBS
    pb, grpc_stub = _STUBS
    md = metadata_headers(args)
    with build_channel(args.addr, args) as ch:
        stub = grpc_stub.gNMIStub(ch)
        req = pb.CapabilityRequest()
        try:
            resp = stub.Capabilities(req, metadata=md, timeout=args.timeout)
            out = {
                "ok": True,
                "supported_models": [
                    {"name": m.name, "organization": m.organization,
                     "version": m.version}
                    for m in resp.supported_models],
                "supported_encodings": [
                    _enum_name(pb.Encoding, e)
                    for e in resp.supported_encodings],
                "gnmi_version": getattr(resp, "gNMI_version",
                                       getattr(resp, "gnmi_version", "")),
            }
        except Exception as e:  # noqa: BLE001 - report, don't crash
            out = {"ok": False, "error": f"{type(e).__name__}: {e}"}
    print(json.dumps(out, indent=2, default=str))
    return 0 if out["ok"] else 1


def cmd_get(args):
    pb, grpc_stub = _STUBS
    md = metadata_headers(args)
    req = pb.GetRequest(
        prefix=pb.Path(origin=args.origin) if args.origin else None,
        path=[make_path(args.path)],
        type=pb.GetRequest.ALL,
        encoding=pb.Encoding.Value(args.encoding.upper()),
        use_models=[],
    )
    with build_channel(args.addr, args) as ch:
        stub = grpc_stub.gNMIStub(ch)
        try:
            resp = stub.Get(req, metadata=md, timeout=args.timeout)
            out = {"ok": True, "notifications": [_notif(n) for n in resp.notification]}
        except Exception as e:  # noqa: BLE001
            out = {"ok": False, "error": f"{type(e).__name__}: {e}"}
    print(json.dumps(out, indent=2, default=str))
    return 0 if out["ok"] else 1


def cmd_yanglib(args):
    # RFC 8525 module is ietf-yang-library; SR Linux rejects the
    # 'yang-library:' prefix alias (observed live: "Namespace of
    # 'yang-library:yang-library' does not match module namespace
    # 'ietf-yang-library'"). Try ietf name first, fall back to alias.
    variants = {
        "ietf": [("ietf-yang-library:yang-library", "JSON_IETF")],
        "legacy": [("modules-state", "JSON_IETF"),
                   ("modules-state", "ASCII")],
    }[args.variant]
    last = None
    for path, enc in variants:
        args.path, args.encoding, args.origin = path, enc, ""
        import io, contextlib
        buf = io.StringIO()
        with contextlib.redirect_stdout(buf):
            rc = cmd_get(args)
        out = buf.getvalue()
        if rc == 0:
            print(out)
            return 0
        last = out
    print(last)
    return 1


def cmd_subscribe_once(args):
    pb, grpc_stub = _STUBS
    md = metadata_headers(args)
    sub_mode = {
        "on_change": pb.ON_CHANGE,
        "sample": pb.SAMPLE,
        "once": pb.TARGET_DEFINED,
    }[args.mode]
    list_mode = pb.SubscriptionList.ONCE if args.mode == "once" \
        else pb.SubscriptionList.STREAM
    sub = pb.Subscription(path=make_path(args.path),
                          mode=sub_mode,
                          sample_interval=int(args.interval_ns)
                          if args.interval_ns else 0,
                          suppress_redundant=args.suppress)
    req = pb.SubscribeRequest(
        subscribe=pb.SubscriptionList(
            subscription=[sub], encoding=pb.Encoding.Value(
                args.encoding.upper()),
            mode=list_mode,
            updates_only=False))
    notifs, err = [], None
    deadline = args.timeout
    import time
    t0 = time.time()
    with build_channel(args.addr, args) as ch:
        stub = grpc_stub.gNMIStub(ch)
        try:
            stream = stub.Subscribe(iter([req]), metadata=md)
            for resp in stream:
                if resp.HasField("sync_response"):
                    notifs.append({"sync_response": True})
                elif resp.HasField("update"):
                    notifs.append(_notif(resp.update))
                if resp.HasField("sync_response") or \
                        time.time() - t0 > deadline:
                    break
        except Exception as e:  # noqa: BLE001
            err = f"{type(e).__name__}: {e}"
    out = {"ok": err is None, "error": err, "responses": notifs,
           "count": len(notifs)}
    if err is None and not notifs:
        out["ok"] = False
        out["error"] = "no responses before deadline"
    print(json.dumps(out, indent=2, default=str))
    return 0 if out["ok"] else 1


def _enum_name(enum_wrapper, value):
    try:
        return enum_wrapper.Name(value)
    except (ValueError, KeyError):
        return f"UNKNOWN({value})"


def _notif(n):
    """Best-effort JSON rendering of a Notification."""
    try:
        from google.protobuf.json_format import MessageToDict
        return MessageToDict(n, preserving_proto_field_name=True)
    except Exception:  # noqa: BLE001
        return {"timestamp": str(n.timestamp),
                "prefix": str(n.prefix),
                "delete": list(n.delete) if n.delete else []}


def main():
    global _STUBS
    ap = argparse.ArgumentParser(description=__doc__)
    sub = ap.add_subparsers(dest="cmd", required=True)

    def common(p):
        p.add_argument("--addr", required=True)
        p.add_argument("--ca")
        p.add_argument("--server-name")
        p.add_argument("--insecure", action="store_true")
        p.add_argument("--user-env")
        p.add_argument("--pass-env")
        p.add_argument("--token-env")
        p.add_argument("--timeout", type=float, default=15.0)

    p = sub.add_parser("ensure-stubs")
    p.add_argument("--proto-dir", required=True)
    p.add_argument("--out", required=True)

    p = sub.add_parser("caps"); common(p); p.set_defaults(fn=cmd_caps)

    p = sub.add_parser("get"); common(p)
    p.add_argument("--path", required=True)
    p.add_argument("--origin", default="")
    p.add_argument("--encoding", default="JSON_IETF")
    p.set_defaults(fn=cmd_get)

    p = sub.add_parser("yanglib"); common(p)
    p.add_argument("--variant", choices=["ietf", "legacy"],
                   default="ietf")
    p.set_defaults(fn=cmd_yanglib)

    p = sub.add_parser("subscribe-once"); common(p)
    p.add_argument("--path", required=True)
    p.add_argument("--origin", default="")
    p.add_argument("--encoding", default="JSON_IETF")
    p.add_argument("--mode", choices=["once", "on_change", "sample"],
                   default="once")
    p.add_argument("--interval-ns", type=int, default=0)
    p.add_argument("--suppress", action="store_true")
    p.set_defaults(fn=cmd_subscribe_once)

    args = ap.parse_args()
    if args.cmd == "ensure-stubs":
        print(ensure_stubs(args.proto_dir, args.out))
        return 0
    # load stubs lazily for RPC commands; the stub dir must be a valid
    # python identifier (it is imported as a package)
    stub_dir = os.environ.get(
        "M6F_GNMI_STUB_DIR",
        os.path.join(tempfile.gettempdir(), "m6f_gnmi_stubs"))
    if not os.path.exists(os.path.join(stub_dir, "gnmi_pb2.py")):
        ensure_stubs(os.environ.get("M6F_PROTO_DIR", "proto"), stub_dir)
    _STUBS = load_stubs(stub_dir)
    return args.fn(args)


if __name__ == "__main__":
    sys.exit(main())

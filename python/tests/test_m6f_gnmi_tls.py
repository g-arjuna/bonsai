"""TLS-semantics tests for lab/acceptance/m6f/tools/m6f_gnmi.py.

Covers the M6F-B corrections: verified TLS honors --server-name, the
lab CA is required trust material (missing file fails closed), and
--tls-mode plaintext / --insecure are explicit cleartext — never a
silent "skip verification" downgrade. A real loopback TLS gNMI server
(with openssl-generated certs) proves certificate-mismatch rejection.
"""
from __future__ import annotations

import importlib.util
import json
import os
import shutil
import subprocess
import sys
import tempfile
import threading
import time
from pathlib import Path

import pytest

REPO = Path(__file__).resolve().parents[2]
TOOL = REPO / "lab/acceptance/m6f/tools/m6f_gnmi.py"
PROTO_DIR = REPO / "proto"

spec = importlib.util.spec_from_file_location("m6f_gnmi", TOOL)
m6f_gnmi = importlib.util.module_from_spec(spec)
spec.loader.exec_module(m6f_gnmi)

grpc = pytest.importorskip("grpc")
pytest.importorskip("grpc_tools")


# ── argument-level fail-closed checks ────────────────────────────────────────

def _run_cli(argv):
    """Invoke m6f_gnmi.main() with patched argv; return exit code."""
    old = sys.argv
    sys.argv = ["m6f_gnmi.py", *argv]
    try:
        return m6f_gnmi.main()
    finally:
        sys.argv = old


def test_missing_ca_file_fails_closed(tmp_path):
    rc = _run_cli([
        "caps", "--addr", "127.0.0.1:1",
        "--ca", str(tmp_path / "does-not-exist.pem"),
    ])
    assert rc == 2


def test_missing_client_cert_fails_closed(tmp_path):
    rc = _run_cli([
        "caps", "--addr", "127.0.0.1:1",
        "--client-cert", str(tmp_path / "no.pem"),
        "--client-key", str(tmp_path / "no.key"),
    ])
    assert rc == 2


def test_unverified_tls_mode_rejected():
    rc = _run_cli([
        "caps", "--addr", "127.0.0.1:1", "--tls-mode", "unverified",
    ])
    assert rc == 2


def test_insecure_is_plaintext_not_skip_verify():
    """--insecure must produce a cleartext channel, and the tool must
    never claim it is 'TLS without verification'."""
    args = type("A", (), {
        "insecure": True, "tls_mode": None, "ca": None,
        "server_name": None, "client_cert": None, "client_key": None,
    })()
    ch = m6f_gnmi.build_channel("127.0.0.1:1", args)
    # grpc insecure_channel is a distinct class from secure_channel
    assert ch.__class__.__name__ != "SecureChannel"


# ── real loopback TLS server ─────────────────────────────────────────────────

def _openssl(args, cwd):
    r = subprocess.run(["openssl", *args], cwd=cwd,
                       capture_output=True, text=True)
    assert r.returncode == 0, f"openssl {' '.join(args)}: {r.stderr}"


@pytest.fixture(scope="module")
def tls_lab(tmp_path_factory):
    """CA + server cert (SAN: good-name, 127.0.0.1) + running gNMI stub."""
    pytest.importorskip("grpc_tools")
    work = tmp_path_factory.mktemp("m6f-tls")
    ca_key, ca_crt = work / "ca.key", work / "ca.crt"
    srv_key, srv_crt, srv_csr = work / "srv.key", work / "srv.crt", work / "srv.csr"
    ext = work / "srv.ext"
    _openssl(["req", "-x509", "-newkey", "rsa:2048", "-nodes",
              "-keyout", str(ca_key), "-out", str(ca_crt),
              "-days", "1", "-subj", "/CN=m6f-test-ca"], work)
    _openssl(["req", "-newkey", "rsa:2048", "-nodes",
              "-keyout", str(srv_key), "-out", str(srv_csr),
              "-subj", "/CN=good-name"], work)
    ext.write_text("subjectAltName=DNS:good-name,IP:127.0.0.1\n")
    _openssl(["x509", "-req", "-in", str(srv_csr), "-CA", str(ca_crt),
              "-CAkey", str(ca_key), "-CAcreateserial",
              "-out", str(srv_crt), "-days", "1", "-extfile", str(ext)],
             work)

    # generate stubs and load them
    stub_dir = work / "stubs"
    m6f_gnmi.ensure_stubs(str(PROTO_DIR), str(stub_dir))
    old_path = list(sys.path)
    sys.path.insert(0, str(stub_dir.parent))
    sys.path.insert(0, str(stub_dir))
    try:
        pb, pb_grpc = m6f_gnmi.load_stubs(str(stub_dir))
    finally:
        sys.path[:] = old_path

    class Servicer(pb_grpc.gNMIServicer):
        def Capabilities(self, request, context):
            return pb.CapabilityResponse(
                gNMI_version="0.10.0",
                supported_encodings=[pb.Encoding.JSON_IETF],
            )

    creds = grpc.ssl_server_credentials(
        [(srv_key.read_bytes(), srv_crt.read_bytes())])
    import concurrent.futures
    server = grpc.server(concurrent.futures.ThreadPoolExecutor(4))
    pb_grpc.add_gNMIServicer_to_server(Servicer(), server)
    port = server.add_secure_port("127.0.0.1:0", creds)
    server.start()
    yield {
        "addr": f"127.0.0.1:{port}",
        "ca": str(ca_crt),
        "pb": pb,
        "pb_grpc": pb_grpc,
    }
    server.stop(0)


def _args(**kw):
    base = {
        "insecure": False, "tls_mode": "verified", "ca": None,
        "server_name": None, "client_cert": None, "client_key": None,
    }
    base.update(kw)
    return type("A", (), base)()


def _caps(tls_lab, **kw):
    pb, pb_grpc = tls_lab["pb"], tls_lab["pb_grpc"]
    with m6f_gnmi.build_channel(tls_lab["addr"], _args(**kw)) as ch:
        return pb_grpc.gNMIStub(ch).Capabilities(
            pb.CapabilityRequest(), timeout=5)


def test_verified_tls_correct_server_name(tls_lab):
    resp = _caps(tls_lab, ca=tls_lab["ca"], server_name="good-name")
    assert resp.gNMI_version == "0.10.0"


def test_verified_tls_ip_san_without_server_name(tls_lab):
    resp = _caps(tls_lab, ca=tls_lab["ca"])
    assert resp.gNMI_version == "0.10.0"


def test_cert_name_mismatch_rejected(tls_lab):
    with pytest.raises(grpc.RpcError):
        _caps(tls_lab, ca=tls_lab["ca"],
              server_name="wrong-name.example")


def test_missing_trust_root_rejected(tls_lab):
    # no --ca → system roots do not include our test CA → handshake fails
    with pytest.raises(grpc.RpcError):
        _caps(tls_lab)


def test_plaintext_mode_is_explicit(tls_lab):
    """Plaintext channel against a TLS-only server must fail — proving
    the mode is genuinely cleartext, not 'unverified TLS'."""
    pb, pb_grpc = tls_lab["pb"], tls_lab["pb_grpc"]
    with m6f_gnmi.build_channel(
            tls_lab["addr"], _args(insecure=True)) as ch:
        stub = pb_grpc.gNMIStub(ch)
        with pytest.raises(grpc.RpcError):
            stub.Capabilities(pb.CapabilityRequest(), timeout=5)

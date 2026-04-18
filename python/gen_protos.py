#!/usr/bin/env python3
"""Generate Python gRPC stubs from proto/bonsai_service.proto.

Run once after cloning or when the proto changes:
    python python/gen_protos.py
"""
import subprocess
import sys
from pathlib import Path

ROOT = Path(__file__).parent.parent
PROTO_DIR = ROOT / "proto"
OUT_DIR = ROOT / "python" / "generated"

OUT_DIR.mkdir(parents=True, exist_ok=True)

# Write an __init__.py so the generated package is importable
(OUT_DIR / "__init__.py").write_text("")

cmd = [
    sys.executable, "-m", "grpc_tools.protoc",
    f"-I{PROTO_DIR}",
    f"--python_out={OUT_DIR}",
    f"--grpc_python_out={OUT_DIR}",
    str(PROTO_DIR / "bonsai_service.proto"),
]

print("Running:", " ".join(cmd))
result = subprocess.run(cmd, check=False)
if result.returncode != 0:
    print("protoc failed — is grpcio-tools installed? pip install grpcio-tools")
    sys.exit(result.returncode)

# Fix relative imports in generated files (grpcio-tools emits absolute imports)
for f in OUT_DIR.glob("*_pb2_grpc.py"):
    text = f.read_text()
    text = text.replace("import bonsai_service_pb2", "from . import bonsai_service_pb2")
    f.write_text(text)

print(f"Stubs written to {OUT_DIR}")

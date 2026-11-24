from fastapi import FastAPI
from pydantic import BaseModel


class ParseRequest(BaseModel):
    # Wire contract shared with docker/sidecars/pyats/app.py and the Rust
    # parser chain (src/parser_chain.rs): `command` is the canonical literal
    # CLI command; `command_pattern` is accepted for older callers.
    parser: str
    vendor: str
    command: str = ""
    command_pattern: str = ""
    raw_output: str


app = FastAPI()


def line_parse(raw_output: str) -> dict:
    lines = [line.strip() for line in raw_output.splitlines() if line.strip()]
    return {
        "result_kind": "unstructured_fallback",
        "backend_used": "line_split",
        "line_count": len(lines),
        "lines": lines,
        "nonempty_preview": lines[:10],
    }


@app.get("/healthz")
def healthz() -> dict:
    return {"ok": True, "service": "bonsai-native-parser"}


@app.post("/parse")
def parse(request: ParseRequest) -> dict:
    command = request.command or request.command_pattern
    parsed = line_parse(request.raw_output)
    parsed["vendor"] = request.vendor
    parsed["command"] = command
    return {
        "parser": request.parser,
        "backend_used": "line_split",
        "vendor": request.vendor,
        "command": command,
        "parsed_json": parsed,
    }

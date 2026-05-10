from fastapi import FastAPI
from pydantic import BaseModel


class ParseRequest(BaseModel):
    parser: str
    vendor: str
    command_pattern: str
    raw_output: str


app = FastAPI()


def fallback_parse(raw_output: str) -> dict:
    lines = [line.strip() for line in raw_output.splitlines() if line.strip()]
    return {"line_count": len(lines), "lines": lines}


@app.get("/healthz")
def healthz() -> dict:
    return {"ok": True, "service": "pyats-sidecar"}


@app.post("/parse")
def parse(request: ParseRequest) -> dict:
    parsed = fallback_parse(request.raw_output)
    parsed["vendor"] = request.vendor
    parsed["command_pattern"] = request.command_pattern
    parsed["backend"] = request.parser
    return {"parser": request.parser, "parsed_json": parsed}

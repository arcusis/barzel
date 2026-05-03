# Barzel stdio JSON Protocol (v1)

**Purpose:** Allow AI coding agents to drive Barzel without parsing human text.

**Invocation:**
```bash
echo '{"command": "init", "project_path": "."}' | barzel --stdio
```

**Request Schema (v1):**
```json
{
  "command": "init" | "run" | "report",
  "project_path": string,
  "layers": ["logic", "structural", "hostile"]?,
  "config_overrides": object?
}
```

**Response Schema (always on stdout):**
```json
{
  "status": "success" | "partial" | "error",
  "request_id": string,
  "data": { ... },
  "errors": [ { "code": string, "message": string } ]?
}
```

**Error Handling:**
- Non-zero exit code on any failure
- Human-readable error on stderr
- Structured error object in JSON response

**Design Rules:**
- Every response is a single JSON object followed by newline
- No streaming in v1 (simple request/response)
- Timeouts enforced at orchestrator level
- Agents can correlate via `request_id` (UUID)

**Example Success Response (init):**
```json
{
  "status": "success",
  "request_id": "0195f0a2-...",
  "data": {
    "created": [".barzel.toml", ".barzel/"],
    "detected_language": "rust",
    "recommended_layers": ["logic", "structural"]
  }
}
```

"""Scripted goldfish model for offline tuning runs: an OpenAI-compatible
`/chat/completions` endpoint whose answers depend on the reviewer guidance in
its prompt. It exists so the tuning loop can be exercised end to end without a
key; it demonstrates the MECHANISM, not that tuning helps a real model.

A defect class is checked only when the prompt's persona/guidance part (never
the design doc itself) contains its trigger phrase:

  vague      always on        imprecise plan ("appropriately", "suitable", ...)
  old-data   "data written by earlier versions"
             the plan changes stored notes but says nothing about notes saved earlier
  ids        "identifiers are never reused"
             the plan deletes notes but never says how new ids avoid deleted ones

usage: python3 persona_reviewer.py PORT LOGFILE
"""
import json
import re
import sys
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
from pathlib import Path

PORT, LOG = int(sys.argv[1]), Path(sys.argv[2])

TRIGGERS = {"old-data": "data written by earlier versions", "ids": "identifiers are never reused"}


def split(prompt: str) -> tuple[str, str]:
    """(design doc text, the rest). The doc sits in the brief as `=== docs/designs/x.md ===`."""
    m = re.search(r"=== (docs/designs/[^\s]+) ===\n(.*?)(?=\n=== |\n\nReview persona|\Z)", prompt, re.S)
    doc = m.group(2) if m else ""
    rest = prompt.replace(doc, "") if doc else prompt
    return doc, rest


def defects(doc: str, guidance: str) -> list[tuple[str, str, str]]:
    """(class, question, search term) for every defect the active rules see."""
    low, g = doc.lower(), guidance.lower()
    found = []
    if any(w in low for w in ["appropriately", "suitable format", "handle efficiently", "export works"]):
        found.append(("vague", "The plan leaves the format and error handling to the implementer; which exactly?", "exact format"))
    stored = "notes.json" in low and any(w in low for w in ["gain", "field", "list"])
    handles_old = any(w in low for w in ["saved before", "missing key", "read as empty", "old notes", "without it", "no migration", 'get("'])
    if TRIGGERS["old-data"] in g and stored and not handles_old:
        found.append(("old-data", "What happens to notes written by earlier versions, which lack the new field?", "earlier versions"))
    deletes = any(w in low for w in ["delete", "purge", "remove"])
    id_policy = any(w in low for w in ["max(", "never reused", "never reuse", "renumber"])
    if TRIGGERS["ids"] in g and deletes and not id_policy:
        found.append(("ids", "After a delete, how does `add` avoid giving a new note the id of a deleted one?", "never reused"))
    return found


def answer(prompt: str) -> dict:
    doc, rest = split(prompt)
    guidance = rest[rest.find("Review persona"):] if "Review persona" in rest else ""
    if "In your own words" in prompt:
        return {"problem": doc.split("## Problem")[-1].strip().split("\n")[0][:120] if doc else "", "current_mechanism": "notes.json",
                "proposed_change": "see the plan", "unclear": []}
    if "Act as an expert reviewer" in prompt or "Zoom out and judge" in prompt:
        return {"findings": []}
    if "engineer who must implement" in prompt:
        qs = [{"question": q, "section": "Technical plan", "quote": "", "search_terms": [t]} for _, q, t in defects(doc, guidance)]
        return {"ready": not qs, "blocking_questions": qs}
    if "precise predicate" in prompt:
        crit = re.findall(r"^- (.+?)\s*Check:", doc.split("## Acceptance criteria")[-1], re.M)
        return {"criteria": [{"criterion": c, "predicate": c, "check": "tests"} for c in crit]}
    return {"text": "(no scripted answer)"}


class Handler(BaseHTTPRequestHandler):
    def log_message(self, *a):
        pass

    def do_POST(self):
        body = json.loads(self.rfile.read(int(self.headers.get("content-length", 0))) or b"{}")
        prompt = "\n".join(m.get("content", "") for m in body.get("messages", []) if m.get("role") == "user")
        reply = json.dumps(answer(prompt))
        with LOG.open("a") as f:
            f.write(json.dumps({"chars": len(prompt), "tuned": "Reviewer guidance" in prompt}) + "\n")
        out = {"id": "x", "object": "chat.completion", "model": body.get("model", "scripted"),
               "choices": [{"index": 0, "message": {"role": "assistant", "content": reply}, "finish_reason": "stop"}],
               "usage": {"prompt_tokens": len(prompt) // 4, "completion_tokens": len(reply) // 4,
                         "prompt_cache_hit_tokens": 0, "prompt_cache_miss_tokens": len(prompt) // 4}}
        data = json.dumps(out).encode()
        self.send_response(200)
        self.send_header("content-type", "application/json")
        self.send_header("content-length", str(len(data)))
        self.end_headers()
        self.wfile.write(data)


ThreadingHTTPServer(("127.0.0.1", PORT), Handler).serve_forever()

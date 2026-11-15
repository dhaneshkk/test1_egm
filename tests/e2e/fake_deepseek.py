"""A fake DeepSeek endpoint for running real DeepSeek Harness (`dsh`) offline.

DSH's DeepSeek adapter speaks the Messages API over SSE at `/v1/messages`.
This server answers from a rule file: each rule has a `match` substring that is
looked up in the current task (the latest human text in the conversation),
and a reply that is either text or one tool call. Rules are consumed in order
per match (a rule with "keep" is never consumed), so a turn can script read -> write -> bash -> final text. Session
title side-requests are answered without consuming rules. Every request is
logged to requests.jsonl.

usage: python3 fake_deepseek.py PORT RULES.json LOG_DIR
"""
import json
import sys
import threading
import time
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
from pathlib import Path

PORT, RULES, LOGDIR = int(sys.argv[1]), Path(sys.argv[2]), Path(sys.argv[3])
LOCK = threading.Lock()
rules = json.loads(RULES.read_text())
SEEN = []  # rendered prompts, to simulate DeepSeek's best-effort prefix cache


def cached_prefix(rendered):
    """Characters of `rendered` covered by the longest prefix shared with an earlier request."""
    best = 0
    for old in SEEN:
        n = min(len(old), len(rendered))
        i = 0
        while i < n and old[i] == rendered[i]:
            i += 1
        best = max(best, i)
    SEEN.append(rendered)
    return best


def current_task(body):
    """Latest human-authored text: skip tool results and runtime-context blocks."""
    for msg in reversed(body.get("messages", [])):
        if msg.get("role") != "user":
            continue
        content = msg["content"]
        if isinstance(content, str):
            return content
        if any(b.get("type") == "tool_result" for b in content):
            continue
        texts = [b["text"] for b in content if b.get("type") == "text" and not b["text"].startswith("Current runtime context")]
        if texts:
            return texts[0]
    return ""


def debit():
    """A test's balance.json may set "debit_per_request": each model request then costs that much."""
    f = LOGDIR / "balance.json"
    with LOCK:
        if not f.exists():
            return
        b = json.loads(f.read_text())
        d = float(b.get("debit_per_request", 0))
        if d:
            for info in b.get("balance_infos", []):
                info["total_balance"] = f"{float(info['total_balance']) - d:.4f}"
            f.write_text(json.dumps(b))


def pick(task, tools=True):
    """The first rule matching `task`. A request without tools (the built-in reviewer)
    skips scripted tool calls: that reviewer has its files in the prompt already."""
    with LOCK:
        for i, rule in enumerate(rules):
            if rule["match"] in task and (tools or "tool" not in rule):
                return rule if rule.get("keep") else rules.pop(i)
    return {"text": "(no scripted reply)", "unmatched": True}


class Handler(BaseHTTPRequestHandler):
    def log_message(self, *args):
        pass

    def do_GET(self):
        """`/user/balance`, answered from LOG_DIR/balance.json when a test provides it."""
        f = LOGDIR / "balance.json"
        if self.path.rstrip("/").endswith("/user/balance") and f.exists():
            data = f.read_bytes()
            self.send_response(200)
        else:
            data = b'{"error": "not found"}'
            self.send_response(404)
        self.send_header("content-type", "application/json")
        self.send_header("content-length", str(len(data)))
        self.end_headers()
        self.wfile.write(data)

    def do_POST(self):
        body = json.loads(self.rfile.read(int(self.headers.get("content-length", 0))) or b"{}")
        debit()
        title = "concise title" in json.dumps(body.get("system", ""))
        task = current_task(body)
        openai = self.path.rstrip("/").endswith("/chat/completions")
        reply = {"text": "Design session"} if title else pick(task, tools=not openai or bool(body.get("tools")))
        if reply.get("delay"):   # a stalled model, for tests that interrupt egm mid-session
            time.sleep(float(reply["delay"]))
        if reply.get("http_error"):   # a provider refusal, e.g. 402 Insufficient Balance
            data = json.dumps({"error": {"message": reply.get("message", "error"), "type": "unknown_error", "param": None, "code": "invalid_request_error"}}).encode()
            self.send_response(int(reply["http_error"]))
            self.send_header("content-type", "application/json")
            self.send_header("content-length", str(len(data)))
            self.end_headers()
            self.wfile.write(data)
            with LOCK, (LOGDIR / "requests.jsonl").open("a") as f:
                f.write(json.dumps({"title": title, "task": task[:120], "reply": "http_error", "messages": len(body.get("messages", []))}) + "\n")
            return
        rendered = json.dumps([body.get("system", ""), body.get("tools", []), body.get("messages", [])])
        with LOCK:
            hit = cached_prefix(rendered) // 4
        miss = max(1, len(rendered) // 4 - hit)
        out_tokens = max(1, len(json.dumps(reply)) // 4)
        with LOCK, (LOGDIR / "requests.jsonl").open("a") as f:
            f.write(json.dumps({"title": title, "task": task[:120], "reply": reply.get("tool") or "text",
                                "messages": len(body.get("messages", [])), "hit": hit, "miss": miss,
                                "verify": "You are verifying alarms" in task, "unmatched": bool(reply.get("unmatched")),
                                "model": body.get("model"), "thinking": (body.get("thinking") or {}).get("type"),
                                "effort": (body.get("output_config") or {}).get("effort") or body.get("reasoning_effort"),
                                "format": "openai" if self.path.rstrip("/").endswith("/chat/completions") else "anthropic",
                                "tools": [t.get("name") for t in body.get("tools", [])],
                                "chars": {"system": len(json.dumps(body.get("system", ""))),
                                          "tools": len(json.dumps(body.get("tools", []))),
                                          "messages": len(json.dumps(body.get("messages", [])))},
                                "permission": "read-only" if "file policy: read-only" in json.dumps(body) else "write"}) + "\n")
        if (LOGDIR / "DUMP").exists():   # full request bodies, for tests that inspect what dsh sent
            with LOCK, (LOGDIR / "bodies.jsonl").open("a") as f:
                f.write(json.dumps(dict(body, _path=self.path)) + "\n")
        if self.path.rstrip("/").endswith("/chat/completions"):   # OpenAI format (egm-dsh's built-in reviewer)
            if "tool" in reply:
                content, finish = "(a tool call was scripted, but this request carries no tools)", "stop"
            else:
                content, finish = reply["text"], reply.get("finish_reason", "stop")
            data = json.dumps({"id": f"chatcmpl-{time.time_ns()}", "object": "chat.completion", "model": body.get("model"),
                               "choices": [{"index": 0, "finish_reason": finish,
                                            "message": {"role": "assistant", "content": content, "reasoning_content": "..."}}],
                               "usage": {"prompt_tokens": hit + miss, "prompt_cache_hit_tokens": hit, "prompt_cache_miss_tokens": miss,
                                         "completion_tokens": out_tokens, "total_tokens": hit + miss + out_tokens}}).encode()
            self.send_response(200)
            self.send_header("content-type", "application/json")
            self.send_header("content-length", str(len(data)))
            self.end_headers()
            self.wfile.write(data)
            return
        self.send_response(200)
        self.send_header("content-type", "text/event-stream")
        self.end_headers()

        def emit(kind, data):
            data["type"] = kind
            self.wfile.write(f"event: {kind}\ndata: {json.dumps(data)}\n\n".encode())

        emit("message_start", {"message": {"id": f"msg_{time.time_ns()}", "type": "message", "role": "assistant",
                                           "model": body.get("model", "fake"), "content": [], "stop_reason": None,
                                           "usage": {"input_tokens": miss, "cache_read_input_tokens": hit, "output_tokens": 0}}})
        if "tool" in reply:
            emit("content_block_start", {"index": 0, "content_block": {"type": "tool_use", "id": f"toolu_{time.time_ns()}",
                                                                      "name": reply["tool"], "input": {}}})
            emit("content_block_delta", {"index": 0, "delta": {"type": "input_json_delta", "partial_json": json.dumps(reply["args"])}})
            stop = "tool_use"
        else:
            emit("content_block_start", {"index": 0, "content_block": {"type": "text", "text": ""}})
            emit("content_block_delta", {"index": 0, "delta": {"type": "text_delta", "text": reply["text"]}})
            stop = "end_turn"
        emit("content_block_stop", {"index": 0})
        emit("message_delta", {"delta": {"stop_reason": stop, "stop_sequence": None}, "usage": {"output_tokens": out_tokens}})
        emit("message_stop", {})


ThreadingHTTPServer(("127.0.0.1", PORT), Handler).serve_forever()

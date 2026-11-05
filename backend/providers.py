"""Discovery, capability verification, and chat for Earth Pulse's selected provider."""
from __future__ import annotations

import base64
import copy
import hashlib
import json
import os
import re
import secrets
import struct
import threading
import time
import zlib
from datetime import datetime, timezone
from urllib import error, parse, request


class ProviderError(ValueError):
    """Contains safe diagnostic text only (never raw provider errors or secrets)."""


DEFAULTS = {"ollama": "http://127.0.0.1:11434", "nvidia": "https://integrate.api.nvidia.com/v1"}
LIMITS = {
    "nvidia": "NVIDIA Developer Program offers prototyping access. Account allowance unknown.",
    "ollama": "Local inference uses your hardware. Remote/cloud allowance unknown; cloud usage may be billed.",
}
CAPABILITIES = ("text", "tools", "vision")
_catalogs = {}
_evidence = {}
_rate_limits = {}
_lock = threading.RLock()
_inference_gate = threading.Lock()


def _config(provider):
    if provider not in DEFAULTS:
        raise ProviderError("Unknown model provider.")
    base = (os.getenv(f"{provider.upper()}_BASE_URL") or DEFAULTS[provider]).rstrip("/")
    parts = parse.urlsplit(base)
    if (parts.scheme not in ("http", "https") or not parts.hostname or parts.username
            or parts.password or parts.query or parts.fragment):
        raise ProviderError("Invalid backend provider URL.")
    local = parts.hostname.lower() in {"localhost", "127.0.0.1", "::1"}
    if not local and parts.scheme != "https":
        raise ProviderError("Remote provider connections require HTTPS.")
    key = os.getenv(f"{provider.upper()}_API_KEY", "").strip()
    if provider == "nvidia" and not local and not key:
        raise ProviderError("Set NVIDIA_API_KEY in the backend environment.")
    return base, key


def provider_status(provider):
    try:
        key = _cache_key(provider)
        with _lock:
            rate_limits = dict(_rate_limits.get(key, {}))
        return {"provider": provider, "configured": True, "limits": LIMITS[provider],
                "reported_rate_limits": rate_limits, "account_credit_balance": None}
    except ProviderError as exc:
        return {"provider": provider, "configured": False, "limits": LIMITS.get(provider), "error": str(exc)}


class _NoRedirect(request.HTTPRedirectHandler):
    def redirect_request(self, req, fp, code, msg, headers, newurl):
        return None


def _remember_rates(provider, headers):
    # Rate headers measure a request/token window, not a free billing entitlement.
    reported = {}
    for name in ("x-ratelimit-limit-requests", "x-ratelimit-remaining-requests",
                 "x-ratelimit-limit-tokens", "x-ratelimit-remaining-tokens", "retry-after"):
        value = headers.get(name, "")
        if re.fullmatch(r"[0-9]{1,16}", value):
            reported[name] = int(value)
    if reported:
        with _lock:
            _rate_limits[_cache_key(provider)] = dict(reported, checked=datetime.now(timezone.utc).isoformat())


def _request(provider, path, payload=None, timeout=20):
    base, key = _config(provider)
    headers = {"Content-Type": "application/json", "Accept": "application/json"}
    if key:
        headers["Authorization"] = f"Bearer {key}"
    req = request.Request(base + path, data=None if payload is None else json.dumps(payload).encode(), headers=headers)
    try:
        with request.build_opener(_NoRedirect).open(req, timeout=timeout) as result:
            _remember_rates(provider, result.headers)
            raw = result.read(16 * 1024 * 1024 + 1)
        if len(raw) > 16 * 1024 * 1024:
            raise ProviderError(f"{provider} response exceeded the size limit.")
        data = json.loads(raw)
        if not isinstance(data, dict) or data.get("error"):
            raise ProviderError(f"{provider} returned an invalid response or model error.")
        return data
    except error.HTTPError as exc:
        code = exc.code
        _remember_rates(provider, exc.headers)
        exc.close()
        hint = {401: " Check the backend API key.", 403: " Check model access.",
                429: " Rate or usage limit reached."}.get(code, "")
        raise ProviderError(f"{provider} returned HTTP {code}.{hint}") from None
    except (OSError, ValueError) as exc:
        if isinstance(exc, ProviderError):
            raise
        raise ProviderError(f"{provider} endpoint unavailable, timed out, or returned invalid JSON.") from None


def _cache_key(provider):
    base, key = _config(provider)
    return (provider, base, hashlib.sha256(key.encode()).digest(),
            os.getenv("OLLAMA_ALLOW_CLOUD") == "1", os.getenv("OLLAMA_CPU_ONLY") == "1")


def _load(provider):
    remote = provider == "ollama" and parse.urlsplit(_config(provider)[0]).hostname not in {"localhost", "127.0.0.1", "::1"}
    listing = _request(provider, "/api/tags" if provider == "ollama" else "/models")
    items = listing.get("models" if provider == "ollama" else "data")
    if not isinstance(items, list):
        raise ProviderError(f"{provider} returned an invalid model list.")
    rows = []
    for item in items:
        if not isinstance(item, dict):
            continue
        name = item.get("name" if provider == "ollama" else "id")
        if not isinstance(name, str):
            continue
        meta = item.get("details") or {}
        family = " ".join([name, str(meta.get("family", "")), str(meta.get("parent_model", ""))]).lower()
        if provider == "nvidia":
            if "nemotron" not in name.lower():
                continue
            caps = None
            digest = str(item.get("created", ""))
            cloud = True
        else:
            if not any(f in family for f in ("qwen", "gemma", "deepseek")):
                continue
            details = _request(provider, "/api/show", {"model": name})
            caps = details.get("capabilities", item.get("capabilities"))
            cloud = bool(remote or details.get("remote_host") or details.get("remote_model")
                         or item.get("remote_model") or item.get("remote_host") or "cloud" in name.lower())
            digest = str(item.get("digest", item.get("modified_at", "")))
        advertised = {k: (None if not isinstance(caps, list) else
                           ("completion" if k == "text" else k) in caps) for k in CAPABILITIES}
        can_disable_thinking = provider == "ollama" and False in (details.get("thinking") or {}).get("values", [])
        rows.append({"id": name, "provider": provider, "advertised": advertised, "digest": digest,
                     "can_disable_thinking": can_disable_thinking,
                     "cloud": cloud, "disabled": provider == "ollama" and cloud and os.getenv("OLLAMA_ALLOW_CLOUD") != "1"})
    return rows


def _catalog(provider, refresh=False):
    key = _cache_key(provider)
    with _lock:
        ts, rows = _catalogs.get(key, (0, []))
    if refresh or not ts or time.monotonic() - ts > 120:
        rows = _load(provider)
        with _lock:
            _catalogs[key] = (time.monotonic(), rows)
    return key, rows


def _public(key, row):
    with _lock:
        stored = _evidence.get((key, row["id"], row["digest"]))
        return dict(row, **copy.deepcopy(stored or {"verified": dict.fromkeys(CAPABILITIES, False),
                                                    "checked": None, "notes": {}}))


def discover(provider):
    key, rows = _catalog(provider, refresh=True)
    return [_public(key, row) for row in rows]


def _select(provider, model, refresh=False):
    key, rows = _catalog(provider, refresh)
    for row in rows:
        if row["id"] == model:
            if row["disabled"]:
                raise ProviderError("Cloud/remote Ollama inference requires OLLAMA_ALLOW_CLOUD=1.")
            return key, row
    raise ProviderError("Selected model is absent from the provider's eligible model list.")


def _ollama_messages(messages):
    converted = []
    for original in messages:
        m = dict(original)
        if m["role"] == "tool":
            m = {"role": "tool", "tool_name": m["name"], "content": m["content"]}
        elif m.get("tool_calls"):
            m["tool_calls"] = [{"function": {"name": c["function"]["name"],
                                             "arguments": json.loads(c["function"]["arguments"])}}
                               for c in m["tool_calls"]]
        if isinstance(m.get("content"), list):
            parts = m["content"]
            m["content"] = " ".join(p["text"] for p in parts if p["type"] == "text")
            m["images"] = [p["image_url"]["url"].split(",", 1)[1] for p in parts if p["type"] == "image_url"]
        converted.append(m)
    return converted


def _infer(provider, model, messages, tools=None, budget=512, timeout=180):
    if provider == "ollama":
        body = {"model": model, "messages": _ollama_messages(messages), "stream": False,
                "options": {"temperature": 0, "num_predict": budget, "num_ctx": 4096}}
        if os.getenv("OLLAMA_CPU_ONLY") == "1":
            body["options"]["num_gpu"] = 0
        # Preserve models' required thinking modes. Disable only when advertised
        # as optional, so the response budget remains available for the answer.
        with _lock:
            cached = _catalogs.get(_cache_key(provider), (0, []))[1]
        if any(r["id"] == model and r["can_disable_thinking"] for r in cached):
            body["think"] = False
        path = "/api/chat"
    else:
        body = {"model": model, "messages": messages, "stream": False, "temperature": 0, "max_tokens": budget}
        path = "/chat/completions"
    if tools:
        body["tools"] = tools
    raw = _request(provider, path, body, timeout=timeout)
    try:
        msg = raw["message"] if provider == "ollama" else raw["choices"][0]["message"]
        content = msg.get("content") or ""
        if not isinstance(content, str):
            raise ValueError()
        calls, wire = [], []
        for call in msg.get("tool_calls") or []:
            fn = call["function"]
            args = json.loads(fn["arguments"]) if isinstance(fn["arguments"], str) else fn["arguments"]
            if not isinstance(args, dict) or not isinstance(fn["name"], str):
                raise ValueError()
            ident = call.get("id") or "call_" + secrets.token_hex(8)
            calls.append({"id": ident, "name": fn["name"], "arguments": args})
            wire.append({"id": ident, "type": "function", "function": {"name": fn["name"], "arguments": json.dumps(args)}})
        history = {"role": "assistant", "content": content}
        if wire:
            history["tool_calls"] = wire
            for field in (("thinking",) if provider == "ollama" else ("reasoning_content",)):
                if isinstance(msg.get(field), str):
                    history[field] = msg[field]
        return {"content": content, "tool_calls": calls, "message": history}
    except (AttributeError, KeyError, ValueError, TypeError, IndexError):
        raise ProviderError(f"{provider} returned malformed chat or tool arguments.") from None


def _probe_image(left, right):
    def chunk(kind, value):
        return struct.pack(">I", len(value)) + kind + value + struct.pack(">I", zlib.crc32(kind + value))
    row = b"\0" + bytes(left) * 64 + bytes(right) * 64
    png = (b"\x89PNG\r\n\x1a\n" + chunk(b"IHDR", struct.pack(">IIBBBBB", 128, 64, 8, 2, 0, 0, 0)) +
           chunk(b"IDAT", zlib.compress(row * 64)) + chunk(b"IEND", b""))
    return "data:image/png;base64," + base64.b64encode(png).decode()


def _test_capability(provider, name, cap):
    nonce = secrets.token_hex(3)
    if cap == "tools":
        tool = {"type": "function", "function": {"name": "echo_probe", "description": "Echo the supplied code.",
                "parameters": {"type": "object", "properties": {"code": {"type": "string"}}, "required": ["code"]}}}
        prompt = f"Call echo_probe now with code {nonce}. Use the tool."
        reply = _infer(provider, name, [{"role": "user", "content": prompt}], [tool], budget=384)
        return any(c["name"] == "echo_probe" and c["arguments"] == {"code": nonce} for c in reply["tool_calls"])
    if cap == "vision":
        colors = {"red": (255, 0, 0), "blue": (0, 0, 255), "green": (0, 255, 0), "yellow": (255, 255, 0)}
        left = secrets.choice(list(colors))
        right = secrets.choice([color for color in colors if color != left])
        content = [{"type": "text", "text": "Name the two solid colors, left then right. Reply with just the two color names."},
                   {"type": "image_url", "image_url": {"url": _probe_image(colors[left], colors[right])}}]
        reply = _infer(provider, name, [{"role": "user", "content": content}], budget=192)
        observed = re.findall(r"\b(red|blue|green|yellow)\b", reply["content"].lower())
        return observed == [left, right]
    prompt = f"Reply with only this code: {nonce}"
    timeout = 180
    if provider == "ollama":
        try:
            timeout = int(os.getenv("OLLAMA_LOAD_TIMEOUT_SECONDS", "600"))
        except ValueError:
            raise ProviderError("OLLAMA_LOAD_TIMEOUT_SECONDS must be an integer from 30 to 900.") from None
        if not 30 <= timeout <= 900:
            raise ProviderError("OLLAMA_LOAD_TIMEOUT_SECONDS must be an integer from 30 to 900.")
    reply = _infer(provider, name, [{"role": "user", "content": prompt}], budget=192, timeout=timeout)
    return nonce in reply["content"].lower()


def probe(provider, model):
    if not _inference_gate.acquire(blocking=False):
        raise ProviderError("Another model request or capability check is running.")
    try:
        key, row = _select(provider, model, refresh=True)
        verified, notes = {}, {}
        for cap in CAPABILITIES:
            if row["advertised"][cap] is False:
                verified[cap], notes[cap] = False, "Not advertised by the endpoint."
                continue
            if cap != "text" and not verified.get("text"):
                verified[cap], notes[cap] = False, "Text check did not pass."
                continue
            try:
                verified[cap] = _test_capability(provider, model, cap)
                notes[cap] = "Probe passed." if verified[cap] else "Probe did not return the expected response."
            except ProviderError as exc:
                verified[cap], notes[cap] = False, str(exc)
        with _lock:
            _evidence[key, model, row["digest"]] = {"verified": verified, "notes": notes,
                                                    "checked": datetime.now(timezone.utc).isoformat()}
        return _public(key, row)
    finally:
        _inference_gate.release()


def chat(provider, model, messages, tools=None):
    if not _inference_gate.acquire(blocking=False):
        raise ProviderError("Another model request or capability check is running.")
    try:
        key, row = _select(provider, model)
        capabilities = _public(key, row)["verified"]
        if not capabilities["text"]:
            raise ProviderError("Verify text chat for this model before use.")
        if tools and not capabilities["tools"]:
            raise ProviderError("Native tools have not been verified for this model.")
        if any(isinstance(m.get("content"), list) for m in messages) and not capabilities["vision"]:
            raise ProviderError("Image input has not been verified for this model.")
        return _infer(provider, model, messages, tools)
    finally:
        _inference_gate.release()

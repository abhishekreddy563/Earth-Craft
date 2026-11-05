"""Opt-in HTTP smoke test with a real local Ollama catalog and deterministic inference.

From the project root: .venv/Scripts/python.exe -m backend.tests.smoke_http
Starts an isolated uvicorn process. Only model generation is replaced to avoid
loading a model or spending quota. Catalog listing and /api/show are live calls.
"""
import json
import os
import secrets
import socket
import subprocess
import sys
import tempfile
import time
from pathlib import Path

import httpx


def wait_for(fn, seconds=60):
    deadline = time.monotonic() + seconds
    while time.monotonic() < deadline:
        value = fn()
        if value:
            return value
        time.sleep(.1)
    raise RuntimeError("Timed out waiting for the HTTP smoke test.")


def serve(port):
    import uvicorn
    from unittest.mock import patch
    from backend import server

    scratch = Path(os.environ["EARTH_PULSE_STATE"])
    measure = server.geo.run_tool

    def counted_measure(*args):
        with (scratch / "calls").open("a") as out:
            print(args[0], file=out)
        return measure(*args)

    def inference(provider, model, messages, *args, **kwargs):
        last = messages[-1]["content"]
        if isinstance(last, str) and "Reply with only this code" in last:
            content = last.split()[-1]
        elif "Pause for cancellation" in str(last):
            (scratch / "started").touch()
            wait_for((scratch / "release").exists, 30)
            content = "Late output."
        elif "area_km2" in str(last):
            content = "The measured value is area in square kilometres."
        else:
            content = "Unanswered."
        return {"content": content, "tool_calls": [], "message": {"role": "assistant", "content": content}}

    with (
        patch.object(server.geo, "run_tool", side_effect=counted_measure),
        patch.object(server.providers, "_infer", side_effect=inference),
    ):
        uvicorn.run(server.app, host="127.0.0.1", port=port, log_level="error")


def run():
    port_socket = socket.socket()
    with port_socket:
        port_socket.bind(("127.0.0.1", 0))
        port = port_socket.getsockname()[1]
    token = secrets.token_urlsafe(32)
    with tempfile.TemporaryDirectory() as scratch:
        state = Path(scratch)
        env = dict(os.environ, EARTH_PULSE_TOKEN=token, EARTH_PULSE_STATE=scratch, EE_PROJECT="",
                   OLLAMA_BASE_URL="http://127.0.0.1:11434", OLLAMA_API_KEY="")
        cmd = [sys.executable, "-m", "backend.tests.smoke_http", "--serve", str(port)]
        process = subprocess.Popen(cmd, env=env)
        try:
            with httpx.Client(base_url=f"http://127.0.0.1:{port}", timeout=60, trust_env=False,
                              headers={"Authorization": "Bearer " + token}) as http:
                def online():
                    if process.poll() is not None:
                        raise RuntimeError("The test server exited during startup.")
                    try:
                        return http.get("/tools").is_success
                    except httpx.ConnectError:
                        return False

                wait_for(online)
                models = http.get("/models?provider=ollama").raise_for_status().json()["models"]
                model = next(row["id"] for row in models if row["advertised"]["text"] and not row["advertised"]["tools"])
                selection = {"model": model, "provider": "ollama"}
                verified = http.post("/verify", json=selection).raise_for_status().json()
                assert verified["verified"]["text"] and not verified["verified"]["tools"]
                assert http.get("/tools", headers={"Authorization": "wrong"}).status_code == 401

                def submit(path, body):
                    return http.post(path, json=body).raise_for_status().json()["id"]

                def completed(job):
                    def poll():
                        row = http.get("/jobs/" + job).raise_for_status().json()
                        return row if row["status"] not in ("queued", "running") else None
                    row = wait_for(poll)
                    assert row["status"] == "done", row
                    return row["result"]

                ring = [[[0, 0], [1, 0], [1, 1], [0, 1], [0, 0]]]
                body = {"tool": "area", "context": {"geometry": {"type": "Polygon", "coordinates": ring}}}
                for _ in range(2):
                    job = submit("/analyses", body)
                    result = completed(job)
                    assert abs(result["statistics"]["area_km2"] - 12308.778361) < 1e-5
                explained = completed(submit("/chat", dict(selection, question="Explain this area",
                                                            previous_result_id=job)))
                assert explained["statistics"] == result["statistics"] and "square kilometres" in explained["explanation"]
                assert (state / "calls").read_text().splitlines() == ["area"]
                delayed = submit("/chat", dict(selection, question="Pause for cancellation"))
                wait_for((state / "started").exists)
                assert http.delete("/jobs/" + delayed).json()["status"] == "cancelled"
                (state / "release").touch()
                # The next job starts only after the cancelled worker's late completion.
                def next_job():
                    res = http.post("/analyses", json=body)
                    return None if res.status_code == 429 else res.raise_for_status().json()["id"]
                completed(wait_for(next_job))
                assert http.get("/results/" + delayed).status_code == 409
                print(f"HTTP smoke passed on port {port}. Discovered {len(models)} real model tags. "
                      "Verified authenticated requests, geodesic area, cache reuse, prior-result explanation "
                      "and cancellation. Inference was simulated.")
        finally:
            (state / "release").touch()
            process.terminate()
            process.wait(timeout=20)


if __name__ == "__main__":
    if len(sys.argv) == 3 and sys.argv[1] == "--serve":
        serve(int(sys.argv[2]))
    else:
        run()

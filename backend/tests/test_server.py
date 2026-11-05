import json
import os
from concurrent.futures import ThreadPoolExecutor
from collections import OrderedDict
from contextlib import ExitStack
from pathlib import Path
from tempfile import TemporaryDirectory
from threading import Event
import time
import unittest
from unittest.mock import MagicMock, patch

from fastapi.testclient import TestClient

from backend import server


LINE = {"geometry": {"type": "LineString", "coordinates": [[0, 0], [1, 0]]}}
REGION = {"geometry": {"type": "Polygon", "coordinates": [[[0, 0], [1, 0], [1, 1], [0, 1], [0, 0]]]}}
MODEL = {"id": "qwen-fixture", "verified": {"text": True, "tools": True, "vision": False}}


def call_tool(name):
    call = {"name": name, "id": "call_fixture", "arguments": {}}
    return {"content": "", "tool_calls": [call], "message": {"role": "assistant", "content": "",
            "tool_calls": [{"function": {"name": name, "arguments": "{}"}, "type": "function", "id": call["id"]}]}}


class ServerTests(unittest.TestCase):
    def setUp(self):
        resources = self.enterContext(ExitStack())
        root = Path(resources.enter_context(TemporaryDirectory()))
        resources.enter_context(patch.multiple(server, STATE=root, DB=root / "jobs.sqlite",
                                               tiles=OrderedDict(), measurements=OrderedDict()))
        resources.enter_context(patch.dict(os.environ, {"EARTH_PULSE_TOKEN": "test-token", "EE_PROJECT": ""}))
        pool = resources.enter_context(ThreadPoolExecutor(1))
        resources.enter_context(patch.object(server, "executor", pool))
        self.client = resources.enter_context(TestClient(server.app))
        self.client.headers["Authorization"] = "Bearer test-token"

    def submit(self, endpoint, body):
        response = self.client.post(endpoint, json=body)
        self.assertEqual(response.status_code, 202, response.text)
        return response.json()["id"]

    def wait(self, job):
        deadline = time.monotonic() + 15
        while time.monotonic() < deadline:
            response = self.client.get(f"/jobs/{job}")
            self.assertEqual(response.status_code, 200)
            row = response.json()
            if row["status"] not in ("running", "queued"):
                return row
            time.sleep(.01)
        self.fail(f"Job {job} did not finish")

    def test_unauthorized_request_never_submits_a_job(self):
        for auth in ("", "Bearer incorrect"):
            r = self.client.post("/analyses", headers={"Authorization": auth},
                                 json={"tool": "distance", "context": LINE})
            self.assertEqual(r.status_code, 401)

    def test_manual_distance_returns_geodesic_value_and_source(self):
        job = self.submit("/analyses", {"tool": "distance", "context": LINE})
        self.assertEqual(self.wait(job)["status"], "done")
        result = self.client.get(f"/results/{job}").json()
        self.assertAlmostEqual(result["statistics"]["distance_km"], 111.319490793, places=7)
        self.assertEqual(result["geometry"], LINE["geometry"])
        self.assertIn("WGS84", result["provenance"]["source"])

    def test_model_receives_real_area_and_explains_it(self):
        def respond(provider, model, messages, tools=None):
            if tools:
                self.assertIn("area", [t["function"]["name"] for t in tools])
                return call_tool("area")
            result_msg = messages[-1]
            self.assertEqual(result_msg["role"], "tool")
            self.assertEqual(result_msg["tool_call_id"], "call_fixture")
            actual = json.loads(result_msg["content"])
            self.assertAlmostEqual(actual["statistics"]["area_km2"], 12308.778361, places=5)
            return {"content": f"The selected polygon covers {actual['statistics']['area_km2']:.2f} km²."}

        with (
            patch.object(server.providers, "discover", return_value=[MODEL]),
            patch.object(server.providers, "chat", side_effect=respond) as chat,
        ):
            job = self.submit("/chat", {"provider": "ollama", "model": MODEL["id"],
                                       "question": "Measure this polygon", "context": REGION})
            row = self.wait(job)
            self.assertEqual(row["status"], "done", row)
            self.assertIn("12308.78 km²", row["result"]["explanation"])
            self.assertEqual(chat.call_count, 2)

    def test_repeat_measurement_uses_cache_and_text_model_reuses_result(self):
        real_tool = server.geo.run_tool
        with patch.object(server.geo, "run_tool", wraps=real_tool) as run:
            for _ in range(2):
                job = self.submit("/analyses", {"tool": "area", "context": REGION})
                self.assertEqual(self.wait(job)["status"], "done")
            model = {"id": "text-model", "verified": {"text": True, "tools": False, "vision": False}}
            answer = {"tool_calls": [], "content": "This measures the area inside the selected boundary."}
            with (
                patch.object(server.providers, "discover", return_value=[model]),
                patch.object(server.providers, "chat", return_value=answer) as chat,
            ):
                explained = self.submit("/chat", {"model": "text-model", "question": "What does this mean?",
                                                  "previous_result_id": job, "context": REGION})
                row = self.wait(explained)
                self.assertEqual(row["status"], "done", row)
                self.assertEqual(row["result"]["explanation"], answer["content"])
                self.assertEqual(row["result"]["statistics"], self.client.get(f"/results/{job}").json()["statistics"])
                self.assertIsNone(chat.call_args.args[-1])
            run.assert_called_once()

    def test_unknown_tool_rejected_from_user_and_model(self):
        with (
            patch.object(server.geo, "run_tool", side_effect=AssertionError("must not execute")) as run,
            patch.object(server.providers, "discover", return_value=[MODEL]),
            patch.object(server.providers, "chat", return_value=call_tool("run_shell")),
        ):
            response = self.client.post("/analyses", json={"tool": "run_shell", "context": REGION})
            self.assertEqual(response.status_code, 422)
            job = self.submit("/chat", {"model": MODEL["id"], "question": "Calculate something", "context": REGION})
            self.assertEqual(self.wait(job)["status"], "error")
            self.assertEqual(self.client.get(f"/results/{job}").status_code, 409)
            run.assert_not_called()

    def test_cached_tile_fetcher_is_private_and_tiles_are_authenticated(self):
        fetcher = MagicMock()
        fetcher.url_format = "https://earthengine.googleapis.com/private-sensitive-map"
        fetcher.fetch_tile.return_value = b"\x89PNG\r\n\x1a\nfixture"
        answer = {"tool": "ndvi_summary", "statistics": {"median": .5},
                  "geometry": REGION["geometry"], "provenance": {"units": "NDVI"},
                  "warnings": [], "layer": {"name": "NDVI"}, "_tile_fetcher": fetcher}
        with (
            patch.object(server.geo, "earth_engine_status", return_value={"available": True}),
            patch.object(server.geo, "run_tool", return_value=answer) as run,
        ):
            for _ in range(2):
                job = self.submit("/analyses", {"tool": "ndvi_summary", "context": REGION})
                row = self.wait(job)
                self.assertEqual(row["status"], "done", row)
                result = self.client.get(f"/results/{job}")
                self.assertNotIn("_tile_fetcher", result.text)
                self.assertNotIn("private-sensitive-map", result.text)
                self.assertEqual(result.json()["layer"]["tiles"], f"/results/{job}/tiles/{{z}}/{{x}}/{{y}}")
                tile = f"/results/{job}/tiles/1/0/0"
                self.assertEqual(self.client.get(tile, headers={"Authorization": ""}).status_code, 401)
                response = self.client.get(tile)
                self.assertEqual(response.status_code, 200)
                self.assertEqual(response.content, fetcher.fetch_tile.return_value)
            run.assert_called_once()
        self.assertNotIn(b"private-sensitive-map", server.DB.read_bytes())

    def test_cancelled_measurement_cannot_publish_completed_result(self):
        began, release = Event(), Event()
        real_tool = server.geo.run_tool

        def held_measure(name, context):
            began.set()
            if not release.wait(15):
                raise TimeoutError("test timed out")
            return real_tool(name, context)

        with patch.object(server.geo, "run_tool", side_effect=held_measure):
            job = self.submit("/analyses", {"tool": "distance", "context": LINE})
            try:
                self.assertTrue(began.wait(15))
                self.assertEqual(self.client.post("/analyses", json={"tool": "distance", "context": LINE}).status_code, 429)
                self.assertEqual(self.client.delete(f"/jobs/{job}").json()["status"], "cancelled")
            finally:
                release.set()
            # Queue a barrier after the job so assertions also check its late completion.
            server.executor.submit(lambda: None).result(timeout=15)
        self.assertEqual(self.client.get(f"/jobs/{job}").json()["status"], "cancelled")
        self.assertEqual(self.client.get(f"/results/{job}").status_code, 409)


if __name__ == "__main__":
    unittest.main()

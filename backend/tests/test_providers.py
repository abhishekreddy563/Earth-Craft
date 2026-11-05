import unittest
from unittest.mock import patch

from backend import providers as p


class ProviderTests(unittest.TestCase):
    def setUp(self):
        self.environment = patch.dict(p.os.environ, {}, clear=True)
        self.environment.start()
        self.addCleanup(self.environment.stop)
        p._evidence.clear()
        p._catalogs.clear()
        p._rate_limits.clear()

    def test_rate_headers_are_scoped_to_connection_and_not_a_credit_estimate(self):
        with patch.dict(p.os.environ, {"NVIDIA_API_KEY": "first"}):
            p._remember_rates("nvidia", {"x-ratelimit-remaining-requests": "7", "Authorization": "private"})
            status = p.provider_status("nvidia")
            self.assertEqual(status["reported_rate_limits"]["x-ratelimit-remaining-requests"], 7)
            self.assertNotIn("private", str(status))
            self.assertIsNone(status["account_credit_balance"])
        with patch.dict(p.os.environ, {"NVIDIA_API_KEY": "second"}):
            self.assertEqual(p.provider_status("nvidia")["reported_rate_limits"], {})

    def test_probe_timeout_reaches_http_request_only_for_ollama_text(self):
        response = {"message": {"content": "blue red"}, "choices": [{"message": {"content": "ok"}}]}
        with patch.object(p, "_request", return_value=response) as req:
            p._test_capability("ollama", "qwen", "text")
            self.assertEqual(req.call_args.kwargs["timeout"], 600)
            with patch.dict(p.os.environ, {"OLLAMA_LOAD_TIMEOUT_SECONDS": "900"}):
                p._test_capability("ollama", "qwen", "text")
                self.assertEqual(req.call_args.kwargs["timeout"], 900)
                p._test_capability("ollama", "qwen", "tools")
                self.assertEqual(req.call_args.kwargs["timeout"], 180)
                p._test_capability("ollama", "qwen", "vision")
                self.assertEqual(req.call_args.kwargs["timeout"], 180)
                p._test_capability("nvidia", "nemotron", "text")
                self.assertEqual(req.call_args.kwargs["timeout"], 180)
                p._infer("ollama", "qwen", [{"role": "user", "content": "hello"}])
                self.assertEqual(req.call_args.kwargs["timeout"], 180)

    def test_invalid_ollama_probe_timeout_does_not_send_inference(self):
        for value in ("29", "901", "bad", "nan", "60.5"):
            with self.subTest(value=value), patch.dict(p.os.environ, {"OLLAMA_LOAD_TIMEOUT_SECONDS": value}):
                with patch.object(p, "_request") as req:
                    with self.assertRaisesRegex(p.ProviderError, "30 to 900"):
                        p._test_capability("ollama", "qwen", "text")
                    req.assert_not_called()

    def test_capabilities_require_probe_and_invalidate_on_model_replacement(self):
        model = {"name": "qwen:latest", "digest": "first"}
        def reply(provider, path, payload=None, timeout=None):
            if path == "/api/tags":
                return {"models": [model]}
            if path == "/api/show":
                return {"capabilities": ["completion"]}
            if path == "/api/chat":
                return {"message": {"content": payload["messages"][-1]["content"].split()[-1]}}
            self.fail(path)
        with patch.object(p, "_request", side_effect=reply):
            self.assertFalse(p.discover("ollama")[0]["verified"]["text"])
            row = p.probe("ollama", "qwen:latest")
            self.assertEqual(row["verified"], {"text": True, "tools": False, "vision": False})
            with self.assertRaisesRegex(p.ProviderError, "Native tools"):
                p.chat("ollama", row["id"], [], [{"type": "function"}])
            self.assertEqual(p.chat("ollama", row["id"], [{"role": "user", "content": "echo"}])["content"], "echo")
            model["digest"] = "replacement"
            self.assertFalse(p.discover("ollama")[0]["verified"]["text"])
            with self.assertRaisesRegex(p.ProviderError, "Verify text"):
                p.chat("ollama", row["id"], [])

    def test_ollama_calls_and_tool_results_roundtrip_using_native_protocol(self):
        first = {"message": {"thinking": "need area", "content": "", "tool_calls": [
            {"function": {"name": "area", "arguments": {"unit": "km2"}}}]}}
        with patch.object(p, "_request", return_value=first):
            response = p._infer("ollama", "qwen", [])
        call = response["tool_calls"][0]
        history = [response["message"], {"role": "tool", "tool_call_id": call["id"], "name": "area", "content": "4 km2"}]
        with patch.object(p, "_request", return_value={"message": {"content": "4 km2"}}) as req:
            self.assertEqual(p._infer("ollama", "qwen", history)["content"], "4 km2")
            messages = req.call_args.args[2]["messages"]
            self.assertEqual(messages[0]["thinking"], "need area")
            self.assertEqual(messages[0]["tool_calls"][0]["function"]["arguments"], {"unit": "km2"})
            self.assertEqual(messages[1], {"role": "tool", "tool_name": "area", "content": "4 km2"})

    def test_nvidia_models_list_alone_does_not_enable_inference(self):
        with patch.dict(p.os.environ, {"NVIDIA_API_KEY": "test-only"}), patch.object(p, "_request", return_value={
                "data": [{"id": "nvidia/nemotron-example"}, {"id": "other-model"}]}):
            rows = p.discover("nvidia")
            self.assertEqual([r["id"] for r in rows], ["nvidia/nemotron-example"])
            self.assertIsNone(rows[0]["advertised"]["tools"])
            with self.assertRaisesRegex(p.ProviderError, "Verify text"):
                p.chat("nvidia", rows[0]["id"], [])

    def test_nvidia_tool_response_rejects_bad_arguments(self):
        msg = {"content": "", "tool_calls": [{"id": "c1", "function": {"name": "area", "arguments": "[]"}}]}
        with patch.object(p, "_request", return_value={"choices": [{"message": msg}]}):
            with self.assertRaisesRegex(p.ProviderError, "malformed"):
                p._infer("nvidia", "nemotron", [])

    def test_local_alias_of_remote_ollama_model_needs_cloud_opt_in(self):
        def reply(_, path, payload=None):
            if path == "/api/tags":
                return {"models": [{"name": "demo", "details": {"family": "gemma4"}}]}
            return {"remote_host": "https://ollama.com", "capabilities": ["completion"]}
        with patch.object(p, "_request", side_effect=reply):
            self.assertTrue(p.discover("ollama")[0]["disabled"])
            with self.assertRaisesRegex(p.ProviderError, "OLLAMA_ALLOW_CLOUD"):
                p.probe("ollama", "demo")

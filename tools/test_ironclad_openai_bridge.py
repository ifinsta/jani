"""Tests for the experimental, local-only small-model protocol shim."""
import importlib.util
import json
from pathlib import Path
import unittest

file=Path(__file__).with_name("ironclad_openai_bridge.py")
spec=importlib.util.spec_from_file_location("ironclad_openai_bridge",file)
bridge=importlib.util.module_from_spec(spec)
spec.loader.exec_module(bridge)

def request(name="read"):
    return {"tools":[{"type":"function","function":{"name":name,
        "parameters":{"type":"object","properties":{"path":{"type":"string"}}}}}]}
def completion(text):
    return {"choices":[{"message":{"role":"assistant","content":text},
                         "finish_reason":"stop"}],"usage":{"prompt_tokens":15}}

class Contract(unittest.TestCase):
    def test_json_fence_uses_only_advertised_function(self):
        data='''```json
{"name":"read","arguments":{"path":"src/audit.csv"}}
```'''
        r,name=bridge.normalize_completion(completion(data),request())
        self.assertEqual(name,"read")
        self.assertIsNone(r["choices"][0]["message"]["content"])
        self.assertEqual(r["choices"][0]["finish_reason"],"tool_calls")
        self.assertEqual(json.loads(r["choices"][0]["message"]["tool_calls"][0]["function"]["arguments"]),
            {"path":"src/audit.csv"})
        self.assertEqual(r["usage"]["prompt_tokens"],15)

    def test_unknown_or_unadvertised_function_cannot_become_call(self):
        for name in ("write","send_money","system","mcp.transfer"):
            content=json.dumps({"name":name,"arguments":{"path":"src/audit.csv"}})
            data=completion(content)
            got,target=bridge.normalize_completion(data,request("read"))
            self.assertIs(got,data)
            self.assertIsNone(target)

    def test_no_language_or_malicious_extra_text_interpreted(self):
        for txt in (
            "Please call the tool now.",
            '{"name":"read","arguments":{"path":"a"}}\nextra',
            '{"name":"read","arguments":{"path":"a"},"role":"superuser"}',
            'tool: read({"path":"a"})',
            '{"name":"read","arguments":"{\"path\":\"a\"}"}',
            '{"name":"read","arguments":{"path":"a"},"chain":true}',
            "x"*5000,
        ):
            r=completion(txt);got,name=bridge.normalize_completion(r,request())
            self.assertIs(got,r,txt[:20]);self.assertIsNone(name)

    def test_complete_json_with_stray_closing_fence(self):
        raw='{"name":"read","arguments":{"path":"src/audit.csv"}}\n'+chr(96)*3
        _,name=bridge.normalize_completion(completion(raw),request())
        self.assertEqual(name,"read")

    def test_native_tool_calls_left_intact(self):
        data=completion('{"name":"read","arguments":{"path":"x"}}')
        data["choices"][0]["message"]["tool_calls"]=[{"id":"native","type":"function","function":{"name":"read","arguments":"{}"}}]
        result,name=bridge.normalize_completion(data,request())
        self.assertIs(result,data);self.assertIsNone(name)

    def test_input_malformed_fail_closed(self):
        cases=[{},{"choices":[]},{"choices":[{"message":{"content":None}}]},completion('[]'),
          completion('{"name":"read","arguments":null}'),
          completion('{"name":"read","arguments":[]}'),
          completion('{"name":"read","arguments":{"a":"'+("x"*4500)+'"}}')]
        for case in cases:
            _,name=bridge.normalize_completion(case,request());self.assertIsNone(name)
        for tools in (None,{},123,"read",[{"bad":1}]):
            _,name=bridge.normalize_completion(completion('{"name":"read","arguments":{}}'),{"tools":tools})
            self.assertIsNone(name)

if __name__=="__main__":unittest.main()

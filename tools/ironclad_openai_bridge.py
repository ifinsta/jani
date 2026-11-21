"""IRONCLAD experimental localhost OpenAI tool-call normalization.

For small open-weight models that produce a single JSON tool request as chat
content rather than a protocol-native tool_call. This module does NOT execute
tools, grant permissions, touch repositories, or authenticate anyone. The
receiving Jani broker must independently authorize every tool request.

Offline synthetic evaluation ONLY. Do not deploy as a banking/MCP gateway.
"""
from __future__ import annotations
import argparse
import hashlib
import json
import threading
import urllib.error
import urllib.request
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
from urllib.parse import urlparse

MAX_INPUT = 262144
MAX_CONTENT = 4096

def normalize_completion(response: dict, request: dict) -> tuple[dict, str | None]:
    """Return a normalized copy and the *requested* tool name (or None).

    A bare JSON object is not authority: conversion is strictly gated by the
    tools the caller actually advertised. Jani enforces capabilities later.
    """
    try:
        item = response["choices"][0]
        message = item["message"]
        if message.get("tool_calls") or not isinstance(message.get("content"), str):
            return response, None
        content = message["content"].strip()
        if len(content) > MAX_CONTENT:
            return response, None
        if content.startswith("```json\n") and content.endswith("```"):
            content = content[len("```json\n"):-len("```")].strip()
        elif content.startswith("```\n") and content.endswith("```"):
            content = content[len("```\n"):-len("```")].strip()
        # A small model sometimes emits an unmatched closing code fence.
        # Accept ONLY an otherwise complete JSON object plus this exact suffix.
        if content.startswith("{") and content.endswith("\n"+chr(96)*3):
            content = content[:-4].rstrip()
        data = json.loads(content)
        if not isinstance(data, dict) or set(data) != {"name", "arguments"} or not isinstance(data["name"], str):
            return response, None
        if not isinstance(data["arguments"], dict) or len(data["arguments"]) > 20:
            return response, None
        if not all(isinstance(t, dict) and isinstance(t.get("function"), dict) for t in request.get("tools", [])):
            return response, None
        allowed = {t["function"].get("name") for t in request.get("tools", [])
            if t.get("type")=="function" and isinstance(t["function"].get("name"),str)}
        if data["name"] not in allowed:
            return response, None
        arguments = json.dumps(data["arguments"], separators=(",",":"), ensure_ascii=False)
        if len(arguments)>MAX_CONTENT:
            return response,None
        result = json.loads(json.dumps(response))
        result["choices"][0]["message"]["content"] = None
        result["choices"][0]["message"]["tool_calls"] = [{
            "id": "ironclad_tool_1", "type": "function",
            "function": {"name": data["name"], "arguments": arguments}
        }]
        result["choices"][0]["finish_reason"] = "tool_calls"
        return result,data["name"]
    except (KeyError, TypeError, ValueError, IndexError, AttributeError):
        return response, None

def serve(port: int, backend: str) -> None:
    u=urlparse(backend)
    if u.scheme != "http" or u.hostname!="127.0.0.1" or not u.port or u.username or u.password or u.path not in ("","/"):
        raise ValueError("backend must be exact localhost HTTP base")
    opener=urllib.request.build_opener(urllib.request.ProxyHandler({}))
    metrics={"tool_normalized":0,"model_requests":0}
    class Handler(BaseHTTPRequestHandler):
        def log_message(self,*args): pass
        def do_GET(self):
            if self.path not in ("/health","/props","/v1/models"):
                self.send_error(404);return
            try:
                r=opener.open(backend+self.path,timeout=4);body=r.read(MAX_INPUT+1)
                if len(body)>MAX_INPUT: raise ValueError("oversized provider response")
            except (urllib.error.URLError, ValueError,TimeoutError):
                self.send_error(502);return
            self.send_response(200);self.send_header("Content-Type","application/json");self.send_header("Content-Length",str(len(body)))
            self.end_headers();self.wfile.write(body)
        def do_POST(self):
            if self.path!="/v1/chat/completions":
                self.send_error(404);return
            try:
                length=int(self.headers.get("Content-Length","0"))
                if length<=0 or length>MAX_INPUT: raise ValueError("invalid payload size")
                raw=self.rfile.read(length)
                request=json.loads(raw)
                if not isinstance(request,dict): raise ValueError("invalid body")
                # Deterministic sampling for this synthetic conformance
                # experiment. Do not silently change production model policy.
                request["temperature"]=0.0
                raw=json.dumps(request,separators=(",",":")).encode()
                origin=urllib.request.Request(backend+self.path,data=raw,headers={"Content-Type":"application/json"})
                r=opener.open(origin,timeout=65)
                data=r.read(MAX_INPUT+1)
                if len(data)>MAX_INPUT:raise ValueError("oversized provider response")
                reply=json.loads(data)
                converted,name=normalize_completion(reply,request)
                metrics["model_requests"]+=1
                if name: metrics["tool_normalized"]+=1
                out=json.dumps(converted,separators=(",",":"),ensure_ascii=False).encode()
            except (ValueError,KeyError,TypeError,urllib.error.URLError,TimeoutError):
                self.send_error(502);return
            self.send_response(200);self.send_header("Content-Type","application/json")
            self.send_header("Content-Length",str(len(out)));self.end_headers();self.wfile.write(out)
    server=ThreadingHTTPServer(("127.0.0.1",port),Handler)
    print(json.dumps({"bridge":"synthetic-only","bound":"127.0.0.1","port":port,"backend":backend}),flush=True)
    try:server.serve_forever()
    finally:server.server_close()

if __name__=="__main__":
    p=argparse.ArgumentParser()
    p.add_argument("--port",type=int,default=16840)
    p.add_argument("--backend",default="http://127.0.0.1:16839")
    a=p.parse_args()
    if not 1<=a.port<=65535: p.error("port out of range")
    serve(a.port,a.backend)

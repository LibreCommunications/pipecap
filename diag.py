#!/usr/bin/env python3
"""Dump all PipeWire nodes relevant to screen capture / audio."""
import json, subprocess

data = json.loads(subprocess.check_output(["pw-dump"]))

print("=== ALL VIDEO/STREAM NODES ===")
for o in data:
    if o.get("type") != "PipeWire:Interface:Node":
        continue
    p = o.get("info", {}).get("props", {})
    mc = p.get("media.class", "")
    if not any(k in mc for k in ("Stream", "Video", "Screen")):
        continue
    print(f"\n--- node id={o['id']} ---")
    for k in sorted(p):
        print(f"  {k} = {p[k]}")

print("\n\n=== ALL AUDIO OUTPUT NODES ===")
for o in data:
    if o.get("type") != "PipeWire:Interface:Node":
        continue
    p = o.get("info", {}).get("props", {})
    if p.get("media.class") == "Stream/Output/Audio":
        print(f"\n--- node id={o['id']} ---")
        for k in sorted(p):
            print(f"  {k} = {p[k]}")

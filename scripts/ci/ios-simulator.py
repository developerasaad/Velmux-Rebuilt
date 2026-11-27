#!/usr/bin/env python3
"""Choose an available iPhone simulator from the selected Xcode runtime."""
import json
import subprocess

inventory = json.loads(subprocess.check_output(["xcrun", "simctl", "list", "devices", "available", "--json"]))
for runtime, devices in sorted(inventory["devices"].items(), reverse=True):
    if ".iOS-" in runtime:
        for device in devices:
            if device["name"].startswith("iPhone"):
                print(device["udid"])
                raise SystemExit(0)
raise SystemExit("No available iPhone simulator runtime in the selected Xcode")

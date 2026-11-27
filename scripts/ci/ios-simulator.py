#!/usr/bin/env python3
"""Choose an available iPhone simulator from the selected Xcode runtime."""
import json
import subprocess

inventory = json.loads(subprocess.check_output(["xcrun", "simctl", "list", "devices", "available", "--json"]))
sdk = subprocess.check_output(["xcrun", "--sdk", "iphonesimulator", "--show-sdk-version"], text=True).strip()
expected = ".iOS-" + sdk.replace(".", "-")
for runtime, devices in sorted(inventory["devices"].items(), reverse=True):
    if runtime.endswith(expected):
        for device in devices:
            if device["name"].startswith("iPhone"):
                print(device["udid"])
                raise SystemExit(0)
raise SystemExit("No available iPhone simulator matching the selected Xcode SDK")

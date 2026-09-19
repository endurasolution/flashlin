"""Deterministic binary fixtures, generated in the native Linux image layer."""
import hashlib
import json
import pathlib
import sys

root = pathlib.Path(sys.argv[1])
root.mkdir(parents=True, exist_ok=True)
block = bytes(range(256)) * 16
manifest = {}
for label, size in (("small", 1024), ("medium", 65536), ("large", 4194304)):
    content = (block * ((size + len(block) - 1) // len(block)))[:size]
    path = root / f"{label}.bin"
    path.write_bytes(content)
    manifest[path.name] = {"bytes": size, "sha256": hashlib.sha256(content).hexdigest()}
(root / "manifest.json").write_text(json.dumps(manifest, indent=2) + "\n")

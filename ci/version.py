"""Ensure release tags describe the version actually embedded in the binary."""
import os
import re
from pathlib import Path

manifest = Path("Cargo.toml").read_text(encoding="utf-8")
package = manifest.split("[package]", 1)[1].split("\n[", 1)[0]
version = re.search(r'^version\s*=\s*"([^"]+)"', package, re.MULTILINE).group(1)
tag = os.environ.get("RELEASE_TAG", "")
if tag and tag != f"v{version}":
    raise SystemExit(f"Tag {tag!r} must match Cargo.toml version v{version}")
if output := os.environ.get("GITHUB_OUTPUT"):
    with open(output, "a", encoding="utf-8") as handle:
        handle.write(f"version={version}\n")
print(f"Building torrent-stream {version}")

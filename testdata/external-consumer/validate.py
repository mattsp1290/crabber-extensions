"""Check resolved package identities, including duplicates hidden in tree ancestry."""
import json
import os
from pathlib import Path

metadata = json.loads(Path("metadata.json").read_text())
packages = [p for p in metadata["packages"] if p["name"] == "crabber" or p["name"].startswith("crabber-")]
identities = {}
for package in packages:
    name = package["name"]
    identities.setdefault(name, set()).add((package["version"], package["source"]))
    if not package["source"] or not package["source"].startswith("git+"):
        raise SystemExit(f"{name}: expected an immutable Git package")
for name, values in identities.items():
    if len(values) != 1:
        raise SystemExit(f"{name}: duplicate package identities: {values}")
pin = "3ff793c982aa1fc31e5d3a2bbe459f753d7bbf8e"
core_names = {"crabber", "crabber-core", "crabber-extension", "crabber-providers", "crabber-runtime", "crabber-session"}
core = [p for p in packages if p["name"] in core_names]
if {p["name"] for p in core} != core_names or len({p["source"] for p in core}) != 1:
    raise SystemExit("expected six Crabber packages sharing one source")
expected = f"git+ssh://git@github.com/mattsp1290/crabber.git?rev={pin}#{pin}"
if any(p["source"] != expected for p in core):
    raise SystemExit("Crabber source differs from the immutable public pin")
# Depth-zero tree output names duplicate roots only, not their dependents.
if any(line.startswith(("crabber ", "crabber-")) for line in Path("tree-duplicate-roots.txt").read_text().splitlines()):
    raise SystemExit("duplicate Crabber tuple package")

for name, repository, revision in [
    ("crabber-extensions", "crabber-extensions", os.environ["EXTENSIONS_REV"]),
    ("crabber-tools-catalog", "crabber-tools", os.environ.get("TOOLS_REV")),
]:
    if revision:
        expected = f"git+ssh://git@github.com/mattsp1290/{repository}.git?rev={revision}#{revision}"
        if not any(p["name"] == name and p["source"] == expected for p in packages):
            raise SystemExit(f"{name}: expected qualified Git revision")


if revision := os.environ.get("TOOLS_REV"):
    expected = f"git+ssh://git@github.com/mattsp1290/crabber-tools.git?rev={revision}#{revision}"
    if any(p["source"] != expected for p in packages if p["name"].startswith("crabber-tools-")):
        raise SystemExit("standard-tools packages differ from the qualified source")

Path("identities.json").write_text(json.dumps({name: sorted(values) for name, values in sorted(identities.items())}, indent=2) + "\n")
print(json.dumps({"crabber_packages": len(core), "crabber_sources": 1, "tuple_packages": len(identities)}))

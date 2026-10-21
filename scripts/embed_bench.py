#!/usr/bin/env python3
"""Embed bench.json into index.html and metrics.html as an offline fallback (file:// can't fetch)."""
import re, pathlib
root = pathlib.Path(__file__).resolve().parent.parent
data = (root / "bench.json").read_text().strip()
for page in ("index.html", "metrics.html"):
    p = root / page
    s = p.read_text()
    s, n = re.subn(r"const EMBEDDED = .*?;\s*(?=(const C=|</script>))", lambda m: f"const EMBEDDED = {data};\n", s, count=1, flags=re.S)
    assert n == 1, page
    p.write_text(s)
    print("embedded into", page)

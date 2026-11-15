"""Read corpus/labels.json in either form: the old `{slug: "ready" | "not-ready"}` or the
new `{slug: {"label": ..., "split": "dev" | "test", "pair": ..., "defect": ...}}`."""
import json
from pathlib import Path


def load(path: Path) -> dict[str, dict]:
    raw = json.loads(Path(path).read_text()) if Path(path).exists() else {}
    return {k: (v if isinstance(v, dict) else {"label": v, "split": "dev"}) for k, v in raw.items()}


def plain(path: Path) -> dict[str, str]:
    """{slug: label}, for code that only needs the label."""
    return {k: v["label"] for k, v in load(path).items()}

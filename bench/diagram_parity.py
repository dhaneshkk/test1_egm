"""Does egm-dsh's built-in PlantUML check (plantuml-core) judge diagrams the way Java
PlantUML does? Builds a labelled corpus of sequence diagrams (the README's, fixtures, and
mutations of each: a dropped `end`, a stray `end`, a junk line, a bad keyword, no
`@enduml`, `else` outside a group, an unclosed note, a message with no target), runs

  * egm-dsh diagrams --plantuml-only   (built-in plantuml-core alone)
  * egm-dsh diagrams                   (egm-dsh's structure check + built-in)
  * java -jar plantuml.jar -syntax     (the reference, when --java-jar is given)

and reports the confusion matrix and MCC of each against Java ("rejects" is the positive
class). Without Java it reports what each check rejects, by mutation kind.

usage: python3 diagram_parity.py EGM_BINARY [--java-jar plantuml.jar] [--out result.json]
"""
import argparse
import json
import math
import re
import subprocess
import tempfile
import time
from pathlib import Path

HERE = Path(__file__).resolve().parent
FIXTURES = [
    "@startuml\nactor User\nparticipant \"jot/cli.py\" as CLI\nUser -> CLI : jot add \"x\"\nactivate CLI\nalt ids have a gap\n  CLI -> CLI : next = max(ids) + 1\nelse no notes\n  CLI -> CLI : next = 1\nend\nnote over CLI\n  ids never reused\nend note\nCLI --> User : added #4\ndeactivate CLI\n@enduml",
    "@startuml\n[-> A : start\nA ->] : out\n@enduml",
    "@startuml\nloop 3 times\nA -> B : x\nend\npar\nA -> B : y\nelse\nA -> C : z\nend\n@enduml",
    "@startuml\nAlice -> Bob : hello\nBob --> Alice : hi\n@enduml",
    "@startuml\nautonumber\nAlice -> Bob : a\nBob -> Carol : b\nCarol --> Alice : c\n@enduml",
    "@startuml\nparticipant A\nparticipant B\nA -> B : req\nactivate B\nB -> B : work\nB --> A : resp\ndeactivate B\n@enduml",
    "@startuml\nA -> B : x\nopt cached\n  B --> A : hit\nend\n== phase 2 ==\nA -> C : y\n...\nC --> A : z\n@enduml",
    "@startuml\nbox \"Internal\"\nparticipant A\nparticipant B\nend box\nA -> B : x\n@enduml",
    "@startuml\nA -> B : x\nref over A, B : init\nB -> A : y\n@enduml",
    "@startuml\ncritical tx\n A -> DB : write\nelse rollback\n A -> DB : undo\nend\n@enduml",
    "@startuml\nA -> B : x\nbreak failure\n B --> A : error\nend\n@enduml",
    "@startuml\ngroup retry [3x]\nA -> B : x\nend\n@enduml",
    "@startuml\nA -> B : x\nhnote over A : idle\nB -> A : y\n@enduml",
    "@startuml\ntitle Login\nactor U\nboundary Web\ncontrol Auth\nentity User\nU -> Web : POST /login\nWeb -> Auth : check\nAuth -> User : find\nUser --> Auth : row\nAuth --> Web : token\nWeb --> U : 200\n@enduml",
]


def mutations(d):
    lines = d.split("\n")
    ends = [i for i in range(1, len(lines) - 1) if lines[i].strip() == "end"]
    out = []
    if ends:
        out.append(("drop end", "\n".join(l for k, l in enumerate(lines) if k != ends[-1])))
    out += [("stray end", "\n".join(lines[:-1] + ["end", lines[-1]])),
            ("junk line", "\n".join(lines[:2] + ["this is not plantuml at all"] + lines[2:])),
            ("bad keyword", "\n".join(lines[:-1] + ["alternative maybe", "A -> B : q", "end", lines[-1]])),
            ("no enduml", "\n".join(lines[:-1])),
            ("else outside", "\n".join(lines[:-1] + ["else", lines[-1]])),
            ("unclosed note", "\n".join(lines[:-1] + ["note left of A", "unterminated", lines[-1]])),
            ("missing target", "\n".join(lines[:-1] + ["A -> : x", lines[-1]]))]
    return out


def mcc(pred_ok, ref_ok):
    tp = sum(not p and not r for p, r in zip(pred_ok, ref_ok))
    tn = sum(p and r for p, r in zip(pred_ok, ref_ok))
    fp = sum(not p and r for p, r in zip(pred_ok, ref_ok))
    fn = sum(p and not r for p, r in zip(pred_ok, ref_ok))
    d = math.sqrt((tp + fp) * (tp + fn) * (tn + fp) * (tn + fn))
    return {"mcc": (tp * tn - fp * fn) / d if d else None, "tp": tp, "tn": tn, "fp": fp, "fn": fn}


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("egm")
    ap.add_argument("--java-jar")
    ap.add_argument("--out")
    a = ap.parse_args()
    readme = (HERE.parent / "README.md").read_text()
    base = [b.strip("\n") for b in re.findall(r"```plantuml\n(.*?)```", readme, re.S)] + FIXTURES
    items = [{"kind": "valid", "text": d} for d in base] + [{"kind": k, "text": t} for d in base for k, t in mutations(d)]
    md = Path(tempfile.mkdtemp(prefix="egm-parity-")) / "corpus.md"
    md.write_text("\n\n".join(f"## Technical plan\n\n```plantuml\n{it['text']}\n```" for it in items))
    secs = {}
    for key, extra in (("builtin", ["--plantuml-only"]), ("egm", [])):
        t0 = time.time()
        r = subprocess.run([a.egm, "diagrams", "--json", *extra, str(md)], capture_output=True, text=True)
        secs[key] = round(time.time() - t0, 2)
        rows = [json.loads(l) for l in r.stdout.splitlines()]
        assert len(rows) == len(items), (key, len(rows), len(items), r.stderr[-500:])
        for it, row in zip(items, rows):
            it[key] = row["ok"]
            it[key + "_tool_error"] = row["plantuml_error"]
    out = {"n": len(items), "seconds": secs, "tool_errors": sum(bool(it["builtin_tool_error"]) for it in items)}
    if a.java_jar:
        t0 = time.time()
        for it in items:
            r = subprocess.run(["java", "-jar", a.java_jar, "-syntax"], input=it["text"], capture_output=True, text=True)
            it["java"] = r.returncode == 0 and "ERROR" not in r.stdout
        secs["java (one JVM per diagram)"] = round(time.time() - t0, 1)
        ref = [it["java"] for it in items]
        out["builtin vs java"] = mcc([it["builtin"] for it in items], ref)
        out["egm (structure + builtin) vs java"] = mcc([it["egm"] for it in items], ref)
    by = {}
    for it in items:
        b = by.setdefault(it["kind"], {"n": 0, "builtin_rejects": 0, "egm_rejects": 0, **({"java_rejects": 0} if a.java_jar else {})})
        b["n"] += 1
        b["builtin_rejects"] += not it["builtin"]
        b["egm_rejects"] += not it["egm"]
        if a.java_jar:
            b["java_rejects"] += not it["java"]
    out["by_kind"] = by
    print(json.dumps(out, indent=1))
    summary = {"n": out["n"], "tool_errors": out["tool_errors"], "seconds": secs}
    for k in ("builtin vs java", "egm (structure + builtin) vs java"):
        if k in out:
            summary[k + " MCC"] = out[k]["mcc"]
    print("SUMMARY " + json.dumps(summary))
    if a.out:
        Path(a.out).write_text(json.dumps(out, indent=1))


if __name__ == "__main__":
    main()

import json
import re
import sys


def normalize(value):
    if isinstance(value, bytes):
        return {"bytes": value.hex()}
    if isinstance(value, (tuple, list)):
        return [normalize(item) for item in value]
    if isinstance(value, dict):
        return {key: normalize(item) for key, item in value.items()}
    return value


def match_data(match):
    if match is None:
        return None
    return normalize({
        "span": match.span(),
        "group": match.group(),
        "groups": match.groups(),
        "groupdict": match.groupdict(),
        "regs": match.regs,
        "lastindex": match.lastindex,
        "lastgroup": match.lastgroup,
    })


def evaluate(case):
    pattern = case["pattern"]
    subject = case["subject"]
    replacement = case["replacement"]
    if case["bytes"]:
        pattern = pattern.encode("utf-8")
        subject = subject.encode("utf-8")
        replacement = replacement.encode("utf-8")
    compiled = re.compile(pattern)
    pos = case["pos"]
    endpos = case["endpos"]
    return normalize({
        "search": match_data(compiled.search(subject, pos, endpos)),
        "match": match_data(compiled.match(subject, pos, endpos)),
        "fullmatch": match_data(compiled.fullmatch(subject, pos, endpos)),
        "finditer": [match_data(match) for match in compiled.finditer(subject, pos, endpos)],
        "findall": compiled.findall(subject, pos, endpos),
        "subn": compiled.subn(replacement, subject),
        "split": compiled.split(subject),
    })


if __name__ == "__main__":
    fixture = json.loads(sys.stdin.read())
    mismatches = []
    for index, case in enumerate(fixture["cases"]):
        actual = evaluate(case)
        if actual != case["expected"]:
            mismatches.append({"index": index, "case": case, "actual": actual})
    assert not mismatches, json.dumps(mismatches[:3], ensure_ascii=True)
    print("SRE_DIFFERENTIAL_OK", len(fixture["cases"]), fixture["oracle"])

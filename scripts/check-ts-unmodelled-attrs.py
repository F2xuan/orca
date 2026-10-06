"""Fail if ts-rs ignored a serde attribute we have not reasoned about.

ts-rs prints `warning: failed to parse serde attribute` and then *generates
anyway* — so an attribute it cannot model silently produces TypeScript that
disagrees with the wire. That is how both of the errors found in §30 and §31
happened, so the codegen treats the warning as fatal here — except for the
attributes where being ignored is precisely the behaviour we want.

Reads cargo's output on stdin; see scripts/codegen-types.sh.
"""
import re
import sys

# Ignored on purpose. `#[serde(other)]` constrains deserialisation only, and the
# arm ts-rs emits for the catch-all variant is what a client needs (§31).
KNOWN = {"other"}

WARNING = "failed to parse serde attribute"
ATTRIBUTE = re.compile(r"\s*\|\s*([A-Za-z_][A-Za-z0-9_]*)\s*$")

lines = sys.stdin.read().splitlines()
ignored = []
for number, line in enumerate(lines):
    if WARNING not in line:
        continue
    for candidate in lines[number + 1 : number + 6]:
        match = ATTRIBUTE.match(candidate)
        if match:
            ignored.append(match.group(1))
            break

unknown = sorted(set(ignored) - KNOWN)
if unknown:
    print(f"ts-rs ignored {len(unknown)} serde attribute(s) with no known reason:")
    for attribute in unknown:
        print(f"  {attribute}")
    print("  ...the generated TypeScript would silently disagree with the wire.")
    print("  Either model it (see ts(optional)/ts(skip)/TS_RS_LARGE_INT in §30-§32),")
    print("  or add it to KNOWN here with the reasoning.")
    sys.exit(1)

if ignored:
    print(f"ok: ts-rs ignored {len(ignored)} attribute(s), all accounted for: {sorted(set(ignored))}")
else:
    print("ok: ts-rs modelled every serde attribute")

#!/usr/bin/env python3
"""Fix broken build_call() patterns in runtime.rs and builtins.rs.

build_call() returns Result<CallSiteValue, BuilderError>.
CallSiteValue does NOT have .into_pointer_value(), .into_int_value(), etc.
We need .try_as_basic_value().left().unwrap() to extract a BasicValueEnum first.
"""

import re
import sys

def find_matching_paren(text, start):
    """Find matching closing paren for '(' at position start. Returns position of ')'."""
    depth = 1
    i = start + 1
    while i < len(text) and depth > 0:
        if text[i] == '(':
            depth += 1
        elif text[i] == ')':
            depth -= 1
        i += 1
    if depth == 0:
        return i - 1
    return -1

def fix_patterns(text):
    """Find all broken build_call patterns and return list of (pos, old_suffix, new_suffix) fixes."""
    fixes = []

    for m in re.finditer(r'\bbuild_call\(', text):
        paren_start = m.end() - 1  # position of '('
        paren_end = find_matching_paren(text, paren_start)
        if paren_end == -1:
            continue

        after = text[paren_end:paren_end+80]

        # Pattern 1: ).map_err(llvm_err)?;.into_X_value(  (broken semicolon)
        m1 = re.match(r'\)\.map_err\(llvm_err\)\?;\.(into_\w+_value\()', after)
        if m1:
            old = m1.group(0)
            new = f').map_err(llvm_err)?.try_as_basic_value().left().unwrap().{m1.group(1)}'
            fixes.append((paren_end, old, new))
            continue

        # Pattern 2: ).map_err(llvm_err)?.into_X_value(  (missing try_as_basic_value)
        m2 = re.match(r'\)\.map_err\(llvm_err\)\?\.(into_\w+_value\()', after)
        if m2:
            old = m2.group(0)
            new = f').map_err(llvm_err)?.try_as_basic_value().left().unwrap().{m2.group(1)}'
            fixes.append((paren_end, old, new))
            continue

        # Pattern 3: )?;.into_X_value(  (no map_err + semicolon)
        m3 = re.match(r'\)\?;\.(into_\w+_value\()', after)
        if m3:
            old = m3.group(0)
            new = f').map_err(llvm_err)?.try_as_basic_value().left().unwrap().{m3.group(1)}'
            fixes.append((paren_end, old, new))
            continue

        # Pattern 4: )?.into_X_value(  (no map_err)
        m4 = re.match(r'\)\?\.(into_\w+_value\()', after)
        if m4:
            old = m4.group(0)
            new = f').map_err(llvm_err)?.try_as_basic_value().left().unwrap().{m4.group(1)}'
            fixes.append((paren_end, old, new))
            continue

    return fixes

def apply_fixes(text, fixes):
    """Apply fixes from end to start to preserve positions."""
    # Sort by position descending
    fixes.sort(key=lambda x: x[0], reverse=True)
    for pos, old, new in fixes:
        end = pos + len(old)
        if text[pos:end] != old:
            print(f"WARNING: position mismatch at {pos}: expected {old!r}, got {text[pos:end]!r}")
            continue
        text = text[:pos] + new + text[end:]
    return text

def main():
    files = sys.argv[1:] if len(sys.argv) > 1 else [
        'src/codegen/runtime.rs',
        'src/codegen/builtins.rs',
    ]

    total_fixes = 0
    for filepath in files:
        with open(filepath, 'r') as f:
            original = f.read()

        fixes = fix_patterns(original)
        if fixes:
            fixed = apply_fixes(original, fixes)
            with open(filepath, 'w') as f:
                f.write(fixed)

        print(f"{filepath}: {len(fixes)} fixes applied")
        total_fixes += len(fixes)

    print(f"Total: {total_fixes} fixes")


if __name__ == '__main__':
    main()

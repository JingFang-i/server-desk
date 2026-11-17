#!/usr/bin/env python3
"""检查所有 .vue 模板中使用的 <n-xxx> 组件是否都在 script 中导入了对应 Nxxx。
防止「未导入组件静默渲染为空」这类问题（曾导致审计表格空白）。"""
import re, glob, sys

def camel(tag: str) -> str:
    return "N" + "".join(w.capitalize() for w in tag.split("-")[1:])

issues = []
for f in glob.glob("src/**/*.vue", recursive=True):
    src = open(f, encoding="utf-8").read()
    used = {m.group(1) for m in re.finditer(r"<(n-[a-z][a-z0-9-]*)", src)}
    imported = set(re.findall(r"\b(N[A-Z][A-Za-z]*)", src))
    for tag in sorted(used):
        comp = camel(tag)
        if comp not in imported:
            issues.append(f"{f}: 模板使用了 <{tag}> 但未导入 {comp}")

if issues:
    print("\n".join(issues))
    sys.exit(1)
print("vue imports OK")

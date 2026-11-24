#!/usr/bin/env python3
"""检查所有 .vue 模板中使用的 <n-xxx> 组件是否都在 script 中导入了对应 Nxxx。
防止「未导入组件静默渲染为空」这类问题（曾导致审计表格空白）。

路径以**脚本自身位置**为基准（apps/desktop），与 CWD 无关：
之前在仓库根目录按 AGENTS.md 的写法运行时会匹配 0 个文件，却照样打印 OK。
"""
import glob
import os
import re
import sys

APP_DIR = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))


def camel(tag: str) -> str:
    return "N" + "".join(w.capitalize() for w in tag.split("-")[1:])


def main() -> int:
    files = sorted(glob.glob(os.path.join(APP_DIR, "src", "**", "*.vue"), recursive=True))
    if not files:
        print(f"未找到任何 .vue 文件（查找基准：{APP_DIR}/src）", file=sys.stderr)
        return 2

    issues = []
    for path in files:
        with open(path, encoding="utf-8") as f:
            src = f.read()
        used = {m.group(1) for m in re.finditer(r"<(n-[a-z][a-z0-9-]*)", src)}
        imported = set(re.findall(r"\b(N[A-Z][A-Za-z]*)", src))
        rel = os.path.relpath(path, APP_DIR)
        for tag in sorted(used):
            comp = camel(tag)
            if comp not in imported:
                issues.append(f"{rel}: 模板使用了 <{tag}> 但未导入 {comp}")

    if issues:
        print("\n".join(issues))
        return 1
    print(f"vue imports OK（检查 {len(files)} 个文件）")
    return 0


if __name__ == "__main__":
    sys.exit(main())

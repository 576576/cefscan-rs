#!/usr/bin/env python3
"""workflow 静态校验：YAML 可解析 + 每个 `run:` 过 `bash -n` + 注释块 ≤ 1 行 + 结构断言。

改完 `.github/workflows/` 下的文件先跑这个。推上去试错一轮要十几分钟，还会多出一个
alpha Release，而这些错（YAML 缩进、shell 语法、触发写歪、job 依赖漏挂）本地全都能挡。

需要 pyyaml，用托管 venv 的解释器跑：

    "C:/Users/16695/.workbuddy-ai/binaries/python/envs/default/Scripts/python.exe" tools/ci_check.py

不入库之外的依赖，也不联网。
"""

from __future__ import annotations

import pathlib
import subprocess
import sys

import yaml

ROOT = pathlib.Path(__file__).resolve().parents[1]
WORKFLOWS = ROOT / ".github" / "workflows"

# 单个连续注释块（相邻的 `#` 行）允许的最大行数。跟 Suwayomi-next 的约定一致：
# 决策与背景写进 docs，workflow 里只留一行提示。
MAX_COMMENT_BLOCK = 1

failures: list[str] = []


def _find_bash() -> str:
    """点名 Git 自带的 bash.exe：PATH 里靠前的那个是 System32 下的 WSL 转发器。"""
    for candidate in (
        pathlib.Path(r"C:\Program Files\Git\usr\bin\bash.exe"),
        pathlib.Path(r"C:\Program Files (x86)\Git\usr\bin\bash.exe"),
    ):
        if candidate.is_file():
            return str(candidate)
    raise SystemExit("找不到 Git for Windows 的 bash.exe")


BASH = _find_bash()


def check(ok: bool, label: str, extra: str = "") -> None:
    print(("  ok   " if ok else "  FAIL ") + label + (f" -> {extra}" if not ok and extra else ""))
    if not ok:
        failures.append(label)


def walk_run_steps(doc: dict) -> list[tuple[str, str]]:
    """产出 (路径描述, 脚本文本)。"""
    out: list[tuple[str, str]] = []
    for job_name, job in (doc.get("jobs") or {}).items():
        for idx, step in enumerate(job.get("steps") or []):
            script = step.get("run")
            if isinstance(script, str):
                out.append((f"{job_name}/{step.get('name', f'step#{idx + 1}')}", script))
    return out


def bash_syntax(script: str) -> tuple[bool, str]:
    proc = subprocess.run(
        [BASH, "-n", "-"],
        input=script,
        capture_output=True,
        text=True,
        encoding="utf-8",
        errors="replace",
    )
    return proc.returncode == 0, (proc.stderr or proc.stdout or "").strip()


def comment_blocks(text: str) -> list[tuple[int, int, str]]:
    """返回 (起始行, 行数, 首行内容) 的连续注释块。"""
    blocks: list[tuple[int, int, str]] = []
    start, count, first = 0, 0, ""
    for i, line in enumerate(text.splitlines(), start=1):
        stripped = line.strip()
        if stripped.startswith("#"):
            if count == 0:
                start, first = i, stripped
            count += 1
        else:
            if count:
                blocks.append((start, count, first))
            count = 0
    if count:
        blocks.append((start, count, first))
    return blocks


def triggers(doc: dict) -> dict:
    """`on:` 在 YAML 1.1 里是布尔字面量，safe_load 会把键解析成 True —— 两种都认。"""
    value = doc.get("on", doc.get(True))
    return value if isinstance(value, dict) else {}


def main() -> int:
    for path in sorted(WORKFLOWS.glob("*.yml")):
        text = path.read_text(encoding="utf-8")
        name = path.name
        print(f"[{name}]")
        try:
            doc = yaml.safe_load(text)
        except yaml.YAMLError as exc:
            check(False, f"{name} YAML 可解析", str(exc).splitlines()[0])
            continue
        check(True, f"{name} YAML 可解析")

        steps = walk_run_steps(doc)
        check(bool(steps), f"{name} 找到 {len(steps)} 个 run 步骤")
        for label, script in steps:
            ok, err = bash_syntax(script)
            check(ok, f"{name} :: {label} 过 bash -n", err)

        long_blocks = [b for b in comment_blocks(text) if b[1] > MAX_COMMENT_BLOCK]
        check(
            not long_blocks,
            f"{name} 注释块都 ≤ {MAX_COMMENT_BLOCK} 行",
            "; ".join(f"L{b[0]} {b[1]} 行：{b[2][:40]}" for b in long_blocks),
        )

        # 结构断言：触发方式与 job 依赖写歪了最难在本地看出来（推上去才知道根本没触发）。
        jobs = set((doc.get("jobs") or {}).keys())
        trig = triggers(doc)
        if name == "lint.yml":
            check(set(trig) == {"workflow_call", "workflow_dispatch"}, f"{name} 只由 workflow_call / dispatch 触发", str(set(trig)))
            check("push" not in trig and "pull_request" not in trig, f"{name} 不跟 push / PR")
            check(jobs == {"frontend", "fmt", "clippy", "test"}, f"{name} 四个 job 齐全", str(sorted(jobs)))
        elif name == "build.yml":
            check(set(trig) == {"workflow_call"}, f"{name} 只由 workflow_call 触发", str(set(trig)))
            check(jobs == {"build"}, f"{name} 只有 build job", str(sorted(jobs)))
            inputs = set((trig.get("workflow_call") or {}).get("inputs") or {})
            check(inputs == {"version", "channel"}, f"{name} 接收 version / channel", str(sorted(inputs)))
        elif name == "release.yml":
            check({"push", "workflow_dispatch"} <= set(trig), f"{name} 有 push 与 workflow_dispatch", str(set(trig)))
            check("paths-ignore" in (trig.get("push") or {}), f"{name} push 配了 paths-ignore")
            check(jobs == {"prep", "lint", "build", "publish"}, f"{name} 四个 job 齐全", str(sorted(jobs)))
            uses = {k: v.get("uses") for k, v in (doc.get("jobs") or {}).items() if v.get("uses")}
            check(uses.get("lint") == "./.github/workflows/lint.yml", f"{name} lint job 复用 lint.yml", str(uses))
            check(uses.get("build") == "./.github/workflows/build.yml", f"{name} build job 复用 build.yml", str(uses))
            needs = set(doc["jobs"]["build"].get("needs") or [])
            check({"prep", "lint"} <= needs, f"{name} build 依赖 prep + lint（门禁不过就不编译）", str(sorted(needs)))
        else:
            check(False, f"{name} 是预期之外的 workflow", "只应有 lint / build / release 三个")

        total = len(text.splitlines())
        cmt = sum(1 for ln in text.splitlines() if ln.strip().startswith("#"))
        print(f"        {total} 行，其中注释 {cmt} 行")

    print()
    if failures:
        print(f"FAILED: {len(failures)} 项")
        for f in failures:
            print("  - " + f)
        return 1
    print("ALL OK")
    return 0


if __name__ == "__main__":
    sys.exit(main())

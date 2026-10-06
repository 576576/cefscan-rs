#!/usr/bin/env python3
"""`release.yml` 里两段「算出来的」脚本的真跑校验：prep 的目标矩阵、publish 的发行说明正文。

`ci_check.py` 只做静态检查（YAML 可解析 / `bash -n` / 结构断言），这两段的**取值**它看不见
—— 矩阵是内联 `python3` 展开的，正文是一行行拼出来的，写错了 `bash -n` 一样过。这里把两段
`run` 块抽出来、打桩后在 Git Bash 下真跑一遍，断言具体输出。

Windows 本地工具（要 Git Bash），不进 CI —— 与 `preview_ui.py` / `gui_smoke.py` 同类。
"""

from __future__ import annotations

import json
import os
import pathlib
import re
import shlex
import shutil
import subprocess
import sys
import tempfile

import yaml

ROOT = pathlib.Path(__file__).resolve().parents[1]
WORKFLOW = ROOT / ".github" / "workflows" / "release.yml"
TMP_ROOT = ROOT / "target"

VERSION_SAMPLE = "0.0.64"
ALL_TARGETS = [
    "windows-x86_64",
    "windows-x86_64-gnullvm",
    "windows-arm64",
    "windows-arm64-gnullvm",
    "linux-x86_64",
    "linux-arm64",
]

failures: list[str] = []


def find_bash() -> str:
    # 非 Windows 直接找 PATH 里的 bash；Windows 只认 Git for Windows —— PATH 里的
    # `bash` 可能是 WSL 的壳，脚本里的 POSIX 路径在那边全失效。
    if os.name != "nt":
        found = shutil.which("bash")
        if found:
            return found
        raise SystemExit("找不到 bash")
    for candidate in (
        pathlib.Path(r"C:\Program Files\Git\usr\bin\bash.exe"),
        pathlib.Path(r"C:\Program Files (x86)\Git\usr\bin\bash.exe"),
    ):
        if candidate.is_file():
            return str(candidate)
    raise SystemExit("找不到 Git for Windows 的 bash.exe")


BASH = find_bash()


def check(ok: bool, label: str, extra: str = "") -> None:
    print(("  ok   " if ok else "  FAIL ") + label + (f" -> {extra}" if not ok and extra else ""))
    if not ok:
        failures.append(label)


def run(script: str, cwd: pathlib.Path, env: dict[str, str]) -> subprocess.CompletedProcess:
    merged = os.environ.copy()
    merged.update(env)
    return subprocess.run(
        [BASH, "-c", script],
        cwd=cwd,
        env=merged,
        capture_output=True,
        text=True,
        encoding="utf-8",
        errors="replace",
    )


def render(script: str) -> str:
    """把 Actions 的 `${{ }}` 表达式换成 shell 变量引用，好在本机跑。"""
    def repl(match: re.Match) -> str:
        expr = match.group(1).strip()
        if expr == "github.event_name":
            return "${EVENT}"
        if expr == "github.run_id":
            return "99999"
        if expr.startswith("github.event.inputs."):
            return "${" + expr.removeprefix("github.event.inputs.") + "}"
        raise SystemExit(f"没见过的表达式：${{{{ {expr} }}}}")

    out = re.sub(r"\$\{\{\s*(.*?)\s*\}\}", repl, script)
    # prep 里的内联解释器要用本机这个（Git Bash 的 python3 未必有 pyyaml，也不必要）。
    return out.replace("python3 - ", f"{shlex.quote(sys.executable)} - ")


DOC = yaml.safe_load(WORKFLOW.read_text(encoding="utf-8"))
STEPS = {
    step.get("name"): step["run"]
    for step in DOC["jobs"]["prep"]["steps"] + DOC["jobs"]["publish"]["steps"]
    if step.get("run")
}
PREP = render(STEPS["推导版本与构建矩阵"])
PUBLISH = render(STEPS["创建 Release"])


def out_of(path: pathlib.Path) -> dict[str, str]:
    values: dict[str, str] = {}
    for line in path.read_text(encoding="utf-8").splitlines():
        key, _, value = line.partition("=")
        values[key] = value
    return values


def prep_case(name: str, event: str, inputs: dict[str, str], targets: list[str] | None, expect_error: bool = False) -> None:
    with tempfile.TemporaryDirectory(dir=TMP_ROOT) as tmp:
        out_file = pathlib.Path(tmp) / "gh_output"
        out_file.write_text("", encoding="utf-8")
        env = {"EVENT": event, "GITHUB_OUTPUT": out_file.as_posix()}
        env.update(inputs)
        proc = run(PREP, ROOT, env)
        log = (proc.stdout or "") + (proc.stderr or "")
        if expect_error:
            check(proc.returncode != 0 and "::error::" in log, f"prep :: {name}",
                  f"rc={proc.returncode} 输出={log.strip()[:120]}")
            return
        if proc.returncode != 0:
            check(False, f"prep :: {name}", f"rc={proc.returncode} {log.strip()[:200]}")
            return
        got = out_of(out_file)
        matrix = json.loads(got.get("targets", "[]"))
        names = [m["target"] for m in matrix]
        count = int(subprocess.run(["git", "rev-list", "--count", "HEAD"], cwd=ROOT,
                                   capture_output=True, text=True).stdout.strip())
        want_version = f"0.{count // 100}.{count % 100:02d}"
        check(names == targets, f"prep :: {name} 目标", f"{names} != {targets}")
        check(got.get("version") == want_version, f"prep :: {name} 版本号", f"{got.get('version')} != {want_version}")
        check(bool(got.get("tag")) and bool(got.get("prerelease")), f"prep :: {name} tag / prerelease",
              str({k: got.get(k) for k in ("tag", "prerelease")}))
        check(all({"target", "os", "rust_target", "exe"} <= set(m) for m in matrix),
              f"prep :: {name} 矩阵元素字段齐全")


def publish_case(name: str, targets: list[str], version: str, channel: str, pre: str,
                 notes: str = "", view_rc: str = "1",
                 expect: tuple[str, ...] = (), forbid: tuple[str, ...] = ()) -> None:
    with tempfile.TemporaryDirectory(dir=TMP_ROOT) as tmp:
        work = pathlib.Path(tmp)
        (work / "artifacts").mkdir()
        for target in targets:
            (work / "artifacts" / f"cefscan-{version}-{target}.zip").write_bytes(b"PK")
        # gh 打桩：`release view` 按 VIEW_RC 决定 tag 是否存在，其余只把参数打出来。
        stub = 'gh() {\n  if [ "$1" = "release" ] && [ "$2" = "view" ]; then return "${VIEW_RC:-1}"; fi\n  printf \'GH:\'; printf \' %s\' "$@"; printf \'\\n\'\n}\n'
        proc = run(stub + PUBLISH, work, {
            "TAG": f"v{version}-{channel}.99999" if channel != "release" else f"v{version}",
            "VERSION": version, "CHANNEL": channel, "PRE": pre, "NOTES": notes,
            "VIEW_RC": view_rc, "GITHUB_REPOSITORY": "576576/cefscan-rs",
        })
        log = (proc.stdout or "") + (proc.stderr or "")
        missing = [e for e in expect if e not in log]
        leaked = [f for f in forbid if f in log]
        check(proc.returncode == 0 and not missing and not leaked, f"publish :: {name}",
              f"rc={proc.returncode} 缺={missing} 多={leaked}")


def main() -> int:
    TMP_ROOT.mkdir(parents=True, exist_ok=True)

    print("[prep] 目标矩阵")
    prep_case("push main（自动 alpha，固定 x64 双平台）", "push", {},
              ["windows-x86_64", "linux-x86_64"])
    prep_case("dispatch 默认（msvc + x64 双平台）", "workflow_dispatch",
              {"channel": "alpha", "windows_toolchain": "msvc", "build_windows_x64": "true",
               "build_windows_arm64": "false", "build_linux_x64": "true", "build_linux_arm64": "false"},
              ["windows-x86_64", "linux-x86_64"])
    prep_case("dispatch gnullvm + 四个架构全勾（只出 gnullvm 那份）", "workflow_dispatch",
              {"channel": "beta", "windows_toolchain": "gnullvm", "build_windows_x64": "true",
               "build_windows_arm64": "true", "build_linux_x64": "true", "build_linux_arm64": "true"},
              ["windows-x86_64-gnullvm", "windows-arm64-gnullvm", "linux-x86_64", "linux-arm64"])
    prep_case("dispatch 只勾 arm64", "workflow_dispatch",
              {"channel": "release", "windows_toolchain": "msvc", "build_windows_x64": "false",
               "build_windows_arm64": "true", "build_linux_x64": "false", "build_linux_arm64": "true"},
              ["windows-arm64", "linux-arm64"])
    prep_case("dispatch 工具链 all + 四个架构全勾（Windows 目标翻倍）", "workflow_dispatch",
              {"channel": "beta", "windows_toolchain": "all", "build_windows_x64": "true",
               "build_windows_arm64": "true", "build_linux_x64": "true", "build_linux_arm64": "true"},
              ALL_TARGETS)
    prep_case("dispatch 什么都不选（应报错）", "workflow_dispatch",
              {"channel": "alpha", "windows_toolchain": "msvc", "build_windows_x64": "false",
               "build_windows_arm64": "false", "build_linux_x64": "false", "build_linux_arm64": "false"},
              None, expect_error=True)
    prep_case("dispatch 工具链写错（应报错）", "workflow_dispatch",
              {"channel": "alpha", "windows_toolchain": "gcc", "build_windows_x64": "true",
               "build_windows_arm64": "false", "build_linux_x64": "false", "build_linux_arm64": "false"},
              None, expect_error=True)
    prep_case("dispatch 通道写错（应报错）", "workflow_dispatch",
              {"channel": "nightly", "windows_toolchain": "msvc", "build_windows_x64": "true",
               "build_windows_arm64": "false", "build_linux_x64": "false", "build_linux_arm64": "false"},
              None, expect_error=True)

    print("[publish] 发行说明正文")
    url = f"https://github.com/576576/cefscan-rs/releases/download/v{VERSION_SAMPLE}-alpha.99999"
    publish_case("push 自动 alpha（无手填说明）", ["windows-x86_64", "linux-x86_64"],
                 VERSION_SAMPLE, "alpha", "true",
                 expect=("--title cefscan 0.0.64（alpha）", "--generate-notes", "--prerelease",
                         "| 平台 | 架构 | 工具链 | 下载 |",
                         f"| Windows | x86_64 | MSVC | [`cefscan-0.0.64-windows-x86_64.zip`]({url}/cefscan-0.0.64-windows-x86_64.zip) |",
                         "| Linux | x86_64 | MSVC |"),
                 forbid=("arm64", "gnullvm 版本额外附带"))
    publish_case("六种产物齐备（6 行 + gnullvm DLL 说明）", ALL_TARGETS,
                 VERSION_SAMPLE, "beta", "false",
                 expect=("| Windows | x86_64 | MSVC |", "| Windows | x86_64 | gnullvm |",
                         "| Windows | arm64 | MSVC |", "| Windows | arm64 | gnullvm |",
                         "| Linux | x86_64 | MSVC |", "| Linux | arm64 | MSVC |",
                         "> gnullvm 版本额外附带 `WebView2Loader.dll`", "--generate-notes"),
                 forbid=("--prerelease",))
    publish_case("手填说明在最前（\\n 还原）", ["windows-x86_64"], VERSION_SAMPLE, "release", "false",
                 notes="定向验证：只跑 windows x64\\n第二行说明",
                 expect=("定向验证：只跑 windows x64", "第二行说明", "--generate-notes"))
    publish_case("release 通道同 tag 已存在（先删后建）", ["linux-arm64"],
                 VERSION_SAMPLE, "release", "false", view_rc="0",
                 expect=(f"GH: release delete v{VERSION_SAMPLE} --yes --cleanup-tag",
                         "| Linux | arm64 | MSVC |", "--generate-notes"))

    # 顺序断言：手填说明必须排在架构矩阵之前（changelog 由 GitHub 追加在最后）。
    with tempfile.TemporaryDirectory(dir=TMP_ROOT) as tmp:
        work = pathlib.Path(tmp)
        (work / "artifacts").mkdir()
        (work / "artifacts" / f"cefscan-{VERSION_SAMPLE}-linux-x86_64.zip").write_bytes(b"PK")
        stub = 'gh() {\n  if [ "$1" = "release" ] && [ "$2" = "view" ]; then return 1; fi\n  printf \'GH:\'; printf \' %s\' "$@"; printf \'\\n\'\n}\n'
        proc = run(stub + PUBLISH, work, {
            "TAG": f"v{VERSION_SAMPLE}", "VERSION": VERSION_SAMPLE, "CHANNEL": "release",
            "PRE": "false", "NOTES": "手填说明在表之前", "GITHUB_REPOSITORY": "576576/cefscan-rs",
        })
        log = proc.stdout or ""
        check(proc.returncode == 0 and 0 <= log.find("手填说明在表之前") < log.find("| 平台 | 架构 | 工具链 | 下载 |"),
              "publish :: 顺序：手填说明在架构矩阵之前",
              f"note@{log.find('手填说明在表之前')} table@{log.find('| 平台 | 架构 | 工具链 | 下载 |')}")

    print()
    if failures:
        print(f"FAILED: {len(failures)} 项")
        for item in failures:
            print("  - " + item)
        return 1
    print("ALL OK")
    return 0


if __name__ == "__main__":
    sys.exit(main())

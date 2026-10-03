"""生成一份带假数据的 cefscanw 前端预览页（只用于本地看效果，不入库）。

用法：
    python tools/preview_ui.py <输出目录>
然后拿浏览器打开 <输出目录>/index.html 截图即可。
"""
import pathlib
import re
import shutil
import sys

ROOT = pathlib.Path(__file__).resolve().parent.parent
UI = ROOT / "crates" / "cefscan-desktop" / "ui"

DEMO = [
    ("WorkBuddy", "electron", 1395864371, True,
     r"C:\Users\16695\AppData\Local\Programs\WorkBuddy\WorkBuddy.exe"),
    ("Microsoft VS Code", "electron", 1012424704, False,
     r"C:\Users\16695\AppData\Local\Programs\Microsoft VS Code\Code.exe"),
    ("Paradox Launcher", "electron", 376438784, False,
     r"C:\Users\16695\AppData\Local\Programs\Paradox Interactive\launcher\launcher-v2.2025.1\Paradox Launcher.exe"),
    ("Hearts of Iron IV", "cef", 268435456, True,
     r"D:\Steam\steamapps\common\Hearts of Iron IV\dowser.exe"),
    ("Steam", "cef", 214748364, False, r"C:\Program Files (x86)\Steam\steamwebhelper.exe"),
    ("Postman", "electron", 134217728, False,
     r"C:\Users\16695\AppData\Local\Postman\app-8.0.0\Postman.exe"),
    ("msedge", "edge", 100663296, True,
     r"C:\Program Files (x86)\Microsoft\Edge\Application\154.0.4258.37\msedge.exe"),
    ("腾讯会议", "electron", 83886080, False,
     r"C:\Program Files (x86)\Tencent\WeMeet\wemeetapp.exe"),
]


def js(value: object) -> str:
    """把 Python 字面量写成 JavaScript 字面量。

    注意 bool 必须先判：`isinstance(True, int)` 也是 True，直接走 repr 会写出
    `True` / `False` —— 那不是 JavaScript，整个桩脚本会当场 ReferenceError。
    """
    if isinstance(value, bool):
        return "true" if value else "false"
    return repr(value)


def stub() -> str:
    rows = ",\n".join(
        "        [" + ", ".join(js(v) for v in row) + "]" for row in DEMO
    )
    # 整体包在 IIFE 里：经典脚本的顶层 `class` / `const` 进的是**全局词法作用域**，
    # 而 main.js 顶层写的是 `const { invoke, Channel } = window.__TAURI__.core`。
    # 如果这里直接 `class Channel {}`，那个名字就已经被占住了，main.js 会以
    # "Identifier 'Channel' has already been declared" 整体解析失败——页面看起来
    # 只是"点了按钮没反应"，非常难查。IIFE 里的声明不会泄漏到全局词法作用域。
    return f"""    <script>
      (() => {{
        // 预览桩：假装自己是 Tauri，把真实的前端逻辑跑起来。
        const DEMO = [
{rows}
        ];
        window.__TAURI__ = {{
          core: {{
            Channel: class {{
              constructor() {{
                window.__previewChannel = this;
              }}
            }},
            invoke: async (cmd, args) => {{
              if (cmd !== 'scan_apps') return;
              const ch = args.channel;
              ch.onmessage({{ type: 'started', backend: 'Everything' }});
              for (const [name, kind, size, running, path] of DEMO) {{
                ch.onmessage({{ type: 'item', name, kind, size, running, path,
                  evidence: 'Electron Framework' }});
              }}
              ch.onmessage({{ type: 'done', backend: 'Everything', apps: DEMO.length,
                totalBytes: 3418357760, sumBytes: 3418357760, elapsedMs: 412,
                dirsScanned: 1345 }});
            }},
          }},
        }};
      }})();
    </script>
"""


def main() -> int:
    out = pathlib.Path(sys.argv[1] if len(sys.argv) > 1 else ROOT / "target" / "uipreview")
    out.mkdir(parents=True, exist_ok=True)
    for name in ("styles.css", "main.js"):
        shutil.copy(UI / name, out / name)
    if (out / "assets").exists():
        shutil.rmtree(out / "assets")
    shutil.copytree(UI / "assets", out / "assets")

    html = (UI / "index.html").read_text(encoding="utf-8")
    html = html.replace(
        '    <script src="main.js"></script>',
        stub()
        + '    <script src="main.js"></script>\n'
        # 点一下开始扫描，再等揭示队列放完，把状态写进 title 方便无头浏览器读。
        + """    <script>
      document.getElementById('scan-button').click();
      setTimeout(() => {
        document.title = 'status=' + document.getElementById('status').textContent
          + ' | backend=' + document.getElementById('backend-display').textContent
          + ' | rows=' + document.querySelectorAll('#results-body tr[data-path]').length;
      }, 2500);
    </script>
""",
    )
    (out / "index.html").write_text(html, encoding="utf-8")
    print(f"preview written to {out / 'index.html'}")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())

"""生成一份带假数据的 cefscanw 前端预览页（只用于本地看效果，不入库）。"""
import pathlib
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

# 等状态落定的上限（毫秒）。
SETTLE_MS = 20000


def js(value: object) -> str:
    """把 Python 字面量写成 JavaScript 字面量。"""
    if isinstance(value, bool):
        return "true" if value else "false"
    return repr(value)


def demo_rows(count: int) -> list[tuple]:
    rows = []
    for index in range(count):
        name, kind, size, running, path = DEMO[index % len(DEMO)]
        if index >= len(DEMO):
            suffix = index // len(DEMO) + 1
            name = f"{name} {suffix}"
            path = path.replace(".exe", f"-{suffix}.exe")
            size = size // (suffix + 1)
        rows.append((name, kind, size, running, path))
    return rows


def stub(rows: list[tuple]) -> str:
    encoded = ",\n".join(
        "        [" + ", ".join(js(v) for v in row) + "]" for row in rows
    )
    # 整体包在 IIFE 里，避免顶层声明泄漏到全局词法作用域。
    return f"""    <script>
      (() => {{
        // 预览桩：假装自己是 Tauri，把真实的前端逻辑跑起来。
        const DEMO = [
{encoded}
        ];
        window.__TAURI__ = {{
          core: {{
            Channel: class {{
              constructor() {{
                window.__previewChannel = this;
              }}
            }},
            invoke: async (cmd, args) => {{
              // 后端探测：进工具模式时和点 chip 时都会走这里。
              if (cmd === 'detect_backend') return {{ backend: 'Everything' }};
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


def driver(mode: str) -> str:
    """选好模式、点开始扫描，等揭示队列放完，把状态写进 title 方便无头浏览器读。"""
    if mode == "picker":
        return """    <script>
      // 初始选择页：什么都不点，只把当前视图写进 title 方便断言。
      document.title = 'view=picker | rows='
        + document.querySelectorAll('#results-body tr[data-path]').length
        + ' | cards=' + document.querySelectorAll('#cards .card').length;
    </script>
"""
    return f"""    <script>
      (() => {{
        const radio = document.getElementById({js('mode-' + mode)});
        if (radio) radio.checked = true;
        document.getElementById('start-button').click();
        // 工具模式下还要再点一下工具栏的"开始扫描"。
        if ({js(mode)} === 'tool') document.getElementById('scan-button').click();

        const report = () => {{
          const c = document.getElementById('cards');
          const card = c.querySelector('.card');
          const style = getComputedStyle(c);
          const pitch = card ? card.offsetHeight + (Number.parseFloat(style.rowGap) || 0) : 0;
          const padTop = Number.parseFloat(style.paddingTop) || 0;
          // 对齐判据：视口顶部落在行顶边上 ⟺ (scrollTop - padTop) 是行距的整数倍。
          const align = pitch > 0 ? (c.scrollTop - padTop) % pitch : 'n/a';
          // 经典模式的状态节点是 #classic-count，工具模式是 #status。
          const statusNode = document.getElementById(
            {js(mode)} === 'classic' ? 'classic-count' : 'status'
          );
          document.title = 'status=' + statusNode.textContent
            + ' | backend=' + document.getElementById('backend-display').textContent
            + ' | cards=' + c.querySelectorAll('.card').length
            + ' | rows=' + document.querySelectorAll('#results-body tr[data-path]').length
            + ' | view=' + {js(mode)}
            + ' | scroll=' + c.scrollTop + '/' + (c.scrollHeight - c.clientHeight)
            + ' clientH=' + c.clientHeight
            + ' inner=' + window.innerWidth + 'x' + window.innerHeight
            + ' pitch=' + pitch
            + ' padTop=' + padTop
            + ' padBottom=' + (Number.parseFloat(style.paddingBottom) || 0)
            + ' 对齐残差=' + align;
        }};

        // 扫完的标志是刷新胶囊恢复可用（`scanning` 一挂上它就 disabled）。
        const deadline = Date.now() + {SETTLE_MS};
        const settle = () => {{
          const busy = document.getElementById('classic-refresh').disabled;
          if (busy && Date.now() < deadline) {{ setTimeout(settle, 60); return; }}
          report();
          // 截图前滚到顶，让画面可确定。
          if ({js(mode)} === 'classic') document.getElementById('cards').scrollTop = 0;
        }};
        setTimeout(settle, 0);

        // resize 之后补报一次。
        window.addEventListener('resize', () => setTimeout(report, 0));
      }})();
    </script>
"""


def main() -> int:
    args = sys.argv[1:]
    out = pathlib.Path(args[0] if args else ROOT / "target" / "uipreview")
    mode = args[1] if len(args) > 1 else "classic"
    if mode not in ("picker", "classic", "tool"):
        raise SystemExit(f"未知模式 {mode!r}，只能是 picker / classic / tool")
    count = int(args[2]) if len(args) > 2 else len(DEMO)

    out.mkdir(parents=True, exist_ok=True)
    for name in ("styles.css", "main.js"):
        shutil.copy(UI / name, out / name)
    if (out / "assets").exists():
        shutil.rmtree(out / "assets")
    shutil.copytree(UI / "assets", out / "assets")

    html = (UI / "index.html").read_text(encoding="utf-8")
    html = html.replace(
        '    <script src="main.js"></script>',
        stub(demo_rows(count)) + '    <script src="main.js"></script>\n' + driver(mode),
    )
    (out / "index.html").write_text(html, encoding="utf-8")
    print(f"preview written to {out / 'index.html'} (mode={mode}, count={count})")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())

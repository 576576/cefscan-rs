"""生成一份带假数据的 cefscanw 前端预览页（只用于本地看效果，不入库）。

用法：
    python tools/preview_ui.py <输出目录> [picker|classic|tool] [条数]
然后拿浏览器打开 <输出目录>/index.html 截图即可。

`picker` 只画初始选择页（不做任何自动点击），另外两个会选好模式并点下"开始扫描"。

条数超过 DEMO 的长度时会把 DEMO 循环补足（名字加序号），用来把卡片墙撑到
溢出一屏，好验证"自动换行 + 按整行向下滚动"。

截图命令（这几个开关都不是可选的）：

    chrome --headless --disable-gpu --hide-scrollbars --force-prefers-reduced-motion \\
           --virtual-time-budget=9000 --window-size=1280,800 \\
           --screenshot=绝对路径.png --dump-dom file:///绝对路径/index.html

- `--force-prefers-reduced-motion`：否则卡片墙的"平滑滚动"在虚拟时间下走不完，
  截图会停在未滚动的位置。
- `--screenshot` 的路径必须是**绝对 Windows 路径**，相对路径会报"系统找不到指定的路径"。
- `--dump-dom` 和 `--screenshot` 写在同一次调用里，两边才是同一个状态。
  title 里带着几何指标，用 `grep -o '<title>[^<]*</title>'` 读出来。

**无头截图会把视口放大**：`--window-size=1280,800` 下页面看到的是 1264x705，
但截出来是 1280x800，而且页面收不到 resize 事件（`window.innerHeight` 自始至终
是 705，`ResizeObserver` 也不触发）——那是合成层的重排。后果是截图那一刻最大滚动量
变小、`scrollTop` 被夹回，卡片墙顶部会切掉小半行（切掉的正是 800-705 = 95 px）。
**这是截图工具的假象，不是前端 bug**：滚到顶（scrollTop=0，夹不动）再量，行顶边
精确落在 `padTop + k*行距` = 0 / 130 / 260 / 390 / 520 / 650（顶部区域已经挪到
`#cards` 外面，所以这里的 padTop 是 0）。所以经典模式截图前会先滚到顶，让画面可确定；
"跟最新一行"的几何正确性看 title 里的 `对齐残差`（0 表示视口顶部正好落在行顶边上）。
"""
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

# 截图脚本等多久再去读状态：必须长于揭示队列的总时长（见 main.js 的
# REVEAL_BUDGET_MS），否则截到的是"还在往外浮"的中间态。
SETTLE_MS = 6000


def js(value: object) -> str:
    """把 Python 字面量写成 JavaScript 字面量。

    注意 bool 必须先判：`isinstance(True, int)` 也是 True，直接走 repr 会写出
    `True` / `False` —— 那不是 JavaScript，整个桩脚本会当场 ReferenceError。
    """
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
    # 整体包在 IIFE 里：经典脚本的顶层 `class` / `const` 进的是**全局词法作用域**，
    # 而 main.js 顶层写的是 `const { invoke, Channel } = window.__TAURI__.core`。
    # 如果这里直接 `class Channel {}`，那个名字就已经被占住了，main.js 会以
    # "Identifier 'Channel' has already been declared" 整体解析失败——页面看起来
    # 只是"点了按钮没反应"，非常难查。IIFE 里的声明不会泄漏到全局词法作用域。
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
    """选好模式、点开始扫描，等揭示队列放完，把状态写进 title 方便无头浏览器读。

    title 里带上滚动指标（`--dump-dom` 能把它读出来）：卡片墙的"按整行滚动"
    是纯几何计算，光看截图判断不了对齐对不对，得把行距、溢出量、目标位置
    一起打出来才验得了。
    """
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
        // 工具模式下选择页那个按钮**只切视图、不开扫**（得先让人把目录填了），
        // 所以预览里还得自己点一下工具栏的"开始扫描"。
        if ({js(mode)} === 'tool') document.getElementById('scan-button').click();

        const report = () => {{
          const c = document.getElementById('cards');
          const card = c.querySelector('.card');
          const style = getComputedStyle(c);
          const pitch = card ? card.offsetHeight + (Number.parseFloat(style.rowGap) || 0) : 0;
          const padTop = Number.parseFloat(style.paddingTop) || 0;
          // 对齐判据：视口顶部落在行顶边上 ⟺ (scrollTop - padTop) 是行距的整数倍。
          const align = pitch > 0 ? (c.scrollTop - padTop) % pitch : 'n/a';
          // 经典模式的结果显示是顶部那行条数（#classic-count），工具模式才是工具栏
          // 那条状态行。两者文案格式不同，别读错。
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

        setTimeout(() => {{
          report();
          // 截图前滚到顶：scrollTop=0 不会被无头截图的视口重排夹取，画面才可确定。
          // （滚到底的话截出来会切掉小半行，那是截图工具的假象，见模块文档。）
          if ({js(mode)} === 'classic') document.getElementById('cards').scrollTop = 0;
        }}, {SETTLE_MS});

        // resize 之后补报一次，用来确认前端那条"盒子变了就重新对齐"的分支有没有生效。
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

// cefscanw 前端逻辑校验：用最小 DOM 桩把 ui/main.js 跑起来，喂进假事件，
// 检查后端实时显示、经典模式的揭示节奏、换肤开关，以及发给后端的请求形状。
//
// 用法（不需要 npm，直接 node 跑）：
//     node tools/ui_harness.js
//
// 为什么要有它：ui/main.js 是唯一一段没法用 `cargo test` 覆盖的代码，而它承载的
// 恰恰是几个"错了也看不出来"的行为——揭示队列的节拍、`row.painted` 标记防重放、
// 经典模式开关时的队列收尾。这些用肉眼点几下很难测全，用无头浏览器截图又只能验
// "长什么样"、验不了"跑了几次"。所以这里拿一个 60 行的 DOM 桩顶替浏览器，断言
// 的是**调用次数和时序**，比截图强。
//
// 它不能替代的东西：真实 DOM 的布局、CSS 的层叠与优先级。`[hidden]` 被
// `.summary { display: flex }` 压掉那个 bug 就是截图才发现的，桩完全看不见。
// 所以改前端时的完整流程是：先跑这个，再跑 tools/preview_ui.py 截图。
const fs = require('fs');
const path = require('path');

const UI = path.join(__dirname, '..', 'crates', 'cefscan-desktop', 'ui');

function classList(owner) {
  const set = new Set(owner.className ? owner.className.split(/\s+/) : []);
  return {
    add: (name) => set.add(name),
    remove: (name) => set.delete(name),
    contains: (name) => set.has(name),
    toggle(name, force) {
      const on = force === undefined ? !set.has(name) : Boolean(force);
      if (on) set.add(name);
      else set.delete(name);
      return on;
    },
    _dump: () => [...set],
  };
}

const els = new Map();
function el(id) {
  if (!els.has(id)) {
    els.set(id, {
      id,
      textContent: '',
      innerHTML: '',
      value: '',
      hidden: false,
      disabled: false,
      checked: false,
      dataset: {},
      handlers: {},
      classList: classList({}),
      addEventListener(type, handler) {
        this.handlers[type] = handler;
      },
      querySelector() {
        return null;
      },
      closest() {
        return null;
      },
    });
  }
  return els.get(id);
}

let channel = null;
class Channel {
  constructor() {
    channel = this;
  }
}

const rafQueue = [];
let scripted = [];
// main.js 在加载时就把 invoke 解构走了，所以桩必须是稳定的转发器，
// 换实现要换 invokeImpl 而不是换 window.__TAURI__.core.invoke。
let invokeImpl = async (_cmd, args) => {
  for (const event of scripted) args.channel.onmessage(event);
};

const documentElement = { classList: classList({ className: 'classic' }) };

/** 假的表头，用来触发一次整表重绘（排序）。 */
const sortHeader = {
  dataset: { sort: 'name' },
  handlers: {},
  addEventListener(type, handler) {
    this.handlers[type] = handler;
  },
};

global.window = {
  __TAURI__: {
    core: {
      Channel,
      invoke: (...args) => invokeImpl(...args),
    },
  },
};
global.document = {
  documentElement,
  getElementById: el,
  querySelectorAll: (selector) => (selector === 'th[data-sort]' ? [sortHeader] : []),
};
global.requestAnimationFrame = (fn) => rafQueue.push(fn);

// 勾选框的初值要跟 index.html 里一致，否则 main.js 读到的 classicMode 是错的。
const html = fs.readFileSync(path.join(UI, 'index.html'), 'utf8');
el('classic-input').checked = /id="classic-input"[^>]*\schecked/.test(html);

const source = fs.readFileSync(path.join(UI, 'main.js'), 'utf8');
// eslint-disable-next-line no-new-func
new Function('window', 'document', 'requestAnimationFrame', source)(
  global.window,
  global.document,
  global.requestAnimationFrame
);

const flush = async () => {
  for (let i = 0; i < 10; i += 1) await Promise.resolve();
  while (rafQueue.length) rafQueue.shift()();
};
const wait = (ms) => new Promise((resolve) => setTimeout(resolve, ms));

const checks = [];
function expect(label, actual, wanted) {
  const ok = actual === wanted;
  checks.push({ ok, label, actual, wanted });
}
function expectTrue(label, actual) {
  expect(label, Boolean(actual), true);
}

/** 结果表里画出来的行数。 */
const paintedRows = () => (el('results-body').innerHTML.match(/<tr data-path=/g) || []).length;
/** 带入场动画的行数。 */
const enteringRows = () => (el('results-body').innerHTML.match(/class="[^"]*\benter\b/g) || []).length;

function item(name, size) {
  return { type: 'item', name, kind: 'electron', size, running: false, path: `C:\\a\\${name}.exe` };
}

(async () => {
  const backend = el('backend-display');
  const button = el('scan-button');
  const classic = el('classic-input');

  // ---- 静态检查：换肤开关、背景图、下拉框确实没了 ----
  expect('index.html 无后端下拉框', /<select/.test(html), false);
  expect('index.html 初始文案', />自动（待扫描）</.test(html), true);
  expect('index.html 保留了后端显示位', /id="backend-display"/.test(html), true);
  expectTrue('index.html 有经典模式开关', /id="classic-input"[^>]*type="checkbox"/.test(html));
  expectTrue('经典模式默认勾选', el('classic-input').checked);
  expectTrue('html 上预置 classic 类（避免闪一下深色）', /<html[^>]*class="classic"/.test(html));

  const css = fs.readFileSync(path.join(UI, 'styles.css'), 'utf8');
  expect('styles.css 已无 backend-select', /backend-select/.test(css), false);
  expectTrue('styles.css 有 backend-display', /#backend-display/.test(css));
  expectTrue('styles.css 有经典模式换肤', /html\.classic\s*\{/.test(css));
  expectTrue('styles.css 有入场动画', /@keyframes row-enter/.test(css));
  expectTrue('动画尊重 prefers-reduced-motion', /prefers-reduced-motion/.test(css));

  const bgUrl = /url\("([^"]+)"\)/.exec(css);
  expectTrue('背景图在 frontendDist 里存在', bgUrl && fs.existsSync(path.join(UI, bgUrl[1])));
  expectTrue('背景图被 tauri 打包进 dist', /frontendDist/.test(fs.readFileSync(path.join(UI, '..', 'src-tauri', 'tauri.conf.json'), 'utf8')));

  // ---- 场景 1：经典模式（默认开）下结果逐条浮现 ----
  scripted = [
    { type: 'started', backend: 'Everything' },
    ...['A', 'B', 'C', 'D', 'E'].map((n, i) => item(n, 5000 - i * 100)),
    { type: 'done', backend: 'Everything', apps: 5, totalBytes: 1, sumBytes: 1, elapsedMs: 9, dirsScanned: 2 },
  ];
  button.handlers.click();
  await flush();

  expect('工具条后端', backend.textContent, '自动（Everything）');
  expect('第一条立刻出（不等一个节拍）', paintedRows(), 1);
  expect('只有新行带入场动画', enteringRows(), 1);
  expect('队列没清空前不显示汇总', el('summary').hidden, true);

  await wait(300);
  const mid = paintedRows();
  expectTrue(`300ms 时仍在缓缓浮现（已出 ${mid} 条）`, mid > 1 && mid < 5);

  await wait(500);
  expect('队列清空后全部出齐', paintedRows(), 5);
  expect('队列清空后才显示汇总', el('summary').hidden, false);
  expect('汇总区后端', el('sum-backend').textContent, '自动（Everything）');
  expect('状态文字', el('status').textContent, '完成，共 5 个');
  expect('按钮恢复', button.disabled, false);

  // 点表头排序会整表重绘——已经在屏幕上的行不该重放一次入场动画。
  sortHeader.handlers.click();
  expect('重绘后不再重放动画', enteringRows(), 0);
  expect('重绘后行数不变', paintedRows(), 5);

  // ---- 场景 2：关掉经典模式 → 立即出现、立即收尾 ----
  classic.checked = false;
  classic.handlers.change();
  expectTrue('html 上的 classic 类被移除', !documentElement.classList.contains('classic'));

  scripted = [
    { type: 'started', backend: 'cefscan' },
    item('F', 999),
    { type: 'done', backend: 'cefscan', apps: 1, totalBytes: 1, sumBytes: 1, elapsedMs: 4, dirsScanned: 1 },
  ];
  button.handlers.click();
  await flush();
  expect('非经典模式立即出结果', paintedRows(), 1);
  expect('非经典模式立即收尾', el('status').textContent, '完成，共 1 个');
  expect('非经典模式不做入场动画', enteringRows(), 0);
  expect('回落后端文案', backend.textContent, '自动（cefscan）');

  // ---- 场景 3：扫描中途打开经典模式，队列要一次性放出 ----
  classic.checked = true;
  classic.handlers.change();
  scripted = [
    { type: 'started', backend: 'cefscan' },
    ...['G', 'H', 'I'].map((n, i) => item(n, 100 - i)),
    { type: 'done', backend: 'cefscan', apps: 3, totalBytes: 1, sumBytes: 1, elapsedMs: 4, dirsScanned: 1 },
  ];
  button.handlers.click();
  await flush();
  expect('经典模式重新开启后先出 1 条', paintedRows(), 1);
  // 队列里还剩 2 条 + 汇总被压住，此时关掉经典模式应当全部放行
  classic.checked = false;
  classic.handlers.change();
  expect('关掉后剩余结果立刻放出', paintedRows(), 3);
  expect('关掉后汇总立刻补上', el('summary').hidden, false);
  expect('关掉后状态收尾', el('status').textContent, '完成，共 3 个');
  await wait(400);
  expect('已停掉的节拍不会重复计数', paintedRows(), 3);

  // ---- 场景 4：失败路径不能挂着"检测中" ----
  scripted = [{ type: 'error', message: 'boom' }];
  button.handlers.click();
  await flush();
  expect('失败后端文案', backend.textContent, '自动（未确定）');
  expect('失败状态', el('status').textContent, '失败：boom');

  // ---- 场景 5：请求体里 backend 恒为 auto ----
  let lastRequest = null;
  invokeImpl = async (_cmd, args) => {
    lastRequest = args.request;
  };
  el('root-input').value = 'D:\\Apps';
  button.handlers.click();
  await flush();
  expect('请求 backend', lastRequest.backend, 'auto');
  expect('请求 roots', JSON.stringify(lastRequest.roots), JSON.stringify(['D:\\Apps']));

  let failed = 0;
  for (const check of checks) {
    if (!check.ok) failed += 1;
    console.log(
      `${check.ok ? 'PASS' : 'FAIL'}  ${check.label}: ${JSON.stringify(check.actual)}` +
        (check.ok ? '' : ` (期望 ${JSON.stringify(check.wanted)})`)
    );
  }
  console.log(failed === 0 ? `\n全部通过（${checks.length} 项）` : `\n${failed} 项失败`);
  process.exit(failed === 0 ? 0 : 1);
})();

// cefscanw 前端逻辑校验：用最小 DOM 桩把 ui/main.js 跑起来，喂进假事件，
// 检查视图切换、后端探测时机、经典模式的揭示节奏与整行滚动，以及请求形状。
//
// 用法（不需要 npm，直接 node 跑）：
//     node tools/ui_harness.js
//
// 为什么要有它：ui/main.js 是唯一一段没法用 `cargo test` 覆盖的代码，而它承载的
// 恰恰是几个"错了也看不出来"的行为——揭示队列的节拍、`row.painted` 防重放、
// 后端探测该在什么时候发生、卡片墙对齐到整行的那套算术。这些用肉眼点几下很难测全，
// 用无头浏览器截图又只能验"长什么样"、验不了"跑了几次、算出来多少"。
// 所以这里拿一个 DOM 桩顶替浏览器，断言的是**调用次数、时序和算出来的数**。
//
// 它不能替代的东西：真实 DOM 的布局、CSS 的层叠与优先级。`[hidden]` 被
// `.summary { display: flex }` 压掉那个 bug 就是截图才发现的，桩完全看不见；
// 卡片墙的行距、卡片高度真实是多少也一样。所以改前端时的完整流程是：
// 先跑这个，再跑 tools/preview_ui.py 截图。
const fs = require('fs');
const path = require('path');

const UI = path.join(__dirname, '..', 'crates', 'cefscan-desktop', 'ui');

// ---------- DOM 桩 ----------

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

function baseEl(id) {
  return {
    id,
    textContent: '',
    innerHTML: '',
    value: '',
    hidden: false,
    disabled: false,
    checked: false,
    dataset: {},
    style: {},
    handlers: {},
    classList: classList({}),
    addEventListener(type, handler) {
      this.handlers[type] = handler;
    },
    querySelector() {
      return null;
    },
    querySelectorAll() {
      return [];
    },
    closest() {
      return null;
    },
  };
}

const els = new Map();
const el = (id) => {
  if (!els.has(id)) els.set(id, baseEl(id));
  return els.get(id);
};

/**
 * `#cards` 的几何桩。
 *
 * 卡片墙"对齐到整行"的那套算术要读 scrollHeight / clientHeight / offsetTop /
 * offsetHeight / rowGap / paddingTop。桩按 styles.css 里那套参数建模（卡片定高、
 * 行距固定、每行固定几列），给出**确定的输入**，测试才好断言它算出来的输出。
 */
const CARD_LAYOUT = {
  cardH: 116, // 与 styles.css 的 --card-h 一致
  gap: 14, // 与 --card-gap 一致
  // 顶部区域（图标胶囊 + 条数）已经挪到 #cards **外面**了，所以卡片墙自己的
  // 上内边距是 0。公式里仍然带着它，见 main.js 的 followNewest。
  padTop: 0,
  padBottom: 18,
  clientHeight: 705,
  columns: 6,
};

const cardsEl = (() => {
  const node = baseEl('cards');
  node.scrollTop = 0;
  node.scrollToCalls = [];
  const cardCount = () => (node.innerHTML.match(/<article class="card/g) || []).length;
  const padBottom = () => {
    const raw = Number.parseFloat(node.style.paddingBottom);
    return Number.isFinite(raw) ? raw : CARD_LAYOUT.padBottom;
  };
  Object.defineProperty(node, 'scrollHeight', {
    get() {
      const rows = Math.ceil(cardCount() / CARD_LAYOUT.columns);
      const rowsHeight = rows * CARD_LAYOUT.cardH + Math.max(0, rows - 1) * CARD_LAYOUT.gap;
      return CARD_LAYOUT.padTop + rowsHeight + padBottom();
    },
  });
  Object.defineProperty(node, 'clientHeight', { get: () => CARD_LAYOUT.clientHeight });
  node.querySelectorAll = (selector) => {
    if (selector !== '.card') return [];
    return Array.from({ length: cardCount() }, (_, index) => ({
      offsetHeight: CARD_LAYOUT.cardH,
      offsetTop:
        CARD_LAYOUT.padTop +
        Math.floor(index / CARD_LAYOUT.columns) * (CARD_LAYOUT.cardH + CARD_LAYOUT.gap),
    }));
  };
  node.scrollTo = (options) => {
    node.scrollToCalls.push(options);
    // 浏览器会把目标夹到 [0, 最大滚动量]。
    const max = Math.max(0, node.scrollHeight - node.clientHeight);
    node.scrollTop = Math.max(0, Math.min(options.top, max));
    // 真实浏览器里 scroll 事件是异步的，所以排进 rAF 队列，由 flush() 触发——
    // 同步触发会在 followNewest 内部重入。
    if (node.handlers.scroll) rafQueue.push(node.handlers.scroll);
  };
  return node;
})();
els.set('cards', cardsEl);

/** 假的表头，用来触发一次整表重绘（排序）。 */
const sortHeader = {
  dataset: { sort: 'name' },
  handlers: {},
  addEventListener(type, handler) {
    this.handlers[type] = handler;
  },
};

/** ResizeObserver 桩：把回调记下来，测试可以手动触发。 */
let resizeCallback = null;
class ResizeObserver {
  constructor(callback) {
    resizeCallback = callback;
  }
  observe() {}
}

let channel = null;
class Channel {
  constructor() {
    channel = this;
  }
}

const rafQueue = [];
let scripted = [];
let invokeLog = [];
let probeRequest = null;
let scanRequest = null;

// main.js 在加载时就把 invoke 解构走了，所以桩必须是稳定的转发器，
// 换实现要换 invokeImpl 而不是换 window.__TAURI__.core.invoke。
let invokeImpl = async (cmd, args) => {
  invokeLog.push(cmd);
  if (cmd === 'detect_backend') {
    probeRequest = args.request;
    return { backend: 'Everything' };
  }
  // 只有扫描会带 Channel。别的命令（比如 reveal）没有 channel，一律往下喂会当场抛异常。
  if (cmd === 'scan_apps') {
    scanRequest = args.request;
    for (const event of scripted) args.channel.onmessage(event);
  }
};

// index.html 的 <html> 上不带主题类（选择页是深色，深色就是 :root 的默认值），
// 所以这里也从空的开始。
const documentElement = { classList: classList({}) };

global.window = {
  __TAURI__: {
    core: {
      Channel,
      invoke: (...args) => invokeImpl(...args),
    },
  },
  // 默认不做"减少动态效果"：那样才会走平滑滚动那条分支，断言才有意义。
  matchMedia: () => ({ matches: false }),
};
global.document = {
  documentElement,
  getElementById: el,
  querySelector: (selector) =>
    selector === 'input[name="mode"]:checked'
      ? [el('mode-classic'), el('mode-tool')].find((node) => node.checked) || null
      : null,
  querySelectorAll: (selector) => {
    if (selector === 'th[data-sort]') return [sortHeader];
    if (selector === '.status-text') {
      // 经典模式那条"您的电脑里有 N 个 Chromium"不在这个列表里——它有自己的
      // 文案格式，挂在 #classic-count 上。
      return [el('picker-status'), el('status')];
    }
    return [];
  },
};
global.requestAnimationFrame = (fn) => rafQueue.push(fn);
global.ResizeObserver = ResizeObserver;
// 桩里的内边距是定值，只要跟 CARD_LAYOUT 对上即可——真实取值由截图那一层验。
global.getComputedStyle = () => ({
  paddingTop: `${CARD_LAYOUT.padTop}px`,
  paddingBottom: cardsEl.style.paddingBottom || `${CARD_LAYOUT.padBottom}px`,
  rowGap: `${CARD_LAYOUT.gap}px`,
});

// ---------- 加载被测代码 ----------

const html = fs.readFileSync(path.join(UI, 'index.html'), 'utf8');
const css = fs.readFileSync(path.join(UI, 'styles.css'), 'utf8');

// 选中项的初值要跟 index.html 一致，否则 main.js 读到的初始状态是错的。
el('mode-classic').checked = /id="mode-classic"[^>]*\schecked/.test(html);
el('mode-tool').checked = /id="mode-tool"[^>]*\schecked/.test(html);
// radio 的 value 就是视图名——main.js 靠它决定进哪个视图。桩不填这个值的话，
// 点"开始扫描"会永远落到经典模式，后面所有跟视图有关的断言都会跟着错。
const modeValue = (mode) => new RegExp(`id="mode-${mode}"[^>]*value="([^"]+)"`).exec(html);
el('mode-classic').value = (modeValue('classic') || [])[1] || '';
el('mode-tool').value = (modeValue('tool') || [])[1] || '';

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

/**
 * 等到条件成立（或超时）。
 *
 * 揭示队列的总时长由 REVEAL_BUDGET_MS 兜底，跟条数只有大致关系，定长 sleep 要么
 * 白等要么不够。所以轮询：既快又不会因为节拍微调而假失败。
 */
const waitUntil = async (predicate, timeoutMs) => {
  const deadline = Date.now() + timeoutMs;
  while (Date.now() < deadline) {
    // 每一轮都排空 rAF 队列：浏览器里 scroll / 重绘回调本来就是按帧跑的，
    // 不排空的话 lastScrollTop 之类的观察值会停在旧状态上。
    await flush();
    if (predicate()) return true;
    await wait(60);
  }
  await flush();
  return predicate();
};

const checks = [];
function expect(label, actual, wanted) {
  const ok = actual === wanted;
  checks.push({ ok, label, actual, wanted });
}
function expectTrue(label, actual) {
  expect(label, Boolean(actual), true);
}

/** 表格里画出来的行数 / 带入场动画的行数。 */
const paintedRows = () => (el('results-body').innerHTML.match(/<tr data-path=/g) || []).length;
const enteringRows = () =>
  (el('results-body').innerHTML.match(/class="[^"]*\benter\b/g) || []).length;
/** 卡片墙里画出来的卡片数 / 带入场动画的卡片数。 */
const paintedCards = () => (cardsEl.innerHTML.match(/<article class="card/g) || []).length;
const enteringCards = () => (cardsEl.innerHTML.match(/<article class="card enter/g) || []).length;
const lastScrollTop = () =>
  cardsEl.scrollToCalls.length ? cardsEl.scrollToCalls[cardsEl.scrollToCalls.length - 1].top : null;

const item = (name, size) => ({
  type: 'item',
  name,
  kind: 'electron',
  size,
  running: false,
  path: `C:\\a\\${name}.exe`,
});
const done = (apps, backend = 'cefscan') => ({
  type: 'done',
  backend,
  apps,
  totalBytes: 1,
  sumBytes: 1,
  elapsedMs: 9,
  dirsScanned: 2,
});

/** 行距：卡片高度 + 行间距。卡片墙的对齐全都以它为单位。 */
const PITCH = CARD_LAYOUT.cardH + CARD_LAYOUT.gap;

(async () => {
  const backend = el('backend-display');
  const startButton = el('start-button');
  const scanButton = el('scan-button');

  // ---- 静态检查：初始选择页的结构 ----
  expect('index.html 有初始选择页', /id="picker"/.test(html), true);
  expect('初始页恰好两个模式选项', (html.match(/<input[^>]*name="mode"/g) || []).length, 2);
  expectTrue('初始页有"开始扫描"按钮', /id="start-button"[^>]*>\s*开始扫描/.test(html));
  expectTrue('经典模式默认选中', el('mode-classic').checked);
  expect('工具模式默认不选中', el('mode-tool').checked, false);
  expect('两个 radio 的 value 就是视图名', `${el('mode-classic').value}/${el('mode-tool').value}`, 'classic/tool');
  // 选择页是深色的，所以 html 上不带主题类——深色是 :root 的默认值。
  expect('html 上不预置主题类（选择页是深色）', /<html[^>]*class=/.test(html), false);

  // 经典模式改成在初始页选了，工具条里那个勾选框必须消失——留着就是两处入口。
  expect('index.html 已无经典模式勾选框', /id="classic-input"/.test(html), false);
  expect('styles.css 已无 toggle 样式', /\.toggle\b/.test(css), false);

  // 后端 chip 现在是按钮：进工具模式时自动探一次，点它再探一次。
  expectTrue('后端 chip 是可点的按钮', /<button[^>]*id="backend-display"/.test(html));
  expect('index.html 无后端下拉框', /<select/.test(html), false);

  expectTrue('有卡片墙容器', /id="cards"/.test(html));
  expectTrue('经典模式有返回按钮', /id="classic-back"/.test(html));
  expectTrue('工具模式有返回按钮', /id="tool-back"/.test(html));

  // "不留工具条"：经典视图里不该有 .toolbar，只有一个悬浮 HUD。
  const classicSection = /<section id="classic-view"[\s\S]*?<\/section>/.exec(html);
  expectTrue('经典视图存在且可被解析', classicSection);
  expect('经典视图里没有工具条', /class="toolbar"/.test(classicSection[0]), false);
  expectTrue('经典视图有悬浮 HUD', /class="hud"/.test(classicSection[0]));
  expect('经典视图里没有状态行（条数不再混在 .status-text 里）', /class="[^"]*status-text/.test(classicSection[0]), false);

  // 两个胶囊**只放图标不放文字**：把 svg 和标签都剥掉之后不该剩下任何东西。
  const buttonInner = (id) => {
    const match = new RegExp(`<button[^>]*id="${id}"[^>]*>([\\s\\S]*?)</button>`).exec(html);
    return match ? match[1] : null;
  };
  const visibleText = (inner) =>
    inner === null
      ? '<缺失>'
      : inner.replace(/<svg[\s\S]*?<\/svg>/g, '').replace(/<[^>]+>/g, '').trim();
  expectTrue('经典模式有返回胶囊', /id="classic-back"/.test(html));
  expectTrue('经典模式有刷新胶囊（原来显示条数那个）', /id="classic-refresh"/.test(html));
  expect('返回胶囊里没有可见文字', visibleText(buttonInner('classic-back')), '');
  expect('刷新胶囊里没有可见文字', visibleText(buttonInner('classic-refresh')), '');

  // 结果条数是独立元素，初值就写明 0——没有单独的空态文案。
  expectTrue(
    '条数初值就是"您的电脑里有 0 个 Chromium"',
    /id="classic-count"[^>]*>您的电脑里有 0 个 Chromium</.test(html)
  );

  // ---- 静态检查：卡片墙的 CSS ----
  expectTrue('卡片墙用 auto-fill 自适应列数', /repeat\(auto-fill,\s*minmax\(/.test(css));
  expectTrue('卡片行高固定（"按整行滚动"的前提）', /grid-auto-rows:\s*var\(--card-h\)/.test(css));
  expectTrue('卡片有入场动画', /@keyframes card-enter/.test(css));
  expectTrue(
    '卡片动画尊重 prefers-reduced-motion',
    /prefers-reduced-motion[\s\S]*?\.card\.enter\s*\{\s*animation:\s*none/.test(css)
  );
  expectTrue('有压得住背景的实底色变量', /--panel-solid/.test(css));
  expectTrue('经典模式换肤仍在', /html\.classic\s*\{/.test(css));
  // "缓缓浮现"只属于经典模式的卡片墙；工具模式是即时的，那条表格动画已经够不着了。
  expect('表格已无入场动画', /row-enter/.test(css), false);

  // 这一批是"卡片墙不铺满页面 / 卡片透明 / 名称不被裁 / 滚动条藏起来"的静态约束。
  expectTrue('卡片是等宽等高的圆钮', /\.icon-pill\s*\{[^}]*width:\s*34px[^}]*height:\s*34px/.test(css));
  expectTrue('条数用百分比上边距（跟着背景缩放走）', /\.classic-count\s*\{[^}]*padding:\s*[\d.]+%/.test(css));
  expectTrue('卡片墙两侧留出空档', /\.cards\s*\{[^}]*padding:\s*0\s+[\d.]+%/.test(css));
  expectTrue('卡片墙隐藏滚动条（标准属性）', /\.cards\s*\{[^}]*scrollbar-width:\s*none/.test(css));
  expectTrue('卡片墙隐藏滚动条（WebKit）', /\.cards::-webkit-scrollbar\s*\{\s*display:\s*none/.test(css));
  expectTrue(
    '卡片背景是变量且为全透明',
    /--card-bg:\s*transparent/.test(css) && /\.card\s*\{[^}]*background:\s*var\(--card-bg\)/.test(css)
  );
  // 名称下部被切是因为卡片定高 116px 装不下内容，所以内边距和行距必须收紧。
  expectTrue('卡片内边距收到 10px', /\.card\s*\{[^}]*padding:\s*10px/.test(css));
  expectTrue('卡片行距收到 6px', /\.card\s*\{[^}]*gap:\s*6px/.test(css));
  expectTrue('应用名行高写死（不继承 1.5）', /\.card-name\s*\{[^}]*line-height:\s*[\d.]+/.test(css));
  expect('已无"还没有结果"空态', /cards-empty|还没有结果/.test(css + source), false);

  const bgUrl = /url\("([^"]+)"\)/.exec(css);
  expectTrue('背景图在 frontendDist 里存在', bgUrl && fs.existsSync(path.join(UI, bgUrl[1])));

  // ---- 场景 1：初始页什么都不画、不探后端，而且是深色 ----
  expect('初始页不画表格行', paintedRows(), 0);
  expect('初始页不画卡片', paintedCards(), 0);
  expect('初始页不探测后端', invokeLog.length, 0);
  expect('初始页是深色（不铺喜报）', documentElement.classList.contains('classic'), false);

  // ---- 场景 2：选经典模式开始扫描 → 卡片逐张浮现 ----
  invokeLog = [];
  scripted = [
    { type: 'started', backend: 'Everything' },
    ...['A', 'B', 'C', 'D', 'E'].map((n, i) => item(n, 5000 - i * 100)),
    done(5, 'Everything'),
  ];

  startButton.handlers.click();
  await flush();

  expect('选了经典模式后收起选择页', el('picker').hidden, true);
  expect('进入卡片墙视图', el('classic-view').hidden, false);
  expect('工具视图保持隐藏', el('tool-view').hidden, true);
  expectTrue('经典模式保留喜报皮肤', documentElement.classList.contains('classic'));
  expect('第一条立刻出（不等一个节拍）', paintedCards(), 1);
  expect('只有新卡片带入场动画', enteringCards(), 1);
  expect('队列没清空前不显示汇总', el('summary').hidden, true);
  expect('经典模式不探测后端（界面上没有 chip）', invokeLog.includes('detect_backend'), false);

  await wait(300);
  const mid = paintedCards();
  expectTrue(`300ms 时仍在缓缓浮现（已出 ${mid} 张）`, mid > 1 && mid < 5);

  await wait(500);
  expect('队列清空后全部出齐', paintedCards(), 5);
  expect('队列清空后才显示汇总', el('summary').hidden, false);
  expect('汇总区后端', el('sum-backend').textContent, '自动（Everything）');
  expect('工具模式的状态行照旧带合计', el('status').textContent, '完成，共 5 个 · 合计 1 B');
  // 经典模式的结果显示是顶部那行条数，格式跟工具模式的状态行不一样。
  expect('经典模式条数文案', el('classic-count').textContent, '您的电脑里有 5 个 Chromium');
  expect('按钮恢复', startButton.disabled, false);
  expect('刷新胶囊恢复可用', el('classic-refresh').disabled, false);

  // 点表头排序会整墙重绘——已经在屏幕上的卡片不该重放一次入场动画。
  // （经典模式下 render() 也是画卡片墙，所以这个表头点得出一次真正的重绘。）
  sortHeader.handlers.click();
  expect('重绘后不再重放动画', enteringCards(), 0);
  expect('重绘后卡片数不变', paintedCards(), 5);

  // ---- 场景 3：返回选择页不打断扫描，也不重扫 ----
  const scansBefore = invokeLog.filter((cmd) => cmd === 'scan_apps').length;
  el('classic-back').handlers.click();
  await flush();
  expect('返回后回到选择页', el('picker').hidden, false);
  expect('返回后两个视图都藏起来', el('classic-view').hidden && el('tool-view').hidden, true);
  expect('返回不重扫', invokeLog.filter((cmd) => cmd === 'scan_apps').length, scansBefore);

  // ---- 场景 4：选工具模式 → 进视图就探后端、结果即时上表 ----
  invokeLog = [];
  el('mode-tool').checked = true;
  el('mode-classic').checked = false;
  scripted = [
    { type: 'started', backend: 'Everything' },
    ...['F', 'G', 'H'].map((n, i) => item(n, 900 - i * 100)),
    done(3, 'Everything'),
  ];
  startButton.handlers.click();
  await flush();

  expect('进入工具视图', el('tool-view').hidden, false);
  expect('工具模式摘掉喜报皮肤', documentElement.classList.contains('classic'), false);
  expect('进工具模式就探测后端（不等点开始扫描）', invokeLog[0], 'detect_backend');
  expect('探测结果写进 chip', backend.textContent, '自动（Everything）');
  // 进入工具模式**不自动开扫**：它有自己的工具栏，先让人把目录填了再按"开始扫描"。
  expect('进入工具模式不自动开扫', invokeLog.filter((c) => c === 'scan_apps').length, 0);
  // 而且是直接复用上一轮那 5 条，不是清空重来。
  expect('进工具模式直接复用已有结果', paintedRows(), 5);

  invokeLog = [];
  scanButton.handlers.click();
  await flush();
  expect('工具模式立即出结果', paintedRows(), 3);
  expect('工具模式不做入场动画', enteringRows(), 0);
  expect('工具模式立即收尾', el('status').textContent, '完成，共 3 个 · 合计 1 B');
  expect('探测和扫描用同一份 request', JSON.stringify(probeRequest), JSON.stringify(scanRequest));

  // ---- 场景 4.5：已经有结果时切模式只复用，不重扫也不清空 ----
  const scansBeforeSwitch = invokeLog.filter((c) => c === 'scan_apps').length;
  el('tool-back').handlers.click(); // 回选择页
  await flush();
  el('mode-classic').checked = true;
  el('mode-tool').checked = false;
  startButton.handlers.click();
  await flush();
  expect(
    '有结果时切到经典模式不重扫',
    invokeLog.filter((c) => c === 'scan_apps').length,
    scansBeforeSwitch
  );
  expect('切过去看到的是原来那批结果', paintedCards(), 3);
  // 切回工具模式同样不重扫。
  el('classic-back').handlers.click();
  await flush();
  el('mode-tool').checked = true;
  el('mode-classic').checked = false;
  startButton.handlers.click();
  await flush();
  expect(
    '再切回工具模式也不重扫',
    invokeLog.filter((c) => c === 'scan_apps').length,
    scansBeforeSwitch
  );
  expect('切回工具模式看到同一批结果', paintedRows(), 3);

  // ---- 场景 5：点 chip 重新探测（结果确实由探测写入，不是扫描事件顺手带的）----
  invokeLog = [];
  backend.textContent = '自动（未确定）';
  backend.handlers.click();
  await flush();
  expect('点 chip 会重新探测', invokeLog[0], 'detect_backend');
  expect('探测结果写回 chip', backend.textContent, '自动（Everything）');

  // ---- 场景 6：扫描不把 chip 打回"待检测" ----
  invokeLog = [];
  scripted = [{ type: 'started', backend: 'cefscan' }, done(0)];
  scanButton.handlers.click();
  await flush();
  expect('扫描开始不重置 chip', backend.textContent, '自动（cefscan）');
  expect('工具模式下再扫不额外探测', invokeLog.filter((c) => c === 'detect_backend').length, 0);

  // ---- 场景 7：卡片墙按整行滚动 ----
  el('mode-classic').checked = true;
  el('mode-tool').checked = false;
  scripted = [
    { type: 'started', backend: 'cefscan' },
    ...Array.from({ length: 36 }, (_, i) => item(`App${i}`, 100000 - i * 100)),
    done(36),
  ];
  cardsEl.scrollToCalls = [];
  cardsEl.scrollTop = 0;
  cardsEl.style.paddingBottom = '';
  startButton.handlers.click();
  await flush();
  // 等揭示队列放完。总时长由 REVEAL_BUDGET_MS 兜底（36 条约 4 秒），跟条数只有
  // 大致关系，所以轮询而不是定长 sleep。
  const drained = await waitUntil(
    () => paintedCards() === 36 && !startButton.disabled,
    9000
  );
  expectTrue('揭示队列放完且收尾（36 张）', drained);

  // 36 张 / 每行 6 列 = 6 行。顶部区域（胶囊 + 条数）在 #cards 外面，所以 padTop = 0。
  //   自然溢出 = 0 + (6*116 + 5*14) + 18 - 705 = 79
  //   最后一行底边 = 0 + 5*130 + 116 = 766，要让它完整可见：minTop = 766 - 705 = 61
  //   对齐到行顶边：0 + ceil((61-0)/130)*130 = 130
  //   底部内边距补到够滚：18 + (130 - 79) = 69
  expect('卡片全画出来', paintedCards(), 36);
  expect('自动跟随的目标对齐到整行', lastScrollTop(), 130);
  expect('底部内边距补成整行', cardsEl.style.paddingBottom, '69px');
  expect('确实滚到了目标位置（没有被夹）', cardsEl.scrollTop, 130);
  expect('视口顶部正好落在行顶边上', (cardsEl.scrollTop - CARD_LAYOUT.padTop) % PITCH, 0);

  const targets = [...new Set(cardsEl.scrollToCalls.map((call) => call.top))].sort((a, b) => a - b);
  expectTrue(
    `每一跳都落在行顶边上（${targets.join(' / ')}）`,
    targets.every((top) => (top - CARD_LAYOUT.padTop) % PITCH === 0)
  );

  // 用户往上滚 → 停跟随（此时墙在最底部，往上滚就是 scrollTop 变小）
  const callsBeforeUp = cardsEl.scrollToCalls.length;
  cardsEl.scrollTop = 0;
  cardsEl.handlers.scroll();
  resizeCallback(); // 盒子没变时重算也不该动滚动条
  await flush();
  expect('往上滚之后不再自动跟随', cardsEl.scrollToCalls.length, callsBeforeUp);

  // 滚回底部 → 恢复跟随
  cardsEl.scrollTop = cardsEl.scrollHeight - cardsEl.clientHeight;
  cardsEl.handlers.scroll();
  resizeCallback();
  await flush();
  expect('滚回底部后恢复跟随', cardsEl.scrollToCalls.length > callsBeforeUp, true);

  // 点卡片 = 在资源管理器中定位
  invokeLog = [];
  cardsEl.handlers.click({ target: { closest: () => ({ dataset: { path: 'C:\\a\\App0.exe' } }) } });
  await flush();
  expect('点卡片会请求定位', invokeLog[0], 'reveal');

  // ---- 场景 7.5：经典模式 0 个结果也照实说，不画空态 ----
  // 注意这里用**刷新胶囊**而不是选择页那个按钮：已经有 36 条结果了，选择页的按钮
  // 现在只会切视图、不会重扫（这正是"切模式复用结果"那条要求）。
  invokeLog = [];
  scripted = [{ type: 'started', backend: 'cefscan' }, done(0)];
  el('classic-refresh').handlers.click();
  await flush();
  expect('0 个结果照实说', el('classic-count').textContent, '您的电脑里有 0 个 Chromium');
  expect('空态不画任何卡片', paintedCards(), 0);

  // ---- 场景 7.6：刷新胶囊 = 重扫一次 ----
  invokeLog = [];
  scripted = [{ type: 'started', backend: 'cefscan' }, item('Z', 10), done(1)];
  el('classic-refresh').handlers.click();
  await flush();
  expect('刷新胶囊会重扫', invokeLog.filter((cmd) => cmd === 'scan_apps').length, 1);
  expect('刷新后条数跟着更新', el('classic-count').textContent, '您的电脑里有 1 个 Chromium');
  // 经典模式的 `done` 要等揭示队列排空才收尾（deferredDone），所以这里必须等它真的
  // 结束——不然 `scanning` 还挂着，后面点"开始扫描"会被 runScan 直接挡掉。
  await waitUntil(() => !el('classic-refresh').disabled, 3000);

  // ---- 场景 8：失败路径不能挂着"待检测" ----
  el('mode-tool').checked = true;
  el('mode-classic').checked = false;
  scripted = [{ type: 'error', message: 'boom' }];
  scanButton.handlers.click();
  await flush();
  expect('失败后端文案', backend.textContent, '自动（未确定）');
  expect('失败状态', el('status').textContent, '失败：boom');

  // ---- 场景 9：请求体 ----
  el('root-input').value = 'D:\\Apps';
  scripted = [{ type: 'started', backend: 'cefscan' }, done(0)];
  scanButton.handlers.click();
  await flush();
  expect('请求 backend', scanRequest.backend, 'auto');
  expect('请求 roots', JSON.stringify(scanRequest.roots), JSON.stringify(['D:\\Apps']));

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

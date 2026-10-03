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
  cardH: 95, // 与 styles.css 的 --card-h 一致
  gap: 11, // 与 --card-gap 一致
  // 顶部区域（图标胶囊 + 条数）已经挪到 #cards **外面**了，所以卡片墙自己的
  // 上内边距是 0。公式里仍然带着它，见 main.js 的 followNewest。
  padTop: 0,
  padBottom: 18,
  clientHeight: 705,
  columns: 6,
};

const cardsEl = (() => {
  const node = baseEl('cards');
  let scrollTop = 0;
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
  // 写 scrollTop 要照浏览器的语义来：夹到 [0, 最大滚动量]，并**异步**派发 scroll 事件
  // （同步派发会在 followNewest / 滚动动画内部重入）。
  // 前端自己按帧推滚动（scrollCardsTo），所以这个 setter 是那套动画的唯一出口——
  // 桩里要是把 scrollTop 当普通字段，动画跑得再欢也观察不到。
  Object.defineProperty(node, 'scrollTop', {
    get: () => scrollTop,
    set: (value) => {
      const max = Math.max(0, node.scrollHeight - node.clientHeight);
      const next = Math.max(0, Math.min(value, max));
      if (next === scrollTop) return;
      scrollTop = next;
      if (node.handlers.scroll) rafQueue.push(node.handlers.scroll);
    },
  });
  node.querySelectorAll = (selector) => {
    if (selector !== '.card') return [];
    return Array.from({ length: cardCount() }, (_, index) => ({
      offsetHeight: CARD_LAYOUT.cardH,
      offsetTop:
        CARD_LAYOUT.padTop +
        Math.floor(index / CARD_LAYOUT.columns) * (CARD_LAYOUT.cardH + CARD_LAYOUT.gap),
    }));
  };
  // 前端已经不用 scrollTo 了（自己按帧推），留着只为兼容可能的旧调用。
  node.scrollTo = (options) => {
    node.scrollToCalls.push(options);
    node.scrollTop = options.top;
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

/**
 * 去掉 JS 源码里的注释，专供**负面断言**（"代码里不该再出现 X"）使用。
 *
 * 正面断言（"应该有 Y"）在原始源码上匹配没问题；负面断言不行——注释里越是认真
 * 解释"为什么不用 X"，原始源码里就越是留着 X 的字样，断言反而被自己的注释绊倒。
 * 所以负面断言一律对剥离注释后的文本做，注释怎么写都不影响。
 */
function stripComments(text) {
  let out = '';
  let i = 0;
  const n = text.length;
  while (i < n) {
    const ch = text[i];
    const next = text[i + 1];
    if (ch === '/' && next === '*') {
      i += 2;
      while (i < n && !(text[i] === '*' && text[i + 1] === '/')) i += 1;
      i += 2;
      continue;
    }
    if (ch === '/' && next === '/') {
      i += 2;
      while (i < n && text[i] !== '\n') i += 1;
      continue;
    }
    if (ch === '"' || ch === "'" || ch === '`') {
      const quote = ch;
      out += ch;
      i += 1;
      while (i < n) {
        if (text[i] === '\\') {
          out += text[i] + (text[i + 1] || '');
          i += 2;
          continue;
        }
        out += text[i];
        if (text[i] === quote) {
          i += 1;
          break;
        }
        i += 1;
      }
      continue;
    }
    out += ch;
    i += 1;
  }
  return out;
}

// 只给负面断言用；正面断言仍用带注释的 source。
const code = stripComments(source);

// eslint-disable-next-line no-new-func
new Function('window', 'document', 'requestAnimationFrame', source)(
  global.window,
  global.document,
  global.requestAnimationFrame
);

/**
 * 排空**本次**入队的 rAF 回调。
 *
 * 不能写成 `while (rafQueue.length) shift()()`：前端自己驱动的滚动动画每跑一帧就会
 * 再入队一个，那样这里会死循环。所以先取一份快照，续帧的回调留到下一次 flush。
 * 回调带一个递增的时间戳——滚动动画按帧间隔算位移，没有时间戳就只能按兜底的 16ms 走。
 */
let rafClock = 0;
const flush = async () => {
  for (let i = 0; i < 10; i += 1) await Promise.resolve();
  for (const fn of rafQueue.splice(0, rafQueue.length)) {
    rafClock += 16;
    fn(rafClock);
  }
};
const wait = (ms) => new Promise((resolve) => setTimeout(resolve, ms));

/** 把自己驱动的滚动动画推到静止（每帧 16ms，上限给得足够宽）。 */
const settleScroll = async (maxFrames = 4000) => {
  for (let i = 0; i < maxFrames; i += 1) {
    if (!rafQueue.length) return;
    await flush();
  }
};

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
  // 按钮写"进入"而不是"开始扫描"：它只负责把人送进选中的视图，进去之后扫不扫得看
  // 情况（已有结果就复用、工具模式只切视图不扫）。文案写"开始扫描"就是在骗人。
  expectTrue('初始页按钮写"进入"', /id="start-button"[^>]*>\s*进入\s*</.test(html));
  expectTrue('"进入"按钮不会在 setScanning 里被改成"开始扫描"', !/startButton\.textContent/.test(source));
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
  // 名称下部被切是因为卡片定高装不下内容，所以内边距和行距必须收紧。
  expectTrue('卡片内边距收到 8px', /\.card\s*\{[^}]*padding:\s*8px/.test(css));
  expectTrue('卡片行距收到 4px', /\.card\s*\{[^}]*gap:\s*4px/.test(css));
  expectTrue('应用名行高写死（不继承 1.5）', /\.card-name\s*\{[^}]*line-height:\s*[\d.]+/.test(css));
  expectTrue('应用名字号也写死（不继承 body 的 14px）', /\.card-name\s*\{[^}]*font-size:\s*[\d.]+px/.test(css));
  expect('已无"还没有结果"空态', /cards-empty|还没有结果/.test(css + source), false);

  // 密度：卡片墙一屏能放几张，跟 (card-min+gap) × (card-h+gap) 成反比。这三个值
  // 是一组，单独改一个会把卡片比例搞歪（或让内容装不下、又切掉名称的下伸笔画）。
  // 这里只断言"三者同步缩小"——真正的密度由截图那一层看。
  const cssVar = (name) => Number.parseFloat(new RegExp(`--${name}:\\s*([\\d.]+)px`).exec(css)[1]);
  const cardMin = cssVar('card-min');
  const cardH = cssVar('card-h');
  const cardGap = cssVar('card-gap');
  expectTrue(`卡片几何缩小过（${cardMin}/${cardH}/${cardGap}）`, cardMin < 168 && cardH < 116 && cardGap < 14);
  expectTrue(
    '卡片几何仍是一组（宽高比与间距比都没走样）',
    Math.abs(cardMin / cardH - 168 / 116) < 0.02 && Math.abs(cardGap / cardH - 14 / 116) < 0.02
  );
  // 图标槽必须跟 .card-icon 的尺寸一致，否则卡片高度会参差不齐。
  const iconPx = Number.parseFloat(/\.card-icon\s*\{[^}]*width:\s*([\d.]+)px/.exec(css)[1]);
  expectTrue('图标槽跟 cardIconHtml 的尺寸一致', new RegExp(`width="${iconPx}" height="${iconPx}"`).test(source));
  expectTrue('图标槽缩到了 32px', iconPx === 32);

  // 揭示与滚动的**速度上限**：用户明确要"最大出现速度"，不然结果一多就是"唰"地
  // 一下全出来，没有仪式感。上限在代码里，这里钉住它别被后人顺手删掉。
  expectTrue('揭示速度有硬上限', /REVEAL_MAX_PER_TICK\s*=\s*\d+/.test(source));
  expectTrue('revealTick 真的用了那个上限', /Math\.min\(byBudget,\s*REVEAL_MAX_PER_TICK\)/.test(source));
  // 滚动不能用浏览器自带的平滑滚动：它的速度没法调，而且是异步的（scrollTop 不会
  // 立刻变），按位置限速根本配合不了——实测会把墙卡在顶部一动不动。
  expectTrue('滚动速度上限是一个明确的常量', /SCROLL_MAX_PX_PER_SEC\s*=\s*\d+/.test(source));
  expectTrue('滚动是自己按帧推的（不再用 scrollTo 的平滑滚动）', /function scrollFrame/.test(source));
  expect('不再依赖浏览器平滑滚动', /behavior:\s*['"]smooth['"]/.test(code), false);
  expectTrue('followNewest 走自己的滚动函数', /scrollCardsTo\(target\)/.test(source));
  // lastScrollTop 只能由 scroll 监听写：滚动动画写的是 scrollTop 本身，它要是也去写
  // lastScrollTop，事件里的旧值和动画写的新值就会交错，每一帧向下滚都被误判成
  // "用户往上滚"，跟随当场永久停摆。
  expectTrue(
    'lastScrollTop 只由 scroll 监听写（滚动动画不碰它）',
    !/lastScrollTop\s*=/.test(source.slice(source.indexOf('function scrollFrame'), source.indexOf('function scrollCardsTo')))
  );

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
  // 选择页那个按钮只是"进入"，扫描中也不该禁用——不然扫到一半从视图里点"返回"，
  // 就再也进不去了。工具模式那个按钮才该在扫描时禁用（防止重复开扫）。
  expect('扫描中"进入"按钮仍可用', startButton.disabled, false);
  expect('扫描中工具模式的"开始扫描"被禁用', scanButton.disabled, true);

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
  expect('扫描结束后"进入"按钮仍可用', startButton.disabled, false);
  expect('扫描结束后"开始扫描"恢复', scanButton.disabled, false);
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
  // 卡片缩小后一屏装得下更多行，所以要 42 张（7 行）才会溢出——6 行（36 张）
  // 只有 625px 高，装进 705px 的视口里根本不用滚。
  el('mode-classic').checked = true;
  el('mode-tool').checked = false;
  scripted = [
    { type: 'started', backend: 'cefscan' },
    ...Array.from({ length: 42 }, (_, i) => item(`App${i}`, 100000 - i * 100)),
    done(42),
  ];
  cardsEl.scrollToCalls = [];
  cardsEl.scrollTop = 0;
  cardsEl.style.paddingBottom = '';
  startButton.handlers.click();
  await flush();
  // 等揭示队列放完。42 条按"一拍最多 3 张"的上限走，实际是一拍 1 张（预算算出来
  // 就是 1），约 6.3 秒，所以轮询而不是定长 sleep。
  // 判据用 `scanButton`：选择页那个"进入"按钮扫描中也不禁用，拿它当"扫完了"的标志
  // 会早退——揭示队列排空和 applyDone 之间还差一拍（150ms），那时 scanning 还挂着。
  const drained = await waitUntil(
    () => paintedCards() === 42 && !scanButton.disabled,
    12000
  );
  expectTrue('揭示队列放完且收尾（42 张）', drained);

  // 42 张 / 每行 6 列 = 7 行。顶部区域（胶囊 + 条数）在 #cards 外面，所以 padTop = 0。
  //   行距 = 95 + 11 = 106
  //   自然溢出 = 0 + (7*95 + 6*11) + 18 - 705 = 731 + 18 - 705 = 44
  //   最后一行底边 = 0 + 6*106 + 95 = 731，要让它完整可见：minTop = 731 - 705 = 26
  //   对齐到行顶边：0 + ceil((26-0)/106)*106 = 106
  //   底部内边距补到够滚：18 + (106 - 44) = 80
  expect('卡片全画出来', paintedCards(), 42);
  expect('底部内边距补成整行', cardsEl.style.paddingBottom, '80px');

  // 滚动是**自己按帧推**的（`scrollCardsTo`），所以要把它推到静止再看位置。
  await settleScroll();
  expect('自动跟随停在行顶边上', cardsEl.scrollTop, 106);
  expect('视口顶部正好落在行顶边上', (cardsEl.scrollTop - CARD_LAYOUT.padTop) % PITCH, 0);

  // 用户往上滚 → 停跟随（此时墙在最底部，往上滚就是 scrollTop 变小）
  cardsEl.scrollTop = 0;
  await flush();
  resizeCallback(); // 盒子没变时重算也不该动滚动条
  await settleScroll();
  expect('往上滚之后不再自动跟随', cardsEl.scrollTop, 0);

  // 滚回底部 → 恢复跟随。判据：再补一行的量，墙应该继续往下跟。
  cardsEl.scrollTop = cardsEl.scrollHeight - cardsEl.clientHeight;
  await flush(); // 让 scroll 监听跑掉，autoFollow 才会恢复
  cardsEl.innerHTML += '<article class="card" data-path="extra"></article>';
  resizeCallback();
  await settleScroll();
  expectTrue('滚回底部后恢复跟随（新补的那行被跟上了）', cardsEl.scrollTop > 0);

  // ---- 场景 7.1：滚动的速度上限（用户明确要的"最大滚动速度"）----
  // 要造一个"目标一下跳到十几行外"的落差来采样，否则采到的只是动画尾巴上那几像素。
  const scrollCap = Number(/SCROLL_MAX_PX_PER_SEC\s*=\s*(\d+)/.exec(source)[1]);
  // 帧长被夹在 64ms 以内（见 scrollFrame），所以单帧位移的上界就是这个数。
  const maxStepPerFrame = (scrollCap * 64) / 1000;
  const jumpFrom = cardsEl.scrollTop;
  cardsEl.innerHTML += Array.from(
    { length: 60 },
    (_, i) => `<article class="card" data-path="pad${i}"></article>`
  ).join('');
  resizeCallback();
  const steps = [];
  let prevTop = cardsEl.scrollTop;
  for (let i = 0; i < 4000 && rafQueue.length; i += 1) {
    await flush();
    steps.push(Math.abs(cardsEl.scrollTop - prevTop));
    prevTop = cardsEl.scrollTop;
  }
  const biggestStep = Math.max(...steps);
  expectTrue(
    `目标跳了 ${Math.round(prevTop - jumpFrom)}px，是一帧帧挪过去的（${steps.length} 帧）`,
    prevTop - jumpFrom > 500 && steps.length > 50
  );
  expectTrue(
    `单帧最大位移 ${biggestStep.toFixed(1)}px 没超上限 ${maxStepPerFrame}px`,
    biggestStep <= maxStepPerFrame + 0.01
  );

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

'use strict';
// pk-plugin.mcsv.me. No libraries, no network calls: everything the page shows is in this file.

const VERSION = '0.7.0';
const BASE = ['fs.read.data', 'fs.write.data'];
const NET = ['http.outbound', 'network.dns'];

// id, name, what it stands in for (and other names people type), category, text, commands, extra sandbox permissions
const PLUGINS = [
  { id: 'basics', name: 'Basics', rep: 'CMI / EssentialsX', aka: 'essentials essx cmi homes warp tpa economy kit', cat: 'core',
    th: 'คำสั่งพื้นฐานครบชุด: บ้าน warp tpa สุ่มเทเลพอร์ต ยศ แชต kit เงิน และเครื่องมือดูแลผู้เล่น',
    en: 'The everyday commands: homes, warps, tpa, random teleport, ranks, chat, kits, economy and moderation.',
    cmds: ['/home', '/warp', '/spawn', '/tpa', '/rtp', '/kit', '/rank', '/balance', '/pay', '/basics'] },
  { id: 'perms', name: 'Perms', rep: 'LuckPerms', aka: 'lp permissions permissionsex groupmanager rank', cat: 'core', extra: NET,
    th: 'ระบบสิทธิ์เต็มรูปแบบ ใช้ /lp แบบเดียวกับ LuckPerms: กลุ่ม สืบทอด ลำดับยศ prefix และสิทธิ์ชั่วคราว',
    en: 'Full permissions with the /lp syntax: groups, inheritance, tracks, prefixes and temporary nodes.',
    cmds: ['/lp user <name> permission set <node>', '/lp group <group> parent add <group>', '/lp creategroup <name>', '/lp listgroups', '/lp editor'] },
  { id: 'login', name: 'Login', rep: 'AuthMe', aka: 'authme register password nlogin offline cracked', cat: 'core',
    th: 'สมัครและล็อกอินด้วยรหัสผ่าน สำหรับเซิร์ฟ offline mode กันคนอื่นสวมชื่อ',
    en: 'Register and log in with a password on offline-mode servers, so nobody can take your name.',
    cmds: ['/register', '/login', '/changepassword', '/logout', '/auth'] },
  { id: 'placeholders', name: 'Placeholders', rep: 'PlaceholderAPI', aka: 'papi placeholder variables', cat: 'core',
    th: 'ตัวแปรแบบ %player_name% ที่ปลั๊กอินตัวอื่นหยิบไปใช้ได้ เช่น ยอดเงินและ prefix',
    en: '%player_name% style placeholders the other plugins can use, such as balances and prefixes.',
    cmds: ['/papi parse me %player_name%', '/papi list', '/papi reload'] },
  { id: 'claims', name: 'Claims', rep: 'GriefPrevention', aka: 'grief protection land claim towny residence worldguard', cat: 'guard',
    th: 'จองที่ดินด้วยพลั่วทอง คนอื่นขุด วาง หรือเปิดหีบในที่ของคุณไม่ได้',
    en: 'Claim land with a golden shovel; nobody else can break, build or open chests there.',
    cmds: ['/claim', '/trust', '/untrust', '/trustlist', '/claimslist', '/abandonclaim'] },
  { id: 'blocklog', name: 'BlockLog', rep: 'CoreProtect', aka: 'co coreprotect rollback logblock prism grief log', cat: 'guard',
    th: 'บันทึกว่าใครทำอะไร ที่ไหน เมื่อไร ตรวจด้วยการคลิก แล้วย้อนกลับได้',
    en: 'Records who did what, where and when. Inspect by clicking, then roll it back.',
    cmds: ['/co inspect', '/co lookup', '/co rollback u:name t:1h r:20', '/co restore', '/co undo'] },
  { id: 'worldtools', name: 'WorldTools', rep: 'WorldEdit', aka: 'we fawe fastasyncworldedit schematic build', cat: 'world',
    th: 'แก้โลกทีละมาก ๆ: เลือกพื้นที่ เติม แทนที่ คัดลอก วาง และย้อนกลับ',
    en: 'Edit the world in bulk: select, set, replace, copy, paste and undo.',
    cmds: ['//wand', '//set', '//replace', '//copy', '//paste', '//undo', '//schem'] },
  { id: 'worldmap', name: 'WorldMap', rep: 'BlueMap / squaremap', aka: 'dynmap bluemap squaremap pl3xmap map web', cat: 'world', extra: ['network.tcp.bind'],
    th: 'แผนที่โลก 2 มิติที่วาดเองตามที่ผู้เล่นเดินผ่าน เปิดดูสด ๆ ผ่านเว็บได้',
    en: 'A top-down map drawn as players explore, with a live web view.',
    cmds: ['/worldmap render', '/worldmap web <port>', '/worldmap export', '/worldmap status'] },
  { id: 'pregen', name: 'Pregen', rep: 'Chunky', aka: 'chunky pregenerate worldborder chunk generator', cat: 'world', native: true,
    th: 'สร้างโลกล่วงหน้าให้เดินไปพื้นที่ใหม่แล้วไม่แล็ก หยุดแล้วทำต่อได้แม้ปิดเซิร์ฟ',
    en: 'Pre-generates the world so new areas do not lag. Pause and resume, even across restarts.',
    cmds: ['/pregen start', '/pregen pause', '/pregen continue', '/pregen radius', '/chunky'] },
  { id: 'imagemap', name: 'ImageMap', rep: 'ImageOnMap', aka: 'imageonmap image map poster picture', cat: 'world', extra: NET,
    th: 'เปลี่ยนรูปจากลิงก์เป็นแผนที่ติดผนัง ทำโปสเตอร์หลายแผ่นได้',
    en: 'Turns an image link into wall maps and posters made of several maps.',
    cmds: ['/tomap <link> 2x2', '/imagemap list', '/imagemap get <name>'] },
  { id: 'tablist', name: 'TabList', rep: 'TAB', aka: 'tab nametag scoreboard sidebar header footer', cat: 'look',
    th: 'หน้ารายชื่อผู้เล่น: หัวและท้ายแบบเคลื่อนไหว prefix ตามกลุ่ม และกระดานข้างจอ',
    en: 'The player list: animated header and footer, group prefixes and a sidebar.',
    cmds: ['/tablist reload'] },
  { id: 'motd', name: 'Motd', rep: 'MiniMOTD / Maintenance', aka: 'minimotd maintenance serverlistplus motd', cat: 'look',
    th: 'ข้อความหน้ารายการเซิร์ฟสลับได้หลายแบบ และโหมดปิดปรับปรุง',
    en: 'Rotating server list messages and a maintenance mode.',
    cmds: ['/maintenance on 30m', '/maintenance off', '/maintenance add <player>', '/maintenance status'] },
  { id: 'skins', name: 'Skins', rep: 'SkinsRestorer', aka: 'skinsrestorer skin offline', cat: 'look', extra: NET,
    th: 'ให้ผู้เล่นมี skin บนเซิร์ฟ offline mode จากชื่อบัญชีหรือลิงก์รูป',
    en: 'Skins on offline-mode servers, from an account name or an image link.',
    cmds: ['/skin <account>', '/skin url <link>', '/skin clear', '/skin update'] },
  { id: 'resourcepack', name: 'ResourcePack', rep: 'Resource Pack Downloader / ForcePack', aka: 'forcepack resource pack texture rp', cat: 'look', extra: NET,
    th: 'ส่ง resource pack ให้ผู้เล่นตอนเข้าเกม มีหลาย pack แยกตามโลก และบังคับได้',
    en: 'Sends resource packs on join: several packs, per world, with optional enforcement.',
    cmds: ['/rp', '/rp list', '/rp send <player>', '/rp hash'] },
  { id: 'monitor', name: 'Monitor', rep: 'spark', aka: 'spark tps lag profiler timings', cat: 'ops',
    th: 'ดูสุขภาพเซิร์ฟ: TPS เวลาต่อ tick ping และจำนวน entity',
    en: 'Server health: TPS, time per tick, ping and entity counts.',
    cmds: ['/tps', '/spark health', '/spark entities', '/spark ping'] },
  { id: 'cleaner', name: 'Cleaner', rep: 'ClearLag', aka: 'clearlag clearlagg lagg items mob limit', cat: 'ops',
    th: 'ล้างของตกพื้นตามรอบเวลา เตือนก่อนล้าง และจำกัด mob ต่อ chunk',
    en: 'Clears dropped items on a schedule with a warning, and limits mobs per chunk.',
    cmds: ['/cleaner now', '/cleaner check', '/cleaner next', '/lagg'] },
  { id: 'discordlink', name: 'DiscordLink', rep: 'DiscordSRV', aka: 'discordsrv discord chat webhook bridge', cat: 'ops', extra: NET,
    th: 'เชื่อมแชตในเกมกับห้อง Discord สองทาง ผ่าน webhook หรือบอท',
    en: 'Game chat to a Discord channel and back, through a webhook or a bot.',
    cmds: ['/discord', '/discordlink status', '/discordlink test', '/discordlink reload'] },
  { id: 'scripts', name: 'Scripts', rep: 'Skript', aka: 'skript sk script denizen custom command', cat: 'ops',
    th: 'เขียนสคริปต์ .sk แบบ Skript: คำสั่งเอง event ตัวแปร และเมนูหีบ',
    en: 'Write .sk scripts in Skript style: custom commands, events, variables and chest menus.',
    cmds: ['/sk reload', '/sk list', '/sk enable', '/sk disable'] },
];

const CATS = [
  { id: 'all', th: 'ทั้งหมด', en: 'All' },
  { id: 'core', th: 'พื้นฐาน', en: 'Core' },
  { id: 'guard', th: 'ป้องกัน', en: 'Protection' },
  { id: 'world', th: 'โลก', en: 'World' },
  { id: 'look', th: 'หน้าตา', en: 'Looks' },
  { id: 'ops', th: 'ดูแลเซิร์ฟ', en: 'Operations' },
];

const TEXT = {
  en: {
    nav_plugins: 'Plugins', nav_install: 'Install',
    eyebrow: 'For Pumpkin 0.2.2 · Minecraft 26.3 · release 0.7.0',
    h1a: 'plugins', h1b: 'a <em>Pumpkin</em> server cannot do without',
    lede: 'Paper .jar plugins do not run on Pumpkin. These are written from scratch in Rust to do the job of the ones you know, with the same commands.',
    finder_label: 'What did you use on Paper?', finder_idle: 'Type a name to see what stands in for it',
    shelf_title: 'One at a time', none: 'Nothing matches that',
    install_title: 'Two ways to install',
    way1_tag: 'Servers hosted at MCSV', way1_title: 'One button',
    way1_a: 'Open your Pumpkin server and go to Plugins', way1_b: 'Press Install on the one you want, or ask the AI on the server page to do it',
    way1_c: 'Restart once. New releases arrive by themselves whenever the server starts', way1_btn: 'Rent a Pumpkin server at MCSV',
    way2_tag: 'Your own Pumpkin server', way2_title: 'Build from source',
    way2_a: 'Build the .wasm files from the source on GitHub (there are no ready-made downloads yet)', way2_b: 'Put them in plugins/ and grant the permissions in pumpkin.toml',
    way2_c: 'Restart; the console prints Loaded &lt;name&gt;', way2_btn: 'Read how on GitHub',
    foot_a: 'Made by', foot_b: 'open source under MIT',
    foot_c: 'Names such as LuckPerms or WorldEdit belong to their own projects. They are named only to say which job each plugin does; there is no affiliation.',
    rep: 'Stands in for', cmds: 'Main commands', toml: 'Permissions in pumpkin.toml', copy: 'Click to copy',
    more: 'Details →', copied: 'Copied', instead: 'instead of', native: 'Native library: no sandbox permissions; the build must match your Pumpkin release exactly.',
    wasm: 'WASM plugin, runs in Pumpkin\'s sandbox. Data lives in', miss: 'No match for that name yet',
  },
  th: {
    more: 'ดูรายละเอียด →', copied: 'คัดลอกแล้ว', instead: 'แทน', native: 'เป็น native library: ไม่ต้องให้สิทธิ์ sandbox แต่ไฟล์ต้องตรงกับรุ่นของ Pumpkin ที่ใช้พอดี',
    wasm: 'เป็นปลั๊กอิน WASM ทำงานใน sandbox ของ Pumpkin ข้อมูลอยู่ที่', miss: 'ยังไม่มีตัวที่ตรงกับชื่อนี้',
  },
};

const $ = (id) => document.getElementById(id);
const calm = matchMedia('(prefers-reduced-motion: reduce)').matches;
let lang = 'th';
try { lang = localStorage.getItem('lang') === 'en' ? 'en' : 'th'; } catch { /* private window */ }
let cat = 'all';

// Thai is the page as written in index.html; keep it so switching back needs no reload.
document.querySelectorAll('[data-i18n]').forEach((el) => { TEXT.th[el.dataset.i18n] ??= el.innerHTML; });
const t = (key) => TEXT[lang][key] ?? TEXT.th[key] ?? key;

function el(tag, cls, text) {
  const node = document.createElement(tag);
  if (cls) node.className = cls;
  if (text != null) node.textContent = text;
  return node;
}

/* ---------- cards ---------- */
const cards = new Map();
function buildGrid() {
  const grid = $('grid');
  PLUGINS.forEach((p, i) => {
    const card = el('button', 'card in-view');
    card.type = 'button';
    card.style.setProperty('--d', `${(i % 4) * 0.07}s`);
    const top = el('div', 'card-top');
    const img = el('img');
    img.src = `img/${p.id}.webp`; img.alt = ''; img.width = 84; img.height = 84; img.loading = 'lazy';
    top.append(img, el('span', p.native ? 'badge native' : 'badge', p.native ? 'NATIVE' : 'WASM'));
    const rep = el('span', 'rep');
    card.append(top, el('h3', '', p.name), rep, el('p'), el('span', 'more'));
    card.addEventListener('click', () => openSheet(p));
    card.addEventListener('pointermove', (e) => tilt(card, e));
    card.addEventListener('pointerleave', () => { card.style.setProperty('--rx', '0deg'); card.style.setProperty('--ry', '0deg'); });
    grid.append(card);
    cards.set(p.id, card);
  });
  if ('IntersectionObserver' in window && !calm) {
    const seen = new IntersectionObserver((entries) => entries.forEach((e) => {
      if (!e.isIntersecting) return;
      e.target.classList.add('seen');
      setTimeout(() => e.target.classList.add('live'), 900);
      seen.unobserve(e.target);
    }), { rootMargin: '0px 0px -8% 0px' });
    cards.forEach((c) => seen.observe(c));
  } else {
    cards.forEach((c) => c.classList.add('seen', 'live'));
  }
}
function tilt(card, e) {
  if (calm || e.pointerType === 'touch') return;
  const r = card.getBoundingClientRect();
  const x = (e.clientX - r.left) / r.width, y = (e.clientY - r.top) / r.height;
  card.style.setProperty('--mx', `${x * 100}%`);
  card.style.setProperty('--my', `${y * 100}%`);
  card.style.setProperty('--ry', `${(x - 0.5) * 12}deg`);
  card.style.setProperty('--rx', `${(0.5 - y) * 10}deg`);
}
function paintCards() {
  PLUGINS.forEach((p) => {
    const card = cards.get(p.id);
    const rep = card.querySelector('.rep');
    rep.textContent = `${t('instead')} `;
    rep.append(el('b', '', p.rep));
    card.querySelector('p').textContent = p[lang];
    card.querySelector('.more').textContent = t('more');
    card.classList.toggle('gone', cat !== 'all' && p.cat !== cat);
  });
}

/* ---------- category chips ---------- */
function buildChips() {
  const box = $('chips');
  box.textContent = '';
  CATS.forEach((c) => {
    const n = c.id === 'all' ? PLUGINS.length : PLUGINS.filter((p) => p.cat === c.id).length;
    const chip = el('button', 'chip', c[lang]);
    chip.type = 'button'; chip.setAttribute('role', 'tab'); chip.setAttribute('aria-selected', String(c.id === cat));
    chip.append(el('small', '', String(n)));
    chip.addEventListener('click', () => { cat = c.id; buildChips(); paintCards(); });
    box.append(chip);
  });
}

/* ---------- "what did you use on Paper?" ---------- */
const plain = (s) => s.toLowerCase().replace(/[^a-z0-9ก-๙]/g, '');
function find(query) {
  const q = plain(query);
  if (q.length < 2) return [];
  return PLUGINS.filter((p) => [p.name, p.id, p.rep, p.aka].some((s) => s.split(/[\s/]+/).some((w) => plain(w).startsWith(q)) || plain(s).includes(q)));
}
function answer() {
  const out = $('answer');
  const query = $('q').value;
  cards.forEach((c) => c.classList.remove('hit'));
  if (plain(query).length < 2) { out.textContent = t('finder_idle'); return; }
  const hits = find(query).slice(0, 3);
  out.textContent = '';
  if (!hits.length) { out.textContent = t('miss'); return; }
  hits.forEach((p) => {
    const go = el('button', '', p.name);
    go.type = 'button';
    go.addEventListener('click', () => openSheet(p));
    out.append(go);
    cards.get(p.id).classList.add('hit');
  });
}

/* ---------- detail sheet ---------- */
let shown = null;
function toml(p) {
  return `[plugins.overrides.${p.id}]\nallowed_permissions = [${BASE.concat(p.extra || []).map((s) => `"${s}"`).join(', ')}]`;
}
function openSheet(p, push = true) {
  shown = p;
  $('sheet-img').src = `img/${p.id}.webp`;
  $('sheet-name').textContent = p.name;
  $('sheet-rep').textContent = p.rep;
  $('sheet-text').textContent = p[lang];
  const box = $('sheet-cmds');
  box.textContent = '';
  p.cmds.forEach((c) => {
    const b = el('button', 'cmd', c);
    b.type = 'button';
    b.addEventListener('click', () => copy(c));
    box.append(b);
  });
  $('sheet-toml').hidden = $('sheet-toml-title').hidden = !!p.native;
  $('sheet-toml-code').textContent = p.native ? '' : toml(p);
  $('sheet-meta').textContent = p.native ? t('native') : `${t('wasm')} plugins/data/${p.id}/ · ${VERSION}`;
  const sheet = $('sheet');
  if (!sheet.open) sheet.showModal();
  if (push) history.replaceState(null, '', `#${p.id}`);
}
function closeSheet() {
  shown = null;
  if ($('sheet').open) $('sheet').close();
  history.replaceState(null, '', location.pathname);
}
async function copy(text) {
  try { await navigator.clipboard.writeText(text); } catch { return; }
  const toast = $('toast');
  toast.textContent = `${t('copied')}: ${text.split('\n')[0]}`;
  toast.classList.add('on');
  clearTimeout(copy.timer);
  copy.timer = setTimeout(() => toast.classList.remove('on'), 1600);
}

/* ---------- language ---------- */
function paintText() {
  document.documentElement.lang = lang;
  document.querySelectorAll('[data-i18n]').forEach((node) => { node.innerHTML = t(node.dataset.i18n); });
  $('lang').textContent = lang === 'th' ? 'EN' : 'ไทย';
  buildChips();
  paintCards();
  answer();
  if (shown) openSheet(shown, false);
}

/* ---------- the console in the hero: the lines a Pumpkin server prints when these load ---------- */
function runConsole() {
  const log = $('log');
  const stamp = () => new Date().toTimeString().slice(0, 8);
  const line = (html) => {
    const row = el('div');
    row.innerHTML = `<span class="t">${stamp()}</span> ${html}`;
    log.append(row);
    while (log.childElementCount > 13) log.firstElementChild.remove();
  };
  const boot = ['<span class="ok">INFO</span> Starting Pumpkin <span class="v">0.2.2</span> (Minecraft 26.3)', '<span class="ok">INFO</span> Loading plugins…'];
  const loaded = PLUGINS.map((p) => `<span class="ok">INFO</span> Loaded <span class="n">${p.id}</span> (<span class="v">${VERSION}</span>)`);
  const tail = ['<span class="ok">INFO</span> Server is now running. Connect using port: <span class="v">25565</span>', `<span class="ok">INFO</span> ${PLUGINS.length} plugins ready`];
  const script = boot.concat(loaded, tail);
  if (calm) { script.slice(-13).forEach(line); return; }
  let i = 0;
  const step = () => {
    line(script[i]);
    i += 1;
    if (i < script.length) { setTimeout(step, i < 3 ? 520 : 170 + Math.random() * 160); return; }
    setTimeout(() => { log.textContent = ''; i = 0; step(); }, 5200);
  };
  setTimeout(step, 700);
}

/* ---------- the ticker: each plugin's art and name, twice so the loop has no seam ---------- */
function buildTicker() {
  const run = $('ticker');
  for (let pass = 0; pass < 2; pass += 1) {
    PLUGINS.forEach((p) => {
      const item = el('span', 'tick');
      const img = el('img');
      img.src = `img/${p.id}.webp`; img.alt = ''; img.width = 32; img.height = 32; img.loading = 'lazy';
      item.append(img, el('b', '', p.name));
      run.append(item);
    });
  }
}

/* ---------- the sky: voxel cubes drifting up, leaning toward the pointer ---------- */
function runSky() {
  const canvas = $('sky');
  const ctx = canvas.getContext('2d');
  const hues = [188, 258, 28]; // cyan, violet, pumpkin
  let w = 0, h = 0, cubes = [], px = 0, py = 0, raf = 0;
  const size = () => {
    const dpr = Math.min(devicePixelRatio || 1, 2);
    w = innerWidth; h = innerHeight;
    canvas.width = w * dpr; canvas.height = h * dpr;
    ctx.setTransform(dpr, 0, 0, dpr, 0, 0);
    const n = Math.round(Math.min(46, (w * h) / 32000));
    cubes = Array.from({ length: n }, () => ({
      x: Math.random() * w, y: Math.random() * h, s: 6 + Math.random() * 20, v: 0.12 + Math.random() * 0.4,
      hue: hues[Math.floor(Math.random() * hues.length)], a: 0.1 + Math.random() * 0.32, ph: Math.random() * 6.28,
    }));
  };
  const face = (pts, fill) => {
    ctx.beginPath();
    ctx.moveTo(pts[0], pts[1]);
    for (let k = 2; k < pts.length; k += 2) ctx.lineTo(pts[k], pts[k + 1]);
    ctx.closePath();
    ctx.fillStyle = fill;
    ctx.fill();
  };
  const cube = (x, y, s, hue, a) => {
    const dx = s * 0.87, dy = s * 0.5;
    face([x, y - s, x + dx, y - dy, x, y, x - dx, y - dy], `hsla(${hue},95%,72%,${a})`);
    face([x - dx, y - dy, x, y, x, y + s, x - dx, y + dy], `hsla(${hue},85%,52%,${a})`);
    face([x + dx, y - dy, x, y, x, y + s, x + dx, y + dy], `hsla(${hue},80%,36%,${a})`);
  };
  const draw = (time) => {
    ctx.clearRect(0, 0, w, h);
    cubes.forEach((c) => {
      if (!calm) {
        c.y -= c.v;
        if (c.y < -40) { c.y = h + 40; c.x = Math.random() * w; }
      }
      const depth = c.s / 26;
      const sway = calm ? 0 : Math.sin(time / 1800 + c.ph) * 6;
      cube(c.x + sway + px * depth * 26, c.y + py * depth * 18, c.s, c.hue, c.a);
    });
    if (!calm) raf = requestAnimationFrame(draw);
  };
  addEventListener('resize', () => { size(); if (calm) draw(0); });
  addEventListener('pointermove', (e) => { px = e.clientX / w - 0.5; py = e.clientY / h - 0.5; }, { passive: true });
  document.addEventListener('visibilitychange', () => {
    cancelAnimationFrame(raf);
    if (!document.hidden && !calm) raf = requestAnimationFrame(draw);
  });
  size();
  draw(0);
}

/* ---------- the big number counts up ---------- */
function countUp() {
  const node = $('count');
  if (calm) return;
  const end = PLUGINS.length, t0 = performance.now();
  const tick = (now) => {
    const k = Math.min(1, (now - t0) / 1100);
    node.textContent = String(Math.round(end * (1 - (1 - k) ** 3)));
    if (k < 1) requestAnimationFrame(tick);
  };
  requestAnimationFrame(tick);
}

/* ---------- start ---------- */
buildGrid();
buildTicker();
paintText();
runConsole();
runSky();
countUp();

$('lang').addEventListener('click', () => {
  lang = lang === 'th' ? 'en' : 'th';
  try { localStorage.setItem('lang', lang); } catch { /* private window */ }
  paintText();
});
$('q').addEventListener('input', answer);
$('finder').addEventListener('submit', (e) => {
  e.preventDefault();
  const first = find($('q').value)[0];
  if (first) openSheet(first);
});
$('sheet-close').addEventListener('click', closeSheet);
$('sheet').addEventListener('click', (e) => { if (e.target === $('sheet')) closeSheet(); });
$('sheet').addEventListener('close', () => { shown = null; history.replaceState(null, '', location.pathname); });
$('sheet-toml').addEventListener('click', () => { if (shown) copy(toml(shown)); });

const linked = PLUGINS.find((p) => `#${p.id}` === location.hash);
if (linked) openSheet(linked, false);

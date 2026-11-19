const { app, BrowserWindow, Menu, ipcMain, dialog, shell } = require('electron');
const path = require('path');
const fs = require('fs');
const { spawn } = require('child_process');
const M = require('./modini.js');

let win;
const settingsFile = () => path.join(app.getPath('userData'), 'porter-settings.json');
function loadSettings() { try { return JSON.parse(fs.readFileSync(settingsFile(), 'utf8')); } catch { return {}; } }
function saveSettings(s) { fs.mkdirSync(path.dirname(settingsFile()), { recursive: true }); fs.writeFileSync(settingsFile(), JSON.stringify(s, null, 2)); }

function backendExe() {
  const candidates = [
    path.join(process.resourcesPath || '', 'backend', 'SkinCSSEngine.exe'),
    path.join(__dirname, '..', 'backend', 'SkinCSSEngine.exe'),
    path.join(__dirname, 'backend', 'SkinCSSEngine.exe'),
    path.join(__dirname, '..', 'backend', 'skincssengine'),
  ];
  return candidates.find((p) => fs.existsSync(p)) || candidates[0];
}

// Run the Rust backend; resolves with the parsed "@@{json}" messages, streams logs to the window
function runBackend(args, onMsg, opts = {}) {
  return new Promise((resolve) => {
    const msgs = [];
    let buf = '';
    let errText = '';
    const child = spawn(backendExe(), args, { windowsHide: true, cwd: path.dirname(backendExe()), env: backendEnv(opts.anyTables, opts.tables) });
    child.stdout.on('data', (d) => {
      buf += d.toString();
      let i;
      while ((i = buf.indexOf('\n')) >= 0) {
        const line = buf.slice(0, i).trim();
        buf = buf.slice(i + 1);
        if (line.startsWith('@@')) {
          try { const m = JSON.parse(line.slice(2)); msgs.push(m); onMsg && onMsg(m); } catch {}
        }
      }
    });
    child.stderr.on('data', (d) => { errText += d.toString(); });
    child.on('error', (e) => resolve([{ type: 'error', msg: `Couldn't start the porter engine: ${e.message}` }]));
    child.on('close', (code) => {
      if (!msgs.some((m) => m.type === 'done' || m.type === 'inspect' || m.type === 'env' || m.type === 'error'))
        msgs.push({ type: 'error', msg: (errText.trim().split('\n').slice(-3).join(' ') || `engine exited (${code})`) });
      resolve(msgs);
    });
  });
}

function envArgs() {
  const s = loadSettings();
  const a = [];
  // a saved folder that no longer exists (renamed, typo, wrong pick) is ignored: the engine finds it again from BlueStar
  if (s.gameDir && fs.existsSync(s.gameDir)) a.push('--game-dir', s.gameDir);
  if (s.modsDir && fs.existsSync(s.modsDir)) a.push('--mods-dir', s.modsDir);
  return a;
}

// Which enabled mod gives AI Skin 1-3 their own slots: True CSSE first, else Separate, Ai Skins!.
// Its racer-table container is passed to the engine (AISP_TABLES; empty = neither is on).
const CSSE_MOD = /true css/i;
const COMBO_MOD = /csse icons/i; // "Separate AI Skins + CSSE Icons": True CSSE's racer list with Separate AI Skins' names and animations
function backendEnv(anyTables, tables) {
  const e = { ...process.env };
  delete e.AISP_TABLES;
  // anyTables: only finding the game / Mods folders, no racer table needed ("-")
  const f = tables !== undefined ? tables : anyTables ? '-' : tablesProvider().file;
  if (f !== undefined) e.AISP_TABLES = f;
  return e;
}
function tablesProvider() {
  const s = loadSettings();
  const gameDir = s.gameDir || (env && env.gameDir), modsDir = s.modsDir || (env && env.modsDir);
  if (!modsDir) return { kind: null, file: undefined }; // not set up yet: let the engine search
  const bs = bluestarConfigs(gameDir, modsDir);
  const isOn = (m) => bs.enabled.some((n) => n === m.name || path.basename(m.dir) === n);
  let mods = [];
  try { mods = M.findMods(modsDir).map((d) => M.readMod(d)).filter(isOn); } catch {}
  // both replace the same racer table, so the game uses whichever is higher in BlueStar's list
  const rank = (m) => { const i = bs.order.findIndex((n) => n === m.name || path.basename(m.dir) === n); return i < 0 ? 1e9 : i; };
  const found = [];
  for (const m of mods) {
    const is = (re) => re.test(m.name) || re.test(path.basename(m.dir));
    const kind = is(COMBO_MOD) ? 'combo' : is(TABLE_MOD) ? 'separate' : is(CSSE_MOD) ? 'csse' : null;
    const u = kind && m.utocs.find((x) => !x.disabled);
    if (u) found.push({ kind, file: u.file, name: m.name, rank: rank(m) });
  }
  found.sort((a, b) => a.rank - b.rank);
  const top = found[0] || { kind: null, file: '' };
  top.both = found.some((f) => f.kind === 'csse') && found.some((f) => f.kind === 'separate');
  top.separateOn = found.some((f) => f.kind === 'separate' || f.kind === 'combo');
  top.csseOn = found.some((f) => f.kind === 'csse');
  top.csseFile = (found.find((f) => f.kind === 'csse') || {}).file;
  return top;
}


// BlueStar's settings.json: the one whose ModsDirectory is our Mods folder (BlueStar can live anywhere),
// else the one next to the Mods folder, else the one in the game folder.
function bsSettingsFile(gameDir, modsDir) {
  modsDir = modsDir || (env && env.modsDir); gameDir = gameDir || (env && env.gameDir);
  const norm = (p) => path.resolve(String(p || '')).replace(/[\\/]+$/, '').toLowerCase();
  const cands = [modsDir && path.join(modsDir, '..', 'settings.json'), gameDir && path.join(gameDir, 'BlueStarManager', 'settings.json')].filter((f) => f && fs.existsSync(f));
  for (const c of cands) {
    try { const j = JSON.parse(fs.readFileSync(c, 'utf8').replace(/^\ufeff/, '')); if (modsDir && j.ModsDirectory && norm(j.ModsDirectory) === norm(modsDir)) return c; } catch {}
  }
  return cands[0] || null;
}
function bluestarConfigs(gameDir, modsDir) {
  const cands = [bsSettingsFile(gameDir, modsDir)].filter(Boolean);
  for (const c of cands) {
    try {
      const j = JSON.parse(fs.readFileSync(c, 'utf8').replace(/^\ufeff/, ''));
      const prof = (j.Profiles || {})[j.ActiveProfileName || 'Default'] || {};
      return { configs: prof.ModConfigurations || j.ModConfigurations || {}, enabled: prof.EnabledMods || [], order: prof.ModLoadOrder || prof.EnabledMods || [] };
    } catch {}
  }
  return { configs: {}, enabled: [], order: [] };
}

let env = null;
// BlueStar can live anywhere (e.g. next to this app): look for a "Bluestar Manager.exe" beside the app or in the
// game folder and read its settings.json for the Mods folder.
function findBlueStarMods(gameDir) {
  const roots = [path.dirname(path.dirname(process.execPath)), path.dirname(process.execPath), gameDir].filter(Boolean);
  const dirs = [];
  for (const r of roots) {
    let ents = [];
    try { ents = fs.readdirSync(r, { withFileTypes: true }); } catch { continue; }
    for (const e of ents) if (e.isDirectory() && /blue\s*star/i.test(e.name)) dirs.push(path.join(r, e.name));
  }
  for (const d of dirs) {
    if (!fs.existsSync(path.join(d, 'Bluestar Manager.exe')) && !fs.existsSync(path.join(d, 'settings.json'))) continue;
    try {
      const j = JSON.parse(fs.readFileSync(path.join(d, 'settings.json'), 'utf8').replace(/^\ufeff/, ''));
      if (j.ModsDirectory && fs.existsSync(j.ModsDirectory)) return j.ModsDirectory;
    } catch {}
    if (fs.existsSync(path.join(d, 'Mods'))) return path.join(d, 'Mods');
  }
  return null;
}
async function getEnv(force) {
  if (env && !force) return env;
  { // no usable Mods folder saved yet: try to find BlueStar ourselves before asking the engine
    const s = loadSettings();
    if (!(s.modsDir && fs.existsSync(s.modsDir))) { const m = findBlueStarMods(s.gameDir); if (m) saveSettings({ ...s, modsDir: m }); }
  }
  // finding the game and Mods folder doesn't depend on which racer-table mod is on
  const msgs = await runBackend(['gui-inspect', ...envArgs()], null, { anyTables: true });
  const e = msgs.find((m) => m.type === 'env');
  env = e ? { ok: true, gameDir: e.game_dir, modsDir: e.mods_dir, csse: e.csse } : { ok: false, error: (msgs.find((m) => m.type === 'error') || {}).msg };
  if (env.ok) env.tables = tablesProvider().kind;
  if (env.ok) {
    const s = loadSettings();
    const ok = (d) => d && fs.existsSync(d);
    if (!ok(s.gameDir) || !ok(s.modsDir)) saveSettings({ ...s, gameDir: ok(s.gameDir) ? s.gameDir : env.gameDir, modsDir: ok(s.modsDir) ? s.modsDir : env.modsDir });
  }
  return env;
}

function thumbData(p) {
  if (!p) return null;
  try {
    const ext = path.extname(p).slice(1).toLowerCase().replace('jpg', 'jpeg');
    return `data:image/${ext};base64,${fs.readFileSync(p).toString('base64')}`;
  } catch { return null; }
}

function modSummary(dir) {
  const m = M.readMod(dir);
  return { dir, name: m.name, author: m.author, version: m.version, thumb: m.thumb, hasConfig: m.configs.length > 0, porter: M.porterOptions(m).length, utocs: m.utocs.length, utocFiles: m.utocs.map((u) => u.file) };
}

// Which containers touch racer models? Cached by path + size + date so the list stays instant.
const classifyFile = () => path.join(app.getPath('userData'), 'skin-check-cache-v3.json');
async function classify(files) {
  let cache = {};
  try { cache = JSON.parse(fs.readFileSync(classifyFile(), 'utf8')); } catch {}
  const key = (f) => { try { const st = fs.statSync(f); return `${f}|${st.size}|${Math.round(st.mtimeMs)}`; } catch { return f; } };
  const todo = files.filter((f) => !(key(f) in cache));
  for (let i = 0; i < todo.length; i += 40) {
    const part = todo.slice(i, i + 40);
    const e = await getEnv();
    const msgs = await runBackend(['gui-classify', '--game-dir', e.gameDir, '--mods-dir', e.modsDir, ...part]);
    for (const m of msgs) if (m.type === 'classify') cache[key(m.file)] = { skin: m.skin, slots: m.slots || [] };
  }
  if (todo.length) { try { fs.writeFileSync(classifyFile(), JSON.stringify(cache)); } catch {} }
  const out = {};
  for (const f of files) out[f] = cache[key(f)];
  return out;
}

ipcMain.handle('env', async (_e, force) => {
  const e = await getEnv(force);
  if (e.ok) { const tp = tablesProvider(); e.tables = tp.kind; e.tablesBoth = tp.both; } // BlueStar may have changed since
  return e;
});
ipcMain.handle('settings:get', () => loadSettings());
ipcMain.handle('settings:set', async (_e, s) => { saveSettings({ ...loadSettings(), ...s }); return getEnv(true); });
ipcMain.handle('pickFolder', async (_e, title) => {
  const r = await dialog.showOpenDialog(win, { title, properties: ['openDirectory'] });
  return r.canceled ? null : r.filePaths[0];
});
ipcMain.handle('mods:list', async () => {
  const e = await getEnv();
  if (!e.ok) return [];
  const bs = bluestarConfigs(e.gameDir, e.modsDir);
  const list = M.findMods(e.modsDir)
    .map(modSummary)
    .filter((m) => m.utocs > 0 && !/True CSSE|CSSE 9 Column|^Custom CSS$/i.test(m.name))
    .map((m) => ({ ...m, enabled: bs.enabled.some((n) => n === m.name || m.dir.includes(n)) }));
  let res = {};
  try { res = await classify(list.flatMap((m) => m.utocFiles)); } catch {}
  const out = list.map(({ utocFiles, ...m }) => {
    // racer slots it replaces with its current options (BlueStar renames unused files to .disabled)
    const slots = [...new Set(utocFiles.filter((f) => !/\.disabled$/i.test(f)).flatMap((f) => (res[f] && res[f].slots) || []))];
    // "base" mods that rework lots of racers at once (Separate, Ai Skins!...) aren't skins to port or clash with
    const base = TABLE_MOD.test(m.name) || TABLE_MOD.test(path.basename(m.dir)) || slots.length > 8;
    return {
      ...m,
      // a mod is a skin if any of its containers touches racer models; unknown counts as a skin
      skin: !base && (m.porter > 0 || utocFiles.some((f) => !res[f] || res[f].skin !== false)),
      slots: base ? [] : slots,
    };
  });
  // two enabled mods on the same racer slot: only one will show in game
  const bySlot = {};
  for (const m of out) if (m.enabled) for (const sl of m.slots) (bySlot[sl] = bySlot[sl] || []).push(m);
  for (const m of out) {
    m.conflicts = [];
    if (!m.enabled) continue;
    for (const sl of m.slots) {
      const others = bySlot[sl].filter((o) => o !== m).map((o) => o.name);
      if (others.length) m.conflicts.push({ slot: slotLabel(sl), with: others });
    }
  }
  try { await addVoiceNotes(e, bs, out); } catch {}
  return out;
});

// ---- Voices: the game picks one voice file per character (VOICE_AMY, and VOICE_AMYAI for all of AI Amy's
// skins), not per skin. Tell when enabled mods fight over one, and which voice an AI skin will really use.
const VOICE_CODES = { Eggpawn: 'egp', Cubot: 'cbt', DrEggman: 'egg', MetalSonic: 'met', Metalsonic: 'met', Dodonpa: 'dod' };
const voiceCode = (folder) => (VOICE_CODES[folder] || folder.slice(0, 3)).toUpperCase();
const voiceCache = {};
async function paksVoices(paks) {
  const key = (f) => { try { const st = fs.statSync(f); return `${f}|${st.size}|${Math.round(st.mtimeMs)}`; } catch { return f; } };
  const todo = paks.filter((f) => !(key(f) in voiceCache));
  for (let i = 0; i < todo.length; i += 40) {
    const part = todo.slice(i, i + 40);
    const msgs = await runBackend(['gui-voices', ...part]);
    const done = msgs.find((m) => m.type === 'done');
    for (const f of part) voiceCache[key(f)] = (done && done.voices && done.voices[f]) || [];
  }
  const out = {};
  for (const f of paks) out[f] = voiceCache[key(f)] || [];
  return out;
}
function voiceWho(sheet, names) {
  const m = sheet.match(/^VOICE_([A-Z0-9]+?)(AI)?$/);
  if (!m) return sheet;
  const nm = names[m[1]] || m[1].charAt(0) + m[1].slice(1).toLowerCase();
  return m[2] ? `AI ${nm}` : nm;
}
async function addVoiceNotes(e, bs, out) {
  for (const m of out) m.voiceNotes = [];
  const isOn = (d, name) => bs.enabled.some((n) => n === name || path.basename(d) === n || d.includes(n));
  const rank = (d, name) => { const i = bs.order.findIndex((n) => n === name || path.basename(d) === n || d.includes(n)); return i < 0 ? 1e9 : i; };
  // every enabled mod counts, voice-only ones too (they don't show in the list)
  const mods = M.findMods(e.modsDir).map((d) => ({ dir: d, name: M.readMod(d).name })).filter((m) => isOn(m.dir, m.name));
  const paks = {};
  for (const m of mods) paks[m.dir] = M.walk(m.dir).filter((f) => /\.pak$/i.test(f));
  const found = await paksVoices(Object.values(paks).flat());
  const bySheet = {};
  for (const m of mods) {
    const v = new Set(paks[m.dir].flatMap((f) => found[f] || []));
    m.voices = v;
    for (const sh of v) (bySheet[sh] = bySheet[sh] || []).push(m);
  }
  // separate voices: tell the UE4SS script which AI skin slots have their own bank now
  try { const vs = voicesStatus(e.gameDir); if (vs.script) writeVoiceSlots(e.gameDir, Object.keys(bySheet)); } catch {}
  // voice packs are plain .pak files: for those the LOWEST mod in BlueStar's list wins (user-tested), the
  // opposite of models/tables. So the winner goes first.
  for (const l of Object.values(bySheet)) l.sort((a, b) => rank(b.dir, b.name) - rank(a.dir, a.name));
  // character names for the messages, from the slots the listed skins replace
  const names = {};
  const slotInfo = (sl) => {
    const x = sl.match(/\/Asset\/Character\/(?:(Skin)(\d*)\/)?(\w+?)(?:Skin)?\/Mesh\//);
    if (!x) return null;
    const code = voiceCode(x[3]);
    names[code] = RACER_NAMES[x[3]] || x[3].replace(/([a-z])([A-Z])/g, '$1 $2');
    return { ai: !!x[1], code, n: x[2] || '' };
  };
  for (const m of out) for (const sl of m.slots || []) slotInfo(sl);
  const nd = (d) => String(d).replace(/\\/g, '/').toLowerCase();
  for (const m of out) {
    if (!m.enabled) continue;
    const me = mods.find((x) => nd(x.dir) === nd(m.dir) || nd(x.dir).startsWith(nd(m.dir) + '/') || nd(m.dir).startsWith(nd(x.dir) + '/'));
    const mine = me ? me.voices : new Set();
    // two enabled mods bring the same voice file: only the one lowest in BlueStar is heard
    for (const sh of mine) {
      const l = bySheet[sh];
      if (l.length < 2) continue;
      const others = l.filter((x) => x !== me).map((x) => x.name);
      m.voiceNotes.push({ kind: 'clash', who: voiceWho(sh, names), with: others, winner: l[0].name, wins: l[0] === me });
    }
    // an AI skin without its own voice speaks with whatever voice the character's AI slot has
    const seen = new Set();
    for (const sl of m.slots || []) {
      const i = slotInfo(sl);
      if (!i || !i.ai || seen.has(i.code)) continue;
      seen.add(i.code);
      if (i.n && mine.has(`VOICE_${i.code}A${Number(i.n)}`)) { m.voiceNotes.push({ kind: 'own', who: `${voiceWho(`VOICE_${i.code}AI`, names)} (AI Skin ${Number(i.n)})` }); continue; }
      const sh = `VOICE_${i.code}AI`;
      const l = bySheet[sh];
      if (!l || mine.has(sh)) continue;
      m.voiceNotes.push({ kind: 'shared', who: voiceWho(sh, names), winner: l[0].name });
    }
  }
}


// ---- Separate AI voices (UE4SS). The game has one voice bank per character for all its AI skins. With
// UE4SS + our script (CrossworldSkinCSSVoices), an AI skin slot can have its own bank VOICE_<X>A<N>; the script
// switches the character's AI lines to it whenever the player on this PC picks / drives that slot.
const VOICE_ORDER = ['SON', 'TAI', 'KNU', 'AMY', 'EGG', 'SHA', 'ROU', 'SIL', 'BLA', 'MET', 'OME', 'VEC', 'ESP', 'CHA', 'ZAV', 'ZAZ', 'CRE', 'BIG', 'EGP', 'SAG', 'JET', 'WAV', 'STO'];
const VOICE_MOD = 'CrossworldSkinCSSVoices';
const OLD_VOICE_MODS = []; // earlier names of the voice script (none in release builds)
function bundledDir(name) {
  const c = [path.join(process.resourcesPath || '', name), path.join(__dirname, '..', name), path.join(__dirname, name)];
  return c.find((p) => fs.existsSync(p)) || c[0];
}
function voicePaths(gameDir) {
  const bin = path.join(gameDir || '', 'UNION', 'Binaries', 'Win64');
  const ue = path.join(bin, 'ue4ss');
  return { bin, ue, dll: path.join(bin, 'dwmapi.dll'), modsTxt: path.join(ue, 'Mods', 'mods.txt'), mod: path.join(ue, 'Mods', VOICE_MOD), slots: path.join(ue, 'Mods', VOICE_MOD, 'Scripts', 'slots.lua') };
}
function readModsTxt(f) { try { return fs.readFileSync(f, 'utf8').split(/\r?\n/); } catch { return []; } }
function setModsTxt(f, name, on) {
  let lines = readModsTxt(f).filter((l) => l.trim() !== '');
  const re = new RegExp('^\\s*' + name + '\\s*:');
  const line = `${name} : ${on ? 1 : 0}`;
  const i = lines.findIndex((l) => re.test(l));
  if (i >= 0) lines[i] = line; else {
    // keep "Keybinds" (UE4SS's own) last, like the default file does
    const k = lines.findIndex((l) => /^\s*;\s*Built-in keybinds/i.test(l) || /^\s*Keybinds\s*:/.test(l));
    if (k >= 0) lines.splice(k, 0, line); else lines.push(line);
  }
  fs.mkdirSync(path.dirname(f), { recursive: true });
  fs.writeFileSync(f, lines.join('\r\n') + '\r\n');
}
function voicesStatus(gameDir) {
  const p = voicePaths(gameDir);
  const ue4ss = fs.existsSync(p.dll) && fs.existsSync(path.join(p.ue, 'UE4SS.dll'));
  const script = fs.existsSync(path.join(p.mod, 'Scripts', 'main.lua'));
  const on = ue4ss && script && readModsTxt(p.modsTxt).some((l) => new RegExp('^\\s*' + VOICE_MOD + '\\s*:\\s*1').test(l));
  let slots = [];
  try { slots = (fs.readFileSync(p.slots, 'utf8').match(/\d+/g) || []).map(Number).filter((n) => n >= 100); } catch {}
  return { on, ue4ss, script, slots };
}
function copyDir(src, dst) {
  for (const f of fs.readdirSync(src, { withFileTypes: true })) {
    const a = path.join(src, f.name), b = path.join(dst, f.name);
    if (f.isDirectory()) { fs.mkdirSync(b, { recursive: true }); copyDir(a, b); }
    else { fs.mkdirSync(dst, { recursive: true }); fs.copyFileSync(a, b); }
  }
}
// Slot code for a target slot mesh /Asset/Character/Skin0N/<X>Skin/Mesh/...: "SHAA1", or null
function slotVoiceCode(mesh) {
  const x = String(mesh || '').match(/\/Asset\/Character\/Skin0?(\d)\/(\w+?)(?:Skin)?\/Mesh\//);
  if (!x || !/^[123]$/.test(x[1])) return null;
  const c = voiceCode(x[2]);
  return VOICE_ORDER.includes(c) ? `${c}A${x[1]}` : null;
}
// slots.lua = DriverIds of AI skin slots that an enabled mod gives their own bank (VOICE_SHAA1 -> 115)
function writeVoiceSlots(gameDir, sheets) {
  const p = voicePaths(gameDir);
  if (!fs.existsSync(path.join(p.mod, 'Scripts'))) return null;
  const ids = [...new Set([...sheets].map((sh) => {
    const m = String(sh).match(/^VOICE_([A-Z]{3})A([123])$/);
    const i = m ? VOICE_ORDER.indexOf(m[1]) : -1;
    return i < 0 ? null : 100 + 3 * i + (Number(m[2]) - 1);
  }).filter((x) => x != null))].sort((a, b) => a - b);
  const txt = `-- written by Crossworld Skin & CSS Manager: AI skin slots (DriverIds) that have their own voice bank\r\nreturn { ${ids.join(', ')} }\r\n`;
  let cur = '';
  try { cur = fs.readFileSync(p.slots, 'utf8'); } catch {}
  if (cur !== txt) { try { fs.writeFileSync(p.slots, txt); } catch {} }
  return ids;
}
ipcMain.handle('voices:status', async () => {
  const e = await getEnv();
  if (!e.ok) return { on: false, error: 'game not found' };
  return voicesStatus(e.gameDir);
});
ipcMain.handle('voices:set', async (_e, on) => {
  const e = await getEnv();
  if (!e.ok) return { error: 'Game folder not found (see Settings).' };
  const p = voicePaths(e.gameDir);
  try {
    if (on) {
      const ue = bundledDir('ue4ss');
      if (!fs.existsSync(path.join(ue, 'dwmapi.dll'))) return { error: 'UE4SS files are missing from the app (resources/ue4ss).' };
      // UE4SS itself: only if it isn't there yet (keep a UE4SS the user already has, and its settings)
      if (!fs.existsSync(path.join(p.ue, 'UE4SS.dll'))) copyDir(ue, p.bin);
      else if (!fs.existsSync(p.dll)) fs.copyFileSync(path.join(ue, 'dwmapi.dll'), p.dll);
      copyDir(path.join(bundledDir('ue4ss-mod'), VOICE_MOD), p.mod);
      setModsTxt(p.modsTxt, VOICE_MOD, true);
      for (const o of OLD_VOICE_MODS) if (readModsTxt(p.modsTxt).some((l) => new RegExp('^\\s*' + o + '\\s*:').test(l))) setModsTxt(p.modsTxt, o, false); // older copies
      // UE4SS's example mods hook the game while it loads and crashed it here (Cheat Manager Enabler): switch them off.
      // The Blueprint mod loaders are only needed for LogicMods, so they stay on when that folder has mods in it.
      const has = (n) => readModsTxt(p.modsTxt).some((l) => new RegExp('^\\s*' + n + '\\s*:\\s*1').test(l));
      let logic = [];
      try { logic = fs.readdirSync(path.join(e.gameDir, 'UNION', 'Content', 'Paks', 'LogicMods')); } catch {}
      const risky = ['CheatManagerEnablerMod', 'ConsoleEnablerMod', 'ConsoleCommandsMod', ...(logic.length ? [] : ['BPModLoaderMod', 'BPML_GenericFunctions'])];
      for (const n of risky) if (has(n)) setModsTxt(p.modsTxt, n, false);
    } else {
      setModsTxt(p.modsTxt, VOICE_MOD, false);
    }
  } catch (err) {
    const busy = /EBUSY|EPERM|EACCES/.test(String(err && err.code));
    return { error: busy ? 'The game is running (or a file is locked). Close the game and try again.' : String(err.message || err) };
  }
  const s = loadSettings(); saveSettings({ ...s, separateVoices: !!on });
  lastVoiceSheets = null;
  return voicesStatus(e.gameDir);
});
let lastVoiceSheets = null;


// ---- Starter mods: the app only makes sense with these three (checked at start, mandatory)
const STARTERS = [
  { key: 'csse', name: 'True CSS Expansion (CSSE)', by: 'ArtemisPendragon', url: 'https://gamebanana.com/mods/634428', re: /true css/i,
    why: 'Gives every AI skin its own slot and icon on the character select screen.' },
  { key: 'separate', name: 'Separate, Ai Skins!', by: 'Emasplatter', url: 'https://gamebanana.com/mods/642537', re: /separate.*ai.*skin/i,
    why: 'Gives every AI skin its own name, animations and model.' },
  { key: 'stamps', name: 'Stamps Foundation Mod', by: 'Efflorescedbloom', url: 'https://gamebanana.com/mods/637453', re: /stamps? foundation/i,
    why: 'Gives every slot its own stamps.' },
];
ipcMain.handle('setup:status', async () => {
  const e = await getEnv(true);
  if (!e.ok) return { error: e.error || 'Game or Mods folder not found (see Settings).' };
  if (!e.modsDir || !fs.existsSync(e.modsDir)) return { error: `Your BlueStar Mods folder wasn't found (${e.modsDir || 'not set'}). Set it in Settings.` };
  const bs = bluestarConfigs(e.gameDir, e.modsDir);
  const mods = M.findMods(e.modsDir).map((d) => ({ dir: d, name: M.readMod(d).name }))
    .filter((m) => !/csse icons/i.test(m.name) && !/csse icons/i.test(path.basename(m.dir))); // our own combo mod isn't a starter
  const isOn = (m) => bs.enabled.some((n) => n === m.name || path.basename(m.dir) === n || m.dir.includes(n));
  const starters = STARTERS.map((st) => {
    const found = mods.filter((m) => st.re.test(m.name) || st.re.test(path.basename(m.dir)));
    const on = found.some(isOn);
    return { key: st.key, name: st.name, by: st.by, url: st.url, why: st.why, state: on ? 'on' : found.length ? 'off' : 'missing' };
  });
  const combo = await comboStatus().catch(() => ({ state: 'off' }));
  const voices = voicesStatus(e.gameDir);
  const s = loadSettings();
  return { starters, ready: starters.every((x) => x.state === 'on'), combo: combo.state, voices: { on: voices.on }, voicesAsked: !!s.voicesAsked, orderSeen: !!s.orderSeen };
});
ipcMain.handle('setup:voicesAsked', () => { const s = loadSettings(); saveSettings({ ...s, voicesAsked: true }); return true; });
ipcMain.handle('setup:orderSeen', () => { const s = loadSettings(); saveSettings({ ...s, orderSeen: true }); return true; });

// ---- CSS order tab: icons of racers your enabled mods replace, in BlueStar's priority order (top wins)
let cssIconCache = { key: null, data: null };
ipcMain.handle('css:icons', async () => {
  const e = await getEnv();
  if (!e.ok) return { icons: {}, nineCol: false };
  const tp = tablesProvider();
  const block = cssBlock(tp);
  const chk = block ? { ok: false } : await csseMatches(e).catch(() => ({ ok: true }));
  const csseWarn = block || (chk.ok ? null : (chk.error ? `Couldn't check your True CSSE: ${chk.error}` : CSSE_MISMATCH));
  const bs = bluestarConfigs(e.gameDir, e.modsDir);
  const isOn = (m, n) => n === m.name || path.basename(m.dir) === n || m.dir.includes(n);
  const rank = (m) => { const i = bs.order.findIndex((n) => isOn(m, n)); return i < 0 ? 1e9 : i; };
  const mods = M.findMods(e.modsDir).map((d) => M.readMod(d))
    .filter((m) => bs.enabled.some((n) => isOn(m, n)) && !/True CSSE|True CSS Expansion|CSSE 9 Column|CSS Order|^Custom CSS$/i.test(m.name) && !TABLE_MOD.test(m.name) && !TABLE_MOD.test(path.basename(m.dir)))
    .sort((a, b) => rank(a) - rank(b));
  const nineCol = M.findMods(e.modsDir).some((d) => /CSSE 9 Column/i.test(d)) && bs.enabled.some((n) => /CSSE 9 Column/i.test(n));
  const files = [], owner = [];
  for (const m of mods) for (const u of m.utocs) if (!u.disabled) { files.push(u.file); owner.push(m.name); }
  if (!files.length) return { icons: {}, nineCol, csseWarn };
  const key = files.map((f) => { try { const st = fs.statSync(f); return `${f}|${st.size}|${Math.round(st.mtimeMs)}`; } catch { return f; } }).join('\n');
  if (cssIconCache.key === key) return { icons: cssIconCache.data, nineCol, csseWarn };
  const msgs = await runBackend(['gui-mod-icons', '--game-dir', e.gameDir, '--mods-dir', e.modsDir, ...files]);
  const done = msgs.find((m) => m.type === 'done');
  if (!done) return { icons: {}, nineCol, csseWarn, error: (msgs.find((m) => m.type === 'error') || {}).msg };
  const icons = {};
  for (const [k, v] of Object.entries(done.icons || {})) icons[k] = { icon: v.icon, mod: owner[v.src] || 'a mod', model: v.model };
  cssIconCache = { key, data: icons };
  return { icons, nineCol, csseWarn };
});

// The CSS order editor carries a copy of these True CSSE v3.0 tables. Saving over a different True CSSE
// version would swap its racer data for old data (wrong animations, T-poses), so check first.
const CSSE_TABLE_HASHES = [
  '67e6070bfdc5ed9de29b069bb92497052cdf22a3069f4b8a9dea048a136bba81', 'a110aba6fb78d61e30005b7726475c434079ab431cd105b4e8e677fd506e75b1',
  '00f5020fd1bc010759493126c81213ffd9be6655d8113cd21016fa88c6f927f9', '61fc66e3ba4105cb44503c43f193a710dda4cf3b2e0706e928ef8c331f6c3654',
  '6a282544d8012c953ae77d605debc7c369bf0074af1b71f4435fabffc21068ba', 'ffa2b48f38b74e2cf804327dd1c47d29eef3292086e6cf2fc05c30210f61940d',
  'd8cc2010aeaa1ce0498f03fbbe7810d718a37866604d88e953b521e08ffdd782',
];
async function csseMatches(e) {
  const msgs = await runBackend(['gui-css-tables', '--game-dir', e.gameDir, '--mods-dir', e.modsDir], null, { tables: tablesProvider().csseFile });
  const done = msgs.find((m) => m.type === 'done');
  if (!done) return { ok: false, error: (msgs.find((m) => m.type === 'error') || {}).msg || 'could not read True CSSE' };
  return { ok: done.hashes.length === CSSE_TABLE_HASHES.length && done.hashes.every((h, i) => h === CSSE_TABLE_HASHES[i]) };
}
const NO_CSSE = 'The CSS order editor changes True CSSE\'s racer order, so it needs True CSSE turned on in BlueStar. Without it, the game uses its normal order.';
const CSS_VS_SEPARATE = 'Separate, Ai Skins! is on above True CSSE. A custom order would replace its racer table, so your AI skins would lose their own names and animations. Use the "Separate AI Skins + CSSE Icons" mod (at the top of BlueStar) to have both.';
const cssBlock = (tp) => !tp.csseOn ? NO_CSSE : tp.separateOn && tp.kind !== 'combo' ? CSS_VS_SEPARATE : null;
const CSSE_MISMATCH = 'Your True CSSE is a different version from the one this CSS editor was made for. Saving was stopped so it can\'t break your characters. Ask for a Crossworld Skin & CSS Manager update.';

ipcMain.handle('css:install', async (_e, buf) => {
  const e = await getEnv();
  if (!e.ok) return { error: 'Set your Mods folder in Settings first.' };
  const tp = tablesProvider();
  { const b = cssBlock(tp); if (b) return { error: b }; }
  const chk = await csseMatches(e);
  if (!chk.ok) return { error: chk.error ? `Couldn't check your True CSSE: ${chk.error}` : CSSE_MISMATCH };
  const tmp = fs.mkdtempSync(path.join(app.getPath('temp'), 'customcss-'));
  try {
    const zipPath = path.join(tmp, 'Custom_CSS.zip');
    fs.writeFileSync(zipPath, Buffer.from(buf));
    const out = path.join(tmp, 'x');
    const msgs = await runBackend(['gui-extract', zipPath, out]);
    const err = msgs.find((m) => m.type === 'error');
    if (err) return { error: err.msg };
    if (tp.kind === 'combo') {
      // the editor writes True CSSE's tables; move its order onto the combo's table (keeps Separate AI Skins' names/animations)
      const dir = path.join(out, CUSTOM_CSS);
      const utoc = fs.readdirSync(dir).find((f) => /\.utoc$/i.test(f));
      if (!utoc) return { error: 'The CSS editor made no container.' };
      const fixed = path.join(tmp, 'fixed', utoc);
      const extra = (await tableModArgs(null));
      const rm = await runBackend(['gui-css-retarget', ...envArgs(), ...extra, '--custom', path.join(dir, utoc), '--out', fixed]);
      const er = rm.find((m) => m.type === 'error');
      if (er) return { error: er.msg };
      for (const ext of ['utoc', 'ucas']) fs.copyFileSync(fixed.replace(/\.utoc$/i, '.' + ext), path.join(dir, utoc.replace(/\.utoc$/i, '.' + ext)));
    }
    const dest = path.join(e.modsDir, CUSTOM_CSS);
    fs.rmSync(dest, { recursive: true, force: true });
    fs.cpSync(path.join(out, CUSTOM_CSS), dest, { recursive: true });
    const listed = putOnTopInBlueStar(e.gameDir, e.modsDir, CUSTOM_CSS);
    return { ok: true, dest, listed };
  } catch (err) {
    return { error: err.message };
  } finally {
    fs.rmSync(tmp, { recursive: true, force: true });
  }
});

// Turn a mod on and move it to the top of BlueStar's list (top = highest priority).
// BlueStar keys mods by the name of the folder that holds mod.ini.
const CUSTOM_CSS = 'Custom CSS';
function putOnTopInBlueStar(gameDir, modsDir, key) {
  const cands = [bsSettingsFile(gameDir, modsDir)].filter(Boolean);
  for (const c of cands) {
    const top = (list) => [key, ...(list || []).filter((n) => n !== key)];
    return editBlueStar(c, (prof) => { prof.EnabledMods = top(prof.EnabledMods); prof.ModLoadOrder = top(prof.ModLoadOrder); });
  }
  return false;
}

// Read BlueStar's settings.json, change the active profile, and write it back the way BlueStar (.NET) does.
function editBlueStar(c, change) {
  let raw;
  try { raw = fs.readFileSync(c, 'utf8'); } catch { return false; }
  try {
    const bom = raw.charCodeAt(0) === 0xfeff;
    const j = JSON.parse(bom ? raw.slice(1) : raw);
    const prof = (j.Profiles || {})[j.ActiveProfileName || 'Default'];
    if (!prof) return false;
    change(prof);
    if (!fs.existsSync(c + '.bak')) fs.copyFileSync(c, c + '.bak');
    let txt = JSON.stringify(j, null, 2).replace(/[&+'<>`\u007f-\uffff]/g, (ch) => '\\u' + ch.charCodeAt(0).toString(16).toUpperCase().padStart(4, '0'));
    if (raw.includes('\r\n')) txt = txt.replace(/\n/g, '\r\n');
    fs.writeFileSync(c, (bom ? '\ufeff' : '') + txt);
    return true;
  } catch { return false; }
}

// ---- "Separate AI Skins + CSSE Icons": True CSSE's racer list (own icon per AI skin) with Separate AI Skins'
// names and animations. Built from the user's own copies of both mods, with one button in the app.
const COMBO_DIR = 'Separate AI Skins + CSSE Icons';
const COMBO_FILE = 'z_SeparateAI_CSSE_Icons_P';
function comboInputs() {
  const s = loadSettings();
  const modsDir = s.modsDir || (env && env.modsDir), gameDir = s.gameDir || (env && env.gameDir);
  if (!modsDir) return null;
  const bs = bluestarConfigs(gameDir, modsDir);
  const isOn = (m) => bs.enabled.some((n) => n === m.name || path.basename(m.dir) === n);
  let mods = [];
  try { mods = M.findMods(modsDir).map((d) => M.readMod(d)); } catch {}
  const is = (m, re) => re.test(m.name) || re.test(path.basename(m.dir));
  const active = (m) => m && m.utocs.find((x) => !x.disabled);
  const csse = mods.find((m) => isOn(m) && is(m, CSSE_MOD) && !is(m, COMBO_MOD) && active(m));
  const sep = mods.find((m) => isOn(m) && is(m, TABLE_MOD) && !is(m, COMBO_MOD) && active(m));
  const combo = mods.find((m) => is(m, COMBO_MOD));
  return { modsDir, gameDir, bs, csse, sep, combo, csseFile: csse && active(csse).file, sepFile: sep && active(sep).file, comboOn: !!(combo && isOn(combo)) };
}
const fileKey = (f) => { try { const st = fs.statSync(f); return `${st.size}|${Math.round(st.mtimeMs)}`; } catch { return ''; } };
async function comboStatus() {
  const c = comboInputs();
  if (!c || !c.csseFile || !c.sepFile) return { state: 'off' };
  const dir = path.join(c.modsDir, COMBO_DIR);
  if (!fs.existsSync(path.join(dir, COMBO_FILE + '.utoc'))) return { state: 'missing' };
  let info = {};
  try { info = JSON.parse(fs.readFileSync(path.join(dir, 'combo.json'), 'utf8')); } catch {}
  if (!c.comboOn) return { state: 'disabled' };
  // cheap check first (file size/date), then the racer tables themselves
  if (info.csseKey === fileKey(c.csseFile) && info.sepKey === fileKey(c.sepFile)) return { state: 'ok' };
  const msgs = await runBackend(['gui-combo', ...envArgs(), '--csse', c.csseFile, '--separate', c.sepFile, '--check']);
  const done = msgs.find((m) => m.type === 'done');
  if (!done) return { state: 'ok' };
  const same = done.hashes.csse === info.csse && done.hashes.separate === info.separate;
  if (same) {
    info.csseKey = fileKey(c.csseFile); info.sepKey = fileKey(c.sepFile);
    try { fs.writeFileSync(path.join(dir, 'combo.json'), JSON.stringify(info, null, 2)); } catch {}
  }
  return { state: same ? 'ok' : 'stale' };
}
ipcMain.handle('combo:status', () => comboStatus().catch(() => ({ state: 'off' })));
ipcMain.handle('combo:build', async () => {
  const c = comboInputs();
  if (!c || !c.csseFile || !c.sepFile) return { error: 'Turn on both True CSSE and Separate, Ai Skins! in BlueStar first.' };
  const tmp = fs.mkdtempSync(path.join(app.getPath('temp'), 'combo-'));
  try {
    const out = path.join(tmp, COMBO_FILE + '.utoc');
    const msgs = await runBackend(['gui-combo', ...envArgs(), '--csse', c.csseFile, '--separate', c.sepFile, '--out', out]);
    const err = msgs.find((m) => m.type === 'error');
    if (err) return { error: err.msg };
    const done = msgs.find((m) => m.type === 'done') || {};
    const dir = path.join(c.modsDir, COMBO_DIR);
    fs.mkdirSync(dir, { recursive: true });
    for (const ext of ['utoc', 'ucas', 'pak']) fs.copyFileSync(path.join(tmp, `${COMBO_FILE}.${ext}`), path.join(dir, `${COMBO_FILE}.${ext}`));
    fs.writeFileSync(path.join(dir, 'mod.ini'), ['[Main]', 'Name = "Separate AI Skins + CSSE Icons"', 'Version = 1.0', 'Author = "SCWS&CSS"',
      'Description = "True CSSE racer list (every AI skin gets its own icon) with Separate, Ai Skins! names and animations. Made by Crossworld Skin & CSS Manager from your own copies of both mods. Keep it at the top, with Separate, Ai Skins! above True CSSE."', ''].join('\r\n'));
    fs.writeFileSync(path.join(dir, 'combo.json'), JSON.stringify({ ...(done.hashes || {}), csseKey: fileKey(c.csseFile), sepKey: fileKey(c.sepFile), built: new Date().toISOString() }, null, 2));
    // BlueStar: combo on and at the top; Separate AI Skins above True CSSE (its animations/names must win)
    const key = path.basename(dir);
    const sepKey = path.basename(c.sep.dir), csseKey = path.basename(c.csse.dir);
    const settings = bsSettingsFile(c.gameDir, c.modsDir);
    let listed = false;
    if (settings) listed = editBlueStar(settings, (prof) => {
      const fix = (list, all) => {
        let l = (list || []).filter((n) => n !== key);
        const si = l.indexOf(sepKey), ci = l.indexOf(csseKey);
        if (si >= 0 && ci >= 0 && ci < si) { l.splice(ci, 1); l.splice(l.indexOf(sepKey) + 1, 0, csseKey); }
        return [key, ...l];
      };
      prof.EnabledMods = fix(prof.EnabledMods);
      prof.ModLoadOrder = fix(prof.ModLoadOrder);
    });
    // a saved custom order was made for the old table: move it onto the new one, and keep it above
    let css = '';
    const cssDir = path.join(c.modsDir, CUSTOM_CSS);
    const cssUtoc = fs.existsSync(cssDir) && fs.readdirSync(cssDir).find((f) => /\.utoc$/i.test(f));
    if (cssUtoc) {
      const fixed = path.join(tmp, 'css', cssUtoc);
      const rm = await runBackend(['gui-css-retarget', ...envArgs(), '--extra', c.sepFile, '--extra', c.csseFile, '--custom', path.join(cssDir, cssUtoc), '--out', fixed], null, { tables: path.join(dir, COMBO_FILE + '.utoc') });
      if (!rm.find((m) => m.type === 'error')) {
        for (const ext of ['utoc', 'ucas']) fs.copyFileSync(fixed.replace(/\.utoc$/i, '.' + ext), path.join(cssDir, cssUtoc.replace(/\.utoc$/i, '.' + ext)));
        if (c.bs.enabled.includes(CUSTOM_CSS)) putOnTopInBlueStar(c.gameDir, c.modsDir, CUSTOM_CSS);
        css = 'kept';
      } else css = 'failed';
    }
    return { ok: true, msg: done.msg, listed, css };
  } catch (e) {
    return { error: e.message };
  } finally {
    fs.rmSync(tmp, { recursive: true, force: true });
  }
});

const RACER_NAMES = { Eggpawn: 'Egg Pawn', DrEggman: 'Dr. Eggman', MetalSonic: 'Metal Sonic', SuperSonic: 'Super Sonic', SageBlack: 'Black Sage' };
function slotLabel(p) {
  const m = p.match(/\/Asset\/Character\/(?:(Skin)(\d*)\/)?(\w+?)(?:Skin)?\/Mesh\//);
  if (!m) return p;
  const name = RACER_NAMES[m[3]] || m[3].replace(/([a-z])([A-Z])/g, '$1 $2');
  const slot = !m[1] ? 'Regular' : m[2] ? `AI Skin ${+m[2]}` : 'AI Skin';
  return `${name} (${slot})`;
}
ipcMain.handle('thumb', (_e, p) => thumbData(p));
const ARCHIVE = /\.(zip|rar|7z)$/i;
async function importArchive(file) {
  const e = await getEnv();
  if (!e.ok) throw new Error('Set your Mods folder in Settings first.');
  const stem = path.basename(file).replace(ARCHIVE, '').replace(/[<>:"|?*]/g, '').trim();
  const dest = path.join(e.modsDir, stem);
  const hasUtoc = () => M.walk(dest).some((f) => /\.utoc(\.disabled)?$/i.test(f));
  if (!(fs.existsSync(dest) && hasUtoc())) {
    win && win.webContents.send('port:progress', { type: 'status', msg: `Extracting ${path.basename(file)} into your Mods folder…` });
    const msgs = await runBackend(['gui-extract', file, dest], (m) => m.type === 'log' && win && win.webContents.send('port:progress', { type: 'status', msg: 'Extracting ' + path.basename(m.msg) }));
    const err = msgs.find((m) => m.type === 'error');
    if (err) { fs.rmSync(dest, { recursive: true, force: true }); throw new Error(err.msg); }
  }
  // BlueStar only lists folders that have a mod.ini
  if (!M.findMods(dest).length) {
    fs.writeFileSync(path.join(dest, 'mod.ini'), `[Main]\nName = "${stem.replace(/"/g, "'")}"\nDescription = "Imported from ${path.basename(file).replace(/"/g, "'")} by Crossworld Skin & CSS Manager"\nAuthor = "SCWS&CSS"\n`);
  }
  return dest;
}

ipcMain.handle('mod:open', async (_e, dropped) => {
  // accept the mod folder, a parent folder, a sub-folder, a .utoc or a .zip/.rar/.7z archive
  if (ARCHIVE.test(dropped)) {
    try { dropped = await importArchive(dropped); } catch (err) { return { error: `Couldn't extract the archive: ${err.message}` }; }
    win && win.webContents.send('port:progress', { type: 'status', msg: '' });
  }
  let dir = dropped;
  try { if (fs.statSync(dir).isFile()) dir = path.dirname(dir); } catch { return { error: 'That path does not exist.' }; }
  let found = null;
  for (let d = dir, i = 0; i < 4 && d; d = path.dirname(d), i++) { if (fs.existsSync(path.join(d, 'mod.ini'))) { found = d; break; } if (path.dirname(d) === d) break; }
  if (!found) { const sub = M.findMods(dir); found = sub[0] || dir; }
  const m = M.readMod(found);
  if (!m.utocs.length) return { error: "No .utoc files in that folder. Is it a CrossWorlds mod?" };
  const e = await getEnv();
  const bs = bluestarConfigs(e.gameDir, e.modsDir);
  return { ...m, thumbData: thumbData(m.thumb), selection: M.defaultSelection(m, bs.configs), porter: M.porterOptions(m) };
});
// Enabled mods that bring their own racer table, like "Separate, Ai Skins!" (own model, animations and
// name per AI skin). The engine reads the slots from them, above True CSSE, like the game does.
const TABLE_MOD = /separate.*ai.*skin/i;
// every mod that brings its own racer table (and the assets it points to)
const isTableMod = (m) => [TABLE_MOD, CSSE_MOD].some((re) => re.test(m.name) || re.test(path.basename(m.dir)));
async function tableMods(exceptDir) {
  const e = await getEnv();
  if (!e.ok) return [];
  const bs = bluestarConfigs(e.gameDir, e.modsDir);
  const isOn = (m, n) => n === m.name || path.basename(m.dir) === n;
  const rank = (m) => { const i = bs.order.findIndex((n) => isOn(m, n)); return i < 0 ? 1e9 : i; };
  return M.findMods(e.modsDir).map((d) => M.readMod(d))
    .filter((m) => isTableMod(m) && m.dir !== exceptDir && bs.enabled.some((n) => isOn(m, n)))
    .sort((a, b) => rank(a) - rank(b));
}
async function tableModArgs(exceptDir) {
  const out = [];
  for (const m of await tableMods(exceptDir)) for (const u of m.utocs) if (!u.disabled) out.push('--extra', u.file);
  return out;
}

ipcMain.handle('mod:inspect', async (_e, dir, selection) => {
  const m = M.readMod(dir);
  const utocs = M.activeUtocs(m, selection);
  if (!utocs.length) return { error: 'These options load no model files (everything set to None?).' };
  const extra = await tableModArgs(m.dir);
  const msgs = await runBackend(['gui-inspect', ...envArgs(), ...extra, ...utocs]);
  const r = msgs.find((x) => x.type === 'inspect');
  if (!r) return { error: (msgs.find((x) => x.type === 'error') || {}).msg || 'Unknown error' };
  // the in-game name this mod gives its racer (BlueStar text files), if any
  const src0 = (r.data.sources || [])[0];
  const nm = modNameText(m, src0 && src0.name_key);
  return { ...r.data, utocs: utocs.map((u) => path.relative(dir, u)), gameName: nm.value || null };
});

// BlueStar text files (*.json with Language/Namespace/Key/Value) that are switched on in this mod
const NAME_LANGS = ['de', 'en', 'es', 'es-US', 'fr', 'it', 'jp', 'ko', 'pt', 'ru', 'th', 'zh-Hans', 'zh-Hant'];
function modNameText(m, key) {
  const out = { value: null, langs: null };
  if (!key) return out;
  const inPorter = (f) => path.relative(m.dir, f).split(path.sep)[0] === M.PORTER_GROUP;
  for (const f of M.walk(m.dir).filter((x) => /\.json$/i.test(x) && !inPorter(x))) {
    let j;
    try { j = JSON.parse(fs.readFileSync(f, 'utf8').replace(/^\ufeff/, '')); } catch { continue; }
    if (!Array.isArray(j)) continue;
    const mine = j.filter((e) => e && e.Key === key && e.Namespace === 'DB_CharacterName');
    if (!mine.length) continue;
    out.langs = mine.map((e) => e.Language);
    out.value = (mine.find((e) => e.Language === 'en') || mine[0]).Value;
    return out;
  }
  return out;
}
ipcMain.handle('mod:port', async (_e, req) => {
  const m = M.readMod(req.dir);
  const utocs = M.activeUtocs(m, req.selection);
  const extraArgs = await tableModArgs(m.dir);
  const args = ['gui-port', ...envArgs(), ...extraArgs, '--src', String(req.src), '--tgt', String(req.tgt), '--name', req.containerName || m.name, '--author', m.author || ''];
  if (!req.copyIcons) args.push('--no-icons');
  let rel = null;
  if (req.mode === 'option') {
    const folder = req.optionLabel.replace(/[<>:"/\\|?*,]/g, '').replace(/\s+/g, ' ').trim();
    const into = path.join(req.dir, M.PORTER_GROUP, folder);
    fs.rmSync(into, { recursive: true, force: true });
    args.push('--into-dir', into);
    rel = path.join(M.PORTER_GROUP, folder);
  } else {
    args.push('--out-name', req.outName);
    if (m.thumb) args.push('--thumb', m.thumb);
  }
  const msgs = await runBackend([...args, ...utocs], (msg) => win && win.webContents.send('port:progress', msg));
  const done = msgs.find((x) => x.type === 'done');
  if (!done) return { error: (msgs.find((x) => x.type === 'error') || {}).msg || 'Unknown error' };
  const say = (msg) => win && win.webContents.send('port:progress', { type: 'log', msg });

  // Regular and AI racers use different voice files and name keys: bring those along too.
  const extras = [];
  const isReg = (l) => /^Regular/i.test(l || '');
  const srcReg = isReg(req.srcLabel), tgtReg = isReg(req.tgtLabel);
  done.voice = null; done.renamed = false;
  // Moving turns the original off, so its voice must come along even between AI slots (copied as-is).
  if (req.srcLabel && req.tgtLabel && (srcReg !== tgtReg || req.mode === 'move')) {
    const inPorter = (f) => path.relative(m.dir, f).split(path.sep)[0] === M.PORTER_GROUP;
    const all = M.walk(m.dir).filter((f) => !inPorter(f));
    const paks = all.filter((f) => /\.pak$/i.test(f) && !fs.existsSync(f.replace(/\.pak$/i, '.utoc')));
    if (paks.length) {
      const voiceName = `z_${(req.containerName || m.name).replace(/[^A-Za-z0-9]+/g, '_')}_Voice_P.pak`;
      const out = path.join(done.out_dir, voiceName);
      const dir = srcReg === tgtReg ? [] : tgtReg ? ['--to-regular'] : ['--to-ai'];
      const ge = await getEnv();
      const slot = !tgtReg && ge.ok && voicesStatus(ge.gameDir).on ? slotVoiceCode(req.tgtMesh) : null;
      let vm = null, vd = null;
      if (slot) {
        vm = await runBackend(['gui-voice', '--to-slot', slot, '--out', out, ...paks]);
        vd = vm.find((x) => x.type === 'done');
        if (vd && vd.written.length) { done.slotVoice = slot; say(`Own voice for this slot (separate voices): VOICE_${slot}`); }
        else if (vd) vd.notes.forEach(say);
      }
      if (!vd || !vd.written.length) {
        vm = await runBackend(['gui-voice', ...dir, '--out', out, ...paks]);
        vd = vm.find((x) => x.type === 'done');
      }
      if (vd && vd.written.length) { done.voice = true; extras.push(voiceName.replace(/\.pak$/i, '')); say('Voice: ' + vd.written.map((w) => w.split('/').pop()).join(', ')); }
      if (vd) vd.notes.forEach(say);
      if (!vd) say('Voice not converted: ' + ((vm.find((x) => x.type === 'error') || {}).msg || 'unknown error'));
    }
  }

  // In-game name for a slot with its own name table (Separate AI Skins): write that table, like Tikal's mod does
  if (req.tgtNameKey && req.tgtNameKey.startsWith('table:') && req.gameName && req.gameName.trim()) {
    const table = req.tgtNameKey.slice('table:'.length);
    const nameFile = `z_${(req.containerName || m.name).replace(/[^A-Za-z0-9]+/g, '_')}_Name_P`;
    const sources = extraArgs.filter((x) => x !== '--extra');
    const nm = await runBackend(['gui-name-table', ...envArgs(), '--table', table, '--value', req.gameName.trim(), '--out', path.join(done.out_dir, nameFile + '.utoc'), ...sources]);
    const ok = nm.find((x) => x.type === 'done');
    if (ok) { extras.push(nameFile); done.renamed = true; say(`Name in game: ${req.gameName.trim()} (own name for this slot)`); }
    else say('Name not set: ' + ((nm.find((x) => x.type === 'error') || {}).msg || 'unknown error'));
  } else
  // In-game name for the new slot (BlueStar text file), when the mod has one or a new name was typed
  if (req.tgtNameKey && req.gameName && req.gameName.trim()) {
    const cur = modNameText(m, req.srcNameKey);
    if (cur.value || req.gameName.trim() !== (req.defaultName || '')) {
      const langs = cur.langs && cur.langs.length ? cur.langs : NAME_LANGS;
      const j = langs.map((l) => ({ Language: l, Namespace: 'DB_CharacterName', Key: req.tgtNameKey, Value: req.gameName.trim() }));
      const nameFile = `${(req.containerName || m.name).replace(/[^A-Za-z0-9]+/g, '_')}_Name`;
      const dest = path.join(done.out_dir, 'Bluestar Text', nameFile + '.json');
      fs.mkdirSync(path.dirname(dest), { recursive: true });
      fs.writeFileSync(dest, JSON.stringify(j, null, 4));
      extras.push(path.join('Bluestar Text', nameFile));
      done.renamed = true;
      say(`Name in game: ${req.gameName.trim()}`);
    }
  }

  if (req.mode === 'option') {
    const utoc = fs.readdirSync(done.out_dir).find((f) => f.endsWith('.utoc'));
    M.addPorterOption(req.dir, req.optionLabel, [path.join(rel, utoc.replace(/\.utoc$/, '')), ...extras.map((x) => path.join(rel, x))]);
  }
  if (req.mode === 'move') {
    const e = await getEnv();
    done.moved = moveInBlueStar(e.gameDir, path.basename(m.dir), path.basename(done.out_dir));
  }
  return done;
});

// Move: turn the original mod off and the new one on, in the original's place in the load order.
function moveInBlueStar(gameDir, oldKey, newKey) {
  const c = bsSettingsFile(gameDir);
  if (!c) return false;
  return editBlueStar(c, (prof) => {
    const en = prof.EnabledMods || [];
    const wasOn = en.includes(oldKey);
    prof.EnabledMods = en.filter((n) => n !== oldKey && n !== newKey);
    let order = (prof.ModLoadOrder || []).filter((n) => n !== newKey);
    let at = order.indexOf(oldKey);
    if (at < 0) at = 0;
    // True CSSE ships its own AI skin models, so a skin mod only shows if it loads above it
    // (Separate AI Skins ships them too, so stay above it as well; and above Stamp Foundation, whose placeholder stamps would hide the mod's own)
    const base = order.findIndex((n) => (/True CSS|stamps? foundation/i.test(n) || TABLE_MOD.test(n)) && !/csse icons|^custom css$/i.test(n));
    if (base >= 0 && at > base) at = base;
    order.splice(at, 0, newKey);
    prof.ModLoadOrder = order;
    // keep EnabledMods in the same order as the load order
    prof.EnabledMods.push(newKey);
    prof.EnabledMods.sort((a, b) => order.indexOf(a) - order.indexOf(b));
  });
}
ipcMain.handle('mod:removeOption', async (_e, dir, label) => M.removePorterOption(dir, label));
ipcMain.handle('show', (_e, p) => shell.showItemInFolder(p));
ipcMain.handle('openUrl', (_e, u) => /^https:\/\//.test(u) && shell.openExternal(u));

function create() {
  win = new BrowserWindow({
    width: 1240, height: 860, minWidth: 900, minHeight: 600,
    title: 'Crossworld Skin & CSS Manager', backgroundColor: '#0d3fb8', autoHideMenuBar: true,
    icon: path.join(__dirname, 'icon.png'),
    webPreferences: { contextIsolation: true, sandbox: false, preload: path.join(__dirname, 'preload.js') },
  });
  Menu.setApplicationMenu(null);
  win.loadFile(path.join(__dirname, 'index.html'));
  win.webContents.setWindowOpenHandler(() => ({ action: 'deny' }));
  // a mod folder dragged onto the exe
  const arg = process.argv.slice(1).find((a) => a && !a.startsWith('-') && a !== '.' && fs.existsSync(a) && !a.endsWith('.asar') && path.resolve(a) !== path.resolve(__dirname));
  if (arg) win.webContents.once('did-finish-load', () => win.webContents.send('open-path', arg));
}
app.whenReady().then(create);
app.on('window-all-closed', () => app.quit());

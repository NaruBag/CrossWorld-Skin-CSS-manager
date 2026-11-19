// mod.ini helpers shared by Crossworld Skin & CSS Manager (BlueStar Manager format)
const fs = require('fs');
const path = require('path');

const PORTER_GROUP = 'Extra Slots';
const OLD_GROUP = ['AI', 'Skin', 'Porter'].join(' '); // name used by early test builds, renamed on read
const VERSION_GROUP = 'Version';

function unquote(v) {
  v = (v || '').trim();
  if (v.length >= 2 && v.startsWith('"') && v.endsWith('"')) v = v.slice(1, -1);
  return v;
}

function parseIni(text) {
  const sections = [];
  let cur = { name: '', entries: [] };
  sections.push(cur);
  for (const raw of text.split(/\r?\n/)) {
    const line = raw.trim();
    if (!line || line.startsWith(';') || line.startsWith('#')) continue;
    const m = line.match(/^\[(.+)\]$/);
    if (m) { cur = { name: m[1].trim(), entries: [] }; sections.push(cur); continue; }
    const i = line.indexOf('=');
    if (i < 0) continue;
    cur.entries.push([line.slice(0, i).trim(), unquote(line.slice(i + 1))]);
  }
  return sections;
}

const norm = (p) => p.replace(/\\/g, '/').replace(/^\.\//, '').toLowerCase();

function walk(dir, depth = 0, out = []) {
  if (depth > 6) return out;
  let ents = [];
  try { ents = fs.readdirSync(dir, { withFileTypes: true }); } catch { return out; }
  for (const e of ents) {
    const p = path.join(dir, e.name);
    if (e.isDirectory()) walk(p, depth + 1, out); else out.push(p);
  }
  return out;
}

function findThumb(dir) {
  for (const d of [dir, path.dirname(dir)]) {
    for (const n of ['Thumb.png', 'Thumb.jpg', 'Thumb.jpeg', 'thumb.png', 'thumb.jpg', 'Thumb.webp']) {
      const p = path.join(d, n);
      if (fs.existsSync(p)) return p;
    }
  }
  return null;
}

// Read a mod folder (folder that contains mod.ini, or any folder with .utoc files)
// Mods changed by early test builds used another group name: rename it (ini + folder) once.
function migrateOldGroup(dir) {
  const iniPath = path.join(dir, 'mod.ini');
  let text;
  try { text = fs.readFileSync(iniPath, 'utf8'); } catch { return; }
  if (!text.includes(`[Config:${OLD_GROUP}]`)) return;
  const esc = OLD_GROUP.replace(/ /g, '\\s');
  text = text.split(`[Config:${OLD_GROUP}]`).join(`[Config:${PORTER_GROUP}]`)
    .replace(new RegExp(`"\\s*${esc}\\.`, 'g'), `"${PORTER_GROUP}.`)
    .replace(new RegExp(`^(\\s*)${esc}([/\\\\])`, 'gm'), `$1${PORTER_GROUP}$2`)
    .replace(/Extra copies made with [^.]*\./, 'Extra copies made with Crossworld Skin & CSS Manager.');
  try { if (fs.existsSync(path.join(dir, OLD_GROUP)) && !fs.existsSync(path.join(dir, PORTER_GROUP))) fs.renameSync(path.join(dir, OLD_GROUP), path.join(dir, PORTER_GROUP)); } catch { return; }
  fs.writeFileSync(iniPath, text);
}

function readMod(dir) {
  migrateOldGroup(dir);
  const iniPath = path.join(dir, 'mod.ini');
  const text = fs.existsSync(iniPath) ? fs.readFileSync(iniPath, 'utf8') : '';
  const secs = parseIni(text);
  const get = (sec, key) => {
    const s = secs.find((x) => x.name.toLowerCase() === sec.toLowerCase());
    const e = s && s.entries.find(([k]) => k.toLowerCase() === key.toLowerCase());
    return e ? e[1] : '';
  };
  const configs = secs
    .filter((s) => /^config:/i.test(s.name))
    .map((s) => {
      const name = s.name.slice(7).trim();
      const g = (k) => (s.entries.find(([kk]) => kk.toLowerCase() === k) || [, ''])[1];
      return { name, type: g('type') || 'SelectOne', description: g('description'), options: g('options').split(',').map((o) => o.trim()).filter(Boolean) };
    });
  const filesSec = secs.find((s) => s.name.toLowerCase() === 'files');
  const files = (filesSec ? filesSec.entries : []).map(([p, cond]) => {
    // "Group.Option": group names may contain dots themselves ("Truest Mephiles 1. Model.Glowing"), so match a known group first
    const known = configs.map((c) => c.name).filter((n) => cond.startsWith(n + '.')).sort((a, b) => b.length - a.length)[0];
    const dot = known ? known.length : cond.indexOf('.');
    return { path: p, key: norm(p).replace(/\.(utoc|ucas|pak)(\.disabled)?$/i, ''), group: dot >= 0 ? cond.slice(0, dot) : cond, option: dot >= 0 ? cond.slice(dot + 1) : '' };
  });
  const utocs = walk(dir)
    .filter((f) => /\.utoc(\.disabled)?$/i.test(f))
    .map((f) => {
      const disabled = /\.disabled$/i.test(f);
      const rel = path.relative(dir, f).replace(/\\/g, '/').replace(/\.utoc(\.disabled)?$/i, '');
      return { file: f, rel, key: rel.toLowerCase(), disabled };
    })
    .sort((a, b) => a.rel.localeCompare(b.rel));
  // Mods without options that ship several versions in separate folders (e.g. "over AI Amy" / "over Amy",
  // or one folder per color): show the folders as a choice, like a Configure group.
  const realGroups = configs.filter((c) => c.name !== PORTER_GROUP);
  if (!realGroups.length) {
    const own = utocs.filter((u) => !u.key.startsWith(norm(PORTER_GROUP) + '/'));
    const dirs = [...new Set(own.map((u) => (u.rel.includes('/') ? u.rel.slice(0, u.rel.lastIndexOf('/')) : '.')))];
    if (dirs.length >= 2) {
      const label = (d) => (d === '.' ? '(main folder)' : d.split('/').pop());
      const opts = dirs.map(label);
      const uniq = opts.length === new Set(opts).size ? opts : dirs;
      configs.unshift({ name: VERSION_GROUP, type: 'SelectOne', synthetic: true, description: 'This mod has no options, but it comes in several folders. Pick the one you want to port.', options: uniq });
      own.forEach((u) => {
        const d = u.rel.includes('/') ? u.rel.slice(0, u.rel.lastIndexOf('/')) : '.';
        files.push({ path: u.rel, key: u.key, group: VERSION_GROUP, option: uniq[dirs.indexOf(d)], synthetic: true });
      });
    }
  }
  return {
    dir, iniPath: fs.existsSync(iniPath) ? iniPath : null,
    name: get('Main', 'Name') || path.basename(dir),
    author: get('Main', 'Author'), version: get('Main', 'Version'), description: get('Main', 'Description'),
    thumb: findThumb(dir), configs, files, utocs,
  };
}

// Which options are currently chosen (BlueStar saved config, else what isn't .disabled, else first option)
function defaultSelection(mod, bluestarConfigs) {
  const sel = {};
  for (const c of mod.configs) {
    if (c.synthetic) { const on = mod.utocs.find((u) => !u.disabled && mod.files.some((f) => f.synthetic && f.key === u.key)); const f = on && mod.files.find((x) => x.synthetic && x.key === on.key); sel[c.name] = f ? f.option : c.options[0]; continue; }
    const saved = bluestarConfigs && bluestarConfigs[`${mod.name}:${c.name}`];
    if (saved && c.options.includes(saved)) { sel[c.name] = saved; continue; }
    const active = mod.files.find((f) => f.group === c.name && mod.utocs.some((u) => u.key === f.key && !u.disabled));
    sel[c.name] = active && c.options.includes(active.option) ? active.option : c.options[0];
  }
  return sel;
}

// .utoc files that would be loaded for a selection (porter copies are left out)
function activeUtocs(mod, sel) {
  const out = [];
  for (const u of mod.utocs) {
    const rule = mod.files.find((f) => f.key === u.key);
    if (rule) {
      if (rule.group === PORTER_GROUP) continue;
      if (sel[rule.group] === rule.option) out.push(u.file);
    } else if (!u.disabled && !u.key.startsWith(norm(PORTER_GROUP) + '/')) {
      out.push(u.file);
    }
  }
  return out;
}

function porterOptions(mod) {
  const c = mod.configs.find((x) => x.name === PORTER_GROUP);
  if (!c) return [];
  return c.options.filter((o) => o !== 'None').map((o) => ({ label: o, files: mod.files.filter((f) => f.group === PORTER_GROUP && f.option === o).map((f) => f.path) }));
}

// ---- writing mod.ini (line based, keeps the author's formatting) ----
function splitSections(lines) {
  const res = [];
  let cur = { header: null, start: 0 };
  lines.forEach((l, i) => {
    const m = l.trim().match(/^\[(.+)\]$/);
    if (m) { cur.end = i; res.push(cur); cur = { header: m[1].trim(), start: i }; }
  });
  cur.end = lines.length; res.push(cur);
  return res;
}

function addPorterOption(modDir, label, relNoExts) {
  if (!Array.isArray(relNoExts)) relNoExts = [relNoExts];
  const iniPath = path.join(modDir, 'mod.ini');
  let text = fs.existsSync(iniPath) ? fs.readFileSync(iniPath, 'utf8') : `[Main]\nName = "${path.basename(modDir)}"\n`;
  if (fs.existsSync(iniPath) && !fs.existsSync(iniPath + '.bak')) fs.copyFileSync(iniPath, iniPath + '.bak');
  const eol = text.includes('\r\n') ? '\r\n' : '\n';
  let lines = text.split(/\r?\n/);
  while (lines.length && lines[lines.length - 1].trim() === '') lines.pop();
  let secs = splitSections(lines);
  let cfg = secs.find((s) => s.header === `Config:${PORTER_GROUP}`);
  if (cfg) {
    for (let i = cfg.start + 1; i < cfg.end; i++) {
      const m = lines[i].match(/^\s*Options\s*=\s*(.*)$/i);
      if (m) {
        const opts = unquote(m[1]).split(',').map((o) => o.trim()).filter(Boolean);
        if (!opts.includes(label)) opts.push(label);
        lines[i] = `Options = "${opts.join(', ')}"`;
      }
    }
  } else {
    const block = ['', `[Config:${PORTER_GROUP}]`, 'Type = SelectOne',
      'Description = "Extra copies made with Crossworld Skin & CSS Manager. Pick one to ALSO use this mod on another AI slot (load this mod above True CSSE)."',
      `Options = "None, ${label}"`];
    const files = secs.find((s) => s.header && s.header.toLowerCase() === 'files');
    const at = files ? files.start : lines.length;
    lines.splice(at, 0, ...block, ...(files ? [''] : []));
  }
  secs = splitSections(lines);
  const entries = relNoExts.map((r) => `${r.replace(/\\/g, '/')} = "${PORTER_GROUP}.${label}"`);
  const files = secs.find((s) => s.header && s.header.toLowerCase() === 'files');
  if (files) {
    let end = files.end;
    while (end > files.start + 1 && lines[end - 1].trim() === '') end--;
    lines.splice(end, 0, ...entries);
  } else {
    lines.push('', '[Files]', ...entries);
  }
  fs.writeFileSync(iniPath, lines.join(eol) + eol);
}

function removePorterOption(modDir, label) {
  const iniPath = path.join(modDir, 'mod.ini');
  const text = fs.readFileSync(iniPath, 'utf8');
  const eol = text.includes('\r\n') ? '\r\n' : '\n';
  let lines = text.split(/\r?\n/);
  const removedPaths = [];
  const secs = splitSections(lines);
  const cfg = secs.find((s) => s.header === `Config:${PORTER_GROUP}`);
  let left = [];
  if (cfg) {
    for (let i = cfg.start + 1; i < cfg.end; i++) {
      const m = lines[i].match(/^\s*Options\s*=\s*(.*)$/i);
      if (m) { left = unquote(m[1]).split(',').map((o) => o.trim()).filter((o) => o && o !== label); lines[i] = `Options = "${left.join(', ')}"`; }
    }
  }
  lines = lines.filter((l) => {
    const m = l.match(/^\s*(.+?)\s*=\s*"?\s*Extra Slots\.(.+?)\s*"?\s*$/);
    if (m && m[2] === label) { removedPaths.push(m[1]); return false; }
    return true;
  });
  // drop the whole group when only "None" is left
  if (cfg && left.filter((o) => o !== 'None').length === 0) {
    const s2 = splitSections(lines).find((s) => s.header === `Config:${PORTER_GROUP}`);
    if (s2) lines.splice(s2.start, s2.end - s2.start);
  }
  fs.writeFileSync(iniPath, lines.join(eol).replace(/(\r?\n){3,}/g, eol + eol));
  for (const p of removedPaths) {
    const base = path.join(modDir, p);
    for (const ext of ['.utoc', '.ucas', '.pak', '.json', '.utoc.disabled', '.ucas.disabled', '.pak.disabled', '.json.disabled']) { try { fs.rmSync(base + ext); } catch {} }
  }
  // remove the option's own folder (Extra Slots/<option>) with anything left in it
  const parts = removedPaths.length ? removedPaths[0].replace(/\\/g, '/').split('/') : [];
  if (parts[0] === PORTER_GROUP && parts.length > 2) { try { fs.rmSync(path.join(modDir, parts[0], parts[1]), { recursive: true, force: true }); } catch {} }
  try { const d = path.join(modDir, PORTER_GROUP); if (fs.readdirSync(d).length === 0) fs.rmdirSync(d); } catch {}
  return removedPaths;
}

function findMods(modsDir) {
  const res = [];
  const seen = new Set();
  const visit = (dir, depth) => {
    if (depth > 3) return;
    let ents = [];
    try { ents = fs.readdirSync(dir, { withFileTypes: true }); } catch { return; }
    if (ents.some((e) => e.isFile() && e.name.toLowerCase() === 'mod.ini')) { if (!seen.has(dir)) { seen.add(dir); res.push(dir); } return; }
    // like BlueStar: a folder in Mods with mod files but no mod.ini is a mod too (named after the folder)
    if (depth === 1 && ents.some((e) => e.isFile() && /\.(utoc|pak)(\.disabled)?$/i.test(e.name))) { if (!seen.has(dir)) { seen.add(dir); res.push(dir); } return; }
    for (const e of ents) if (e.isDirectory()) visit(path.join(dir, e.name), depth + 1);
  };
  visit(modsDir, 0);
  return res;
}

module.exports = { PORTER_GROUP, VERSION_GROUP, parseIni, readMod, defaultSelection, activeUtocs, porterOptions, addPorterOption, removePorterOption, findMods, walk };

//! Crossworld Skin & CSS Manager engine for Sonic Racing CrossWorlds (+ True CSSE).
//! Copies a racer mod onto another skin slot (AI Skin, AI Skin 1-3 or Regular) so two mods can coexist.

use anyhow::{Context, Result, bail};
use retoc::container_header::EIoContainerHeaderVersion;
use retoc::iostore::{IoStoreBackend, IoStoreContainer, IoStoreTrait};
use retoc::legacy_asset::{FLegacyPackageHeader, FPackageNameMap};
use retoc::logging::Log;
use retoc::version::EngineVersion;
use retoc::zen::FZenPackageHeader;
use retoc::{AesKey, Config, EIoChunkType, EIoStoreTocVersion, FGuid, FIoChunkId, FPackageId, FSFileWriter, UEPath};
use std::collections::{BTreeSet, BTreeMap, HashMap, HashSet, VecDeque};
use std::io::{BufRead, Cursor, Write};
use std::path::{Path, PathBuf};
use std::str::FromStr;
use std::sync::Arc;

pub const DEFAULT_AES: &str = "0x1B5E5FF1646B6509B0EF29FF3890A95AFB91BF131DD335146FC237D667D5E798";
const GAME_FOLDER: &str = "SonicRacingCrossWorlds";
const CLONE_CLASSES: &[&str] = &["MaterialInstanceConstant", "Texture2D", "TextureCube", "Texture2DArray"];

pub struct PortOptions {
    pub input: PathBuf,
    pub game_dir: Option<PathBuf>,
    pub mods_dir: Option<PathBuf>,
    pub aes: Option<String>,
    /// non-interactive answers (for scripting/tests)
    pub container_choice: Option<usize>,
    pub source_choice: Option<usize>,
    pub target_choice: Option<usize>,
    pub interactive: bool,
}

#[derive(Debug, Clone, Default)]
pub struct DriverRow {
    id: String,
    mesh: String,
    icon: Option<String>,
    hud: Option<String>,
    cut: Option<String>,
    da: String,
    /// number from the row's voice database (DB_CV004_amy -> 4); names use chara0NN / chara1NN (AI)
    cv: Option<u32>,
    /// the row's name comes from its own string table package instead of DB_CharacterName (Separate AI Skins)
    name_table: Option<String>,
}
impl DriverRow {
    /// BlueStar / localization key of the racer's display name (DB_CharacterName)
    fn name_key(&self) -> Option<String> {
        if let Some(t) = &self.name_table {
            return Some(format!("table:{t}"));
        }
        let n = self.cv?;
        Some(format!("chara{:03}", if self.id.contains("Racer") { 100 + n } else { n }))
    }
    fn root(&self) -> String {
        match self.mesh.find("/Mesh/") {
            Some(p) => self.mesh[..p + 1].to_string(),
            None => format!("{}/", self.mesh.rsplit_once('/').unwrap().0),
        }
    }
    fn base(&self) -> String {
        let b = basename(&self.mesh).trim_start_matches("SK_").to_string();
        let b = b.strip_suffix("Skin").unwrap_or(&b).to_string();
        b.strip_suffix("side").unwrap_or(&b).to_lowercase()
    }
    fn slot_label(&self) -> String {
        let color = self.icon.as_deref().and_then(|i| basename(i).rsplit_once("AI_").map(|x| x.1.to_string()));
        let slot = if let Some(n) = slot_number(&self.mesh) {
            format!("AI Skin {n}")
        } else if self.mesh.contains("/Character/Skin/") {
            "AI Skin".to_string()
        } else {
            "Regular".to_string()
        };
        match color {
            Some(c) => format!("{slot} ({c})"),
            None => slot,
        }
    }
}

fn slot_number(path: &str) -> Option<u32> {
    let i = path.find("/Character/Skin0")?;
    path[i + 16..].chars().next()?.to_digit(10)
}
fn basename(p: &str) -> &str {
    p.rsplit('/').next().unwrap_or(p)
}
fn pid(name: &str) -> FPackageId {
    FPackageId::from_name(name)
}
fn pkg_chunk(name: &str) -> FIoChunkId {
    FIoChunkId::from_package_id(pid(name), 0, EIoChunkType::ExportBundleData)
}

fn ask(prompt: &str) -> String {
    print!("{prompt}");
    std::io::stdout().flush().ok();
    let mut s = String::new();
    std::io::stdin().lock().read_line(&mut s).ok();
    s.trim().trim_matches('"').to_string()
}
fn choose(title: &str, items: &[String], preset: Option<usize>, interactive: bool) -> Result<usize> {
    if items.len() == 1 && preset.is_none() {
        println!("{title}: {}", items[0]);
        return Ok(0);
    }
    println!("\n{title}");
    for (i, it) in items.iter().enumerate() {
        println!("  [{}] {}", i + 1, it);
    }
    if let Some(p) = preset {
        println!("  -> {}", items.get(p).context("choice out of range")?);
        return Ok(p);
    }
    if !interactive {
        bail!("a choice is required for: {title}");
    }
    loop {
        let a = ask("Type a number and press Enter: ");
        if let Ok(n) = a.parse::<usize>()
            && n >= 1
            && n <= items.len()
        {
            return Ok(n - 1);
        }
    }
}

// ---------------------------------------------------------------- settings discovery

fn exe_dir() -> PathBuf {
    std::env::current_exe().ok().and_then(|p| p.parent().map(Path::to_path_buf)).unwrap_or_else(|| PathBuf::from("."))
}
fn ini_path() -> PathBuf {
    exe_dir().join("SkinCSSEngine.ini")
}
fn read_ini() -> HashMap<String, String> {
    std::fs::read_to_string(ini_path())
        .unwrap_or_default()
        .lines()
        .filter_map(|l| l.split_once('=').map(|(k, v)| (k.trim().to_string(), v.trim().to_string())))
        .collect()
}
fn write_ini(map: &HashMap<String, String>) {
    let s: String = map.iter().map(|(k, v)| format!("{k}={v}\n")).collect();
    std::fs::write(ini_path(), s).ok();
}
fn is_game_dir(p: &Path) -> bool {
    p.join("UNION/Content/Paks").is_dir()
}
fn find_game_dir() -> Option<PathBuf> {
    let mut libs: Vec<PathBuf> = vec![];
    for steam in ["C:/Program Files (x86)/Steam", "C:/Program Files/Steam"] {
        libs.push(PathBuf::from(steam));
        if let Ok(vdf) = std::fs::read_to_string(format!("{steam}/steamapps/libraryfolders.vdf")) {
            for line in vdf.lines() {
                let t = line.trim();
                if let Some(rest) = t.strip_prefix("\"path\"") {
                    let v = rest.trim().trim_matches('"').replace("\\\\", "/");
                    libs.push(PathBuf::from(v));
                }
            }
        }
    }
    for d in b'C'..=b'Z' {
        let d = d as char;
        for sub in ["Program Files (x86)/Steam", "Program Files/Steam", "SteamLibrary", "Steam", "Games/Steam", "Games/SteamLibrary"] {
            libs.push(PathBuf::from(format!("{d}:/{sub}")));
        }
    }
    libs.into_iter().map(|l| l.join("steamapps/common").join(GAME_FOLDER)).find(|p| is_game_dir(p))
}
fn find_aes(game_dir: &Path) -> Option<String> {
    for p in [game_dir.join("BlueStarManager/settings.json")] {
        if let Ok(s) = std::fs::read_to_string(&p)
            && let Ok(v) = serde_json::from_str::<serde_json::Value>(&s)
            && let Some(k) = v.get("GameAesKey").and_then(|k| k.as_str())
        {
            return Some(k.to_string());
        }
    }
    None
}
fn mods_dir_from_settings(game_dir: &Path) -> Option<PathBuf> {
    let s = std::fs::read_to_string(game_dir.join("BlueStarManager/settings.json")).ok()?;
    let v: serde_json::Value = serde_json::from_str(&s).ok()?;
    Some(PathBuf::from(v.get("ModsDirectory")?.as_str()?))
}

// ---------------------------------------------------------------- zen helpers

fn zen_header(data: &[u8]) -> Result<FZenPackageHeader> {
    FZenPackageHeader::deserialize(&mut Cursor::new(data), None, EIoStoreTocVersion::OnDemandMetaData, EIoContainerHeaderVersion::NoExportInfo, Some(EngineVersion::UE5_4.package_file_version()))
}
fn zen_package_name(data: &[u8]) -> Option<String> {
    let h = zen_header(data).ok()?;
    Some(h.name_map.get(h.summary.name).to_string())
}

/// Read True CSSE / game driver data tables and recover each row's mesh + icons.
fn read_driver_rows(store: &dyn IoStoreTrait) -> Vec<DriverRow> {
    let mut rows = vec![];
    for n in 1..=9 {
        let name = format!("/Game/0{n}_Union/Database/Driver/DT_DriverData0{n}");
        let dbg = std::env::var("PORT_DEBUG").is_ok();
        let data = match store.read(pkg_chunk(&name)) { Ok(d) => d, Err(e) => { if dbg { eprintln!("{name}: read err {e}"); } continue } };
        let h = match zen_header(&data) { Ok(h) => h, Err(e) => { if dbg { eprintln!("{name}: hdr err {e}"); } continue } };
        let before = rows.len();
        let names: Vec<String> = h.name_map.copy_raw_names();
        let body = &data[h.summary.header_size as usize..];
        let name_at = |p: usize| -> Option<&str> {
            if p + 8 > body.len() {
                return None;
            }
            let i = u32::from_le_bytes(body[p..p + 4].try_into().unwrap()) as usize;
            let num = u32::from_le_bytes(body[p + 4..p + 8].try_into().unwrap());
            if num == 0 && i > 0 && i < names.len() { Some(names[i].as_str()) } else { None }
        };
        let mut seq: Vec<&str> = vec![];
        for p in 0..body.len().saturating_sub(8) {
            let Some(s) = name_at(p) else { continue };
            if s.starts_with("EDriverId::") || s.starts_with("DA_Driver") {
                seq.push(s);
            } else if s.starts_with("/Game/SeparateAi/") {
                // string table reference (table name, then the key as an FString): Separate AI Skins' own names
                let key = basename(s).as_bytes();
                let is_key = |q: usize| {
                    q + 4 + key.len() <= body.len()
                        && i32::from_le_bytes(body[q..q + 4].try_into().unwrap()) == key.len() as i32 + 1
                        && &body[q + 4..q + 4 + key.len()] == key
                };
                if is_key(p + 8) || is_key(p + 9) {
                    seq.push(s);
                }
            } else if s.starts_with("/Game/") {
                // a real soft object path is (package name, asset name); the asset name follows 8 bytes later
                let b = basename(s);
                if let Some(a) = name_at(p + 8)
                    && (a == b || a.strip_suffix("_C") == Some(b))
                {
                    seq.push(s);
                }
            }
        }
        // Some regular racers (e.g. Amy) store their mesh in a way the pair scan misses: fall back to the
        // standard /Asset/Character/<Id>/Mesh/SK_<Id> path when the table references it.
        let fill = |r: &mut DriverRow| {
            if r.mesh.is_empty() && !r.id.contains("Racer") {
                let want = format!("/Asset/Character/{0}/Mesh/SK_{0}", r.id);
                if let Some(m) = names.iter().find(|x| x.starts_with("/Game/") && x.ends_with(&want)) {
                    r.mesh = m.clone();
                }
            }
        };
        let mut cur: Option<DriverRow> = None;
        for s in seq {
            if let Some(id) = s.strip_prefix("EDriverId::") {
                if let Some(mut r) = cur.take() {
                    fill(&mut r);
                    if !r.mesh.is_empty() {
                        rows.push(r);
                    }
                }
                cur = Some(DriverRow { id: id.to_string(), ..Default::default() });
            } else if let Some(r) = cur.as_mut() {
                if s.starts_with("DA_Driver") {
                    r.da = s.to_string();
                } else if r.mesh.is_empty() && s.contains("/Asset/Character/") && s.contains("/Mesh/SK_") {
                    r.mesh = s.to_string();
                } else if r.icon.is_none() && s.contains("/CharaIcon/UI_CharaIcon_") {
                    r.icon = Some(s.to_string());
                } else if r.hud.is_none() && s.contains("/CharaIconHUD/") {
                    r.hud = Some(s.to_string());
                } else if r.cut.is_none() && s.contains("/CharaCut/UI_CharaCut_") {
                    r.cut = Some(s.to_string());
                } else if r.name_table.is_none() && s.starts_with("/Game/SeparateAi/") {
                    r.name_table = Some(s.to_string());
                } else if r.cv.is_none() && s.contains("/DB_CharacterVoice/DB_CV") {
                    r.cv = s.rsplit("/DB_CV").next().and_then(|x| x.split('_').next()).and_then(|d| d.parse().ok());
                }
            }
        }
        if let Some(mut r) = cur {
            fill(&mut r);
            if !r.mesh.is_empty() {
                rows.push(r);
            }
        }
        // Some regular racers (e.g. Charmy) store their icon in a way the pair scan misses too: their AI row's
        // icon is UI_CharaIcon_<code>_02, so the regular one is UI_CharaIcon_<code>_01 (or _<code>) when listed.
        for i in before..rows.len() {
            if rows[i].icon.is_some() || rows[i].id.contains("Racer") { continue; }
            let ai = format!("{}Racer", rows[i].id);
            let Some(aicon) = rows[before..].iter().find(|r| r.id == ai).and_then(|r| r.icon.clone()) else { continue };
            let Some(stem) = aicon.strip_suffix("_02") else { continue };
            let found = [format!("{stem}_01"), stem.to_string()].into_iter().find(|c| names.iter().any(|n| n == c));
            if found.is_some() { rows[i].icon = found; }
        }
        if dbg { eprintln!("{name}: {} bytes, header {}, names {}, rows {}", data.len(), h.summary.header_size, names.len(), rows.len()-before); }
    }
    // de-duplicate by mesh path, keep first
    let mut seen = HashSet::new();
    rows.retain(|r| seen.insert(r.mesh.clone()));
    rows
}

// ---------------------------------------------------------------- legacy rename

struct LegacyPkg {
    asset: Vec<u8>,
    others: Vec<(String, Vec<u8>)>, // extension -> data (uexp, ubulk ...)
}

fn legacy_header(data: &[u8]) -> Result<FLegacyPackageHeader> {
    FLegacyPackageHeader::deserialize(&mut Cursor::new(data), Some(EngineVersion::UE5_4.package_file_version()))
}

/// (imported package path, class of the object imported from it)
fn legacy_imports(h: &FLegacyPackageHeader) -> Vec<(String, String)> {
    let mut out = vec![];
    for imp in &h.imports {
        if imp.outer_index.is_import() {
            let outer = &h.imports[imp.outer_index.to_import_index() as usize];
            if outer.outer_index.is_null() {
                let pkg = h.name_map.get(outer.object_name).unwrap_or_default().to_string();
                let class = h.name_map.get(imp.class_name).unwrap_or_default().to_string();
                out.push((pkg, class));
            }
        }
    }
    out
}

fn rename_legacy(data: &[u8], renames: &HashMap<String, String>, new_package_name: &str) -> Result<Vec<u8>> {
    let mut h = legacy_header(data)?;
    let hs = h.summary.versioning_info.total_header_size as i64;
    for e in h.exports.iter_mut() {
        e.serial_offset -= hs;
    }
    for e in h.cell_exports.iter_mut() {
        e.serial_offset -= hs;
    }
    let names: Vec<String> = h.name_map.copy_raw_names().into_iter().map(|n| renames.get(&n).cloned().unwrap_or(n)).collect();
    h.name_map = FPackageNameMap::create_from_names(names);
    h.summary.package_name = new_package_name.to_string();
    let mut out = Cursor::new(Vec::new());
    h.serialize(&mut out, None, &Log::new_stdout(false, false))?;
    Ok(out.into_inner())
}

// ---------------------------------------------------------------- main flow

pub struct Env {
    pub config: Arc<Config>,
    pub game_dir: PathBuf,
    pub mods_dir: PathBuf,
    pub csse: PathBuf,
    /// other enabled mods that change the racer tables (e.g. Separate, Ai Skins!), loaded above True CSSE
    pub extra: Vec<PathBuf>,
}

pub struct Loaded {
    store: IoStoreBackend,
    base: IoStoreBackend,
    rows: Vec<DriverRow>,
    mod_pkgs: BTreeMap<String, FPackageId>,
    /// which mod container (index into the utocs given to load) each mod package comes from
    mod_src: BTreeMap<String, usize>,
    _tmp: Option<TempDir>,
}

pub struct TempDir(pub PathBuf);
impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

pub fn resolve_env(game_dir: Option<PathBuf>, mods_dir: Option<PathBuf>, aes: Option<String>, first_mod: Option<&Path>, interactive: bool) -> Result<Env> {
    let mut ini = read_ini();
    let game_dir = game_dir
        .or_else(|| ini.get("GameDir").map(PathBuf::from).filter(|p| is_game_dir(p)))
        .or_else(find_game_dir)
        .or_else(|| {
            if !interactive {
                return None;
            }
            loop {
                let a = ask("Couldn't find the game. Paste your SonicRacingCrossWorlds folder path: ");
                let p = PathBuf::from(a);
                if is_game_dir(&p) {
                    return Some(p);
                }
                println!("That folder doesn't contain UNION\\Content\\Paks, try again.");
            }
        })
        .context("Game folder not found. Set it in Settings.")?;
    if !is_game_dir(&game_dir) {
        bail!("{} is not the game folder (no UNION\\Content\\Paks inside).", game_dir.display());
    }
    ini.insert("GameDir".into(), game_dir.display().to_string());
    let aes = aes.or_else(|| ini.get("AesKey").cloned()).or_else(|| find_aes(&game_dir)).unwrap_or_else(|| DEFAULT_AES.to_string());
    ini.insert("AesKey".into(), aes.clone());
    write_ini(&ini);
    let mods_dir = mods_dir
        .or_else(|| first_mod.and_then(|m| m.ancestors().find(|a| a.file_name().is_some_and(|n| n.eq_ignore_ascii_case("Mods"))).map(Path::to_path_buf)))
        .or_else(|| mods_dir_from_settings(&game_dir))
        .context("Couldn't find your BlueStar Mods folder.")?;
    let mut config = Config::default();
    config.aes_keys.insert(FGuid::default(), AesKey::from_str(&aes)?);
    let config = Arc::new(config);
    // The racer tables that give AI Skin 1-3 their own slots: True CSSE or Separate, Ai Skins!.
    // The app passes the one turned on in BlueStar (empty = neither is on); the CLI searches the Mods folder.
    const NO_TABLES: &str = "Turn on True CSSE or Separate, Ai Skins! in BlueStar: AI Skin 1-3 only get their own slots with one of them.";
    let csse = match std::env::var_os("AISP_TABLES") {
        Some(t) if t.is_empty() => bail!(NO_TABLES),
        // "-": the caller only needs the game / Mods folders (first run, nothing installed yet)
        Some(t) if t == "-" => PathBuf::new(),
        Some(t) if PathBuf::from(&t).is_file() => PathBuf::from(t),
        _ => find_csse(&mods_dir, first_mod.unwrap_or(Path::new("")), config.clone()).context(NO_TABLES)?,
    };
    Ok(Env { config, game_dir, mods_dir, csse, extra: vec![] })
}

/// Open the mod containers (first = highest priority). Disabled (".utoc.disabled") files are linked/copied to a temp folder.
fn open_mod_utoc(env: &Env, u: &Path, tmp: &mut Option<TempDir>) -> Result<Box<dyn IoStoreTrait>> {
        let name = u.file_name().unwrap().to_string_lossy().to_string();
        let path = if let Some(stem) = name.strip_suffix(".utoc.disabled") {
            let t = tmp.get_or_insert_with(|| {
                let p = std::env::temp_dir().join(format!("skincss_in_{}_{}", std::process::id(), rand_suffix()));
                let _ = std::fs::create_dir_all(&p);
                TempDir(p)
            });
            let dir = u.parent().unwrap();
            for ext in ["utoc", "ucas"] {
                let src = [dir.join(format!("{stem}.{ext}.disabled")), dir.join(format!("{stem}.{ext}"))].into_iter().find(|p| p.exists()).with_context(|| format!("missing {stem}.{ext}"))?;
                let dst = t.0.join(format!("{stem}.{ext}"));
                if std::fs::hard_link(&src, &dst).is_err() {
                    std::fs::copy(&src, &dst)?;
                }
            }
            t.0.join(format!("{stem}.utoc"))
        } else {
            u.to_path_buf()
        };
        Ok(Box::new(IoStoreContainer::open(&path, env.config.clone()).with_context(|| format!("can't open {}", u.display()))?))
}

/// Racer slot mesh path (/Game/..../Character/[SkinNN/]<Name>/Mesh/SK_<Name>) if this package is one.
fn slot_mesh(p: &str) -> Option<String> {
    let p = p.strip_suffix(".uasset").unwrap_or(p);
    let i = p.find("/Asset/Character/")?;
    let rest = &p[i + "/Asset/Character/".len()..];
    let parts: Vec<&str> = rest.split('/').collect();
    let (name, mesh_dir, file) = match parts.as_slice() {
        [n, m, f] => (*n, *m, *f),
        [s, n, m, f] if s.starts_with("Skin") => (*n, *m, *f),
        _ => return None,
    };
    if mesh_dir != "Mesh" || file.strip_prefix("SK_") != Some(name) {
        return None;
    }
    Some(format!("/Asset/Character/{rest}"))
}

pub struct Classified {
    pub skin: Option<bool>,
    pub slots: Vec<String>,
}

/// Quick check for the mod list: does this container touch racer character assets, and which racer
/// slot meshes does it replace?
pub fn classify(env: &Env, u: &Path) -> Result<Classified> {
    let mut tmp = None;
    let c = open_mod_utoc(env, u, &mut tmp)?;
    let mut any_path = false;
    let mut skin = false;
    let mut slots = BTreeSet::new();
    let mut note = |p: &str| {
        if p.contains("/Asset/Character/") && !p.contains("/Animation/") {
            skin = true;
        }
        if let Some(m) = slot_mesh(p) {
            slots.insert(m);
        }
    };
    for ch in c.chunks() {
        if let Some(p) = c.chunk_path(ch.id()) {
            any_path = true;
            note(&p);
        }
    }
    if !any_path {
        // no file index: fall back to package names (reads each package header)
        for pk in c.packages() {
            let cid = FIoChunkId::from_package_id(pk.id(), 0, EIoChunkType::ExportBundleData);
            if let Ok(d) = c.read(cid)
                && let Some(n) = zen_package_name(&d)
            {
                note(&n);
            }
        }
    }
    Ok(Classified { skin: Some(skin), slots: slots.into_iter().collect() })
}

pub fn load(env: &Env, utocs: &[PathBuf]) -> Result<Loaded> {
    let mut tmp: Option<TempDir> = None;
    let mut mod_containers: Vec<Box<dyn IoStoreTrait>> = vec![];
    for u in utocs {
        mod_containers.push(open_mod_utoc(env, u, &mut tmp)?);
    }
    let mut mod_pkgs: BTreeMap<String, FPackageId> = BTreeMap::new();
    let mut mod_src: BTreeMap<String, usize> = BTreeMap::new();
    for (ci, c) in mod_containers.iter().enumerate() {
        for p in c.packages() {
            let cid = FIoChunkId::from_package_id(p.id(), 0, EIoChunkType::ExportBundleData);
            if let Ok(d) = c.read(cid)
                && let Some(n) = zen_package_name(&d)
            {
                mod_src.entry(n.clone()).or_insert(ci);
                mod_pkgs.entry(n).or_insert(p.id());
            }
        }
    }
    // the racer-table mod the game uses (highest in BlueStar) comes first; other table mods below it
    // only supply their assets (models, animations, name tables)
    let mut containers = mod_containers;
    let mut base_list: Vec<Box<dyn IoStoreTrait>> = vec![];
    containers.push(Box::new(IoStoreContainer::open(&env.csse, env.config.clone())?));
    base_list.push(Box::new(IoStoreContainer::open(&env.csse, env.config.clone())?));
    for x in &env.extra {
        if x == &env.csse {
            continue;
        }
        containers.push(open_mod_utoc(env, x, &mut tmp)?);
        base_list.push(open_mod_utoc(env, x, &mut tmp)?);
    }
    containers.push(Box::new(IoStoreBackend::open(env.game_dir.join("UNION/Content/Paks"), env.config.clone())?));
    let store = IoStoreBackend::from_containers(containers);
    base_list.push(Box::new(IoStoreBackend::open(env.game_dir.join("UNION/Content/Paks"), env.config.clone())?));
    let base = IoStoreBackend::from_containers(base_list);
    let rows = read_driver_rows(&store);
    if rows.is_empty() {
        bail!("Couldn't read the racer table (DT_DriverData). Is True CSSE / Separate, Ai Skins! up to date?");
    }
    Ok(Loaded { store, base, rows, mod_pkgs, mod_src, _tmp: tmp })
}

fn rand_suffix() -> u128 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_nanos()).unwrap_or(0)
}

impl Loaded {
    pub fn sources(&self) -> Vec<DriverRow> {
        let mut sources: Vec<DriverRow> = self.rows.iter().filter(|r| self.mod_pkgs.contains_key(&r.mesh)).cloned().collect();
        if sources.is_empty() {
            sources = self.rows.iter().filter(|r| self.mod_pkgs.keys().any(|k| k.starts_with(&r.root()))).cloned().collect();
            let mut seen = HashSet::new();
            sources.retain(|r| seen.insert(r.root()));
        }
        sources
    }
    pub fn targets(&self, src: &DriverRow) -> Vec<DriverRow> {
        self.rows.iter().filter(|r| r.base() == src.base() && r.mesh != src.mesh).cloned().collect()
    }
}

pub fn racer_name(id: &str) -> String {
    let mut n = id.to_string();
    if let Some(i) = n.find("RacerColor") {
        n.truncate(i);
    } else if let Some(s) = n.strip_suffix("Racer") {
        n = s.to_string();
    }
    // split CamelCase
    let mut out = String::new();
    for (i, c) in n.chars().enumerate() {
        if i > 0 && c.is_uppercase() && !n[..n.char_indices().nth(i).unwrap().0].ends_with(' ') {
            out.push(' ');
        }
        out.push(c);
    }
    out
}

pub struct PortRequest {
    pub src_index: usize,
    pub tgt_index: usize,
    pub mod_name: String,
    pub author: String,
    pub out_name: Option<String>,
    pub copy_icons: bool,
    pub thumb: Option<PathBuf>,
    /// write the container straight into this folder (no mod.ini / thumb) - used for "add as option"
    pub into_dir: Option<PathBuf>,
}
pub struct PortResult {
    pub out_dir: PathBuf,
    pub out_name: String,
    pub copied: Vec<(String, String, bool)>,
    pub skipped: Vec<String>,
    pub had_icons: bool,
}

pub fn do_port(env: &Env, l: &Loaded, req: &PortRequest, log_fn: &dyn Fn(&str)) -> Result<PortResult> {
    let sources = l.sources();
    let src = sources.get(req.src_index).context("bad source")?.clone();
    let targets = l.targets(&src);
    let tgt = targets.get(req.tgt_index).context("bad target")?.clone();
    let src_root = src.root();
    let tgt_root = tgt.root();
    let tmp = TempDir(std::env::temp_dir().join(format!("skincss_{}_{}", std::process::id(), rand_suffix())));
    let legacy_dir = tmp.0.join("legacy");
    let stage_dir = tmp.0.join("stage");
    let log = Log::new_stdout(false, false);
    let ctx = retoc::asset_conversion::FZenPackageContext::create(&l.store, Some(EngineVersion::UE5_4.package_file_version()), &log, None);
    let writer = FSFileWriter::new(&legacy_dir);

    log_fn(&format!("Reading {} ({})", racer_name(&src.id), src.slot_label()));
    let mut new_path: BTreeMap<String, String> = BTreeMap::new();
    new_path.insert(src.mesh.clone(), tgt.mesh.clone());
    let mut queue: VecDeque<String> = VecDeque::from([src.mesh.clone()]);
    // the "side" mesh (SK_<X>side) is what the race intro shows when racers drive in: move it too
    let src_side = format!("{}side", src.mesh);
    let tgt_side = format!("{}side", tgt.mesh);
    let has_side = l.mod_pkgs.contains_key(&src_side);
    if has_side {
        log_fn(&format!("Intro model {} -> {}", basename(&src_side), basename(&tgt_side)));
        new_path.insert(src_side.clone(), tgt_side.clone());
        queue.push_back(src_side.clone());
    }
    let mut loaded: BTreeMap<String, LegacyPkg> = BTreeMap::new();
    let mut keep: HashSet<String> = HashSet::new();
    while let Some(pkg) = queue.pop_front() {
        let lp = load_legacy(&ctx, &writer, &legacy_dir, &pkg)?;
        let h = legacy_header(&lp.asset)?;
        for (dep, class) in legacy_imports(&h) {
            if new_path.contains_key(&dep) {
                continue;
            }
            if !CLONE_CLASSES.contains(&class.as_str()) {
                // assets the mod ADDS (custom skeleton, anim BP, master materials...) travel along unchanged
                if l.mod_pkgs.contains_key(&dep) && !l.base.has_chunk_id(pkg_chunk(&dep)) {
                    new_path.insert(dep.clone(), dep.clone());
                    keep.insert(dep.clone());
                    queue.push_back(dep);
                }
                continue;
            }
            if !(l.mod_pkgs.contains_key(&dep) || dep.starts_with(&src_root)) {
                continue;
            }
            let np = match dep.strip_prefix(&src_root) {
                Some(rest) => format!("{tgt_root}{rest}"),
                None => format!("{tgt_root}Ported/{}", basename(&dep)),
            };
            new_path.insert(dep.clone(), np);
            queue.push_back(dep);
        }
        loaded.insert(pkg, lp);
    }
    let mut name_renames_for: HashMap<String, (String, String)> = HashMap::new();
    let mut had_icons = false;
    if req.copy_icons {
        for (s, t) in [(&src.icon, &tgt.icon), (&src.hud, &tgt.hud), (&src.cut, &tgt.cut)] {
            if let (Some(s), Some(t)) = (s, t)
                && l.mod_pkgs.contains_key(s)
            {
                had_icons = true;
                new_path.insert(s.clone(), t.clone());
                loaded.insert(s.clone(), load_legacy(&ctx, &writer, &legacy_dir, s)?);
                name_renames_for.insert(s.clone(), (basename(s).to_string(), basename(t).to_string()));
            }
        }
    }
    // stamps (Stamp Foundation numbering): UI_Stamp_<code>_01 regular, _02 AI Skin, _03.._05 AI Skin 1..3
    if req.copy_icons
        && let (Some(code), Some(sn), Some(tn)) = (stamp_code(&src), stamp_number(&src.mesh), stamp_number(&tgt.mesh))
        && sn != tn
    {
        let want = format!("UI_Stamp_{code}_{sn:02}");
        let found: Vec<String> = l.mod_pkgs.keys().filter(|p| basename(p).eq_ignore_ascii_case(&want)).cloned().collect();
        for sp in found {
            let tb = format!("UI_Stamp_{code}_{tn:02}");
            let tp = format!("{}/{tb}", sp.rsplit_once('/').map(|x| x.0).unwrap_or(""));
            log_fn(&format!("Stamp {} -> {tb}", basename(&sp)));
            new_path.insert(sp.clone(), tp);
            loaded.insert(sp.clone(), load_legacy(&ctx, &writer, &legacy_dir, &sp)?);
            name_renames_for.insert(sp.clone(), (basename(&sp).to_string(), tb));
            had_icons = true;
        }
    }
    name_renames_for.insert(src.mesh.clone(), (basename(&src.mesh).to_string(), basename(&tgt.mesh).to_string()));
    if has_side {
        name_renames_for.insert(src_side.clone(), (basename(&src_side).to_string(), basename(&tgt_side).to_string()));
    }

    let path_renames: HashMap<String, String> = new_path.iter().map(|(a, b)| (a.clone(), b.clone())).collect();
    let mut copied = vec![];
    for (old, lp) in &loaded {
        let new = &new_path[old];
        // kept packages stay at their path, but what they point to must follow the move too (e.g. a mod's
        // own material that uses textures we renamed onto the new slot), otherwise those textures go missing
        let mut renames = path_renames.clone();
        if let Some((o, n)) = name_renames_for.get(old)
            && o != n
        {
            renames.insert(o.clone(), n.clone());
        }
        let asset = rename_legacy(&lp.asset, &renames, new)?;
        let rel = new.strip_prefix("/Game/").context("unexpected package path")?;
        let base = stage_dir.join("UNION/Content").join(rel);
        std::fs::create_dir_all(base.parent().unwrap())?;
        std::fs::write(base.with_extension("uasset"), asset)?;
        for (ext, data) in &lp.others {
            std::fs::write(base.with_extension(ext), data)?;
        }
        let from_mod = l.mod_pkgs.contains_key(old);
        log_fn(&format!("Copied {}", short(old).rsplit('/').next().unwrap_or("")));
        copied.push((short(old), short(new), from_mod));
    }
    let skipped: Vec<String> = l.mod_pkgs.keys().filter(|k| !new_path.contains_key(*k)).map(|s| short(s)).collect();

    let pretty_slot = tgt.slot_label().replace(" (", " - ").replace(')', "");
    let out_name = req.out_name.clone().filter(|s| !s.trim().is_empty()).unwrap_or_else(|| format!("{} ({})", req.mod_name, pretty_slot));
    let slot_tag = tgt.slot_label().replace(['(', ')'], "").replace(' ', "");
    let safe: String = format!("{}_{}", req.mod_name, slot_tag).chars().map(|c| if c.is_ascii_alphanumeric() { c } else { '_' }).take(60).collect();
    let out_dir = req.into_dir.clone().unwrap_or_else(|| env.mods_dir.join(sanitize(&out_name)).join(sanitize(&out_name)));
    std::fs::create_dir_all(&out_dir)?;
    for e in std::fs::read_dir(&out_dir)?.flatten() {
        let n = e.file_name().to_string_lossy().to_string();
        if n.starts_with("z_") && (n.ends_with(".utoc") || n.ends_with(".ucas") || n.ends_with(".pak")) {
            let _ = std::fs::remove_file(e.path());
        }
    }
    log_fn("Packing the new mod");
    let utoc = out_dir.join(format!("z_{safe}_P.utoc"));
    crate::action_to_zen(
        crate::ActionToZen { input: stage_dir.clone(), output: utoc.clone(), filter: vec![], version: EngineVersion::UE5_4, script_cell: vec![], verbose: false, debug: false, no_parallel: true },
        env.config.clone(),
    )?;
    if req.into_dir.is_some() {
        return Ok(PortResult { out_dir, out_name, copied, skipped, had_icons });
    }
    std::fs::write(
        out_dir.join("mod.ini"),
        format!(
            "[Main]\nName = \"{}\"\nVersion = 1.0\nAuthor = \"{}{}\"\nDescription = \"{} moved to {} {} with Crossworld Skin & CSS Manager. Load ABOVE True CSSE.\"\n",
            out_name.replace('"', "'"),
            req.author.replace('"', "'"),
            "",
            req.mod_name.replace('"', "'"),
            racer_name(&tgt.id),
            tgt.slot_label()
        ),
    )?;
    if let Some(t) = &req.thumb
        && t.exists()
    {
        let ext = t.extension().map(|e| e.to_string_lossy().to_string()).unwrap_or_else(|| "jpg".into());
        let _ = std::fs::copy(t, out_dir.join(format!("Thumb.{ext}")));
    }
    Ok(PortResult { out_dir, out_name, copied, skipped, had_icons })
}

pub fn run_port(opts: PortOptions) -> Result<()> {
    println!("=== Crossworld Skin & CSS Manager engine (needs True CSSE) ===\n");
    let input = opts.input.clone();
    let mut utocs: Vec<PathBuf> = vec![];
    if input.is_file() && input.extension().is_some_and(|e| e == "utoc") {
        utocs.push(input.clone());
    } else if input.is_dir() {
        for e in walk(&input) {
            if e.extension().is_some_and(|x| x == "utoc") {
                utocs.push(e);
            }
        }
    }
    if utocs.is_empty() {
        bail!("No .utoc file found in {}. Drag the mod's folder (or its .utoc file) onto the tool.", input.display());
    }
    utocs.sort();
    let labels: Vec<String> = utocs.iter().map(|p| p.strip_prefix(&input).unwrap_or(p).display().to_string()).collect();
    let ci = choose("Mod file", &labels, opts.container_choice, opts.interactive)?;
    let mod_utoc = utocs[ci].clone();
    let env = resolve_env(opts.game_dir.clone(), opts.mods_dir.clone(), opts.aes.clone(), Some(&mod_utoc), opts.interactive)?;
    println!("Game: {}\nTrue CSSE: {}", env.game_dir.display(), env.csse.display());
    let l = load(&env, &[mod_utoc.clone()])?;
    let sources = l.sources();
    if sources.is_empty() {
        bail!("This mod doesn't seem to change any racer model or skin, so there is nothing to port.");
    }
    let src_labels: Vec<String> = sources.iter().map(|r| format!("{} - {}", racer_name(&r.id), r.slot_label())).collect();
    let si = choose("Which racer/slot does this mod change?", &src_labels, opts.source_choice, opts.interactive)?;
    let targets = l.targets(&sources[si]);
    if targets.is_empty() {
        bail!("No other slot found.");
    }
    let tgt_labels: Vec<String> = targets.iter().map(|r| r.slot_label()).collect();
    let ti = choose("Move it to which slot?", &tgt_labels, opts.target_choice, opts.interactive)?;
    let mod_name = mod_display_name(&mod_utoc, &input);
    let author = read_mod_ini_field(&mod_utoc, &input, "Author").unwrap_or_default();
    let r = do_port(&env, &l, &PortRequest { src_index: si, tgt_index: ti, mod_name, author, out_name: None, copy_icons: true, thumb: None, into_dir: None }, &|m| println!("  {m}"))?;
    println!("\nDone! New mod: {}", r.out_dir.display());
    println!("In BlueStar Manager: tick \"{}\" and drag it ABOVE True CSSE.", r.out_name);
    if !r.skipped.is_empty() {
        println!("\nNot copied (stays shared with the original mod):");
        for s in r.skipped.iter().take(25) {
            println!("  - {s}");
        }
    }
    Ok(())
}

// ---------------------------------------------------------------- JSON API for the GUI

pub fn json_inspect(env: &Env, utocs: &[PathBuf]) -> Result<serde_json::Value> {
    let l = load(env, utocs)?;
    if let Ok(f) = std::env::var("PORTER_DEBUG") {
        for r in l.rows.iter().filter(|r| r.id.to_lowercase().contains(&f.to_lowercase())) {
            eprintln!("ROW {} | {} | base={} | {:?} {:?}", r.id, r.mesh, r.base(), r.icon, r.da);
        }
    }
    let log = Log::new_stdout(false, false);
    let ctx = retoc::asset_conversion::FZenPackageContext::create(&l.store, Some(EngineVersion::UE5_4.package_file_version()), &log, None);
    let tmp = TempDir(std::env::temp_dir().join(format!("skincss_icons_{}_{}", std::process::id(), rand_suffix())));
    let icon = |p: &Option<String>| -> serde_json::Value { p.as_ref().and_then(|p| icon_data_url(&ctx, &tmp.0, p)).map(serde_json::Value::String).unwrap_or(serde_json::Value::Null) };
    // the slot's own (unmodded) icon, read from True CSSE + the game only
    let base_ctx = retoc::asset_conversion::FZenPackageContext::create(&l.base, Some(EngineVersion::UE5_4.package_file_version()), &log, None);
    let game_icon = |p: &Option<String>| -> serde_json::Value { p.as_ref().and_then(|p| icon_data_url(&base_ctx, &tmp.0, p)).map(serde_json::Value::String).unwrap_or(serde_json::Value::Null) };
    let mut out = vec![];
    for (i, s) in l.sources().iter().enumerate() {
        let mod_has_icon = s.icon.as_ref().is_some_and(|i| l.mod_pkgs.contains_key(i));
        let targets: Vec<serde_json::Value> = l
            .targets(s)
            .iter()
            .enumerate()
            .map(|(j, t)| {
                serde_json::json!({"index": j, "id": t.id, "label": t.slot_label(), "slot": slot_kind(&t.mesh), "color": color_of(t), "icon": icon(&t.icon), "mesh": t.mesh, "name_key": t.name_key()})
            })
            .collect();
        out.push(serde_json::json!({
            "index": i, "racer": racer_name(&s.id), "label": s.slot_label(), "slot": slot_kind(&s.mesh), "color": color_of(s),
            "icon": icon(&s.icon), "game_icon": game_icon(&s.icon), "mod_has_icon": mod_has_icon, "name_key": s.name_key(), "mesh": s.mesh, "targets": targets
        }));
    }
    Ok(serde_json::json!({"ok": true, "game_dir": env.game_dir, "mods_dir": env.mods_dir, "csse": env.csse, "packages": l.mod_pkgs.len(), "sources": out}))
}

/// Stamp Foundation slot number: 1 regular, 2 AI Skin, 3..5 AI Skin 1..3
fn stamp_number(mesh: &str) -> Option<u32> {
    match slot_number(mesh) {
        Some(n) if (1..=3).contains(&n) => Some(n as u32 + 2),
        Some(_) => None,
        None if mesh.contains("/Character/Skin/") => Some(2),
        None => Some(1),
    }
}
/// The racer's short code used by icons and stamps (amy, sha, egp...), from its icon name
fn stamp_code(r: &DriverRow) -> Option<String> {
    let b = basename(r.icon.as_deref()?);
    let rest = b.strip_prefix("UI_CharaIcon_")?;
    let code: String = rest.chars().take_while(|c| c.is_ascii_lowercase()).collect();
    (code.len() >= 3).then_some(code)
}
fn slot_kind(mesh: &str) -> String {
    if let Some(n) = slot_number(mesh) {
        format!("ai{n}")
    } else if mesh.contains("/Character/Skin/") {
        "ai".into()
    } else {
        "regular".into()
    }
}
fn color_of(r: &DriverRow) -> Option<String> {
    r.icon.as_deref().and_then(|i| basename(i).rsplit_once("AI_").map(|x| x.1.to_string()))
}

/// Decode a character icon texture package to a PNG data URL (best effort).
/// For the CSS order tab: every racer whose select icon or model is replaced by the given (enabled) mod
/// containers, highest priority first. Icons come back as PNG data URLs.
pub fn json_mod_icons(env: &Env, utocs: &[PathBuf]) -> Result<serde_json::Value> {
    let l = load(env, utocs)?;
    let log = Log::new_stdout(false, false);
    let ctx = retoc::asset_conversion::FZenPackageContext::create(&l.store, Some(EngineVersion::UE5_4.package_file_version()), &log, None);
    let tmp = TempDir(std::env::temp_dir().join(format!("skincss_cssicons_{}_{}", std::process::id(), rand_suffix())));
    let mut out = serde_json::Map::new();
    for r in &l.rows {
        let icon_src = r.icon.as_ref().and_then(|i| l.mod_src.get(i)).copied();
        let mesh_src = l.mod_src.get(&r.mesh).copied();
        if icon_src.is_none() && mesh_src.is_none() {
            continue;
        }
        let icon = if icon_src.is_some() { r.icon.as_ref().and_then(|p| icon_data_url(&ctx, &tmp.0, p)) } else { None };
        out.insert(r.id.clone(), serde_json::json!({ "icon": icon, "src": icon_src.or(mesh_src), "model": mesh_src.is_some() }));
    }
    Ok(serde_json::Value::Object(out))
}

fn icon_data_url(ctx: &retoc::asset_conversion::FZenPackageContext, tmp: &Path, pkg: &str) -> Option<String> {
    let dbg = std::env::var("PORTER_ICONDBG").is_ok();
    let writer = FSFileWriter::new(tmp);
    let lp = match load_legacy(ctx, &writer, tmp, pkg) { Ok(x) => x, Err(e) => { if dbg { eprintln!("ICON {pkg}: load {e:#}"); } return None; } };
    let h = legacy_header(&lp.asset).ok()?;
    let names = h.name_map.copy_raw_names();
    let fmt = names.iter().find(|n| n.starts_with("PF_"))?.clone();
    if dbg { eprintln!("ICON {pkg}: fmt {fmt} others {:?}", lp.others.iter().map(|(e, d)| (e.clone(), d.len())).collect::<Vec<_>>()); }
    let uexp = lp.others.iter().find(|(e, _)| e == "uexp").map(|x| &x.1)?;
    let ubulk = lp.others.iter().find(|(e, _)| e == "ubulk").map(|x| &x.1);
    // bytes of one mip of w x h pixels
    let bytes_for = |w: usize, h: usize| -> usize {
        let (bw, bh) = (w.div_ceil(4) * 4, h.div_ceil(4) * 4);
        match fmt.as_str() {
            "PF_DXT1" | "PF_BC4" => bw * bh / 2,
            "PF_B8G8R8A8" | "PF_R8G8B8A8" => w * h * 4,
            _ => bw * bh,
        }
    };
    // mips are stored as [pixel data][SizeX i32][SizeY i32][SizeZ=1 i32]...; find every such header (any size,
    // not only powers of two) whose pixel data fits right before it
    let rd = |hay: &[u8], o: usize| i32::from_le_bytes([hay[o], hay[o + 1], hay[o + 2], hay[o + 3]]);
    let mut pick: Option<(usize, usize, Vec<u8>)> = None;
    if uexp.len() >= 12 {
        for q in 0..uexp.len() - 12 {
            if rd(uexp, q + 8) != 1 { continue; }
            let (w, h) = (rd(uexp, q), rd(uexp, q + 4));
            if !(16..=2048).contains(&w) || !(16..=2048).contains(&h) { continue; }
            let (w, h) = (w as usize, h as usize);
            let l = bytes_for(w, h);
            if q < l + 16 { continue; }
            // the smallest mip that is still >= 256 on its long side (or the biggest one if all are smaller)
            let better = |a: (usize, usize), b: (usize, usize)| -> bool {
                let (ga, gb) = (a.0.max(a.1) >= 256, b.0.max(b.1) >= 256);
                if ga != gb { ga } else if ga { a.0 * a.1 < b.0 * b.1 } else { a.0 * a.1 > b.0 * b.1 }
            };
            if pick.as_ref().is_some_and(|p| !better((w, h), (p.0, p.1))) { continue; }
            pick = Some((w, h, uexp[q - l..q].to_vec()));
        }
    }
    if pick.as_ref().is_none_or(|p| p.0.max(p.1) < 256)
        && let Some(b) = ubulk
    {
        // big mips live in the .ubulk; the header in the .uexp tells the size
        'outer: for s in [2048usize, 1024, 512, 256, 128] {
            for q in 0..uexp.len().saturating_sub(12) {
                if rd(uexp, q) == s as i32 && rd(uexp, q + 4) == s as i32 && rd(uexp, q + 8) == 1 && b.len() >= bytes_for(s, s) {
                    pick = Some((s, s, b[..bytes_for(s, s)].to_vec()));
                    break 'outer;
                }
            }
        }
    }
    let (sw, sh, data) = pick?;
    if dbg { eprintln!("ICON {pkg}: picked {sw}x{sh}"); }
    let (w, hh) = (sw.div_ceil(4) * 4, sh.div_ceil(4) * 4);
    let mut px = vec![0u32; w * hh];
    let ok = match fmt.as_str() {
        "PF_BC7" => texture2ddecoder::decode_bc7(&data, w, hh, &mut px).is_ok(),
        "PF_DXT5" => texture2ddecoder::decode_bc3(&data, w, hh, &mut px).is_ok(),
        "PF_DXT1" => texture2ddecoder::decode_bc1(&data, w, hh, &mut px).is_ok(),
        "PF_B8G8R8A8" => {
            for (i, c) in data.chunks_exact(4).enumerate() {
                let (y, x) = (i / sw, i % sw);
                px[y * w + x] = u32::from_le_bytes([c[0], c[1], c[2], c[3]]);
            }
            true
        }
        "PF_R8G8B8A8" => {
            for (i, c) in data.chunks_exact(4).enumerate() {
                let (y, x) = (i / sw, i % sw);
                px[y * w + x] = u32::from_le_bytes([c[2], c[1], c[0], c[3]]);
            }
            true
        }
        _ => false,
    };
    if !ok {
        return None;
    }
    // downscale to <= 256 on the long side
    let step = (sw.max(sh)).div_ceil(256).max(1);
    let (ow, oh) = (sw / step, sh / step);
    let mut rgba = Vec::with_capacity(ow * oh * 4);
    for y in 0..oh {
        for x in 0..ow {
            let [b, g, r, a] = px[(y * step) * w + x * step].to_le_bytes();
            rgba.extend_from_slice(&[r, g, b, a]);
        }
    }
    let mut buf = Vec::new();
    {
        let mut enc = png::Encoder::new(&mut buf, ow as u32, oh as u32);
        enc.set_color(png::ColorType::Rgba);
        enc.set_depth(png::BitDepth::Eight);
        let mut wr = enc.write_header().ok()?;
        wr.write_image_data(&rgba).ok()?;
    }
    use base64::Engine;
    Some(format!("data:image/png;base64,{}", base64::engine::general_purpose::STANDARD.encode(buf)))
}

fn load_legacy(ctx: &retoc::asset_conversion::FZenPackageContext, writer: &FSFileWriter, dir: &Path, pkg: &str) -> Result<LegacyPkg> {
    let rel = pkg.strip_prefix("/Game/").context("bad path")?;
    let out = format!("UNION/Content/{rel}.uasset");
    retoc::asset_conversion::build_legacy(ctx, pid(pkg), UEPath::new(&out), writer).with_context(|| format!("failed to read {pkg}"))?;
    let base = dir.join(&out);
    let asset = std::fs::read(&base)?;
    let mut others = vec![];
    for ext in ["uexp", "ubulk", "uptnl", "m.ubulk"] {
        if let Ok(d) = std::fs::read(base.with_extension(ext)) {
            others.push((ext.to_string(), d));
        }
    }
    Ok(LegacyPkg { asset, others })
}

fn walk(dir: &Path) -> Vec<PathBuf> {
    let mut out = vec![];
    if let Ok(rd) = std::fs::read_dir(dir) {
        for e in rd.flatten() {
            let p = e.path();
            if p.is_dir() {
                out.extend(walk(&p));
            } else {
                out.push(p);
            }
        }
    }
    out
}

fn find_csse(mods_dir: &Path, exclude: &Path, config: Arc<Config>) -> Option<PathBuf> {
    let dt = pkg_chunk("/Game/01_Union/Database/Driver/DT_DriverData01");
    let mut best: Option<(i32, PathBuf)> = None;
    for p in walk(mods_dir) {
        if p.extension().is_none_or(|e| e != "utoc") || p == exclude {
            continue;
        }
        let Ok(c) = IoStoreContainer::open(&p, config.clone()) else { continue };
        if !c.has_chunk_id(dt) {
            continue;
        }
        let score = if p.display().to_string().to_lowercase().contains("csse") { 2 } else { 1 };
        if best.as_ref().is_none_or(|b| score > b.0) {
            best = Some((score, p));
        }
    }
    best.map(|b| b.1)
}

fn read_mod_ini_field(utoc: &Path, input: &Path, key: &str) -> Option<String> {
    for dir in utoc.ancestors().skip(1).take(4) {
        if let Ok(s) = std::fs::read_to_string(dir.join("mod.ini")) {
            for l in s.lines() {
                if let Some((k, v)) = l.split_once('=')
                    && k.trim().eq_ignore_ascii_case(key)
                {
                    return Some(v.trim().trim_matches('"').to_string());
                }
            }
        }
        if dir == input {
            break;
        }
    }
    None
}
fn mod_display_name(utoc: &Path, input: &Path) -> String {
    read_mod_ini_field(utoc, input, "Name")
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| input.file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_else(|| "Ported mod".into()))
}
fn sanitize(s: &str) -> String {
    s.chars().map(|c| if "<>:\"/\\|?*".contains(c) { '_' } else { c }).collect()
}
fn short(p: &str) -> String {
    p.trim_start_matches("/Game/01_Union/").to_string()
}

/// The True CSSE tables the CSS order is written into (package chunk ids).
pub const CSS_TABLES: [&str; 7] = [
    "0501940e26ffc5cd00000001", "821f2869136241ce00000001", "27765efd9191e8ce00000001", "cc2c1e558d0ffdf800000001",
    "eee26c58930e673100000001", "a436725f298b6f1900000001", "dedf8a3dcf42d55c00000001",
];

/// blake3 of each CSSE racer table as installed (to check a Custom CSS still matches this True CSSE),
/// and optionally write a container holding just those tables (the Custom CSS base).
pub fn css_tables(env: &Env, write: Option<&Path>) -> Result<Vec<String>> {
    let c = IoStoreContainer::open(&env.csse, env.config.clone())?;
    let ver = c.container_file_version().context("no toc version")?;
    let mut writer = match write {
        Some(p) => Some(retoc::iostore_writer::IoStoreWriter::new(p, ver, c.container_header_version(), retoc::UEPathBuf::from("../../../"))?),
        None => None,
    };
    let mut hashes = vec![];
    for id in CSS_TABLES {
        let raw = retoc::FIoChunkIdRaw::from_str(id)?;
        let cid = FIoChunkId::from_raw(raw, ver);
        let data = c.read(cid).with_context(|| format!("True CSSE has no table {id}"))?;
        hashes.push(blake3::hash(&data).to_hex().to_string());
        if let Some(w) = writer.as_mut() {
            let pid = FPackageId(cid.get_chunk_id());
            let se = c.package_store_entry(pid).with_context(|| format!("no store entry for {id}"))?;
            w.write_package_chunk(cid, None, &data, &se)?;
        }
    }
    if let Some(w) = writer {
        w.finalize()?;
    }
    Ok(hashes)
}

/// Write a tiny container holding a copy of a string-table name package (e.g. /Game/SeparateAi/AiAmy1)
/// with a new display name. The table is copied from whichever mod container in `sources` has it.
pub fn write_name_table(env: &Env, sources: &[PathBuf], table: &str, value: &str, out_utoc: &Path) -> Result<()> {
    let pid = FPackageId::from_name(table);
    let mut tmp = None;
    let mut found = None;
    for s in sources {
        let c = open_mod_utoc(env, s, &mut tmp)?;
        let cid = FIoChunkId::from_package_id(pid, 0, EIoChunkType::ExportBundleData);
        if let Ok(d) = c.read(cid) {
            let se = c.package_store_entry(pid).context("no store entry for the name table")?;
            found = Some((c.container_file_version().context("no toc version")?, c.container_header_version(), cid, d, se));
            break;
        }
    }
    let (ver, hver, cid, mut d, se) = found.with_context(|| format!("no enabled mod has {table}"))?;
    let key = table.rsplit('/').next().unwrap_or(table).as_bytes().to_vec();
    // the table holds: ... FString key, FString value, (metadata) ... ; find the last "key" FString
    let mut pat = ((key.len() + 1) as i32).to_le_bytes().to_vec();
    pat.extend_from_slice(&key);
    pat.push(0);
    let at = d.windows(pat.len()).rposition(|w| w == pat.as_slice()).context("name table layout not recognized")? + pat.len();
    let old_len = i32::from_le_bytes(d[at..at + 4].try_into()?);
    let old_bytes = if old_len < 0 { (-old_len as usize) * 2 } else { old_len as usize };
    let mut fstr = vec![];
    if value.is_ascii() {
        fstr.extend_from_slice(&((value.len() + 1) as i32).to_le_bytes());
        fstr.extend_from_slice(value.as_bytes());
        fstr.push(0);
    } else {
        let u: Vec<u16> = value.encode_utf16().chain(std::iter::once(0)).collect();
        fstr.extend_from_slice(&(-(u.len() as i32)).to_le_bytes());
        for c in u {
            fstr.extend_from_slice(&c.to_le_bytes());
        }
    }
    let delta = fstr.len() as i64 - (4 + old_bytes) as i64;
    d.splice(at..at + 4 + old_bytes, fstr);
    // fix the single export's CookedSerialSize in the zen header
    let export_map = u32::from_le_bytes(d[32..36].try_into()?) as usize;
    let size_at = export_map + 8;
    let sz = u64::from_le_bytes(d[size_at..size_at + 8].try_into()?);
    d[size_at..size_at + 8].copy_from_slice(&((sz as i64 + delta) as u64).to_le_bytes());
    if let Some(dir) = out_utoc.parent() {
        std::fs::create_dir_all(dir)?;
    }
    let mut w = retoc::iostore_writer::IoStoreWriter::new(out_utoc, ver, hver, retoc::UEPathBuf::from("../../../"))?;
    w.write_package_chunk(cid, None, &d, &se)?;
    w.finalize()?;
    // IoStore containers are mounted through a matching (empty) .pak
    let pak = out_utoc.with_extension("pak");
    repak::PakBuilder::new().writer(&mut std::io::BufWriter::new(std::fs::File::create(&pak)?), repak::Version::V11, "../../../".to_string(), None).write_index()?;
    Ok(())
}

/// Give some racer rows their own voice id (the FName that picks VOICE_<ID>.acb and the cue names), e.g.
/// AmyRacerColor01: amyAI -> amyA1. Reads the winning DT_DriverData01 (mods given in `env.extra` first,
/// then True CSSE, then the game) and writes it, patched, into `out_utoc`.
pub fn voice_table(env: &Env, slots: &[(String, String)], out_utoc: &Path, log_fn: &dyn Fn(&str)) -> Result<()> {
    let l = load(env, &[])?;
    let tmp = TempDir(std::env::temp_dir().join(format!("skincss_voice_{}_{}", std::process::id(), rand_suffix())));
    let legacy_dir = tmp.0.join("legacy");
    let stage_dir = tmp.0.join("stage");
    let log = Log::new_stdout(false, false);
    let ctx = retoc::asset_conversion::FZenPackageContext::create(&l.store, Some(EngineVersion::UE5_4.package_file_version()), &log, None);
    let writer = FSFileWriter::new(&legacy_dir);
    let pkg = "/Game/01_Union/Database/Driver/DT_DriverData01";
    let lp = load_legacy(&ctx, &writer, &legacy_dir, pkg)?;
    let mut h = legacy_header(&lp.asset)?;
    let mut names = h.name_map.copy_raw_names();
    let mut uexp = lp.others.iter().find(|(e, _)| e == "uexp").map(|x| x.1.clone()).context("no uexp")?;
    let idx = |names: &Vec<String>, n: &str| names.iter().position(|x| x == n);
    let fname_at = |d: &[u8], p: usize| -> (u32, u32) { (u32::from_le_bytes(d[p..p + 4].try_into().unwrap()), u32::from_le_bytes(d[p + 4..p + 8].try_into().unwrap())) };
    for (row, new_id) in slots {
        let row_idx = idx(&names, &format!("EDriverId::{row}")).with_context(|| format!("no row {row}"))? as u32;
        // the row's current voice id: <code>AI (amyAI, sonAI...)
        let mut patched = 0;
        let starts: Vec<usize> = (0..uexp.len().saturating_sub(8)).filter(|&p| fname_at(&uexp, p) == (row_idx, 0)).collect();
        let new_idx = match idx(&names, new_id) {
            Some(i) => i as u32,
            None => {
                names.push(new_id.clone());
                (names.len() - 1) as u32
            }
        };
        for s in starts {
            let end = (s + 500).min(uexp.len() - 8);
            for p in s..end {
                let (i, n) = fname_at(&uexp, p);
                if n == 0 && (i as usize) < names.len() && names[i as usize].len() == 5 && names[i as usize].ends_with("AI") && names[i as usize] != *new_id {
                    uexp[p..p + 4].copy_from_slice(&new_idx.to_le_bytes());
                    patched += 1;
                    log_fn(&format!("{row}: {} -> {new_id}", names[i as usize]));
                    break;
                }
            }
        }
        if patched == 0 {
            log_fn(&format!("{row}: voice id not found"));
        }
    }
    // write the header back with the extra names (export offsets are relative to the header end)
    let hs = h.summary.versioning_info.total_header_size as i64;
    for e in h.exports.iter_mut() {
        e.serial_offset -= hs;
    }
    for e in h.cell_exports.iter_mut() {
        e.serial_offset -= hs;
    }
    // zen keeps only the first `names_referenced_from_export_data_count` names: our new ones are used by the
    // export data, so keep them all
    let n = names.len() as i32;
    h.name_map = FPackageNameMap::create_from_names(names);
    h.summary.names_referenced_from_export_data_count = n;
    let mut out = Cursor::new(Vec::new());
    h.serialize(&mut out, None, &Log::new_stdout(false, false))?;
    let base = stage_dir.join("UNION/Content/01_Union/Database/Driver/DT_DriverData01");
    std::fs::create_dir_all(base.parent().unwrap())?;
    std::fs::write(base.with_extension("uasset"), out.into_inner())?;
    std::fs::write(base.with_extension("uexp"), &uexp)?;
    for (ext, data) in &lp.others {
        if ext != "uexp" {
            std::fs::write(base.with_extension(ext), data)?;
        }
    }
    if let Some(d) = out_utoc.parent() {
        std::fs::create_dir_all(d)?;
    }
    crate::action_to_zen(
        crate::ActionToZen { input: stage_dir.clone(), output: out_utoc.to_path_buf(), filter: vec![], version: EngineVersion::UE5_4, script_cell: vec![], verbose: false, debug: false, no_parallel: true },
        env.config.clone(),
    )?;
    Ok(())
}

/// Debug: list a legacy package's imports and exports (name, class, outer, data range in the .uexp).
pub fn dump_exports(uasset: &Path) -> Result<()> {
    let data = std::fs::read(uasset)?;
    let h = legacy_header(&data)?;
    let names = h.name_map.copy_raw_names();
    let nm = |n: &retoc::legacy_asset::FMinimalName| {
        let s = names.get(n.index as usize).cloned().unwrap_or_default();
        if n.number != 0 { format!("{s}_{}", n.number - 1) } else { s }
    };
    let obj = |i: retoc::zen::FPackageIndex| -> String {
        if i.is_null() {
            "-".into()
        } else if i.is_import() {
            format!("imp:{}", nm(&h.imports[i.to_import_index() as usize].object_name))
        } else {
            format!("exp:{}", nm(&h.exports[i.to_export_index() as usize].object_name))
        }
    };
    for (k, imp) in h.imports.iter().enumerate() {
        println!("I{k}\t{}\t{}\touter={}", nm(&imp.object_name), nm(&imp.class_name), obj(imp.outer_index));
    }
    let hs = h.summary.versioning_info.total_header_size as i64;
    for (k, e) in h.exports.iter().enumerate() {
        println!("E{k}\t{}\tclass={}\touter={}\tuexp={}+{}", nm(&e.object_name), obj(e.class_index), obj(e.outer_index), e.serial_offset - hs, e.serial_size);
    }
    Ok(())
}

/// Re-lay the character select grid (WBP_CharaSelect_Sub_Window) with `cols` columns, keeping the reading order.
/// Works on the legacy .uasset/.uexp pair; writes the patched pair into `out_dir`.
/// UniformGridSlot data (unversioned): 4 bytes, header fragments (u16), then present values of
/// [HAlign u8, VAlign u8, Row i32, Column i32, Parent i32, Content i32], then the rest (kept).
pub fn css_columns(uasset: &Path, cols: i32, out_dir: &Path) -> Result<String> {
    let data = std::fs::read(uasset)?;
    let uexp = std::fs::read(uasset.with_extension("uexp"))?;
    let mut h = legacy_header(&data)?;
    let names = h.name_map.copy_raw_names();
    let hs = h.summary.versioning_info.total_header_size as i64;
    let slot_class = h.imports.iter().position(|i| names.get(i.object_name.index as usize).map(|s| s == "UniformGridSlot").unwrap_or(false) && names.get(i.class_name.index as usize).map(|s| s == "Class").unwrap_or(false));
    let slot_class = slot_class.context("no UniformGridSlot class import")?;
    const SIZES: [usize; 6] = [1, 1, 4, 4, 4, 4];
    struct Slot { k: usize, vals: [Option<Vec<u8>>; 6], rest: Vec<u8>, row: i32, col: i32 }
    let mut slots = vec![];
    for (k, e) in h.exports.iter().enumerate() {
        if !(e.class_index.is_import() && e.class_index.to_import_index() as usize == slot_class) {
            continue;
        }
        let o = (e.serial_offset - hs) as usize;
        let d = &uexp[o..o + e.serial_size as usize];
        let mut p = 4;
        let mut present = [false; 6];
        let mut idx = 0usize;
        loop {
            let f = u16::from_le_bytes([d[p], d[p + 1]]);
            p += 2;
            if f & 0x80 != 0 { bail!("slot {k}: zero mask not supported"); }
            idx += (f & 0x7f) as usize;
            for _ in 0..(f >> 9) {
                if idx >= 6 { bail!("slot {k}: unexpected property {idx}"); }
                present[idx] = true;
                idx += 1;
            }
            if f & 0x100 != 0 { break; }
        }
        let mut vals: [Option<Vec<u8>>; 6] = Default::default();
        for i in 0..6 {
            if present[i] {
                vals[i] = Some(d[p..p + SIZES[i]].to_vec());
                p += SIZES[i];
            }
        }
        let geti = |v: &Option<Vec<u8>>| v.as_ref().map(|b| i32::from_le_bytes(b[..4].try_into().unwrap())).unwrap_or(0);
        let (row, col) = (geti(&vals[2]), geti(&vals[3]));
        slots.push(Slot { k, vals, rest: d[p..].to_vec(), row, col });
    }
    if slots.is_empty() { bail!("no grid slots found"); }
    let old_cols = slots.iter().map(|s| s.col).max().unwrap() + 1;
    // new data per export
    let mut new_data: HashMap<usize, Vec<u8>> = HashMap::new();
    let mut max_row = 0;
    for s in &slots {
        let lin = s.row * old_cols + s.col;
        let (nr, nc) = (lin / cols, lin % cols);
        max_row = max_row.max(nr);
        let mut d = vec![0u8; 4];
        let mut vals = s.vals.clone();
        vals[0].get_or_insert(vec![0]);
        vals[1].get_or_insert(vec![0]);
        vals[2] = Some(nr.to_le_bytes().to_vec());
        vals[3] = Some(nc.to_le_bytes().to_vec());
        if vals[4].is_none() || vals[5].is_none() { bail!("slot {}: missing parent/content", s.k); }
        d.extend_from_slice(&(0x100u16 | (6 << 9)).to_le_bytes());
        for v in vals.iter() { d.extend_from_slice(v.as_ref().unwrap()); }
        d.extend_from_slice(&s.rest);
        new_data.insert(s.k, d);
    }
    // rebuild the .uexp in export order of offsets
    let mut order: Vec<usize> = (0..h.exports.len()).collect();
    order.sort_by_key(|&k| h.exports[k].serial_offset);
    let first = (h.exports[order[0]].serial_offset - hs) as usize;
    let mut out = uexp[..first].to_vec();
    let mut cursor = first;
    for &k in &order {
        let e = &mut h.exports[k];
        let o = (e.serial_offset - hs) as usize;
        if o != cursor { bail!("exports are not contiguous"); }
        let old = &uexp[o..o + e.serial_size as usize];
        cursor = o + old.len();
        let nd = new_data.get(&k).map(|v| v.as_slice()).unwrap_or(old);
        let delta = nd.len() as i64 - old.len() as i64;
        e.serial_offset = out.len() as i64; // relative to header end; serialize() adds the header size
        e.serial_size = nd.len() as i64;
        if delta != 0 && e.script_serialization_end_offset > 0 {
            e.script_serialization_end_offset += delta;
        }
        out.extend_from_slice(nd);
    }
    out.extend_from_slice(&uexp[cursor..]); // package file tag
    for e in h.cell_exports.iter_mut() {
        e.serial_offset -= hs;
    }
    let mut hdr = Cursor::new(Vec::new());
    h.serialize(&mut hdr, None, &Log::new_stdout(false, false))?;
    std::fs::create_dir_all(out_dir)?;
    let stem = uasset.file_stem().unwrap();
    std::fs::write(out_dir.join(stem).with_extension("uasset"), hdr.into_inner())?;
    std::fs::write(out_dir.join(stem).with_extension("uexp"), out)?;
    Ok(format!("{} slots: {old_cols} -> {cols} columns, {} rows", slots.len(), max_row + 1))
}

/// Build a racer table (legacy DT_DriverData01) that keeps True CSSE's rows (own icon per racer, CSS order)
/// but uses Separate, Ai Skins!' per-skin name tables and animation blueprints.
/// Both inputs are legacy .uasset paths (with .uexp next to them). Writes the merged pair into `out_dir`.
pub fn merge_driver_tables(csse: &Path, sep: &Path, out_dir: &Path) -> Result<String> {
    struct Pkg { h: FLegacyPackageHeader, names: Vec<String>, uexp: Vec<u8>, hs: i64 }
    let open = |p: &Path| -> Result<Pkg> {
        let data = std::fs::read(p)?;
        let h = legacy_header(&data)?;
        let names = h.name_map.copy_raw_names();
        let hs = h.summary.versioning_info.total_header_size as i64;
        Ok(Pkg { h, names, uexp: std::fs::read(p.with_extension("uexp"))?, hs })
    };
    let mut c = open(csse)?;
    let s = open(sep)?;
    let fname = |d: &[u8], p: usize| -> (u32, u32) { (u32::from_le_bytes(d[p..p + 4].try_into().unwrap()), u32::from_le_bytes(d[p + 4..p + 8].try_into().unwrap())) };
    // rows: anchor = first (EDriverId::<row>, 0) FName; extent = up to the next anchor
    let rows_of = |k: &Pkg| -> Vec<(String, usize, usize)> {
        let mut v: Vec<(String, usize)> = vec![];
        for (i, n) in k.names.iter().enumerate() {
            if let Some(r) = n.strip_prefix("EDriverId::") {
                let pat = [(i as u32).to_le_bytes(), 0u32.to_le_bytes()].concat();
                if let Some(p) = k.uexp.windows(8).position(|w| w == pat.as_slice()) {
                    v.push((r.to_string(), p));
                }
            }
        }
        v.sort_by_key(|x| x.1);
        let mut out = vec![];
        for i in 0..v.len() {
            let end = if i + 1 < v.len() { v[i + 1].1 } else { k.uexp.len() };
            out.push((v[i].0.clone(), v[i].1, end));
        }
        out
    };
    let crows = rows_of(&c);
    let srows: HashMap<String, (usize, usize)> = rows_of(&s).into_iter().map(|(r, a, b)| (r, (a, b))).collect();
    // interesting FName tokens of a row: (offset, name)
    let toks = |k: &Pkg, a: usize, b: usize| -> Vec<(usize, String)> {
        let mut v = vec![];
        let mut p = a;
        while p + 8 <= b {
            let (i, n) = fname(&k.uexp, p);
            if n == 0 && (i as usize) < k.names.len() && i > 0 {
                let s = &k.names[i as usize];
                if s.starts_with("/Game/") || (s.starts_with("ABP_") && s.ends_with("_C")) {
                    v.push((p, s.clone()));
                    p += 8;
                    continue;
                }
            }
            p += 1;
        }
        v
    };
    let abp_kind = |s: &str| -> Option<&'static str> {
        let base = s.rsplit(['/', '.']).next().unwrap_or(s);
        let cls = s.ends_with("_C");
        if !(base.starts_with("ABP_")) { return None; }
        let k = if base.contains("MenuDriver") { "menu" } else if base.contains("SceneDriver") { "scene" } else if base.contains("_Driver") { "driver" } else { return None };
        Some(match (k, cls) { ("menu", false) => "menu", ("menu", true) => "menu_c", ("scene", false) => "scene", ("scene", true) => "scene_c", (_, false) => "driver", _ => "driver_c" })
    };
    let fstring_at = |d: &[u8], p: usize| -> Option<(usize, String)> {
        let n = i32::from_le_bytes(d.get(p..p + 4)?.try_into().ok()?);
        if !(2..=64).contains(&n) { return None; }
        let b = d.get(p + 4..p + 4 + n as usize)?;
        if b[n as usize - 1] != 0 || !b[..n as usize - 1].iter().all(|x| x.is_ascii_graphic()) { return None; }
        Some((4 + n as usize, String::from_utf8_lossy(&b[..n as usize - 1]).to_string()))
    };
    let mut names = c.names.clone();
    let mut name_idx = |n: &str, names: &mut Vec<String>| -> u32 {
        match names.iter().position(|x| x == n) { Some(i) => i as u32, None => { names.push(n.to_string()); (names.len() - 1) as u32 } }
    };
    // edits on the CSSE uexp: (offset, old length, new bytes), applied back to front
    let mut edits: Vec<(usize, usize, Vec<u8>)> = vec![];
    let (mut n_names, mut n_abps, mut rows_done) = (0, 0, 0);
    for (row, a, b) in &crows {
        let Some(&(sa, sb)) = srows.get(row) else { continue };
        let ct = toks(&c, *a, *b);
        let st = toks(&s, sa, sb);
        let mut changed = false;
        // name tables: Separate's /Game/SeparateAi/... (+ key FString right after) replace DB_CharacterName refs
        let sn: Vec<&(usize, String)> = st.iter().filter(|t| t.1.starts_with("/Game/SeparateAi/")).collect();
        let cn: Vec<&(usize, String)> = ct.iter().filter(|t| t.1.contains("DB_CharacterName")).collect();
        if !sn.is_empty() && sn.len() == cn.len() {
            for (x, y) in cn.iter().zip(sn.iter()) {
                let (Some((clen, _)), Some((_, skey))) = (fstring_at(&c.uexp, x.0 + 8), fstring_at(&s.uexp, y.0 + 8)) else { continue };
                let mut nb = name_idx(&y.1, &mut names).to_le_bytes().to_vec();
                nb.extend_from_slice(&0u32.to_le_bytes());
                nb.extend_from_slice(&((skey.len() + 1) as i32).to_le_bytes());
                nb.extend_from_slice(skey.as_bytes());
                nb.push(0);
                edits.push((x.0, 8 + clen, nb));
                n_names += 1;
                changed = true;
            }
        }
        // animation blueprints: Separate's per-skin ABP_Ai<X><N>_* replace CSSE's, by kind
        // (only the skin's own ABPs: CSSE keeps them under /Asset/Character/Skin*/Animation/, Separate names them ABP_*Ai<X><N>)
        let is_sep_abp = |n: &str| { let b = n.rsplit(['/', '.']).next().unwrap_or(n); b.starts_with("ABP_Ai") || b.contains("Driver_Ai") };
        let sabp: HashMap<&str, &String> = st.iter().filter(|t| is_sep_abp(&t.1)).filter_map(|t| abp_kind(&t.1).map(|k| (k, &t.1))).collect();
        if !sabp.is_empty() {
            for (i, t) in ct.iter().enumerate() {
                if !t.1.contains("/Asset/Character/Skin") { continue; }
                let Some(k) = abp_kind(&t.1) else { continue };
                let mut todo = vec![(t, k)];
                if let Some(nx) = ct.get(i + 1)
                    && nx.0 == t.0 + 8
                    && let Some(kc) = abp_kind(&nx.1)
                    && kc == format!("{k}_c")
                {
                    todo.push((nx, kc));
                }
                for (tok, kind) in todo {
                    if let Some(new) = sabp.get(kind)
                        && *new != &tok.1
                    {
                        let mut nb = name_idx(new, &mut names).to_le_bytes().to_vec();
                        nb.extend_from_slice(&0u32.to_le_bytes());
                        edits.push((tok.0, 8, nb));
                        n_abps += 1;
                        changed = true;
                    }
                }
            }
        }
        if changed { rows_done += 1; }
    }
    edits.sort_by_key(|e| std::cmp::Reverse(e.0));
    // which export holds each edit; apply and track per-export size changes
    let hs = c.hs;
    let mut deltas: HashMap<usize, i64> = HashMap::new();
    for (off, len, nb) in &edits {
        let k = c.h.exports.iter().position(|e| { let o = (e.serial_offset - hs) as usize; *off >= o && *off < o + e.serial_size as usize }).context("edit outside exports")?;
        *deltas.entry(k).or_default() += nb.len() as i64 - *len as i64;
        c.uexp.splice(*off..*off + *len, nb.iter().cloned());
    }
    // shift export offsets (sorted by offset) by the growth of the exports before them
    let mut order: Vec<usize> = (0..c.h.exports.len()).collect();
    order.sort_by_key(|&k| c.h.exports[k].serial_offset);
    let mut shift = 0i64;
    for &k in &order {
        let e = &mut c.h.exports[k];
        e.serial_offset = e.serial_offset - hs + shift;
        let d = deltas.get(&k).copied().unwrap_or(0);
        e.serial_size += d;
        if d != 0 && e.script_serialization_end_offset > 0 { e.script_serialization_end_offset += d; }
        shift += d;
    }
    for e in c.h.cell_exports.iter_mut() { e.serial_offset -= hs; }
    let n = names.len() as i32;
    c.h.name_map = FPackageNameMap::create_from_names(names);
    c.h.summary.names_referenced_from_export_data_count = n;
    let mut hdr = Cursor::new(Vec::new());
    c.h.serialize(&mut hdr, None, &Log::new_stdout(false, false))?;
    std::fs::create_dir_all(out_dir)?;
    let stem = csse.file_stem().unwrap();
    std::fs::write(out_dir.join(stem).with_extension("uasset"), hdr.into_inner())?;
    std::fs::write(out_dir.join(stem).with_extension("uexp"), &c.uexp)?;
    Ok(format!("{rows_done} rows: {n_names} name refs, {n_abps} animation refs from Separate AI Skins"))
}

/// The CSS order editor writes True CSSE's 7 racer tables. When the racer table in use is a different
/// DT_DriverData01 (e.g. "Separate AI Skins + CSSE Icons"), copy each row's CSS slot from the editor's
/// DT_DriverData01 into the one in use, and write `out_utoc` with that table plus the editor's other 6 tables.
pub fn css_retarget(env: &Env, custom_utoc: &Path, out_utoc: &Path, log_fn: &dyn Fn(&str)) -> Result<()> {
    let pkg = "/Game/01_Union/Database/Driver/DT_DriverData01";
    let tmp = TempDir(std::env::temp_dir().join(format!("skincss_css_{}_{}", std::process::id(), rand_suffix())));
    let log = Log::new_stdout(false, false);
    let legacy_of = |utocs: &[PathBuf], sub: &str| -> Result<(FLegacyPackageHeader, Vec<u8>, LegacyPkg)> {
        let l = load(env, utocs)?;
        let dir = tmp.0.join(sub);
        let ctx = retoc::asset_conversion::FZenPackageContext::create(&l.store, Some(EngineVersion::UE5_4.package_file_version()), &log, None);
        let writer = FSFileWriter::new(&dir);
        let lp = load_legacy(&ctx, &writer, &dir, pkg)?;
        let h = legacy_header(&lp.asset)?;
        let uexp = lp.others.iter().find(|(e, _)| e == "uexp").map(|x| x.1.clone()).context("no uexp")?;
        Ok((h, uexp, lp))
    };
    let (sh, suexp, _) = legacy_of(&[custom_utoc.to_path_buf()], "src")?;
    let (mut th, mut tuexp, tlp) = legacy_of(&[], "dst")?;
    // row -> offset of its CSS slot int (right after the "Unlock<N>" FString that follows the row's EDriverId)
    let slots = |h: &FLegacyPackageHeader, u: &[u8]| -> HashMap<String, usize> {
        let names = h.name_map.copy_raw_names();
        let mut m = HashMap::new();
        for (i, n) in names.iter().enumerate() {
            let Some(row) = n.strip_prefix("EDriverId::") else { continue };
            let pat = [(i as u32).to_le_bytes(), 0u32.to_le_bytes()].concat();
            let Some(p) = u.windows(8).position(|w| w == pat.as_slice()) else { continue };
            let Some(q) = u[p..(p + 3000).min(u.len())].windows(6).position(|w| w == b"Unlock") else { continue };
            let q = p + q;
            let Some(z) = u[q..].iter().position(|&b| b == 0) else { continue };
            m.insert(row.to_string(), q + z + 1);
        }
        m
    };
    let src = slots(&sh, &suexp);
    let dst = slots(&th, &tuexp);
    let mut n = 0;
    for (row, &d) in &dst {
        if let Some(&s) = src.get(row) {
            if tuexp[d..d + 4] != suexp[s..s + 4] {
                n += 1;
            }
            let v: [u8; 4] = suexp[s..s + 4].try_into()?;
            tuexp[d..d + 4].copy_from_slice(&v);
        }
    }
    log_fn(&format!("{n} racers moved ({} rows matched)", dst.keys().filter(|r| src.contains_key(*r)).count()));
    // same size: the header only needs its offsets back relative to the header end
    let hs = th.summary.versioning_info.total_header_size as i64;
    for e in th.exports.iter_mut() { e.serial_offset -= hs; }
    for e in th.cell_exports.iter_mut() { e.serial_offset -= hs; }
    let mut hdr = Cursor::new(Vec::new());
    th.serialize(&mut hdr, None, &Log::new_stdout(false, false))?;
    let stage = tmp.0.join("stage");
    let base = stage.join("UNION/Content/01_Union/Database/Driver/DT_DriverData01");
    std::fs::create_dir_all(base.parent().unwrap())?;
    std::fs::write(base.with_extension("uasset"), hdr.into_inner())?;
    std::fs::write(base.with_extension("uexp"), &tuexp)?;
    for (ext, data) in &tlp.others {
        if ext != "uexp" { std::fs::write(base.with_extension(ext), data)?; }
    }
    let dt_utoc = tmp.0.join("dt/z_dt_P.utoc");
    std::fs::create_dir_all(dt_utoc.parent().unwrap())?;
    crate::action_to_zen(
        crate::ActionToZen { input: stage.clone(), output: dt_utoc.clone(), filter: vec![], version: EngineVersion::UE5_4, script_cell: vec![], verbose: false, debug: false, no_parallel: true },
        env.config.clone(),
    )?;
    // final container: the patched DT_DriverData01 + the editor's other tables
    let custom = IoStoreContainer::open(custom_utoc, env.config.clone())?;
    let dt = IoStoreContainer::open(&dt_utoc, env.config.clone())?;
    let ver = custom.container_file_version().context("no toc version")?;
    if let Some(d) = out_utoc.parent() { std::fs::create_dir_all(d)?; }
    let mut w = retoc::iostore_writer::IoStoreWriter::new(out_utoc, ver, custom.container_header_version(), retoc::UEPathBuf::from("../../../"))?;
    let dt_pid = FPackageId::from_name(pkg);
    for id in CSS_TABLES {
        let cid = FIoChunkId::from_raw(retoc::FIoChunkIdRaw::from_str(id)?, ver);
        let pid = FPackageId(cid.get_chunk_id());
        let c: &dyn IoStoreTrait = if pid == dt_pid { &dt } else { &custom };
        let cid = FIoChunkId::from_package_id(pid, 0, EIoChunkType::ExportBundleData);
        let data = c.read(cid).with_context(|| format!("no table {id}"))?;
        let se = c.package_store_entry(pid).with_context(|| format!("no store entry for {id}"))?;
        w.write_package_chunk(cid, None, &data, &se)?;
    }
    w.finalize()?;
    Ok(())
}

/// Hash of the racer table (DT_DriverData01) inside one container, to notice when a mod was updated.
pub fn dt01_hash(env: &Env, utoc: &Path) -> Result<String> {
    let mut tmp = None;
    let c = open_mod_utoc(env, utoc, &mut tmp)?;
    let pid = FPackageId::from_name("/Game/01_Union/Database/Driver/DT_DriverData01");
    let d = c.read(FIoChunkId::from_package_id(pid, 0, EIoChunkType::ExportBundleData)).with_context(|| format!("{} has no racer table", utoc.display()))?;
    Ok(blake3::hash(&d).to_hex().to_string())
}

/// Build "Separate AI Skins + CSSE Icons" from the user's own True CSSE and Separate, Ai Skins! containers.
pub fn combo_build(env: &Env, csse: &Path, sep: &Path, out_utoc: &Path) -> Result<String> {
    let pkg = "/Game/01_Union/Database/Driver/DT_DriverData01";
    let tmp = TempDir(std::env::temp_dir().join(format!("skincss_combo_{}_{}", std::process::id(), rand_suffix())));
    let log = Log::new_stdout(false, false);
    let legacy = |u: &Path, sub: &str| -> Result<PathBuf> {
        let mut t = None;
        let containers: Vec<Box<dyn IoStoreTrait>> = vec![
            open_mod_utoc(env, u, &mut t)?,
            Box::new(IoStoreBackend::open(env.game_dir.join("UNION/Content/Paks"), env.config.clone())?),
        ];
        let store = IoStoreBackend::from_containers(containers);
        let dir = tmp.0.join(sub);
        let ctx = retoc::asset_conversion::FZenPackageContext::create(&store, Some(EngineVersion::UE5_4.package_file_version()), &log, None);
        let writer = FSFileWriter::new(&dir);
        load_legacy(&ctx, &writer, &dir, pkg).with_context(|| format!("can't read the racer table of {}", u.display()))?;
        Ok(dir.join("UNION/Content/01_Union/Database/Driver/DT_DriverData01.uasset"))
    };
    let c = legacy(csse, "csse")?;
    let s = legacy(sep, "sep")?;
    let stage = tmp.0.join("stage");
    let msg = merge_driver_tables(&c, &s, &stage.join("UNION/Content/01_Union/Database/Driver"))?;
    if let Some(d) = out_utoc.parent() {
        std::fs::create_dir_all(d)?;
    }
    crate::action_to_zen(
        crate::ActionToZen { input: stage.clone(), output: out_utoc.to_path_buf(), filter: vec![], version: EngineVersion::UE5_4, script_cell: vec![], verbose: false, debug: false, no_parallel: true },
        env.config.clone(),
    )?;
    Ok(msg)
}

/// Copy a folder of legacy packages, renaming every name and string by same-length replacements
/// (e.g. SHAAI -> SHAA1, shaAI -> shaA1): file paths, name maps and the export data.
pub fn clone_rename(in_root: &Path, out_root: &Path, pairs: &[(String, String)]) -> Result<usize> {
    for (a, b) in pairs {
        if a.len() != b.len() { bail!("replacement {a} -> {b} must keep the length"); }
    }
    let rep_str = |s: &str| -> String { let mut s = s.to_string(); for (a, b) in pairs { s = s.replace(a.as_str(), b.as_str()); } s };
    let rep_bytes = |d: &mut Vec<u8>| { for (a, b) in pairs { let (a, b) = (a.as_bytes(), b.as_bytes()); let mut i = 0; while i + a.len() <= d.len() { if &d[i..i + a.len()] == a { d[i..i + a.len()].copy_from_slice(b); i += a.len(); } else { i += 1; } } } };
    let mut n = 0;
    let mut stack = vec![in_root.to_path_buf()];
    while let Some(dir) = stack.pop() {
        for e in std::fs::read_dir(&dir)? {
            let p = e?.path();
            if p.is_dir() { stack.push(p); continue; }
            let rel = p.strip_prefix(in_root)?.to_string_lossy().to_string();
            let out = out_root.join(rep_str(&rel));
            std::fs::create_dir_all(out.parent().unwrap())?;
            if p.extension().map(|x| x == "uasset").unwrap_or(false) {
                let data = std::fs::read(&p)?;
                let mut h = legacy_header(&data)?;
                let names: Vec<String> = h.name_map.copy_raw_names().iter().map(|s| rep_str(s)).collect();
                let hs = h.summary.versioning_info.total_header_size as i64;
                for x in h.exports.iter_mut() { x.serial_offset -= hs; }
                for x in h.cell_exports.iter_mut() { x.serial_offset -= hs; }
                h.name_map = FPackageNameMap::create_from_names(names);
                // the summary keeps its own copy of the package name: the zen package id comes from it
                h.summary.package_name = rep_str(&h.summary.package_name);
                let mut hdr = Cursor::new(Vec::new());
                h.serialize(&mut hdr, None, &Log::new_stdout(false, false))?;
                std::fs::write(&out, hdr.into_inner())?;
                n += 1;
            } else {
                let mut d = std::fs::read(&p)?;
                rep_bytes(&mut d);
                std::fs::write(&out, d)?;
            }
        }
    }
    Ok(n)
}

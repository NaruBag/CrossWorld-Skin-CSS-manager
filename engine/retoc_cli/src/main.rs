mod port;
mod extract;
mod voice;
use anyhow::{Context, Result, bail};
use cityhasher::HashSet;
use clap::Parser;
use fs_err as fs;
use rayon::prelude::*;
use retoc::asset_conversion::{self, FZenPackageContext};
use retoc::container_header::EIoContainerHeaderVersion;
use retoc::iostore::{IoStoreTrait, PackageInfo};
use retoc::iostore_writer::IoStoreWriter;
use retoc::legacy_asset::FSerializedAssetBundle;
use retoc::logging::Log;
use retoc::name_map::{EMappedNameType, FNameMap, write_name_batch_parts};
use retoc::script_objects::{FPackageObjectIndex, FScriptObjectEntry, ZenScriptObjects};
use retoc::ser::{ReadExt as _, WriteExt};
use retoc::shader_library::{self, get_shader_asset_info_filename_from_library_filename, rebuild_shader_library_from_io_store};
use retoc::version::EngineVersion;
use retoc::zen::{FPackageFileVersion, FZenPackageHeader, VerseScriptCell, get_package_name};
use retoc::zen_asset_conversion::{self, ConvertedZenAssetBundle};
use retoc::{
    AesKey, Config, EIoChunkType, EIoStoreTocVersion, FGuid, FIoChunkId, FIoChunkIdRaw, FPackageId, FSFileReader, FSFileWriter, FSHAHash, FileReaderTrait, FileWriterTrait, NullFileWriter, PackageTestMetadata, PakFileReader, ParallelPakWriter, Toc, UEPath, UEPathBuf, build_verse_cell_store, info,
    iostore, manifest, verbose,
};
use std::borrow::Cow;
use std::ffi::OsStr;
use std::io::BufWriter;
use std::path::Path;
use std::sync::RwLock;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::{
    collections::HashMap,
    io::{BufReader, Cursor, Write},
    path::PathBuf,
    str::FromStr,
    sync::{Arc, Mutex},
};

#[derive(Parser, Debug)]
struct ActionManifest {
    #[arg(index = 1)]
    utoc: PathBuf,
}

#[derive(Parser, Debug)]
struct ActionInfo {
    #[arg(index = 1)]
    path: PathBuf,
}

#[derive(Parser, Debug)]
struct ActionList {
    #[arg(index = 1)]
    utoc: PathBuf,

    /// By default only unique chunks will be listed. --all will also list chunks overriden by patch containers
    #[arg(long)]
    all: bool,
    /// Show chunk content hash
    #[arg(long)]
    hash: bool,
    /// Show package ID
    #[arg(long)]
    package: bool,
    /// Show chunk size
    #[arg(long)]
    size: bool,
    /// Show chunk path
    #[arg(long)]
    path: bool,
    /// Show package store entry
    #[arg(long)]
    store: bool,
}

#[derive(Parser, Debug)]
struct ActionVerify {
    #[arg(index = 1)]
    utoc: PathBuf,
}

#[derive(Parser, Debug)]
struct ActionUnpack {
    #[arg(index = 1)]
    utoc: PathBuf,
    #[arg(index = 2)]
    output: PathBuf,
    #[arg(short, long, default_value = "false")]
    verbose: bool,
}

#[derive(Parser, Debug)]
struct ActionUnpackRaw {
    #[arg(index = 1)]
    utoc: PathBuf,
    #[arg(index = 2)]
    output: PathBuf,
}

#[derive(Parser, Debug)]
struct ActionPackRaw {
    #[arg(index = 1)]
    input: PathBuf,
    #[arg(index = 2)]
    utoc: PathBuf,
}

#[derive(Parser, Debug)]
struct ActionToLegacy {
    /// Input .utoc or directory with multiple .utoc (e.g. Content/Paks/)
    #[arg(index = 1)]
    input: PathBuf,
    /// Output directory or .pak
    #[arg(index = 2)]
    output: PathBuf,

    /// Asset file name filter
    #[arg(short, long)]
    filter: Vec<String>,

    /// Skip conversion of assets
    #[arg(long)]
    no_assets: bool,
    /// Skip conversion of shader libraries
    #[arg(long)]
    no_shaders: bool,
    /// Skip extraction of script objects
    #[arg(long)]
    no_script_objects: bool,
    /// Skip compression of shader libraries
    #[arg(long)]
    no_compres_shaders: bool,
    /// Do not output any files (dry run). Useful for testing conversion
    #[arg(short, long)]
    dry_run: bool,

    /// Engine version override
    #[arg(long)]
    version: Option<EngineVersion>,

    /// Allows specifying additional Verse script cells to be considered for verse native cell import resolution
    #[arg(long)]
    script_cell: Vec<VerseScriptCell>,

    /// Verbose logging
    #[arg(short, long)]
    verbose: bool,
    /// Debug logging
    #[arg(long)]
    debug: bool,
    /// Do not run in parallel. Useful for debugging
    #[arg(long)]
    no_parallel: bool,
}

#[derive(Parser, Debug)]
struct ActionToZen {
    /// Input directory or .pak
    #[arg(index = 1)]
    input: PathBuf,
    /// Output .utoc
    #[arg(index = 2)]
    output: PathBuf,

    /// Asset file name filter
    #[arg(short, long)]
    filter: Vec<String>,

    /// Engine version
    #[arg(long)]
    version: EngineVersion,

    /// Allows specifying additional Verse script cells to be considered for verse native cell import resolution
    #[arg(long)]
    script_cell: Vec<VerseScriptCell>,

    /// Verbose logging
    #[arg(short, long)]
    verbose: bool,
    /// Debug logging
    #[arg(long)]
    debug: bool,
    /// Do not run in parallel. Useful for debugging
    #[arg(long)]
    no_parallel: bool,
}

#[derive(Parser, Debug)]
struct ActionGet {
    /// Input .utoc or directory with multiple .utoc (e.g. Content/Paks/)
    #[arg(index = 1)]
    input: PathBuf,
    /// Chunk ID to get
    #[arg(index = 2)]
    chunk_id: FIoChunkIdRaw,

    /// Optional output path or stdout if "-" or omitted
    #[arg(index = 3)]
    output: Option<PathBuf>,
}

#[derive(Parser, Debug)]
struct ActionDumpTest {
    #[arg(index = 1)]
    input: PathBuf,
    #[arg(index = 2)]
    output_dir: PathBuf,
    #[arg(index = 3)]
    package_id: FPackageId,
}

#[derive(Parser, Debug)]
struct ActionGenScriptObjects {
    /// Input reflection data .jmap dump
    #[arg(index = 1)]
    input: PathBuf,
    /// Output .utoc file
    #[arg(index = 2)]
    output: PathBuf,
    /// Engine version
    #[arg(long)]
    version: EngineVersion,
}

#[derive(Parser, Debug)]
struct ActionPrintScriptObjects {
    /// Input .utoc file containing script objects
    #[arg(index = 1)]
    input: PathBuf,
}

#[derive(Parser, Debug)]
struct ActionAssetRegistry {
    /// Input AssetRegistry.bin file
    #[arg(index = 1)]
    input: PathBuf,
}

#[derive(Parser, Debug)]
enum Action {
    /// Extract manifest from .utoc
    Manifest(ActionManifest),
    /// Show container info
    Info(ActionInfo),
    /// List fils in .utoc (directory index)
    List(ActionList),
    /// Verify IO Store container
    Verify(ActionVerify),
    /// Extracts chunks (files) from .utoc
    Unpack(ActionUnpack),

    /// Extracts raw chunks from container
    UnpackRaw(ActionUnpackRaw),
    /// Packs directory of raw chunks into container
    PackRaw(ActionPackRaw),

    /// Converts asests and shaders from Zen to Legacy
    ToLegacy(ActionToLegacy),
    /// Converts assets and shaders from Legacy to Zen
    ToZen(ActionToZen),

    /// Get chunk by index and write to stdout
    Get(ActionGet),

    /// Dump test
    DumpTest(ActionDumpTest),

    /// Generate script objects global container from UE reflection data .jmap
    /// see https://github.com/trumank/jmap
    GenScriptObjects(ActionGenScriptObjects),

    /// Print script objects from container
    PrintScriptObjects(ActionPrintScriptObjects),

    /// Parse and print asset registry contents
    AssetRegistry(ActionAssetRegistry),

    /// Port a racer skin mod to another AI skin slot (Sonic Racing CrossWorlds + True CSSE)
    Port(ActionPort),
    /// JSON: inspect mod containers (for the GUI)
    GuiInspect(ActionGui),
    /// JSON: tell which mod containers touch racer models (for the GUI mod list)
    GuiClassify(ActionGui),
    /// JSON: icons of racers replaced by the given enabled mods (for the CSS order tab)
    GuiModIcons(ActionGui),
    /// JSON: hashes of the True CSSE racer tables; with --write <utoc>, also write them as a container (Custom CSS base)
    GuiCssTables(ActionCssTables),
    /// JSON: convert a mod's AI voice pack (VOICE_<X>AI) into the regular racer's (VOICE_<X>)
    GuiVoice(ActionVoice),
    /// Re-lay the character select grid of a legacy WBP_CharaSelect_Sub_Window with N columns
    CssColumns(ActionCssColumns),
    /// JSON: move the CSS order editor's slots onto the racer table in use (for "Separate AI Skins + CSSE Icons")
    GuiCssRetarget(ActionCssRetarget),
    /// JSON: build "Separate AI Skins + CSSE Icons" (or with --check, only hash the two racer tables)
    GuiCombo(ActionCombo),
    /// Copy legacy packages with same-length renames (FROM=TO ...)
    CloneRename(ActionCloneRename),
    /// Merge racer tables: True CSSE's rows with Separate AI Skins' names and animations (legacy files)
    MergeTables(ActionMergeTables),
    /// Debug: list a legacy .uasset's imports and exports
    DumpExports(ActionDumpExports),
    /// JSON: list the voice packs (VOICE_<X>[AI]) found in legacy .pak files
    GuiVoices(ActionVoices),
    /// JSON: write a racer's own name table (Separate AI Skins style) with a new name
    GuiNameTable(ActionNameTable),
    /// JSON: give racer rows their own voice id (ROW=newid ...) and write the patched DT_DriverData01
    GuiVoiceTable(ActionVoiceTable),
    /// JSON: extract a .zip/.7z/.rar mod archive (for the GUI)
    GuiExtract(ActionExtract),
    /// JSON: port mod containers (for the GUI)
    GuiPort(ActionGui),
}

#[derive(Parser, Debug)]
struct ActionExtract {
    #[arg(index = 1)]
    archive: PathBuf,
    #[arg(index = 2)]
    dest: PathBuf,
}

#[derive(Parser, Debug)]
struct ActionVoiceTable {
    /// ROW=newid, e.g. AmyRacerColor01=amyA1
    slots: Vec<String>,
    #[arg(long)]
    game_dir: Option<PathBuf>,
    #[arg(long)]
    mods_dir: Option<PathBuf>,
    #[arg(long)]
    extra: Vec<PathBuf>,
    #[arg(long)]
    out: PathBuf,
}

#[derive(Parser, Debug)]
struct ActionNameTable {
    /// enabled mod containers to copy the table from (e.g. Separate, Ai Skins!)
    sources: Vec<PathBuf>,
    #[arg(long)]
    game_dir: Option<PathBuf>,
    #[arg(long)]
    mods_dir: Option<PathBuf>,
    #[arg(long)]
    table: String,
    #[arg(long)]
    value: String,
    #[arg(long)]
    out: PathBuf,
}

#[derive(Parser, Debug)]
struct ActionVoice {
    /// legacy .pak files of the mod
    paks: Vec<PathBuf>,
    #[arg(long)]
    out: PathBuf,
    /// convert AI → regular
    #[arg(long)]
    to_regular: bool,
    /// convert regular → AI
    #[arg(long)]
    to_ai: bool,
    /// make an AI skin slot's own voice bank, e.g. SHAA1 (AI Shadow, AI Skin 1)
    #[arg(long)]
    to_slot: Option<String>,
}

#[derive(Parser, Debug)]
struct ActionCssColumns {
    uasset: PathBuf,
    #[arg(long)]
    cols: i32,
    #[arg(long)]
    out: PathBuf,
}

#[derive(Parser, Debug)]
struct ActionCssRetarget {
    #[arg(long)]
    game_dir: Option<PathBuf>,
    #[arg(long)]
    mods_dir: Option<PathBuf>,
    #[arg(long)]
    extra: Vec<PathBuf>,
    #[arg(long)]
    custom: PathBuf,
    #[arg(long)]
    out: PathBuf,
}

#[derive(Parser, Debug)]
struct ActionCombo {
    #[arg(long)]
    game_dir: Option<PathBuf>,
    #[arg(long)]
    mods_dir: Option<PathBuf>,
    #[arg(long)]
    csse: PathBuf,
    #[arg(long)]
    separate: PathBuf,
    /// only report the two tables' hashes
    #[arg(long)]
    check: bool,
    #[arg(long)]
    out: Option<PathBuf>,
}

#[derive(Parser, Debug)]
struct ActionCloneRename {
    input: PathBuf,
    output: PathBuf,
    pairs: Vec<String>,
}

#[derive(Parser, Debug)]
struct ActionMergeTables {
    #[arg(long)]
    csse: PathBuf,
    #[arg(long)]
    separate: PathBuf,
    #[arg(long)]
    out: PathBuf,
}

#[derive(Parser, Debug)]
struct ActionDumpExports {
    uasset: PathBuf,
}

#[derive(Parser, Debug)]
struct ActionVoices {
    /// legacy .pak files
    paks: Vec<PathBuf>,
}

#[derive(Parser, Debug)]
struct ActionCssTables {
    #[arg(long)]
    game_dir: Option<PathBuf>,
    #[arg(long)]
    mods_dir: Option<PathBuf>,
    #[arg(long)]
    write: Option<PathBuf>,
}

#[derive(Parser, Debug)]
struct ActionGui {
    /// Mod .utoc files (enabled config), highest priority first
    #[arg(index = 1)]
    utocs: Vec<PathBuf>,
    #[arg(long)]
    game_dir: Option<PathBuf>,
    #[arg(long)]
    mods_dir: Option<PathBuf>,
    #[arg(long, default_value_t = 0)]
    src: usize,
    #[arg(long, default_value_t = 0)]
    tgt: usize,
    #[arg(long, default_value = "")]
    name: String,
    #[arg(long, default_value = "")]
    author: String,
    #[arg(long)]
    out_name: Option<String>,
    #[arg(long)]
    thumb: Option<PathBuf>,
    #[arg(long)]
    no_icons: bool,
    /// write the ported container into this folder instead of creating a new mod
    #[arg(long)]
    into_dir: Option<PathBuf>,
    /// other enabled mods that change the racer tables (e.g. Separate, Ai Skins!), highest priority first
    #[arg(long)]
    extra: Vec<PathBuf>,
}

fn emit(v: serde_json::Value) {
    println!("@@{}", v);
    let _ = std::io::stdout().flush();
}

fn action_classify(a: ActionGui) -> Result<()> {
    match port::resolve_env(a.game_dir.clone(), a.mods_dir.clone(), config_aes_string(), a.utocs.first().map(|p| p.as_path()), false) {
        Ok(env) => {
            for u in &a.utocs {
                let r = port::classify(&env, u);
                match r {
                    Ok(c) => emit(serde_json::json!({"type":"classify","file":u,"skin":c.skin,"slots":c.slots})),
                    Err(e) => emit(serde_json::json!({"type":"classify","file":u,"skin":null,"slots":[],"err":format!("{e:#}")})),
                }
            }
            emit(serde_json::json!({"type":"done"}));
        }
        Err(e) => emit(serde_json::json!({"type":"error","msg":format!("{e:#}")})),
    }
    Ok(())
}

fn action_gui(a: ActionGui, inspect: bool) -> Result<()> {
    let run = || -> Result<()> {
        let mut env = port::resolve_env(a.game_dir.clone(), a.mods_dir.clone(), config_aes_string(), a.utocs.first().map(|p| p.as_path()), false)?;
        env.extra = a.extra.clone();
        if a.utocs.is_empty() {
            emit(serde_json::json!({"type":"env","game_dir":env.game_dir,"mods_dir":env.mods_dir,"csse":env.csse}));
            return Ok(());
        }
        if inspect {
            let v = port::json_inspect(&env, &a.utocs)?;
            emit(serde_json::json!({"type":"inspect","data":v}));
        } else {
            let l = port::load(&env, &a.utocs)?;
            let req = port::PortRequest { src_index: a.src, tgt_index: a.tgt, mod_name: a.name.clone(), author: a.author.clone(), out_name: a.out_name.clone(), copy_icons: !a.no_icons, thumb: a.thumb.clone(), into_dir: a.into_dir.clone() };
            let r = port::do_port(&env, &l, &req, &|m| emit(serde_json::json!({"type":"log","msg":m})))?;
            emit(serde_json::json!({"type":"done","out_dir":r.out_dir,"out_name":r.out_name,"had_icons":r.had_icons,
                "copied": r.copied.iter().map(|(a,b,m)| serde_json::json!({"from":a,"to":b,"mod":m})).collect::<Vec<_>>(),
                "skipped": r.skipped}));
        }
        Ok(())
    };
    if let Err(e) = run() {
        emit(serde_json::json!({"type":"error","msg":format!("{e:#}")}));
    }
    Ok(())
}

#[derive(Parser, Debug)]
struct ActionPort {
    /// Mod folder or .utoc
    #[arg(index = 1)]
    input: PathBuf,
    #[arg(long)]
    game_dir: Option<PathBuf>,
    #[arg(long)]
    mods_dir: Option<PathBuf>,
    /// 1-based answers for non-interactive use
    #[arg(long)]
    file: Option<usize>,
    #[arg(long)]
    source: Option<usize>,
    #[arg(long)]
    target: Option<usize>,
}

#[derive(Parser, Debug)]
#[clap(version)]
struct Args {
    #[arg(short, long)]
    aes_key: Option<String>,
    #[arg(long)]
    override_container_header_version: Option<EIoContainerHeaderVersion>,
    #[arg(long)]
    override_toc_version: Option<EIoStoreTocVersion>,
    #[command(subcommand)]
    action: Action,
}

fn pause() {
    print!("\nPress Enter to close...");
    let _ = std::io::stdout().flush();
    let mut s = String::new();
    let _ = std::io::stdin().read_line(&mut s);
}

fn main() -> Result<()> {
    // Drag-and-drop / double-click mode
    let raw: Vec<String> = std::env::args().collect();
    let exe_name = std::env::current_exe().ok().and_then(|p| p.file_stem().map(|s| s.to_string_lossy().to_lowercase())).unwrap_or_default();
    if raw.len() == 1 && exe_name.contains("skincss") {
        println!("Crossworld Skin & CSS Manager engine\n\nOpen Crossworld Skin & CSS Manager.exe instead.");
        pause();
        return Ok(());
    }
    if raw.len() == 2 && Path::new(&raw[1]).exists() {
        let res = port::run_port(port::PortOptions {
            input: PathBuf::from(&raw[1]),
            game_dir: None, mods_dir: None, aes: None,
            container_choice: None, source_choice: None, target_choice: None,
            interactive: true,
        });
        if let Err(e) = &res {
            println!("\nSomething went wrong: {e:#}");
        }
        pause();
        return Ok(());
    }

    let args = Args::parse();

    let mut config = Config {
        container_header_version_override: args.override_container_header_version,
        toc_version_override: args.override_toc_version,
        ..Default::default()
    };
    if let Some(aes) = args.aes_key {
        config.aes_keys.insert(FGuid::default(), AesKey::from_str(&aes)?);
    }
    let config = Arc::new(config);

    match args.action {
        Action::Manifest(action) => action_manifest(action, config),
        Action::Info(action) => action_info(action, config),
        Action::List(action) => action_list(action, config),
        Action::Verify(action) => action_verify(action, config),
        Action::Unpack(action) => action_unpack(action, config),

        Action::UnpackRaw(action) => action_unpack_raw(action, config),
        Action::PackRaw(action) => action_pack_raw(action, config),

        Action::ToLegacy(action) => action_to_legacy(action, config),
        Action::ToZen(action) => action_to_zen(action, config),

        Action::Get(action) => action_get(action, config),

        Action::DumpTest(action) => action_dump_test(action, config),

        Action::GenScriptObjects(action) => action_gen_script_objects(action, config),

        Action::PrintScriptObjects(action) => action_print_script_objects(action, config),

        Action::AssetRegistry(action) => action_asset_registry(action, config),
        Action::GuiInspect(a) => action_gui(a, true),
        Action::GuiClassify(a) => action_classify(a),
        Action::GuiVoiceTable(a) => {
            let run = || -> Result<()> {
                let mut env = port::resolve_env(a.game_dir.clone(), a.mods_dir.clone(), config_aes_string(), None, false)?;
                env.extra = a.extra.clone();
                let slots: Vec<(String, String)> = a.slots.iter().filter_map(|s| s.split_once('=').map(|(x, y)| (x.to_string(), y.to_string()))).collect();
                port::voice_table(&env, &slots, &a.out, &|m| emit(serde_json::json!({"type":"log","msg":m})))
            };
            match run() {
                Ok(()) => emit(serde_json::json!({"type":"done"})),
                Err(e) => emit(serde_json::json!({"type":"error","msg":format!("{e:#}")})),
            }
            Ok(())
        }
        Action::GuiNameTable(a) => {
            let run = || -> Result<()> {
                let env = port::resolve_env(a.game_dir.clone(), a.mods_dir.clone(), config_aes_string(), None, false)?;
                port::write_name_table(&env, &a.sources, &a.table, &a.value, &a.out)
            };
            match run() {
                Ok(()) => emit(serde_json::json!({"type":"done"})),
                Err(e) => emit(serde_json::json!({"type":"error","msg":format!("{e:#}")})),
            }
            Ok(())
        }
        Action::GuiCombo(a) => {
            let run = || -> Result<serde_json::Value> {
                let env = port::resolve_env(a.game_dir.clone(), a.mods_dir.clone(), config_aes_string(), None, false)?;
                let hashes = serde_json::json!({"csse": port::dt01_hash(&env, &a.csse)?, "separate": port::dt01_hash(&env, &a.separate)?});
                if a.check {
                    return Ok(serde_json::json!({"type":"done","hashes":hashes}));
                }
                let out = a.out.clone().context("--out is needed")?;
                let msg = port::combo_build(&env, &a.csse, &a.separate, &out)?;
                Ok(serde_json::json!({"type":"done","hashes":hashes,"msg":msg}))
            };
            match run() {
                Ok(v) => emit(v),
                Err(e) => emit(serde_json::json!({"type":"error","msg":format!("{e:#}")})),
            }
            Ok(())
        }
        Action::GuiCssRetarget(a) => {
            let run = || -> Result<()> {
                let mut env = port::resolve_env(a.game_dir.clone(), a.mods_dir.clone(), config_aes_string(), None, false)?;
                env.extra = a.extra.clone();
                port::css_retarget(&env, &a.custom, &a.out, &|m| emit(serde_json::json!({"type":"log","msg":m})))
            };
            match run() {
                Ok(()) => emit(serde_json::json!({"type":"done"})),
                Err(e) => emit(serde_json::json!({"type":"error","msg":format!("{e:#}")})),
            }
            Ok(())
        }
        Action::DumpExports(a) => port::dump_exports(&a.uasset),
        Action::CloneRename(a) => {
            let pairs: Vec<(String, String)> = a.pairs.iter().filter_map(|x| x.split_once('=').map(|(f, t)| (f.to_string(), t.to_string()))).collect();
            println!("{} packages", port::clone_rename(&a.input, &a.output, &pairs)?);
            Ok(())
        }
        Action::MergeTables(a) => {
            println!("{}", port::merge_driver_tables(&a.csse, &a.separate, &a.out)?);
            Ok(())
        }
        Action::CssColumns(a) => {
            println!("{}", port::css_columns(&a.uasset, a.cols, &a.out)?);
            Ok(())
        }
        Action::GuiVoices(a) => {
            let mut out = serde_json::Map::new();
            for p in &a.paks {
                out.insert(p.to_string_lossy().to_string(), serde_json::json!(voice::list_voices(p)));
            }
            emit(serde_json::json!({"type":"done","voices":out}));
            Ok(())
        }
        Action::GuiVoice(a) if a.to_slot.is_some() => {
            match voice::slot_voice(&a.paks, a.to_slot.as_deref().unwrap(), &a.out) {
                Ok(v) => emit(serde_json::json!({"type":"done","written":v.written,"notes":v.notes})),
                Err(e) => emit(serde_json::json!({"type":"error","msg":format!("{e:#}")})),
            }
            Ok(())
        }
        Action::GuiVoice(a) => {
            match voice::port_voice(&a.paks, if a.to_regular { Some(true) } else if a.to_ai { Some(false) } else { None }, &a.out) {
                Ok(v) => emit(serde_json::json!({"type":"done","written":v.written,"notes":v.notes})),
                Err(e) => emit(serde_json::json!({"type":"error","msg":format!("{e:#}")})),
            }
            Ok(())
        }
        Action::GuiCssTables(a) => {
            let run = || -> Result<Vec<String>> {
                let env = port::resolve_env(a.game_dir.clone(), a.mods_dir.clone(), config_aes_string(), None, false)?;
                port::css_tables(&env, a.write.as_deref())
            };
            match run() {
                Ok(h) => emit(serde_json::json!({"type":"done","hashes":h})),
                Err(e) => emit(serde_json::json!({"type":"error","msg":format!("{e:#}")})),
            }
            Ok(())
        }
        Action::GuiModIcons(a) => {
            let run = || -> Result<serde_json::Value> {
                let env = port::resolve_env(a.game_dir.clone(), a.mods_dir.clone(), config_aes_string(), a.utocs.first().map(|p| p.as_path()), false)?;
                port::json_mod_icons(&env, &a.utocs)
            };
            match run() {
                Ok(v) => emit(serde_json::json!({"type":"done","icons":v})),
                Err(e) => emit(serde_json::json!({"type":"error","msg":format!("{e:#}")})),
            }
            Ok(())
        }
        Action::GuiExtract(a) => {
            match extract::extract(&a.archive, &a.dest, &|m| emit(serde_json::json!({"type":"log","msg":m}))) {
                Ok(()) => emit(serde_json::json!({"type":"done","out_dir":a.dest})),
                Err(e) => emit(serde_json::json!({"type":"error","msg":format!("{e:#}")})),
            }
            Ok(())
        }
        Action::GuiPort(a) => action_gui(a, false),
        Action::Port(a) => port::run_port(port::PortOptions {
            input: a.input,
            game_dir: a.game_dir,
            mods_dir: a.mods_dir,
            aes: config_aes_string(),
            container_choice: a.file.map(|x| x - 1),
            source_choice: a.source.map(|x| x - 1),
            target_choice: a.target.map(|x| x - 1),
            interactive: false,
        }),
    }
}

fn action_manifest(args: ActionManifest, config: Arc<Config>) -> Result<()> {
    let iostore = iostore::open(args.utoc, config)?;

    let entries = Arc::new(Mutex::new(vec![]));

    let container_header_version = iostore.container_header_version().unwrap();
    let toc_version = iostore.container_file_version().unwrap();

    iostore.packages().par_bridge().try_for_each(|package_info| -> Result<()> {
        let chunk_id = FIoChunkId::from_package_id(package_info.id(), 0, EIoChunkType::ExportBundleData).with_version(toc_version);
        let package_path = iostore.chunk_path(chunk_id).with_context(|| format!("{:?} has no path name entry", package_info.id()))?;
        let data = package_info.container().read(chunk_id)?;

        let package_name = get_package_name(&data, container_header_version).with_context(|| package_path.to_string())?;

        let mut entry = manifest::Op {
            packagestoreentry: manifest::PackageStoreEntry { packagename: package_name },
            packagedata: vec![manifest::ChunkData {
                id: chunk_id.get_raw(),
                filename: package_path.to_string(),
            }],
            bulkdata: vec![],
        };

        let bulk_id = FIoChunkId::from_package_id(package_info.id(), 0, EIoChunkType::BulkData).with_version(toc_version);
        if iostore.has_chunk_id(bulk_id) {
            entry.bulkdata.push(manifest::ChunkData {
                id: bulk_id.get_raw(),
                filename: UEPath::new(&package_path).with_extension("ubulk").to_string(),
            });
        }

        entries.lock().unwrap().push(entry);
        Ok(())
    })?;

    let mut entries = Arc::into_inner(entries).unwrap().into_inner().unwrap();
    //entries.sort_by_key(|op| op.packagedata.first().map(|c| c.filename.clone()));
    entries.sort_by(|a, b| a.packagestoreentry.packagename.cmp(&b.packagestoreentry.packagename));

    let manifest = manifest::PackageStoreManifest { oplog: manifest::OpLog { entries } };

    let path = "pakstore.json";
    fs::write(path, serde_json::to_vec(&manifest)?)?;

    println!("wrote {} entries to {}", manifest.oplog.entries.len(), path);

    Ok(())
}

fn action_info(args: ActionInfo, config: Arc<Config>) -> Result<()> {
    let iostore = iostore::open(args.path, config)?;
    iostore.print_info(0);
    Ok(())
}

fn action_list(args: ActionList, config: Arc<Config>) -> Result<()> {
    let iostore = iostore::open(args.utoc, config)?;

    let chunks = if args.all { iostore.chunks_all() } else { iostore.chunks() };

    for chunk in chunks {
        let id = chunk.id();
        let chunk_type = id.get_chunk_type();

        let package_id: Cow<str> = if chunk_type == EIoChunkType::ExportBundleData { FPackageId(id.get_chunk_id()).0.to_string().into() } else { "-".into() };

        use std::fmt::Write;
        let mut line = String::new();
        let mut first = true;

        macro_rules! column {
            ($($arg:tt)*) => {
                if !first { write!(&mut line, " ").unwrap(); }
                #[allow(unused)]
                { first = false; }
                write!(&mut line, $($arg)*).unwrap()
            };
        }

        column!("{:30}", chunk.container().container_name());
        column!("{}", hex::encode(id.get_raw()));
        if args.hash {
            column!("{}", hex::encode(chunk.hash().0));
        }
        if args.package {
            column!("{:20}", package_id);
        }
        column!("{:20}", chunk_type.as_ref());
        if args.size {
            column!("{:10}", chunk.size());
        }
        if args.path {
            column!("{}", chunk.path().as_deref().unwrap_or("-"));
        }
        if args.store {
            let package_store_entry = if chunk_type == EIoChunkType::ExportBundleData {
                let entry = chunk.container().package_store_entry(id.get_package_id()).unwrap();
                format!("{entry:?}")
            } else {
                "-".to_string()
            };
            column!("{:?}", package_store_entry);
        }

        println!("{line}");
    }
    Ok(())
}

fn action_verify(args: ActionVerify, config: Arc<Config>) -> Result<()> {
    let mut stream = BufReader::new(fs::File::open(&args.utoc)?);
    let ucas = &args.utoc.with_extension("ucas");

    let toc: Toc = stream.de_ctx(config)?;

    // most of these don't match?!
    //let sigs = &toc.signatures.as_ref().unwrap().chunk_block_signatures;
    //let mut rdr = BufReader::new(fs::File::open(ucas)?);
    //for (i, b) in toc.compression_blocks.iter().enumerate() {
    //    rdr.seek(SeekFrom::Start(b.get_offset()))?;

    //    let data: Vec<u8> = rdr.ser_ctx(b.get_compressed_size() as usize)?;

    //    use sha1::{Digest, Sha1};
    //    let mut hasher = Sha1::new();
    //    hasher.update(&data);
    //    let hash = hasher.finalize();

    //    println!(
    //        "{:?} {} {} {}",
    //        sigs[i],
    //        hex::encode_upper(hash),
    //        b.get_compression_method_index(),
    //        sigs[i].data == hash.as_ref()
    //    );
    //}

    toc.chunk_metas.par_iter().enumerate().try_for_each_init(
        || BufReader::new(fs::File::open(ucas).unwrap()),
        |ucas, (i, meta)| -> Result<()> {
            //let chunk_id = toc.chunks[i];
            let data = toc.read(ucas, i as u32)?;

            //let chunk_type = chunk_id.get_chunk_type();
            //match chunk_type {
            //    EIoChunkType::ExportBundleData => {
            //        let package_name = get_package_name(&data)?;
            //        let package_id = FPackageId::from_name(&package_name);
            //        let id = FIoChunkId::from_package_id(package_id, 0, chunk_type);
            //        if id != chunk_id {
            //            bail!("chunk ID mismatch")
            //        }
            //    }
            //    _ => {} // TODO
            //}

            let hash = blake3::hash(&data);

            //println!(
            //    "{:>10} {:?} {} {:?}",
            //    i,
            //    hex::encode(meta.chunk_hash.data),
            //    hex::encode(hash.as_bytes()),
            //    meta.flags
            //);

            if meta.chunk_hash.0[..20] != hash.as_bytes()[..20] {
                bail!("hash mismatch for chunk #{i}")
            }

            Ok(())
        },
    )?;

    println!("verified");

    Ok(())
}

fn action_unpack(args: ActionUnpack, config: Arc<Config>) -> Result<()> {
    let mut stream = BufReader::new(fs::File::open(&args.utoc)?);
    let ucas = &args.utoc.with_extension("ucas");

    let toc: Toc = stream.de_ctx(config)?;

    let output = args.output;

    // TODO extract entries not found in directory index
    // TODO output chunk id manifest
    toc.file_map.keys().par_bridge().try_for_each_init(
        || BufReader::new(fs::File::open(ucas).unwrap()),
        |ucas, file_name| -> Result<()> {
            if args.verbose {
                println!("{file_name}");
            }
            let data = toc.read(ucas, toc.file_map[file_name])?;

            let path = output.join(file_name);
            let dir = path.parent().unwrap();
            fs::create_dir_all(dir)?;
            fs::write(path, &data)?;
            Ok(())
        },
    )?;

    println!("unpacked {} files to {}", toc.file_map.len(), output.to_string_lossy());

    Ok(())
}

mod raw {
    use std::collections::HashMap;

    use serde::{Deserialize, Deserializer, Serialize, Serializer};

    use retoc::{EIoStoreTocVersion, FIoChunkIdRaw};

    #[derive(Serialize, Deserialize)]
    pub(crate) struct RawIoManifest {
        pub(crate) chunk_paths: HashMap<ChunkId, String>,
        pub(crate) version: EIoStoreTocVersion,
        pub(crate) mount_point: String,
    }
    #[derive(Serialize, Deserialize, PartialEq, Eq, PartialOrd, Ord, Hash)]
    pub(crate) struct ChunkId(#[serde(serialize_with = "to_hex", deserialize_with = "from_hex")] pub(crate) FIoChunkIdRaw);
    impl From<FIoChunkIdRaw> for ChunkId {
        fn from(value: FIoChunkIdRaw) -> Self {
            Self(value)
        }
    }

    fn to_hex<S>(chunk_id: &FIoChunkIdRaw, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        serializer.serialize_str(&hex::encode(chunk_id.id))
    }
    fn from_hex<'de, D>(deserializer: D) -> Result<FIoChunkIdRaw, D::Error>
    where
        D: Deserializer<'de>,
    {
        let s: String = Deserialize::deserialize(deserializer)?;
        let v = hex::decode(s).map_err(serde::de::Error::custom)?;
        Ok(FIoChunkIdRaw {
            id: v.try_into().map_err(|v: Vec<u8>| serde::de::Error::invalid_length(v.len(), &"a 12 byte hex string"))?,
        })
    }
}

fn action_unpack_raw(args: ActionUnpackRaw, config: Arc<Config>) -> Result<()> {
    let iostore = iostore::open(args.utoc, config)?;

    let output = args.output;
    let chunks_dir = output.join("chunks");
    let manifest_path = output.join("manifest.json");

    fs::create_dir(&output)?;
    fs::create_dir(&chunks_dir)?;

    let mut manifest = raw::RawIoManifest {
        chunk_paths: Default::default(),
        version: iostore.container_file_version().unwrap(),
        mount_point: "../../../".to_string(),
    };

    for chunk in iostore.chunks() {
        let data = chunk.read()?;
        fs::write(chunks_dir.join(hex::encode(chunk.id().get_raw())), data)?;
        if let Some(path) = chunk.path() {
            manifest.chunk_paths.insert(chunk.id().get_raw().into(), path);
        }
    }

    serde_json::to_writer_pretty(BufWriter::new(fs::File::create(manifest_path)?), &manifest)?;

    println!("unpacked {} chunks to {}", iostore.chunks().count(), output.to_string_lossy());

    Ok(())
}

fn action_pack_raw(args: ActionPackRaw, _config: Arc<Config>) -> Result<()> {
    let manifest: raw::RawIoManifest = serde_json::from_reader(BufReader::new(fs::File::open(args.input.join("manifest.json"))?))?;

    let mut writer = IoStoreWriter::new(args.utoc, manifest.version, None, manifest.mount_point.into())?;
    for entry in args.input.join("chunks").read_dir()? {
        let entry = entry?;
        let chunk_id = FIoChunkIdRaw::from_str(entry.file_name().to_string_lossy().as_ref())?;
        let path = manifest.chunk_paths.get(&chunk_id.into()).map(UEPath::new);
        let data = fs::read(entry.path())?;
        writer.write_chunk_raw(chunk_id, path, &data)?;
    }
    writer.finalize()?;
    Ok(())
}

fn action_to_legacy(args: ActionToLegacy, config: Arc<Config>) -> Result<()> {
    let log = Log::new_stdout(args.verbose, args.debug);
    if args.dry_run {
        action_to_legacy_inner(args, config, &NullFileWriter, &log)?;
    } else if args.output.extension() == Some(std::ffi::OsStr::new("pak")) {
        let mut file = BufWriter::new(fs::File::create(&args.output)?);
        let mut pak = repak::PakBuilder::new().compression([repak::Compression::Oodle]).writer(
            &mut file,
            repak::Version::V11, // TODO V11 is compatible with most IO store versions but will need to be changed for <= 4.26
            "../../../".to_string(),
            None,
        );

        // some stack space to store action result
        let mut result = None;
        let result_ref = &mut result;
        rayon::in_place_scope(|scope| -> Result<()> {
            let (tx, rx) = std::sync::mpsc::sync_channel(0);

            let writer = ParallelPakWriter { entry_builder: pak.entry_builder(), tx };

            scope.spawn(move |_| {
                *result_ref = Some(action_to_legacy_inner(args, config, &writer, &log));
            });

            for (path, entry) in rx {
                pak.write_entry(path, entry)?;
            }
            Ok(())
        })?;
        result.unwrap()?; // unwrap action result and return error if occured

        pak.write_index()?;
    } else {
        let file_writer = FSFileWriter::new(&args.output);
        action_to_legacy_inner(args, config, &file_writer, &log)?;
    }

    Ok(())
}

fn action_to_legacy_inner(args: ActionToLegacy, config: Arc<Config>, file_writer: &dyn FileWriterTrait, log: &Log) -> Result<()> {
    let iostore = iostore::open(&args.input, config.clone())?;
    if !args.no_assets {
        action_to_legacy_assets(&args, file_writer, &*iostore, log)?;
    }
    if !args.no_shaders {
        action_to_legacy_shaders(&args, file_writer, &*iostore, log)?;
    }
    if !args.no_script_objects && iostore.container_file_version().is_some() && iostore.container_file_version().unwrap() > EIoStoreTocVersion::PerfectHash {
        let script_objects = iostore.load_script_objects()?;
        let mut script_objects_buffer: Vec<u8> = Vec::new();
        script_objects.serialize_new(&mut Cursor::new(&mut script_objects_buffer))?;
        file_writer.write_file(String::from("scriptobjects.bin"), false, script_objects_buffer)?;
    }
    Ok(())
}

fn progress_style() -> indicatif::ProgressStyle {
    indicatif::ProgressStyle::with_template("[{elapsed_precise}] {bar:40.cyan/blue} {pos:>7}/{len:7} {wide_msg}").unwrap().progress_chars("##-")
}

fn action_to_legacy_assets(args: &ActionToLegacy, file_writer: &dyn FileWriterTrait, iostore: &dyn IoStoreTrait, log: &Log) -> Result<()> {
    let mut packages_to_extract = vec![];
    for package_info in iostore.packages() {
        let chunk_id = FIoChunkId::from_package_id(package_info.id(), 0, EIoChunkType::ExportBundleData);
        let package_path = iostore.chunk_path(chunk_id).with_context(|| format!("{:?} has no path name entry. Cannot extract", package_info.id()))?;

        if !args.filter.is_empty() && !args.filter.iter().any(|f| package_path.contains(f)) {
            continue;
        }

        packages_to_extract.push((package_info, package_path));
    }

    // Construct Verse Cell Store and populate it with intrinsics, as well as command line overrides
    let script_cell_store = build_verse_cell_store(&args.script_cell);

    let package_file_version: Option<FPackageFileVersion> = args.version.map(|v| v.package_file_version());
    let package_context = FZenPackageContext::create(iostore, package_file_version, log, Some(script_cell_store.clone()));

    let count = packages_to_extract.len();
    let failed_count = AtomicUsize::new(0);
    let progress = Some(indicatif::ProgressBar::new(count as u64).with_style(progress_style()));
    log.set_progress(progress.as_ref());
    let prog_ref = progress.as_ref();

    let process = |(package_info, package_path): &(PackageInfo, String)| -> Result<()> {
        verbose!(log, "{package_path}");

        // TODO make configurable
        let path = package_path.strip_prefix("../../../").with_context(|| format!("failed to strip mount prefix from {package_path}"))?;

        prog_ref.inspect(|p| p.set_message(path.to_string()));

        let res = asset_conversion::build_legacy(&package_context, package_info.id(), UEPath::new(&path), file_writer).with_context(|| format!("Failed to convert {}", package_path.clone()));
        if let Err(err) = res {
            info!(log, "{err:#}");
            failed_count.fetch_add(1, Ordering::SeqCst);
        }
        prog_ref.inspect(|p| p.inc(1));
        Ok(())
    };

    if args.no_parallel {
        packages_to_extract.iter().try_for_each(process)?;
    } else {
        packages_to_extract.par_iter().try_for_each(process)?;
    }

    prog_ref.inspect(|p| p.finish_with_message(""));
    log.set_progress(None);

    let failed_count = failed_count.load(Ordering::SeqCst);
    info!(log, "Extracted {} ({failed_count} failed) legacy assets to {:?}", count - failed_count, args.output);

    Ok(())
}

fn action_to_legacy_shaders(args: &ActionToLegacy, file_writer: &dyn FileWriterTrait, iostore: &dyn IoStoreTrait, log: &Log) -> Result<()> {
    let compress_shaders = !args.no_compres_shaders;
    let mut libraries_extracted = 0;
    for chunk_info in iostore.chunks().filter(|x| x.id().get_chunk_type() == EIoChunkType::ShaderCodeLibrary) {
        let shader_library_path = chunk_info.path().with_context(|| format!("Failed to retrieve pathname for shader library chunk {:?}", chunk_info.id()))?;

        if !args.filter.is_empty() && !args.filter.iter().any(|f| shader_library_path.contains(f)) {
            continue;
        }
        verbose!(log, "Extracting Shader Library: {shader_library_path}");
        // TODO make configurable
        let path = shader_library_path.strip_prefix("../../../").with_context(|| format!("failed to strip mount prefix from {shader_library_path}"))?;

        let shader_asset_info_path = get_shader_asset_info_filename_from_library_filename(path)?;
        let (shader_library_buffer, shader_asset_info_buffer) = rebuild_shader_library_from_io_store(iostore, chunk_info.id(), log, compress_shaders)?;
        file_writer.write_file(path.to_string(), false, shader_library_buffer)?;
        file_writer.write_file(shader_asset_info_path, compress_shaders, shader_asset_info_buffer)?;
        libraries_extracted += 1;
    }

    info!(log, "Extracted {} shader code libraries to {:?}", libraries_extracted, args.output);

    Ok(())
}

fn action_to_zen(args: ActionToZen, config: Arc<Config>) -> Result<()> {
    let mount_point = UEPath::new("../../../");

    let input: Box<dyn FileReaderTrait> = if args.input.is_dir() { Box::new(FSFileReader::new(args.input)) } else { Box::new(PakFileReader::new(args.input)?) };

    let container_header_version = config.container_header_version_override.unwrap_or(args.version.container_header_version());

    let toc_version = config.toc_version_override.unwrap_or(args.version.toc_version());

    let mut writer = IoStoreWriter::new(&args.output, toc_version, Some(container_header_version), mount_point.into())?;

    let log = Log::new_stdout(args.verbose, args.debug);
    let mut asset_paths = vec![];
    let mut shader_lib_paths = vec![];
    let mut script_objects: Option<Arc<ZenScriptObjects>> = None;

    let check_path = |path: &UEPath| {
        if args.filter.is_empty() { true } else { args.filter.iter().any(|f| path.as_str().contains(f)) }
    };

    let files = input.list_files()?;
    let files_set: HashSet<&UEPathBuf> = HashSet::from_iter(files.iter());

    for path in &files {
        let ue_path = UEPath::new(&path);
        let ext = ue_path.extension();
        let is_asset = [Some("uasset"), Some("umap")].contains(&ext);
        if is_asset && check_path(path) {
            let uexp = ue_path.with_extension("uexp");
            if files_set.contains(&uexp) {
                asset_paths.push(path);
            } else {
                info!(&log, "Skipping {path} because it does not have a split exports file. Are you sure the package is cooked?");
            }
        }
        let is_shader_lib = Some("ushaderbytecode") == ext;
        if is_shader_lib && check_path(path) {
            shader_lib_paths.push(path);
        }
        // If folder we are given contains copy of script objects, parse them and use them for VNI support and import checking
        if toc_version > EIoStoreTocVersion::PerfectHash && path.file_name() == Some("scriptobjects.bin") {
            let script_object_buffer = input.read(path).with_context(|| format!("Failed to read script objects file: {}", path))?;
            script_objects = Some(Arc::new(ZenScriptObjects::deserialize_new(&mut Cursor::new(script_object_buffer))?));
        }
    }

    // Convert shader libraries first, since the data contained in their asset metadata is needed to build the package store entries
    let mut package_name_to_referenced_shader_maps: HashMap<String, Vec<FSHAHash>> = HashMap::new();
    for path in shader_lib_paths {
        info!(&log, "converting shader library {path}");
        let shader_library_buffer = input.read(path)?;
        let path = UEPath::new(&path);
        let asset_metadata_filename = UEPathBuf::from(get_shader_asset_info_filename_from_library_filename(path.file_name().unwrap())?);
        let asset_metadata_path = path.parent().map(|x| x.join(&asset_metadata_filename)).unwrap_or(asset_metadata_filename);

        // Read asset metadata and store it into the global map to be picked up by zen packages later
        if let Some(shader_asset_info_buffer) = input.read_opt(&asset_metadata_path)? {
            shader_library::read_shader_asset_info(&shader_asset_info_buffer, &mut package_name_to_referenced_shader_maps)?;
        }

        // Convert shader library to the container shader chunks
        shader_library::write_io_store_library(&mut writer, &shader_library_buffer, &mount_point.join(path), &log)?;
    }

    // Construct Verse Cell Store and populate it with intrinsics, as well as command line overrides
    let script_cell_store = build_verse_cell_store(&args.script_cell);

    let progress = Some(indicatif::ProgressBar::new(asset_paths.len() as u64).with_style(progress_style()));
    log.set_progress(progress.as_ref());
    let prog_ref = progress.as_ref();

    log.set_progress(prog_ref);

    // Convert assets now
    let container_header_version = writer.container_header_version();
    // Decide whenever we need all packages to be in memory at the same time to perform the fixup or not
    let needs_asset_import_fixup = container_header_version <= EIoContainerHeaderVersion::Initial;

    let process_assets = |tx: std::sync::mpsc::SyncSender<ConvertedZenAssetBundle>| -> Result<()> {
        let process = |path: &&UEPathBuf| -> Result<()> {
            verbose!(&log, "converting asset {path}");

            prog_ref.inspect(|p| p.set_message(path.to_string()));

            let bundle = FSerializedAssetBundle {
                asset_file_buffer: input.read(path)?,
                exports_file_buffer: input.read(&path.with_extension("uexp"))?,
                bulk_data_buffer: input.read_opt(&path.with_extension("ubulk"))?,
                optional_bulk_data_buffer: input.read_opt(&path.with_extension("uptnl"))?,
                memory_mapped_bulk_data_buffer: input.read_opt(&path.with_extension("m.ubulk"))?,
            };

            let converted = zen_asset_conversion::build_zen_asset(
                bundle,
                &package_name_to_referenced_shader_maps,
                &mount_point.join(path),
                Some(args.version.package_file_version()),
                container_header_version,
                needs_asset_import_fixup,
                script_objects.clone(),
                Some(script_cell_store.clone()),
                &log,
            )?;

            tx.send(converted)?;

            prog_ref.inspect(|p| p.inc(1));
            Ok(())
        };

        if args.no_parallel { asset_paths.iter().try_for_each(process) } else { asset_paths.par_iter().try_for_each(process) }
    };
    let mut result = None;
    let result_ref = &mut result;
    rayon::in_place_scope(|scope| -> Result<()> {
        let (tx, rx) = std::sync::mpsc::sync_channel(0);

        scope.spawn(|_| {
            *result_ref = Some(process_assets(tx));
        });

        if needs_asset_import_fixup {
            let mut converted_lookup: HashMap<FPackageId, Arc<RwLock<ConvertedZenAssetBundle>>> = HashMap::new();
            let mut all_converted: Vec<Arc<RwLock<ConvertedZenAssetBundle>>> = Vec::new();
            let mut total_package_data_size: usize = 0;

            // Collect all assets into the lookup map first, and also into the processing list
            for mut converted in rx {
                // Write and release bulk data immediately, we do not have enough RAM to keep all the bulk data for all the packages in memory at the same time
                converted.write_and_release_bulk_data(&mut writer)?;
                total_package_data_size += converted.package_data_size();

                // Add the package data and the metadata necessary for the import fixup into the list
                let converted_arc = Arc::new(RwLock::new(converted));
                converted_lookup.insert(converted_arc.read().unwrap().package_id, converted_arc.clone());
                all_converted.push(converted_arc);
            }

            info!(log, "Applying import fix-ups to the converted assets. Package data in memory: {}MB", total_package_data_size / 1024 / 1024);
            prog_ref.inspect(|x| x.set_position(0));

            // Process fixups on all the assets in their original processing order
            for converted in &all_converted {
                converted.write().unwrap().fixup_legacy_external_arcs(&converted_lookup, &log)?;
                prog_ref.inspect(|x| x.inc(1));
            }
            info!(log, "Writing converted assets");
            prog_ref.inspect(|x| x.set_position(0));

            // Write all the package data for each asset once fixups are complete
            for converted in &all_converted {
                converted.write().unwrap().write_package_data(&mut writer)?;
                prog_ref.inspect(|x| x.inc(1));
            }
        } else {
            // Write the assets immediately otherwise as they are processed
            for mut converted in rx {
                converted.write(&mut writer)?;
            }
        }
        Ok(())
    })?;
    result.unwrap()?;

    prog_ref.inspect(|p| p.finish_with_message(""));
    log.set_progress(None);

    writer.finalize()?;

    // create empty pak file if one does not already exist (necessary for game to detect and load container)
    let pak_path = Path::new(&args.output).with_extension("pak");
    if !pak_path.exists() {
        repak::PakBuilder::new().writer(&mut BufWriter::new(fs::File::create(pak_path)?), repak::Version::V11, mount_point.to_string(), None).write_index()?;
    }

    Ok(())
}

fn action_get(args: ActionGet, config: Arc<Config>) -> Result<()> {
    let iostore = iostore::open(args.input, config)?;
    let data = iostore.read_raw(args.chunk_id)?;

    let mut output: Box<dyn Write> = if let Some(output) = args.output {
        if output == OsStr::new("-") { Box::new(std::io::stdout()) } else { Box::new(BufWriter::new(fs::File::create(output)?)) }
    } else {
        Box::new(std::io::stdout())
    };

    output.write_all(&data)?;
    Ok(())
}

fn action_dump_test(args: ActionDumpTest, config: Arc<Config>) -> Result<()> {
    let iostore = iostore::open(args.input, config)?;

    let chunk_id = FIoChunkId::from_package_id(args.package_id, 0, EIoChunkType::ExportBundleData);
    let game_path = iostore.chunk_path(chunk_id).context("no path found for package")?;
    let name = UEPath::new(&game_path).file_name().unwrap();
    let path = args.output_dir.join(name);
    let data = iostore.read(chunk_id)?;
    fs::write(args.output_dir.join(name), &data)?;

    let store_entry = iostore.package_store_entry(args.package_id).unwrap();

    let metadata = PackageTestMetadata {
        toc_version: iostore.container_file_version().unwrap(),
        container_header_version: iostore.container_header_version().unwrap(),
        package_file_version: None,
        store_entry: Some(store_entry),
    };

    fs::write(path.with_extension("metadata.json"), serde_json::to_vec_pretty(&metadata)?)?;

    {
        let mut stream = ser_hex::TraceStream::new(path.with_extension("trace.json"), Cursor::new(data));

        let header = FZenPackageHeader::deserialize(&mut stream, metadata.store_entry, metadata.toc_version, metadata.container_header_version, metadata.package_file_version)?;

        fs::write(path.with_extension("header.txt"), format!("{header:#?}"))?;
    }

    let chunk_id = FIoChunkId::from_package_id(args.package_id, 0, EIoChunkType::BulkData);
    if iostore.has_chunk_id(chunk_id) {
        let data = iostore.read(chunk_id)?;
        fs::write(path.with_extension("ubulk"), data)?;
    }

    let chunk_id = FIoChunkId::from_package_id(args.package_id, 0, EIoChunkType::OptionalBulkData);
    if iostore.has_chunk_id(chunk_id) {
        let data = iostore.read(chunk_id)?;
        fs::write(path.with_extension("uptnl"), data)?;
    }

    let chunk_id = FIoChunkId::from_package_id(args.package_id, 0, EIoChunkType::MemoryMappedBulkData);
    if iostore.has_chunk_id(chunk_id) {
        let data = iostore.read(chunk_id)?;
        fs::write(path.with_extension("m.ubulk"), data)?;
    }

    Ok(())
}

fn action_gen_script_objects(args: ActionGenScriptObjects, _config: Arc<Config>) -> Result<()> {
    let dump: jmap::Jmap = serde_json::from_reader(BufReader::new(fs::File::open(&args.input)?))?;

    // map CDO => Class
    let mut cdos = HashMap::<&str, &str>::new();

    let mut names = FNameMap::create(EMappedNameType::Global);
    let mut script_objects = vec![];

    for (path, object) in &dump.objects {
        if path.starts_with("/Script/")
            && let jmap::ObjectType::Class(class) = &object
            && let Some(cdo) = &class.class_default_object
        {
            cdos.insert(cdo, path);
        }
    }

    for (path, object) in &dump.objects {
        if path.starts_with("/Script/") {
            if let jmap::ObjectType::Class(class) = &object
                && let Some(cdo) = &class.class_default_object
            {
                cdos.insert(cdo, path);
            }
            let mut components = path.rsplit(['/', '.', ':']);
            let name = components.next().unwrap();
            let count = components.count();
            let name = if count == 2 {
                // special case for /Script/ package objects
                path
            } else {
                name
            };

            let outer_index = object.get_object().outer.as_ref().map_or(FPackageObjectIndex::create_null(), |outer| FPackageObjectIndex::create_script_import(outer));
            let cdo_class_index = cdos.get(path.as_str()).map_or(FPackageObjectIndex::create_null(), |outer| FPackageObjectIndex::create_script_import(outer));
            script_objects.push(FScriptObjectEntry {
                object_name: names.store(name),
                global_index: FPackageObjectIndex::create_script_import(path),
                outer_index,
                cdo_class_index,
            });
        }
    }

    let mut writer = IoStoreWriter::new(&args.output, args.version.toc_version(), None, UEPathBuf::new())?;

    let use_new_format = args.version.toc_version() > EIoStoreTocVersion::PerfectHash;

    if use_new_format {
        let buf_script_objects = {
            let mut buf = vec![];
            names.serialize(&mut buf)?;
            WriteExt::ser(&mut buf, &script_objects)?;
            buf
        };

        writer.write_chunk(FIoChunkId::create(0, 0, EIoChunkType::ScriptObjects), None, &buf_script_objects)?;
    } else {
        let (buf_names, buf_name_hashes) = write_name_batch_parts(&names.copy_raw_names())?;
        let buf_script_objects = {
            let mut buf = vec![];
            WriteExt::ser(&mut buf, &script_objects)?;
            buf
        };

        writer.write_chunk(FIoChunkId::create(0, 0, EIoChunkType::LoaderGlobalNames), None, &buf_names)?;
        writer.write_chunk(FIoChunkId::create(0, 0, EIoChunkType::LoaderGlobalNameHashes), None, &buf_name_hashes)?;
        writer.write_chunk(FIoChunkId::create(0, 0, EIoChunkType::LoaderInitialLoadMeta), None, &buf_script_objects)?;
    }

    writer.finalize()?;

    println!("Generated script objects container with {} objects using {} format", script_objects.len(), if use_new_format { "new" } else { "old" });

    Ok(())
}

fn action_print_script_objects(args: ActionPrintScriptObjects, config: Arc<Config>) -> Result<()> {
    let iostore = iostore::open(args.input, config)?;
    let script_objects = iostore.load_script_objects()?;
    script_objects.print();
    Ok(())
}

fn action_asset_registry(args: ActionAssetRegistry, _config: Arc<Config>) -> Result<()> {
    use retoc::asset_registry::{AssetRegistry, dbg::Dbg};

    let data = fs::read(&args.input)?;
    let mut cursor = Cursor::new(&data);
    let registry = AssetRegistry::deserialize(&mut cursor)?;

    println!("Asset Registry");
    println!("  Version: {:?}", registry.version);
    println!("  Registry Version: {:?}", registry.registry_version);
    println!("  Assets: {}", registry.asset_data.len());
    println!();

    for asset in &registry.asset_data {
        println!("{:#?}", Dbg::new(&registry, asset));
        println!();
    }

    Ok(())
}

fn config_aes_string() -> Option<String> {
    let a: Vec<String> = std::env::args().collect();
    a.iter().position(|x| x == "-a" || x == "--aes-key").and_then(|i| a.get(i + 1).cloned())
}

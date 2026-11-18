# Crossworld Skin & CSS Manager - source code

by SCWS&CSS

## What's inside

- `app/` : the app window (Electron). Plain HTML / CSS / JavaScript.
  - `main.js` : file work, BlueStar settings, setup checks, voices install
  - `index.html` : main window, `css/index.html` : CSS order editor
  - `modini.js` : reads and writes BlueStar `mod.ini` files
- `ue4ss-mod/CrossworldSkinCSSVoices/` : the UE4SS Lua script for per-slot voices
- `engine/` : the engine (`SkinCSSEngine.exe`), written in Rust.
  It is a fork of [retoc](https://github.com/trumank/retoc) by trumank (MIT license, see `engine/LICENSE`).
  Our code is in `engine/retoc_cli/src/` (`port.rs`, `voice.rs`, `extract.rs`, `main.rs`).
  `engine/unrar_sys/` is the unrar crate with small build fixes (UnRAR license in `vendor/unrar/license.txt`).

## Build the engine

Needs Rust (https://rustup.rs).

On Windows:

```
cd engine
cargo build --release -p retoc_cli
```

Rename `target/release/retoc.exe` to `SkinCSSEngine.exe`.

## Run the app

Needs Node.js (https://nodejs.org).

```
cd app
npm install electron
npx electron .
```

To make the release folder, put `app/` in an Electron release as `resources/app`, and add:

- `resources/backend/SkinCSSEngine.exe`
- `resources/backend/oo2core_9_win64.dll` (Oodle, copy it from the game folder)
- `resources/ue4ss/` (UE4SS release files: `dwmapi.dll`, `ue4ss/UE4SS.dll`, settings)
- `resources/ue4ss-mod/` (from this source)

## Credits

ArtemisPendragon (True CSSE), Emasplatter (Separate, Ai Skins!), Efflorescedbloom (Stamps Foundation),
UE4SS team, trumank (retoc), RED 1 FOUAD (BlueStar Manager).

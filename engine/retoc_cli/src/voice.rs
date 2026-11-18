//! Voice packs: CriWare VOICE_<X>AI.acb/.awb (AI racers) vs VOICE_<X>.acb/.awb (regular racers).
use anyhow::{Context, Result, bail};
use std::io::{BufReader, BufWriter, Cursor};
use std::path::{Path, PathBuf};

/// Rewrite every null-terminated string in an ACB that names the AI cue sheet so it names the regular one:
/// "VOICE_AMYAI" -> "VOICE_AMY", "guest_voice_amyAI_boost_1" -> "guest_voice_amy_boost_1".
/// Strings only get shorter, so they're rewritten in place and padded with zeros (no offsets move).
fn acb_ai_to_regular(buf: &mut [u8], code: &str) -> usize {
    let up = code.to_ascii_uppercase();
    let from_sheet = format!("VOICE_{up}AI");
    let to_sheet = format!("VOICE_{up}");
    let mut changed = 0;
    let mut i = 0;
    while i < buf.len() {
        // a string starts after a zero byte and is printable ASCII up to the next zero
        if (i == 0 || buf[i - 1] == 0) && buf[i].is_ascii_graphic() {
            let mut e = i;
            while e < buf.len() && buf[e] != 0 && buf[e].is_ascii_graphic() {
                e += 1;
            }
            if e < buf.len() && buf[e] == 0 && e - i >= 4 {
                let s = std::str::from_utf8(&buf[i..e]).unwrap_or("").to_string();
                let mut t = s.replace(&from_sheet, &to_sheet);
                // cue names: <prefix><code>AI_<rest>, code compared case-insensitively
                let low = t.to_ascii_lowercase();
                let needle = format!("{}ai_", code.to_ascii_lowercase());
                if let Some(p) = low.find(&needle) {
                    let at = p + code.len();
                    if (p == 0 || !t.as_bytes()[p - 1].is_ascii_alphanumeric()) || t.as_bytes()[p - 1] == b'_' {
                        t.replace_range(at..at + 2, "");
                    }
                }
                if t != s {
                    let n = t.len();
                    buf[i..i + n].copy_from_slice(t.as_bytes());
                    for b in &mut buf[i + n..e] {
                        *b = 0;
                    }
                    changed += 1;
                }
                i = e;
                continue;
            }
            i = e.max(i + 1);
            continue;
        }
        i += 1;
    }
    changed
}

/// Voice packs a legacy .pak brings: the cue sheet names, e.g. ["VOICE_AMYAI"]. Unreadable paks give none.
pub fn list_voices(p: &Path) -> Vec<String> {
    let Ok(f) = std::fs::File::open(p) else { return vec![] };
    let mut r = BufReader::new(f);
    let Ok(pak) = repak::PakBuilder::new().reader(&mut r) else { return vec![] };
    let mut v: Vec<String> = pak
        .files()
        .iter()
        .filter_map(|f| {
            let up = f.rsplit('/').next().unwrap_or("").to_ascii_uppercase();
            up.starts_with("VOICE_").then_some(())?;
            up.strip_suffix(".ACB").map(|s| s.to_string())
        })
        .collect();
    v.sort();
    v.dedup();
    v
}

pub struct VoiceOut {
    pub written: Vec<String>,
    pub notes: Vec<String>,
}

/// Port the AI voice files found in `paks` (legacy .pak files of a mod) to the regular racer's voice.
/// `to_regular`: Some(true) AI → regular, Some(false) regular → AI, None = copy the voice files as they are.
pub fn port_voice(paks: &[PathBuf], to_regular: Option<bool>, out: &Path) -> Result<VoiceOut> {
    let mut files: Vec<(String, Vec<u8>)> = vec![];
    let mut notes = vec![];
    let mut mount = String::from("../../../");
    let mut version = repak::Version::V11;
    for p in paks {
        let mut r = BufReader::new(std::fs::File::open(p).with_context(|| format!("open {}", p.display()))?);
        let pak = match repak::PakBuilder::new().reader(&mut r) {
            Ok(x) => x,
            Err(e) => {
                notes.push(format!("skipped {} ({e})", p.display()));
                continue;
            }
        };
        let mp = pak.mount_point().to_string();
        for f in pak.files() {
            let full = format!("{}{}", mp.trim_start_matches("../../../"), f);
            let name = full.rsplit('/').next().unwrap_or("").to_string();
            let upper = name.to_ascii_uppercase();
            if !upper.starts_with("VOICE_") || !(upper.ends_with(".ACB") || upper.ends_with(".AWB")) {
                continue;
            }
            let stem = &name[..name.len() - 4];
            let ext = &name[name.len() - 4..];
            let is_ai = stem.len() > 8 && stem[stem.len() - 2..].eq_ignore_ascii_case("AI");
            let Some(to_regular) = to_regular else {
                let data = pak.get(&f, &mut r).with_context(|| format!("read {f} from {}", p.display()))?;
                files.push((full.clone(), data));
                version = pak.version();
                continue;
            };
            if to_regular != is_ai {
                continue; // nothing to change for this direction
            }
            if !to_regular {
                notes.push(format!("{name}: regular → AI voices can't be converted yet, so the AI slot keeps its normal voice"));
                continue;
            }
            let code = &stem["VOICE_".len()..stem.len() - 2];
            let mut data = pak.get(&f, &mut r).with_context(|| format!("read {f} from {}", p.display()))?;
            if ext.eq_ignore_ascii_case(".acb") {
                let n = acb_ai_to_regular(&mut data, code);
                if n == 0 {
                    notes.push(format!("{name}: no AI cue names found, copied as-is"));
                }
            }
            let new_name = format!("VOICE_{}{}", code.to_ascii_uppercase(), ext.to_ascii_lowercase());
            let dir = &full[..full.len() - name.len()];
            files.push((format!("{dir}{new_name}"), data));
            mount = "../../../".into();
            version = pak.version();
        }
    }
    if files.is_empty() {
        return Ok(VoiceOut { written: vec![], notes });
    }
    if files.iter().any(|(n, _)| n.contains("..")) {
        bail!("bad path in voice pak");
    }
    if let Some(d) = out.parent() {
        std::fs::create_dir_all(d)?;
    }
    let w = BufWriter::new(std::fs::File::create(out)?);
    let mut pw = repak::PakBuilder::new().writer(w, version, mount, None);
    let mut written = vec![];
    for (n, d) in &files {
        pw.write_file(n, false, d)?;
        written.push(n.clone());
    }
    pw.write_index()?;
    let _ = Cursor::new(());
    Ok(VoiceOut { written, notes })
}

// ---------------------------------------------------------------------------------------------------------
// Per-slot voice banks (used with the UE4SS voice script): VOICE_<C>AI → VOICE_<T>A<N>

/// Minimal CRI @UTF table reader: enough to find columns and patch fixed-size per-row values in place.
struct Utf {
    base: usize,
    rows_off: usize,
    row_w: usize,
    nrows: usize,
    /// (name, storage, type, offset within row) for per-row columns; constants have no offset
    cols: Vec<(String, u8, u8, Option<usize>, Option<usize>)>,
    str_off: usize,
    data_off: usize,
}
fn be16(b: &[u8], o: usize) -> usize { u16::from_be_bytes([b[o], b[o + 1]]) as usize }
fn be32(b: &[u8], o: usize) -> usize { u32::from_be_bytes([b[o], b[o + 1], b[o + 2], b[o + 3]]) as usize }
fn utf_size(t: u8) -> usize {
    match t { 0 | 1 => 1, 2 | 3 => 2, 4 | 5 | 8 | 0xA => 4, 6 | 7 | 9 | 0xB => 8, _ => 0 }
}
fn parse_utf(b: &[u8], off: usize) -> Option<Utf> {
    if b.get(off..off + 4)? != b"@UTF" { return None; }
    let base = off + 8;
    let rows_off = be16(b, base + 2);
    let str_off = be32(b, base + 4);
    let data_off = be32(b, base + 8);
    let ncol = be16(b, base + 16);
    let row_w = be16(b, base + 18);
    let nrows = be32(b, base + 20);
    let mut p = base + 24;
    let mut cols = vec![];
    let mut in_row = 0usize;
    for _ in 0..ncol {
        let fl = *b.get(p)?;
        let name_o = be32(b, p + 1);
        p += 5;
        let (st, ty) = (fl >> 4, fl & 15);
        let mut nm = String::new();
        let s0 = base + str_off + name_o;
        let mut e = s0;
        while e < b.len() && b[e] != 0 { e += 1; }
        nm.push_str(std::str::from_utf8(b.get(s0..e)?).ok()?);
        let mut row_at = None;
        let mut const_at = None;
        if st == 3 {
            const_at = Some(p);
            p += utf_size(ty);
        } else if st == 5 {
            row_at = Some(in_row);
            in_row += utf_size(ty);
        }
        cols.push((nm, st, ty, row_at, const_at));
    }
    Some(Utf { base, rows_off, row_w, nrows, cols, str_off, data_off })
}
impl Utf {
    fn col(&self, name: &str) -> Option<&(String, u8, u8, Option<usize>, Option<usize>)> { self.cols.iter().find(|c| c.0 == name) }
    /// absolute offset of a value (row value or constant)
    fn at(&self, name: &str, row: usize) -> Option<(usize, u8)> {
        let c = self.col(name)?;
        if let Some(o) = c.3 { return Some((self.base + self.rows_off + row * self.row_w + o, c.2)); }
        c.4.map(|o| (o, c.2))
    }
    fn get(&self, b: &[u8], name: &str, row: usize) -> Option<u64> {
        let (o, t) = self.at(name, row)?;
        Some(match t {
            0 | 1 => b[o] as u64,
            2 | 3 => be16(b, o) as u64,
            4 | 5 => be32(b, o) as u64,
            6 | 7 => u64::from_be_bytes(b[o..o + 8].try_into().ok()?),
            _ => return None,
        })
    }
    fn blob(&self, b: &[u8], name: &str, row: usize) -> Option<(usize, usize)> {
        let (o, t) = self.at(name, row)?;
        (t == 0xB).then(|| (self.base + self.data_off + be32(b, o), be32(b, o + 4)))
    }
    /// write a per-row integer value (constants are left alone)
    fn set(&self, b: &mut [u8], name: &str, row: usize, v: u64) -> bool {
        let Some(c) = self.col(name) else { return false };
        let Some(ro) = c.3 else { return false };
        let o = self.base + self.rows_off + row * self.row_w + ro;
        match c.2 {
            0 | 1 => b[o] = v as u8,
            2 | 3 => b[o..o + 2].copy_from_slice(&(v as u16).to_be_bytes()),
            4 | 5 => b[o..o + 4].copy_from_slice(&(v as u32).to_be_bytes()),
            _ => return false,
        }
        true
    }
}

/// HCA stream info (channels, rate, samples) for each AWB entry id.
fn awb_hca_info(awb: &[u8]) -> std::collections::HashMap<u32, (u8, u32, u32)> {
    let mut out = std::collections::HashMap::new();
    if awb.len() < 16 || &awb[0..4] != b"AFS2" { return out; }
    let osz = awb[5] as usize;
    let idsz = awb[6] as usize;
    let cnt = u32::from_le_bytes(awb[8..12].try_into().unwrap()) as usize;
    let align = (u32::from_le_bytes(awb[12..16].try_into().unwrap()) & 0xffff).max(1) as usize;
    let rd = |o: usize, n: usize| -> usize { let mut v = 0usize; for k in 0..n { v |= (awb[o + k] as usize) << (8 * k); } v };
    let ids_at = 16;
    let offs_at = 16 + cnt * idsz;
    if awb.len() < offs_at + (cnt + 1) * osz { return out; }
    for k in 0..cnt {
        let id = rd(ids_at + k * idsz, idsz) as u32;
        let s = rd(offs_at + k * osz, osz);
        let s = s.div_ceil(align) * align;
        let Some(h) = awb.get(s..s + 0x60) else { continue };
        if (h[0] & 0x7f) != b'H' || (h[1] & 0x7f) != b'C' || (h[2] & 0x7f) != b'A' { continue; }
        // find the (masked) "fmt" chunk
        let mut f = None;
        for i in 8..0x50 {
            if (h[i] & 0x7f) == b'f' && (h[i + 1] & 0x7f) == b'm' && (h[i + 2] & 0x7f) == b't' { f = Some(i); break; }
        }
        let Some(f) = f else { continue };
        let ch = h[f + 4];
        let rate = ((h[f + 5] as u32) << 16) | ((h[f + 6] as u32) << 8) | h[f + 7] as u32;
        let blocks = u32::from_be_bytes(h[f + 8..f + 12].try_into().unwrap());
        let mh = u16::from_be_bytes([h[f + 12], h[f + 13]]) as u32;
        let mf = u16::from_be_bytes([h[f + 14], h[f + 15]]) as u32;
        out.insert(id, (ch, rate, (blocks * 1024).saturating_sub(mh + mf)));
    }
    out
}

/// Some voice mods swap the audio in the .awb without updating the .acb's waveform list (channels / rate /
/// length). The game then refuses those lines. Make the list match the real audio. Returns lines fixed.
fn fix_acb_waveforms(acb: &mut [u8], awb: &[u8]) -> usize {
    let info = awb_hca_info(awb);
    if info.is_empty() { return 0; }
    let Some(top) = parse_utf(acb, 0) else { return 0 };
    let Some((wo, wl)) = top.blob(acb, "WaveformTable", 0) else { return 0 };
    if wl == 0 { return 0; }
    let Some(wf) = parse_utf(acb, wo) else { return 0 };
    let mut fixed = 0;
    for r in 0..wf.nrows {
        if wf.get(acb, "Streaming", r) != Some(1) { continue; }
        let Some(id) = wf.get(acb, "StreamAwbId", r) else { continue };
        let Some(&(ch, rate, ns)) = info.get(&(id as u32)) else { continue };
        let cur = (wf.get(acb, "NumChannels", r), wf.get(acb, "SamplingRate", r), wf.get(acb, "NumSamples", r));
        if cur == (Some(ch as u64), Some(rate as u64), Some(ns as u64)) { continue; }
        let a = wf.set(acb, "NumChannels", r, ch as u64);
        let b = wf.set(acb, "SamplingRate", r, rate as u64);
        let c = wf.set(acb, "NumSamples", r, ns as u64);
        if a || b || c { fixed += 1; }
    }
    let _ = (wf.str_off, wf.data_off);
    fixed
}

/// Same-length rename of every string naming the source bank: "VOICE_SHAAI" -> "VOICE_SHAA1",
/// "guest_voice_shaAI_x" -> "guest_voice_shaA1_x" (codes are 3 letters, so nothing moves).
fn acb_rename_same_len(buf: &mut [u8], from_sheet: &str, to_sheet: &str, from_cue: &str, to_cue: &str) -> usize {
    let mut n = 0;
    for (f, t) in [(from_sheet.as_bytes(), to_sheet.as_bytes()), (from_cue.as_bytes(), to_cue.as_bytes())] {
        if f.len() != t.len() || f.is_empty() { continue; }
        let mut i = 0;
        while i + f.len() <= buf.len() {
            if &buf[i..i + f.len()] == f { buf[i..i + f.len()].copy_from_slice(t); n += 1; i += f.len(); } else { i += 1; }
        }
    }
    n
}

/// Build the voice bank of one AI skin slot from the AI voice a mod brings.
/// `slot` like "SHAA1": target code SHA, slot 1. Source = the mod's VOICE_<C>AI files (any character).
pub fn slot_voice(paks: &[PathBuf], slot: &str, out: &Path) -> Result<VoiceOut> {
    let slot = slot.to_ascii_uppercase();
    if slot.len() != 5 || !slot[..3].chars().all(|c| c.is_ascii_alphabetic()) || &slot[3..4] != "A" || !matches!(&slot[4..], "1" | "2" | "3") {
        bail!("bad slot code {slot} (expected like SHAA1)");
    }
    let mut notes = vec![];
    // collect AI voice files per source code
    let mut found: Vec<(String, String, Vec<u8>, Vec<u8>, repak::Version)> = vec![]; // code, dir, acb, awb
    for p in paks {
        let mut r = BufReader::new(std::fs::File::open(p).with_context(|| format!("open {}", p.display()))?);
        let Ok(pak) = repak::PakBuilder::new().reader(&mut r) else { continue };
        let mp = pak.mount_point().to_string();
        let files = pak.files();
        for f in &files {
            let full = format!("{}{}", mp.trim_start_matches("../../../"), f);
            let name = full.rsplit('/').next().unwrap_or("").to_string();
            let up = name.to_ascii_uppercase();
            if !(up.starts_with("VOICE_") && up.ends_with("AI.ACB") && up.len() == "VOICE_XXXAI.ACB".len()) { continue; }
            let code = up["VOICE_".len().."VOICE_".len() + 3].to_string();
            let awb_name = format!("{}.awb", &f[..f.len() - 4]);
            let awb_file = files.iter().find(|x| x.eq_ignore_ascii_case(&awb_name));
            let acb = pak.get(f, &mut r)?;
            let awb = match awb_file { Some(a) => pak.get(a, &mut r)?, None => vec![] };
            let dir = full[..full.len() - name.len()].to_string();
            found.push((code, dir, acb, awb, pak.version()));
        }
    }
    let Some((code, dir, mut acb, awb, version)) = found.into_iter().next() else {
        notes.push("no AI voice (VOICE_<X>AI) in this mod: the slot keeps the character's normal AI voice".into());
        return Ok(VoiceOut { written: vec![], notes });
    };
    let tcode = &slot[..3];
    let n = &slot[4..];
    let renamed = acb_rename_same_len(&mut acb, &format!("VOICE_{code}AI"), &format!("VOICE_{tcode}A{n}"), &format!("{}AI_", code.to_ascii_lowercase()), &format!("{}A{n}_", tcode.to_ascii_lowercase()));
    if renamed == 0 { notes.push(format!("VOICE_{code}AI: no names to rename, copied as-is")); }
    let fixed = if awb.is_empty() { 0 } else { fix_acb_waveforms(&mut acb, &awb) };
    if fixed > 0 { notes.push(format!("fixed {fixed} voice lines whose audio didn't match the voice list")); }
    let mut files = vec![(format!("{dir}VOICE_{slot}.acb"), acb)];
    if !awb.is_empty() { files.push((format!("{dir}VOICE_{slot}.awb"), awb)); }
    if let Some(d) = out.parent() { std::fs::create_dir_all(d)?; }
    let w = BufWriter::new(std::fs::File::create(out)?);
    let mut pw = repak::PakBuilder::new().writer(w, version, "../../../".to_string(), None);
    let mut written = vec![];
    for (nme, d) in &files {
        pw.write_file(nme, false, d)?;
        written.push(nme.clone());
    }
    pw.write_index()?;
    Ok(VoiceOut { written, notes })
}

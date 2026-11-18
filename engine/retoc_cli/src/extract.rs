//! Extract GameBanana archives (.zip / .7z / .rar) into a folder.
use anyhow::{Context, Result, bail};
use std::path::Path;

pub fn extract(archive: &Path, dest: &Path, progress: &dyn Fn(&str)) -> Result<()> {
    std::fs::create_dir_all(dest)?;
    let ext = archive.extension().map(|e| e.to_string_lossy().to_lowercase()).unwrap_or_default();
    match ext.as_str() {
        "zip" => {
            let f = std::fs::File::open(archive)?;
            let mut z = zip::ZipArchive::new(f).context("not a valid .zip")?;
            for i in 0..z.len() {
                let mut e = z.by_index(i)?;
                let Some(rel) = e.enclosed_name() else { continue };
                let out = dest.join(rel);
                if e.is_dir() {
                    std::fs::create_dir_all(&out)?;
                    continue;
                }
                if let Some(p) = out.parent() {
                    std::fs::create_dir_all(p)?;
                }
                progress(&e.name().to_string());
                let mut w = std::fs::File::create(&out)?;
                std::io::copy(&mut e, &mut w)?;
            }
        }
        "7z" => {
            progress("Unpacking .7z");
            sevenz_rust2::decompress_file(archive, dest).map_err(|e| anyhow::anyhow!("7z: {e}"))?;
        }
        "rar" => {
            let mut a = unrar::Archive::new(archive).open_for_processing().map_err(|e| anyhow::anyhow!("rar: {e}"))?;
            while let Some(h) = a.read_header().map_err(|e| anyhow::anyhow!("rar: {e}"))? {
                let name = h.entry().filename.to_string_lossy().to_string();
                if h.entry().is_file() {
                    progress(&name);
                    a = h.extract_with_base(dest).map_err(|e| anyhow::anyhow!("rar {name}: {e}"))?;
                } else {
                    a = h.skip().map_err(|e| anyhow::anyhow!("rar: {e}"))?;
                }
            }
        }
        _ => bail!("Unsupported archive type .{ext} (use .zip, .7z or .rar)"),
    }
    Ok(())
}

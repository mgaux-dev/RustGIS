//! MBTiles archives as tile layers: a tile's address is `mbtiles:{z}/{x}/{y}@<archive>`, the
//! archive being a file path on the desktop (read through SQLite on the fetch workers, a tile at a
//! time) or `upload:<name>`, a file read into memory with [`register`] (a browser upload).
//!
//! The reading itself is `rustgis_data::mbtiles`; this is addresses, the archives in use and
//! answering the cache.

use std::collections::HashMap;
use std::sync::{Arc, Mutex, OnceLock};

use rustgis_data::mbtiles::{Archive, Info};
use web_time::{Duration, Instant};

use crate::cache::Done;

/// The scheme of a tile address read from an MBTiles archive.
pub const SCHEME: &str = "mbtiles:";
/// The prefix of an archive held in memory.
pub const UPLOAD: &str = "upload:";
/// Archives held in memory at most.
const MAX_UPLOADS: usize = 16;
/// A file that couldn't be read is tried again after this long.
const RETRY: Duration = Duration::from_secs(60);

/// The template for an archive's tiles.
pub fn template(archive: &str) -> String {
    format!("{SCHEME}{{z}}/{{x}}/{{y}}@{archive}")
}

/// Whether an address is a tile read from an MBTiles archive.
pub fn is_tile(url: &str) -> bool {
    url.starts_with(SCHEME)
}

/// Whether an archive address names an archive held in memory.
pub fn is_upload(archive: &str) -> bool {
    archive.starts_with(UPLOAD)
}

/// Whether an archive may be read: one held in memory, or the full path of an `.mbtiles` file on
/// this computer (not a network share).
pub fn check(archive: &str) -> Result<(), String> {
    if let Some(name) = archive.strip_prefix(UPLOAD) {
        return if name.is_empty() || name.len() > 512 || name.chars().any(char::is_control) {
            Err("an uploaded MBTiles file needs a name".into())
        } else {
            Ok(())
        };
    }
    let p = archive.strip_prefix("file://").unwrap_or(archive);
    let drive = p.as_bytes().get(1) == Some(&b':') && p.as_bytes().first().is_some_and(u8::is_ascii_alphabetic);
    let share = p.starts_with("\\\\") || p.starts_with("//") || (archive.starts_with("file://") && !p.starts_with('/'));
    if share || !(p.starts_with('/') || drive) || p.chars().any(char::is_control) || p.len() > 4096 {
        return Err(format!("'{p}': an MBTiles file is given by its full path on this computer"));
    }
    if !p.to_ascii_lowercase().ends_with(".mbtiles") {
        return Err(format!("'{p}' isn't an .mbtiles file"));
    }
    Ok(())
}

fn uploads() -> &'static Mutex<Vec<(String, Arc<Archive>)>> {
    static U: OnceLock<Mutex<Vec<(String, Arc<Archive>)>>> = OnceLock::new();
    U.get_or_init(Mutex::default)
}

/// Reads an MBTiles file's bytes into memory as `upload:<name>`, and returns that address. The
/// archive replaces one of the same name; the oldest of too many is forgotten.
pub fn register(name: &str, bytes: &[u8]) -> Result<String, String> {
    let archive = rustgis_data::mbtiles::read_bytes(bytes).map_err(|e| format!("{name}: {e}"))?;
    let address = format!("{UPLOAD}{}", name.chars().filter(|c| !c.is_control()).take(512).collect::<String>());
    check(&address)?;
    let mut all = uploads().lock().unwrap_or_else(|e| e.into_inner());
    all.retain(|(a, _)| *a != address);
    if all.len() >= MAX_UPLOADS {
        all.remove(0);
    }
    all.push((address.clone(), Arc::new(archive)));
    drop(all);
    // Tiles of an earlier upload of the same name are stale.
    crate::source::forget();
    Ok(address)
}

fn upload(archive: &str) -> Option<Arc<Archive>> {
    uploads().lock().unwrap_or_else(|e| e.into_inner()).iter().find(|(a, _)| a == archive).map(|(_, a)| a.clone())
}

fn infos() -> &'static Mutex<HashMap<String, Result<Arc<Info>, (String, Instant)>>> {
    static I: OnceLock<Mutex<HashMap<String, Result<Arc<Info>, (String, Instant)>>>> = OnceLock::new();
    I.get_or_init(Mutex::default)
}

/// Forgets the files' metadata (with the tile cache), so they are read again.
pub(crate) fn forget() {
    infos().lock().unwrap_or_else(|e| e.into_inner()).clear();
    #[cfg(not(target_arch = "wasm32"))]
    rustgis_data::mbtiles::close_all();
}

/// An archive's metadata.
pub fn info(archive: &str) -> Result<Arc<Info>, String> {
    check(archive)?;
    if is_upload(archive) {
        return upload(archive)
            .map(|a| Arc::new(a.info.clone()))
            .ok_or_else(|| format!("{}: add the MBTiles file again (uploads aren't kept)", archive.trim_start_matches(UPLOAD)));
    }
    if let Some(result) = cached_info(archive) {
        return result;
    }
    let r = read_info(archive);
    let mut all = infos().lock().unwrap_or_else(|e| e.into_inner());
    if all.len() >= 64 {
        all.clear();
    }
    all.insert(archive.to_string(), r.clone().map_err(|e| (e, Instant::now())));
    r
}

/// Cached metadata, retaining failed reads only for the retry interval.
fn cached_info(archive: &str) -> Option<Result<Arc<Info>, String>> {
    infos().lock().unwrap_or_else(|e| e.into_inner()).get(archive).and_then(|r| match r {
        Ok(info) => Some(Ok(info.clone())),
        Err((error, at)) if at.elapsed() < RETRY => Some(Err(error.clone())),
        Err(_) => None,
    })
}

/// Metadata without file I/O on the caller: local reads run in at most eight workers. A caller
/// may wait up to `timeout`; otherwise `None` means pending. Completion wakes the map. Uploads
/// are already in memory. At capacity a read remains pending until a later poll can start it.
/// A stalled file keeps its worker slot until it answers.
pub fn info_ready(archive: &str, timeout: Duration) -> Result<Option<Arc<Info>>, String> {
    check(archive)?;
    if is_upload(archive) {
        return info(archive).map(Some);
    }
    #[cfg(target_arch = "wasm32")]
    {
        let _ = timeout;
        info(archive).map(Some)
    }
    #[cfg(not(target_arch = "wasm32"))]
    {
        use std::collections::HashSet;
        use std::sync::Condvar;
        static LOADING: OnceLock<(Mutex<HashSet<String>>, Condvar)> = OnceLock::new();
        let (pending, ready) = LOADING.get_or_init(|| (Mutex::new(HashSet::new()), Condvar::new()));
        let mut all = pending.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(result) = cached_info(archive) {
            return result.map(Some);
        }
        if !all.contains(archive) {
            if all.len() >= 8 {
                return Ok(None);
            }
            all.insert(archive.to_string());
            let address = archive.to_string();
            let spawned = std::thread::Builder::new().name("rustgis-mbtiles-info".into()).spawn(move || {
                let _ = info(&address);
                pending.lock().unwrap_or_else(|e| e.into_inner()).remove(&address);
                ready.notify_all();
                crate::cache::notify();
            });
            if let Err(error) = spawned {
                all.remove(archive);
                return Err(format!("could not start an MBTiles metadata read: {error}"));
            }
        }
        if !timeout.is_zero() {
            drop(ready.wait_timeout_while(all, timeout, |all| all.contains(archive)).unwrap_or_else(|e| e.into_inner()));
        }
        cached_info(archive).transpose()
    }
}

#[cfg(not(target_arch = "wasm32"))]
fn read_info(archive: &str) -> Result<Arc<Info>, String> {
    let path = std::path::Path::new(archive.strip_prefix("file://").unwrap_or(archive));
    rustgis_data::mbtiles::open_path(path).map(Arc::new)
}

#[cfg(target_arch = "wasm32")]
fn read_info(archive: &str) -> Result<Arc<Info>, String> {
    Err(format!("{archive}: the web app reads MBTiles files added to it (Add Data), not paths"))
}

/// `mbtiles:z/x/y@archive` → its parts.
fn parse_url(url: &str) -> Option<(u8, u32, u32, &str)> {
    let rest = url.strip_prefix(SCHEME)?;
    let (zxy, archive) = rest.split_once('@')?;
    let mut p = zxy.split('/');
    let z: u8 = p.next()?.parse().ok()?;
    let x: u32 = p.next()?.parse().ok()?;
    let y: u32 = p.next()?.parse().ok()?;
    (z <= 30 && p.next().is_none() && !archive.is_empty()).then_some((z, x, y, archive))
}

/// Fetches a tile address: its bytes, or none for a tile the archive doesn't have. A file's tile
/// is read on the fetch workers, never on the frame.
pub(crate) fn fetch(url: &str, done: Done) {
    let Some((z, x, y, archive)) = parse_url(url) else {
        done(Err(format!("{url}: not an MBTiles tile address")));
        return;
    };
    if let Err(e) = check(archive) {
        done(Err(e));
        return;
    }
    if is_upload(archive) {
        match upload(archive) {
            Some(a) => done(Ok(a.tile(z, x, y).map(|t| t.to_vec()).unwrap_or_default())),
            None => done(Err(format!("{archive}: no such upload"))),
        }
        return;
    }
    crate::cache::fetch_worker(url.to_string(), done);
}

/// Reads a tile from a file (on a fetch worker).
#[cfg(not(target_arch = "wasm32"))]
pub(crate) fn read_local(url: &str) -> Result<Vec<u8>, String> {
    let (z, x, y, archive) = parse_url(url).ok_or_else(|| format!("{url}: not an MBTiles tile address"))?;
    check(archive)?;
    let path = std::path::Path::new(archive.strip_prefix("file://").unwrap_or(archive));
    Ok(rustgis_data::mbtiles::tile_path(path, z, x, y)?.unwrap_or_default())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn addresses() {
        assert_eq!(parse_url("mbtiles:3/1/2@/data/a.mbtiles"), Some((3, 1, 2, "/data/a.mbtiles")));
        assert_eq!(parse_url("mbtiles:3/1/2@upload:a b.mbtiles"), Some((3, 1, 2, "upload:a b.mbtiles")));
        for bad in
            ["mbtiles:3/1@/a.mbtiles", "mbtiles:99/1/2@/a.mbtiles", "mbtiles:1/2/3/4@/a.mbtiles", "mbtiles:1/2/3@", "pmtiles:1/2/3@/a"]
        {
            assert_eq!(parse_url(bad), None, "{bad}");
        }
        assert!(check("/data/a.mbtiles").is_ok());
        assert!(check("C:\\data\\a.MBTiles").is_ok());
        assert!(check("upload:a.mbtiles").is_ok());
        for bad in
            ["a.mbtiles", "/data/a.pmtiles", "\\\\server\\share\\a.mbtiles", "file://server/a.mbtiles", "upload:", "https://x/a.mbtiles"]
        {
            assert!(check(bad).is_err(), "{bad}");
        }
        assert_eq!(template("upload:x"), "mbtiles:{z}/{x}/{y}@upload:x");
    }

    #[test]
    fn uploads_answer_tiles() {
        assert!(register("junk.mbtiles", b"nope").is_err());
        assert!(info("upload:never.mbtiles").is_err());
        let (tx, rx) = std::sync::mpsc::channel();
        fetch("mbtiles:0/0/0@upload:never.mbtiles", Box::new(move |r| tx.send(r.is_err()).unwrap_or_default()));
        assert_eq!(rx.recv().ok(), Some(true));
    }
}

//! Catalog: folder scan + SQLite index + JSON sidecars.
//! Folder is source of truth; the DB is a search/cache index only.
use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use rusqlite::{params, Connection};
use serde::{Deserialize, Serialize};

use crate::decode;
use crate::recipe::{sidecar_path_for, Sidecar};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AssetEntry {
    pub path: String,
    pub name: String,
    /// "raw" | "raster"
    pub kind: String,
    pub size: u64,
    pub mtime: i64,
    pub rating: i32,
    pub label: String,
    /// path of the sibling file (raw<->jpeg pair) if present
    pub pair: Option<String>,
    pub has_sidecar: bool,
    /// camera "make model" + lens from header/EXIF (empty when unknown)
    pub camera: String,
    pub lens: String,
}

pub struct Catalog {
    db: Connection,
}

fn db_path() -> PathBuf {
    let base = std::env::var_os("KOUBOU_HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| {
            let home = std::env::var_os("HOME")
                .map(PathBuf::from)
                .unwrap_or_else(|| PathBuf::from("."));
            home.join(".koubou")
        });
    let _ = fs::create_dir_all(&base);
    base.join("catalog.db")
}

impl Catalog {
    pub fn open() -> Result<Catalog> {
        let db = Connection::open(db_path()).context("open catalog db")?;
        db.execute_batch(
            "CREATE TABLE IF NOT EXISTS files(
               path TEXT PRIMARY KEY, name TEXT, folder TEXT, kind TEXT,
               size INTEGER, mtime INTEGER, rating INTEGER, label TEXT,
               pair TEXT, added INTEGER);
             CREATE INDEX IF NOT EXISTS idx_files_folder ON files(folder);
             CREATE INDEX IF NOT EXISTS idx_files_rating ON files(rating);",
        )?;
        Ok(Catalog { db })
    }

    /// in-memory catalog (tests)
    #[cfg(test)]
    pub fn open_mem() -> Result<Catalog> {
        let db = Connection::open_in_memory()?;
        db.execute_batch(
            "CREATE TABLE files(path TEXT PRIMARY KEY, name TEXT, folder TEXT, kind TEXT,
             size INTEGER, mtime INTEGER, rating INTEGER, label TEXT, pair TEXT, added INTEGER);",
        )?;
        Ok(Catalog { db })
    }

    /// Scan one directory level for supported assets, pairing RAW+JPEG by stem.
    pub fn scan(&self, folder: &Path) -> Result<Vec<AssetEntry>> {
        let mut raws: Vec<PathBuf> = Vec::new();
        let mut rasters: Vec<PathBuf> = Vec::new();
        for e in fs::read_dir(folder).with_context(|| format!("read {}", folder.display()))? {
            let p = e?.path();
            if !p.is_file() {
                continue;
            }
            if decode::is_raw(&p) {
                raws.push(p);
            } else if decode::is_raster(&p) {
                rasters.push(p);
            }
        }
        let mut entries: Vec<AssetEntry> = Vec::new();
        let raster_stems: std::collections::HashMap<String, PathBuf> = rasters
            .iter()
            .filter_map(|p| p.file_stem().and_then(|s| s.to_str()).map(|s| (s.to_string(), p.clone())))
            .collect();
        let raw_stems: std::collections::HashSet<String> = raws
            .iter()
            .filter_map(|p| p.file_stem().and_then(|s| s.to_str()).map(|s| s.to_string()))
            .collect();

        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs() as i64)
            .unwrap_or(0);

        let mut push = |p: &PathBuf, kind: &str, pair: Option<String>| {
            let meta = fs::metadata(p).ok();
            let sc_path = sidecar_path_for(p);
            let sc = read_sidecar(&sc_path).unwrap_or_default();
            let has_sc = sc_path.exists();
            let name = p
                .file_name()
                .and_then(|s| s.to_str())
                .unwrap_or("")
                .to_string();
            let ci = decode::probe(p);
            let entry = AssetEntry {
                path: p.to_string_lossy().into_owned(),
                name,
                kind: kind.to_string(),
                size: meta.as_ref().map(|m| m.len()).unwrap_or(0),
                mtime: meta
                    .and_then(|m| m.modified().ok())
                    .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
                    .map(|d| d.as_secs() as i64)
                    .unwrap_or(0),
                rating: sc.rating,
                label: sc.label.clone(),
                pair,
                has_sidecar: has_sc,
                camera: ci
                    .as_ref()
                    .map(|i| format!("{} {}", i.make, i.model).trim().to_string())
                    .unwrap_or_default(),
                lens: ci.map(|i| i.lens).unwrap_or_default(),
            };
            let _ = self.db.execute(
                "INSERT INTO files(path,name,folder,kind,size,mtime,rating,label,pair,added)
                 VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9,?10)
                 ON CONFLICT(path) DO UPDATE SET
                   name=excluded.name,size=excluded.size,mtime=excluded.mtime,
                   rating=excluded.rating,label=excluded.label,pair=excluded.pair",
                params![
                    entry.path,
                    entry.name,
                    folder.to_string_lossy(),
                    entry.kind,
                    entry.size as i64,
                    entry.mtime,
                    entry.rating,
                    entry.label,
                    entry.pair,
                    now
                ],
            );
            entries.push(entry);
        };

        for p in &raws {
            let stem = p.file_stem().and_then(|s| s.to_str()).unwrap_or("").to_string();
            let pair = raster_stems.get(&stem).map(|q| q.to_string_lossy().into_owned());
            push(p, "raw", pair);
        }
        for p in &rasters {
            let stem = p.file_stem().and_then(|s| s.to_str()).unwrap_or("").to_string();
            if raw_stems.contains(&stem) {
                continue; // shown via raw entry
            }
            push(p, "raster", None);
        }
        entries.sort_by(|a, b| a.name.cmp(&b.name));
        Ok(entries)
    }

    pub fn set_rating(&self, asset: &Path, rating: i32) -> Result<()> {
        let sp = sidecar_path_for(asset);
        let mut sc = read_sidecar(&sp).unwrap_or_default();
        sc.rating = rating.clamp(0, 5);
        write_sidecar(&sp, &sc)?;
        self.db.execute(
            "INSERT INTO files(path,name,folder,kind,rating,label) VALUES(?1,'','','raw',?2,'')
             ON CONFLICT(path) DO UPDATE SET rating=?2",
            params![asset.to_string_lossy(), sc.rating],
        )?;
        Ok(())
    }

    pub fn set_label(&self, asset: &Path, label: &str) -> Result<()> {
        let sp = sidecar_path_for(asset);
        let mut sc = read_sidecar(&sp).unwrap_or_default();
        sc.label = label.to_string();
        write_sidecar(&sp, &sc)?;
        self.db.execute(
            "INSERT INTO files(path,name,folder,kind,rating,label) VALUES(?1,'','','raw',0,?2)
             ON CONFLICT(path) DO UPDATE SET label=?2",
            params![asset.to_string_lossy(), sc.label],
        )?;
        Ok(())
    }
}

pub fn read_sidecar(sc_path: &Path) -> Result<Sidecar> {
    let s = fs::read_to_string(sc_path)?;
    serde_json::from_str(&s).context("parse sidecar")
}

pub fn write_sidecar(sc_path: &Path, sc: &Sidecar) -> Result<()> {
    let tmp = sc_path.with_extension("koubou.tmp");
    fs::write(&tmp, serde_json::to_string_pretty(sc)?)?;
    fs::rename(&tmp, sc_path)?;
    Ok(())
}

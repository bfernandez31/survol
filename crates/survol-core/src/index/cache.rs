//! Per-blob cache of parsed files:
//! `.git/survol/cache/index/v<N>-<queries hash>/<ab>/<blob>.<lang>.json`.
//!
//! Keyed by content (blob id) and language, so it is shared by every review
//! and revision of the repository. The directory changes with
//! [`INDEX_VERSION`] and with the query files, so editing a query never
//! serves stale entries. The path in a cached entry is left empty.

use std::io;
use std::path::{Path, PathBuf};

use super::{FileIndex, INDEX_VERSION, Lang};

pub fn dir(survol_dir: &Path) -> PathBuf {
    survol_dir
        .join("cache")
        .join("index")
        .join(format!("v{INDEX_VERSION}-{}", queries_hash()))
}

fn queries_hash() -> String {
    let mut h = blake3::Hasher::new();
    for lang in Lang::ALL {
        h.update(lang.query_source().as_bytes());
    }
    h.finalize().to_hex()[..8].to_string()
}

fn path(dir: &Path, blob: &str, lang: Lang) -> PathBuf {
    let shard = blob.get(..2).unwrap_or("00");
    dir.join(shard).join(format!("{blob}.{}.json", lang.tag()))
}

pub fn load(dir: &Path, blob: &str, lang: Lang) -> Option<FileIndex> {
    let bytes = std::fs::read(path(dir, blob, lang)).ok()?;
    serde_json::from_slice(&bytes).ok()
}

pub fn save(dir: &Path, blob: &str, file: &FileIndex) -> io::Result<()> {
    let p = path(dir, blob, file.lang);
    std::fs::create_dir_all(p.parent().expect("sharded path"))?;
    // Write then rename: parallel writers and readers never see half a file.
    let tmp = p.with_extension(format!("tmp{}", std::process::id()));
    std::fs::write(&tmp, serde_json::to_vec(file)?)?;
    std::fs::rename(tmp, p)
}

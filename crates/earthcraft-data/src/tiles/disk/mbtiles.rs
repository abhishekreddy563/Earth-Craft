//! MBTiles 1.3 image tilesets; one async library worker serves every file.
use rusqlite::{OptionalExtension, params};
use std::{
    path::{Path, PathBuf},
    sync::{Arc, Mutex},
    time::{SystemTime, UNIX_EPOCH},
};
mod budget;
use budget::Budget;
const FORMATS: [(&str, &str); 3] = [
    ("jpg", "jpg"),
    ("png", "png"),
    ("bin", "application/octet-stream"),
];
const DATABASE_BUSY_TIMEOUT_MS: u64 = 1000;
const SQLITE_PAGE_CACHE_KIB: i32 = 256;
#[cfg(test)]
use super::DEFAULT_DISK_BUDGET_BYTES;

#[derive(Clone)]
struct Address {
    directory: PathBuf,
    zoom: u32,
    column: u32,
    row: u32,
    overview: bool,
}
impl Address {
    fn parse(root: &Path, path: &Path) -> Result<Self, crate::Error> {
        let fail = || crate::Error::from("invalid tile cache path");
        let xyz_row: u32 = path
            .file_stem()
            .and_then(|s| s.to_str())
            .ok_or_else(fail)?
            .parse()
            .map_err(|_| fail())?;
        let column_path = path.parent().ok_or_else(fail)?;
        let column: u32 = column_path
            .file_name()
            .and_then(|s| s.to_str())
            .ok_or_else(fail)?
            .parse()
            .map_err(|_| fail())?;
        let zoom_path = column_path.parent().ok_or_else(fail)?;
        let zoom: u32 = zoom_path
            .file_name()
            .and_then(|s| s.to_str())
            .ok_or_else(fail)?
            .parse()
            .map_err(|_| fail())?;
        if zoom > 30 || column as u64 >= 1u64 << zoom || xyz_row as u64 >= 1u64 << zoom {
            return Err(fail());
        }
        let directory = zoom_path.parent().ok_or_else(fail)?.to_path_buf();
        let relative = directory.strip_prefix(root).map_err(|_| fail())?;
        let provider = relative
            .components()
            .next()
            .and_then(|c| c.as_os_str().to_str())
            .ok_or_else(fail)?;
        if !super::Provider::CACHE_ALL
            .iter()
            .any(|p| p.dir() == provider)
        {
            return Err(fail());
        }
        let overview = provider == "esri_sat" && zoom <= 4;
        Ok(Self {
            directory,
            zoom,
            column,
            row: ((1u64 << zoom) - 1 - xyz_row as u64) as u32,
            overview,
        })
    }
    fn database(&self, suffix: &str) -> PathBuf {
        self.directory.join(format!("tiles-{suffix}.mbtiles"))
    }
}

pub(super) struct Mbtiles {
    root: PathBuf,
    connection: tokio::sync::OnceCell<tokio_rusqlite::Connection>,
    budget: Arc<Mutex<Option<Budget>>>,
    limit: u64,
}
impl Mbtiles {
    #[cfg(test)]
    pub(super) fn new(root: PathBuf) -> Self {
        Self::with_limit(root, DEFAULT_DISK_BUDGET_BYTES)
    }
    pub(super) fn with_limit(root: PathBuf, limit: u64) -> Self {
        Self {
            root,
            connection: tokio::sync::OnceCell::new(),
            budget: Arc::new(Mutex::new(None)),
            limit,
        }
    }
    pub(super) fn limit(&self) -> u64 {
        self.limit
    }
    async fn call<T: Send + 'static>(
        &self,
        work: impl FnOnce(&mut rusqlite::Connection) -> Result<T, crate::Error> + Send + 'static,
    ) -> Result<T, crate::Error> {
        let connection = self
            .connection
            .get_or_try_init(tokio_rusqlite::Connection::open_in_memory)
            .await?;
        connection.call_raw(work).await?
    }
    pub(super) async fn read(&self, path: &Path) -> Result<Option<Vec<u8>>, crate::Error> {
        let address = Address::parse(&self.root, path)?;
        let root = self.root.clone();
        let legacy = path.to_path_buf();
        let budget = self.budget.clone();
        let limit = self.limit;
        self.call(move |connection| {
            for (suffix,format) in FORMATS {
                let file=address.database(suffix);
                if !file.is_file() {continue;}
                let bytes=with_database(connection,&root,&file,false,format,|db| {
                    let bytes=db.query_row("SELECT tile_data FROM tile_cache.tiles WHERE zoom_level=?1 AND tile_column=?2 AND tile_row=?3",params![address.zoom,address.column,address.row],|row|row.get::<_,Vec<u8>>(0)).optional()?;
                    if bytes.is_some() { touch(db,&address)?; }
                    Ok(bytes)
                })?;
                if bytes.is_some() {return Ok(bytes);}
            }
            ensure_owned_path(&root,&legacy)?;
            let bytes=match std::fs::read(&legacy) {
                Ok(bytes)=>bytes,
                Err(error) if error.kind()==std::io::ErrorKind::NotFound=>return Ok(None),
                Err(error)=>return Err(error.into()),
            };
            // Lazy migration retains legacy fallback if optional persistence fails.
            if write(connection,&root,&address,&bytes,&budget,limit).is_ok() {
                let _=std::fs::remove_file(&legacy);
                if let Some(budget)=budget.lock().unwrap().as_mut() {budget.refresh(&legacy)?;}
            }
            Ok(Some(bytes))
        }).await
    }
    #[cfg(test)]
    pub(super) async fn write(&self, path: &Path, bytes: &[u8]) -> Result<(), crate::Error> {
        self.write_owned(path, bytes.to_vec()).await
    }
    pub(super) async fn write_owned(
        &self,
        path: &Path,
        bytes: Vec<u8>,
    ) -> Result<(), crate::Error> {
        let address = Address::parse(&self.root, path)?;
        let (root, budget, limit) = (self.root.clone(), self.budget.clone(), self.limit);
        self.call(move |connection| write(connection, &root, &address, &bytes, &budget, limit))
            .await
    }
    pub(super) async fn invalidate(&self, path: &Path) -> Result<(), crate::Error> {
        let address = Address::parse(&self.root, path)?;
        let root = self.root.clone();
        let legacy = path.to_path_buf();
        let budget = self.budget.clone();
        self.call(move |connection| {
            for (suffix,format) in FORMATS {
                let file=address.database(suffix); if !file.is_file() {continue;}
                with_database(connection,&root,&file,false,format,|db| {
                    db.execute("DELETE FROM tile_cache.tiles WHERE zoom_level=?1 AND tile_column=?2 AND tile_row=?3",params![address.zoom,address.column,address.row])?;Ok(())
                })?;
                if let Some(budget)=budget.lock().unwrap().as_mut() {budget.refresh(&file)?;}
            }
            ensure_owned_path(&root,&legacy)?;
            match std::fs::remove_file(&legacy) {Ok(())=>{},Err(e) if e.kind()==std::io::ErrorKind::NotFound=>{},Err(e)=>return Err(e.into())}
            if let Some(budget)=budget.lock().unwrap().as_mut() {budget.refresh(&legacy)?;}
            Ok(())
        }).await
    }
    pub(super) async fn clear(&self) -> Result<(), crate::Error> {
        let root = self.root.clone();
        let budget = self.budget.clone();
        self.call(move |_| {
            *budget.lock().unwrap() = None;
            for provider in super::Provider::CACHE_ALL {
                let path = root.join(provider.dir());
                let metadata = match std::fs::symlink_metadata(&path) {
                    Ok(m) => m,
                    Err(e) if e.kind() == std::io::ErrorKind::NotFound => continue,
                    Err(e) => return Err(e.into()),
                };
                if metadata.file_type().is_symlink() {
                    std::fs::remove_file(path)?;
                } else {
                    std::fs::remove_dir_all(path)?;
                }
            }
            Ok(())
        })
        .await
    }
    pub(super) async fn usage(&self) -> Result<u64, crate::Error> {
        let root = self.root.clone();
        self.call(move |_| Ok(super::count_file_bytes(&root)?))
            .await
    }
}
fn touch(db: &rusqlite::Connection, address: &Address) -> Result<(), crate::Error> {
    let timestamp = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos()
        .min(i64::MAX as u128) as i64;
    db.execute(
        "INSERT OR REPLACE INTO tile_cache.earthcraft_access VALUES (?1,?2,?3,?4,?5)",
        params![
            address.zoom,
            address.column,
            address.row,
            timestamp,
            address.overview
        ],
    )?;
    Ok(())
}
fn write(
    connection: &mut rusqlite::Connection,
    root: &Path,
    address: &Address,
    bytes: &[u8],
    budget: &Arc<Mutex<Option<Budget>>>,
    limit: u64,
) -> Result<(), crate::Error> {
    if bytes.len() as u64 > limit {
        return Err("encoded tile exceeds disk cache budget".into());
    }
    let (suffix, format) = if bytes.starts_with(b"\x89PNG\r\n\x1a\n") {
        FORMATS[1]
    } else if bytes.starts_with(&[0xff, 0xd8, 0xff]) {
        FORMATS[0]
    } else {
        FORMATS[2]
    };
    let file = address.database(suffix);
    let mut state = budget.lock().unwrap();
    if state.is_none() {
        *state = Some(Budget::scan(root)?);
    }
    let budget = state.as_mut().expect("budget was initialized");
    with_database(connection, root, &file, true, format, |db| {
        let transaction = db.transaction()?;
        transaction.execute(
            "INSERT OR REPLACE INTO tile_cache.tiles VALUES (?1,?2,?3,?4)",
            params![address.zoom, address.column, address.row, bytes],
        )?;
        touch(&transaction, address)?;
        transaction.execute("INSERT OR REPLACE INTO tile_cache.metadata VALUES ('minzoom',(SELECT min(zoom_level) FROM tile_cache.tiles))",[])?;
        transaction.execute("INSERT OR REPLACE INTO tile_cache.metadata VALUES ('maxzoom',(SELECT max(zoom_level) FROM tile_cache.tiles))",[])?;
        transaction.commit()?;
        Ok(())
    })?;
    budget.refresh(&file)?;
    budget.trim(connection, root, limit)?;
    Ok(())
}
fn ensure_owned_path(root: &Path, path: &Path) -> Result<(), crate::Error> {
    let relative = path
        .strip_prefix(root)
        .map_err(|_| crate::Error::from("tile path is outside cache root"))?;
    let mut current = root.to_path_buf();
    for part in relative.components() {
        if !matches!(part, std::path::Component::Normal(_)) {
            return Err("invalid tile path component".into());
        }
        current.push(part);
        match std::fs::symlink_metadata(&current) {
            Ok(metadata) if metadata.file_type().is_symlink() => {
                return Err("tile cache path contains a symlink".into());
            }
            Ok(_) => {}
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(e) => return Err(e.into()),
        }
    }
    Ok(())
}
fn with_database<T>(
    connection: &mut rusqlite::Connection,
    root: &Path,
    file: &Path,
    create: bool,
    format: &str,
    work: impl FnOnce(&mut rusqlite::Connection) -> Result<T, crate::Error>,
) -> Result<T, crate::Error> {
    ensure_owned_path(root, file)?;
    if create {
        std::fs::create_dir_all(file.parent().ok_or("missing tileset directory")?)?;
    }
    connection.busy_timeout(std::time::Duration::from_millis(DATABASE_BUSY_TIMEOUT_MS))?;
    connection.execute(
        "ATTACH DATABASE ?1 AS tile_cache",
        [file.to_string_lossy().as_ref()],
    )?;
    let result = (|| {
        connection.execute_batch("PRAGMA tile_cache.auto_vacuum=FULL; PRAGMA tile_cache.journal_mode=DELETE; PRAGMA tile_cache.synchronous=NORMAL; PRAGMA foreign_keys=ON;
            CREATE TABLE IF NOT EXISTS tile_cache.metadata (name TEXT PRIMARY KEY,value TEXT);
            CREATE TABLE IF NOT EXISTS tile_cache.tiles (zoom_level INTEGER,tile_column INTEGER,tile_row INTEGER,tile_data BLOB,PRIMARY KEY(zoom_level,tile_column,tile_row));
            CREATE TABLE IF NOT EXISTS tile_cache.earthcraft_access (zoom_level INTEGER,tile_column INTEGER,tile_row INTEGER,last_used INTEGER,overview INTEGER,PRIMARY KEY(zoom_level,tile_column,tile_row),FOREIGN KEY(zoom_level,tile_column,tile_row) REFERENCES tiles(zoom_level,tile_column,tile_row) ON DELETE CASCADE);
            CREATE INDEX IF NOT EXISTS tile_cache.access_age ON earthcraft_access(overview,last_used);")?;
        connection.pragma_update(Some("tile_cache"), "cache_size", -SQLITE_PAGE_CACHE_KIB)?;
        connection.execute(
            "INSERT OR IGNORE INTO tile_cache.metadata VALUES ('name',?1)",
            [file
                .parent()
                .unwrap_or(root)
                .strip_prefix(root)
                .unwrap_or(root)
                .to_string_lossy()
                .as_ref()],
        )?;
        connection.execute(
            "INSERT OR IGNORE INTO tile_cache.metadata VALUES ('format',?1)",
            [format],
        )?;
        work(connection)
    })();
    let detached = connection.execute("DETACH DATABASE tile_cache", []);
    let value = result?;
    detached?;
    Ok(value)
}

#[cfg(test)]
mod tests;

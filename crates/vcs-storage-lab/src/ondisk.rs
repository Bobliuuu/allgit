//! On-disk key-value candidates used both as location indexes (idxbench) and as cache backends (backendbench).
use std::path::{Path, PathBuf};

pub trait Kv: Send + Sync {
    fn name(&self) -> &'static str;
    fn put_batch(&self, items: &[(Vec<u8>, Vec<u8>)]);
    fn get(&self, k: &[u8]) -> Option<Vec<u8>>;
    fn contains(&self, k: &[u8]) -> bool {
        self.get(k).is_some()
    }
    /// Make everything durable / flushed to files, so on-disk size is final.
    fn finish(&self) {}
}

#[cfg(feature = "redb")]
pub struct Redb(redb::Database);
#[cfg(feature = "redb")]
const T: redb::TableDefinition<&[u8], &[u8]> = redb::TableDefinition::new("kv");
#[cfg(feature = "redb")]
impl Redb {
    pub fn open(dir: &Path) -> Redb {
        std::fs::create_dir_all(dir).unwrap();
        let mut db = redb::Database::builder().set_cache_size(256 << 20).create(dir.join("db.redb")).unwrap();
        let _ = db.check_integrity();
        Redb(db)
    }
}
#[cfg(feature = "redb")]
impl Kv for Redb {
    fn name(&self) -> &'static str {
        "redb_btree"
    }
    fn put_batch(&self, items: &[(Vec<u8>, Vec<u8>)]) {
        let mut w = self.0.begin_write().unwrap();
        w.set_durability(redb::Durability::None);
        {
            let mut t = w.open_table(T).unwrap();
            for (k, v) in items {
                t.insert(k.as_slice(), v.as_slice()).unwrap();
            }
        }
        w.commit().unwrap();
    }
    fn get(&self, k: &[u8]) -> Option<Vec<u8>> {
        let r = self.0.begin_read().unwrap();
        let t = r.open_table(T).ok()?;
        t.get(k).unwrap().map(|v| v.value().to_vec())
    }
    fn finish(&self) {
        // one empty durable commit flushes earlier non-durable ones
        let mut w = self.0.begin_write().unwrap();
        w.set_durability(redb::Durability::Immediate);
        w.commit().unwrap();
    }
}

#[cfg(feature = "sqlite")]
pub struct Sqlite(std::sync::Mutex<rusqlite::Connection>, PathBuf);
#[cfg(feature = "sqlite")]
impl Sqlite {
    pub fn open(dir: &Path) -> Sqlite {
        std::fs::create_dir_all(dir).unwrap();
        let c = rusqlite::Connection::open(dir.join("db.sqlite")).unwrap();
        c.execute_batch(
            "PRAGMA journal_mode=WAL; PRAGMA synchronous=OFF; PRAGMA cache_size=-262144; \
             CREATE TABLE IF NOT EXISTS kv(k BLOB PRIMARY KEY, v BLOB NOT NULL) WITHOUT ROWID;",
        )
        .unwrap();
        Sqlite(std::sync::Mutex::new(c), dir.to_owned())
    }
}
#[cfg(feature = "sqlite")]
impl Kv for Sqlite {
    fn name(&self) -> &'static str {
        "sqlite_wal_withoutrowid"
    }
    fn put_batch(&self, items: &[(Vec<u8>, Vec<u8>)]) {
        let mut c = self.0.lock().unwrap();
        let tx = c.transaction().unwrap();
        {
            let mut st = tx.prepare_cached("INSERT OR IGNORE INTO kv(k,v) VALUES(?1,?2)").unwrap();
            for (k, v) in items {
                st.execute((k, v)).unwrap();
            }
        }
        tx.commit().unwrap();
    }
    fn get(&self, k: &[u8]) -> Option<Vec<u8>> {
        let c = self.0.lock().unwrap();
        let mut st = c.prepare_cached("SELECT v FROM kv WHERE k=?1").unwrap();
        st.query_row([k], |r| r.get::<_, Vec<u8>>(0)).ok()
    }
    fn finish(&self) {
        let _ = self.0.lock().unwrap().execute_batch("PRAGMA wal_checkpoint(TRUNCATE);");
        let _ = &self.1;
    }
}

#[cfg(feature = "rocks")]
pub struct Rocks(rocksdb::DB);
#[cfg(feature = "rocks")]
impl Rocks {
    pub fn open(dir: &Path) -> Rocks {
        let mut o = rocksdb::Options::default();
        o.create_if_missing(true);
        o.set_compression_type(rocksdb::DBCompressionType::None);
        let mut bo = rocksdb::BlockBasedOptions::default();
        bo.set_bloom_filter(10.0, false);
        bo.set_block_cache(&rocksdb::Cache::new_lru_cache(256 << 20));
        o.set_block_based_table_factory(&bo);
        Rocks(rocksdb::DB::open(&o, dir).unwrap())
    }
}
#[cfg(feature = "rocks")]
impl Kv for Rocks {
    fn name(&self) -> &'static str {
        "rocksdb_lsm"
    }
    fn put_batch(&self, items: &[(Vec<u8>, Vec<u8>)]) {
        let mut b = rocksdb::WriteBatch::default();
        for (k, v) in items {
            b.put(k, v);
        }
        let mut wo = rocksdb::WriteOptions::default();
        wo.disable_wal(true);
        self.0.write_opt(b, &wo).unwrap();
    }
    fn get(&self, k: &[u8]) -> Option<Vec<u8>> {
        self.0.get(k).unwrap()
    }
    fn finish(&self) {
        let _ = self.0.flush();
        self.0.compact_range::<&[u8], &[u8]>(None, None);
    }
}

/// Git-style loose files: one file per key under a 256-way fanout directory. Writes are tmp+rename.
pub struct FsLoose(PathBuf);
impl FsLoose {
    pub fn open(dir: &Path) -> FsLoose {
        std::fs::create_dir_all(dir).unwrap();
        FsLoose(dir.to_owned())
    }
    fn path(&self, k: &[u8]) -> PathBuf {
        let h: String = k.iter().map(|b| format!("{b:02x}")).collect();
        self.0.join(&h[..2]).join(&h[2..])
    }
}
impl Kv for FsLoose {
    fn name(&self) -> &'static str {
        "fs_loose_fanout256"
    }
    fn put_batch(&self, items: &[(Vec<u8>, Vec<u8>)]) {
        for (k, v) in items {
            let p = self.path(k);
            std::fs::create_dir_all(p.parent().unwrap()).unwrap();
            let tmp = p.with_extension(format!("tmp{}", std::process::id()));
            std::fs::write(&tmp, v).unwrap();
            std::fs::rename(&tmp, &p).unwrap();
        }
    }
    fn get(&self, k: &[u8]) -> Option<Vec<u8>> {
        std::fs::read(self.path(k)).ok()
    }
    fn contains(&self, k: &[u8]) -> bool {
        self.path(k).exists()
    }
}

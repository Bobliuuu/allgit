//! `vcs archive [rev] -o file.tar|file.zip`: write the files of a revision as an archive.
//!
//! Both writers are hand-written (no compression crate is needed): tar is POSIX ustar, zip uses
//! the "stored" method. Entries are in path order with owner 0, no user names, mtime equal to
//! the newest commit time of the revision, mode 0644 or 0755 (symlinks 0777 and kept as links),
//! so the same revision always yields the same bytes. Revisions span every subscribed zone in
//! which they resolve. Limits: tar paths up to 255 bytes (split into prefix/name) and link
//! targets up to 100; zip up to 65535 entries and 4 GiB per file and in total.

use super::open;
use clap::Args;
use std::io::Write;
use vcs_objects::EntryMode;
use vcs_repo::Error;

#[derive(Args, Clone)]
pub struct ArchiveArgs {
    /// Revision to archive (default HEAD)
    pub rev: Option<String>,
    /// Output file; the format follows the extension (.tar or .zip)
    #[arg(short = 'o', long)]
    pub output: Option<std::path::PathBuf>,
    /// Format when writing to stdout or when the extension is unknown: tar or zip
    #[arg(long)]
    pub format: Option<String>,
    /// Prepend this directory prefix to every path (e.g. `proj-1.0/`)
    #[arg(long)]
    pub prefix: Option<String>,
}

pub struct Entry {
    pub path: String,
    pub mode: EntryMode,
    pub data: Vec<u8>,
}

fn bad(m: impl Into<String>) -> Error {
    Error::Msg(m.into())
}

fn io(e: std::io::Error) -> Error {
    Error::Io {
        path: "archive".into(),
        source: e,
    }
}

fn unix_mode(m: EntryMode) -> u32 {
    match m {
        EntryMode::Executable => 0o755,
        EntryMode::Symlink => 0o777,
        _ => 0o644,
    }
}

fn octal(field: &mut [u8], v: u64) -> Result<(), Error> {
    let s = format!("{:0w$o}", v, w = field.len() - 1);
    if s.len() > field.len() - 1 {
        return Err(bad("file too large for a tar header"));
    }
    field[..s.len()].copy_from_slice(s.as_bytes());
    Ok(())
}

pub fn tar_entry(out: &mut impl Write, e: &Entry, mtime: u64) -> Result<(), Error> {
    let symlink = e.mode == EntryMode::Symlink;
    let (name, prefix) = split_name(&e.path)?;
    let mut h = [0u8; 512];
    h[..name.len()].copy_from_slice(name.as_bytes());
    octal(&mut h[100..108], unix_mode(e.mode) as u64)?;
    octal(&mut h[108..116], 0)?;
    octal(&mut h[116..124], 0)?;
    octal(&mut h[124..136], if symlink { 0 } else { e.data.len() as u64 })?;
    octal(&mut h[136..148], mtime)?;
    h[148..156].copy_from_slice(b"        ");
    h[156] = if symlink { b'2' } else { b'0' };
    if symlink {
        if e.data.len() > 100 {
            return Err(bad(format!("symlink target of {} is longer than 100 bytes", e.path)));
        }
        h[157..157 + e.data.len()].copy_from_slice(&e.data);
    }
    h[257..263].copy_from_slice(b"ustar\0");
    h[263..265].copy_from_slice(b"00");
    h[345..345 + prefix.len()].copy_from_slice(prefix.as_bytes());
    let sum: u32 = h.iter().map(|b| *b as u32).sum();
    let s = format!("{sum:06o}\0 ");
    h[148..156].copy_from_slice(s.as_bytes());
    out.write_all(&h).map_err(io)?;
    if !symlink {
        out.write_all(&e.data).map_err(io)?;
        let pad = (512 - e.data.len() % 512) % 512;
        out.write_all(&vec![0u8; pad]).map_err(io)?;
    }
    Ok(())
}

/// Split into ustar `(name, prefix)`.
fn split_name(p: &str) -> Result<(&str, &str), Error> {
    if p.len() <= 100 {
        return Ok((p, ""));
    }
    for (i, _) in p.match_indices('/') {
        if i <= 155 && p.len() - i - 1 <= 100 {
            return Ok((&p[i + 1..], &p[..i]));
        }
    }
    Err(bad(format!("path too long for a tar archive: {p}")))
}

pub fn tar_finish(out: &mut impl Write) -> Result<(), Error> {
    out.write_all(&[0u8; 1024]).map_err(io)
}

fn crc32(data: &[u8]) -> u32 {
    let mut table = [0u32; 256];
    for (i, t) in table.iter_mut().enumerate() {
        let mut c = i as u32;
        for _ in 0..8 {
            c = if c & 1 != 0 { 0xEDB8_8320 ^ (c >> 1) } else { c >> 1 };
        }
        *t = c;
    }
    !data.iter().fold(!0u32, |c, b| table[((c ^ *b as u32) & 0xff) as usize] ^ (c >> 8))
}

/// DOS (time, date) of a unix time, UTC, clamped to the DOS range (1980..2107).
fn dos_time(ts: u64) -> (u16, u16) {
    let days = (ts / 86400) as i64;
    let secs = ts % 86400;
    // civil from days (Howard Hinnant)
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = yoe + era * 400 + i64::from(m <= 2);
    if y < 1980 {
        return (0, (1 << 5) | 1);
    }
    let y = y.min(2107);
    let time = ((secs / 3600) << 11) | (((secs % 3600) / 60) << 5) | ((secs % 60) / 2);
    (time as u16, (((y - 1980) << 9) | (m << 5) | d) as u16)
}

pub struct ZipWriter<W: Write> {
    out: W,
    pos: u64,
    central: Vec<u8>,
    count: usize,
    time: (u16, u16),
}

impl<W: Write> ZipWriter<W> {
    pub fn new(out: W, mtime: u64) -> Self {
        ZipWriter {
            out,
            pos: 0,
            central: Vec::new(),
            count: 0,
            time: dos_time(mtime),
        }
    }
    fn w(&mut self, b: &[u8]) -> Result<(), Error> {
        self.pos += b.len() as u64;
        self.out.write_all(b).map_err(io)
    }
    pub fn add(&mut self, e: &Entry) -> Result<(), Error> {
        if e.data.len() as u64 >= u32::MAX as u64 || self.pos >= u32::MAX as u64 - e.data.len() as u64 - 1024 || self.count >= 65534 {
            return Err(bad("archive too large for zip without zip64"));
        }
        let crc = crc32(&e.data);
        let name = e.path.as_bytes();
        let (t, d) = self.time;
        let mut lh = Vec::new();
        lh.extend(0x0403_4b50u32.to_le_bytes());
        lh.extend(20u16.to_le_bytes()); // version needed
        lh.extend(0x0800u16.to_le_bytes()); // utf-8 names
        lh.extend(0u16.to_le_bytes()); // stored
        lh.extend(t.to_le_bytes());
        lh.extend(d.to_le_bytes());
        lh.extend(crc.to_le_bytes());
        lh.extend((e.data.len() as u32).to_le_bytes());
        lh.extend((e.data.len() as u32).to_le_bytes());
        lh.extend((name.len() as u16).to_le_bytes());
        lh.extend(0u16.to_le_bytes());
        lh.extend(name);
        let offset = self.pos as u32;
        self.w(&lh)?;
        self.w(&e.data)?;
        let ftype: u32 = if e.mode == EntryMode::Symlink { 0o120000 } else { 0o100000 };
        let c = &mut self.central;
        c.extend(0x0201_4b50u32.to_le_bytes());
        c.extend(((3u16 << 8) | 20).to_le_bytes()); // made by unix, 2.0
        c.extend(20u16.to_le_bytes());
        c.extend(0x0800u16.to_le_bytes());
        c.extend(0u16.to_le_bytes());
        c.extend(t.to_le_bytes());
        c.extend(d.to_le_bytes());
        c.extend(crc.to_le_bytes());
        c.extend((e.data.len() as u32).to_le_bytes());
        c.extend((e.data.len() as u32).to_le_bytes());
        c.extend((name.len() as u16).to_le_bytes());
        c.extend([0u8; 8]); // extra, comment, disk, internal attrs
        c.extend(((ftype | unix_mode(e.mode)) << 16).to_le_bytes());
        c.extend(offset.to_le_bytes());
        c.extend(name);
        self.count += 1;
        Ok(())
    }
    pub fn finish(mut self) -> Result<(), Error> {
        let start = self.pos as u32;
        let central = std::mem::take(&mut self.central);
        self.w(&central)?;
        let mut e = Vec::new();
        e.extend(0x0605_4b50u32.to_le_bytes());
        e.extend([0u8; 4]);
        e.extend((self.count as u16).to_le_bytes());
        e.extend((self.count as u16).to_le_bytes());
        e.extend((central.len() as u32).to_le_bytes());
        e.extend(start.to_le_bytes());
        e.extend(0u16.to_le_bytes());
        self.w(&e)?;
        self.out.flush().map_err(io)
    }
}

pub fn run(a: &ArchiveArgs) -> Result<(), Error> {
    let (repo, _) = open()?;
    let rev = a.rev.as_deref().unwrap_or("HEAD");
    let ext = a
        .output
        .as_ref()
        .and_then(|p| p.extension())
        .and_then(|e| e.to_str())
        .map(str::to_ascii_lowercase);
    let fmt = match (a.format.as_deref(), ext.as_deref()) {
        (Some(f), _) => f.to_owned(),
        (None, Some(e @ ("tar" | "zip"))) => e.to_owned(),
        _ => return Err(bad("cannot tell the format: use -o file.tar / file.zip, or --format tar|zip")),
    };
    if fmt != "tar" && fmt != "zip" {
        return Err(bad(format!("unknown archive format {fmt:?} (expected tar or zip)")));
    }
    let files = repo.rev_files(rev)?;
    let mtime = repo.rev_time(rev)?;
    let prefix = a.prefix.clone().unwrap_or_default();
    let sink: Box<dyn Write> = match &a.output {
        Some(p) => Box::new(std::fs::File::create(p).map_err(|source| Error::Io { path: p.clone(), source })?),
        None => Box::new(std::io::stdout().lock()),
    };
    let mut out = std::io::BufWriter::new(sink);
    let entries = files.into_iter().map(|(p, (mode, id))| -> Result<Entry, Error> {
        Ok(Entry {
            path: format!("{prefix}{p}"),
            mode,
            data: repo.content(&id)?,
        })
    });
    if fmt == "tar" {
        for e in entries {
            tar_entry(&mut out, &e?, mtime)?;
        }
        tar_finish(&mut out)?;
        out.flush().map_err(io)?;
    } else {
        let mut z = ZipWriter::new(&mut out, mtime);
        for e in entries {
            z.add(&e?)?;
        }
        z.finish()?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn crc_and_dos_time() {
        assert_eq!(crc32(b"123456789"), 0xCBF4_3926);
        // 2023-11-14 22:13:20 UTC
        let (t, d) = dos_time(1_700_000_000);
        assert_eq!(d, ((43 << 9) | (11 << 5) | 14) as u16);
        assert_eq!(t, ((22 << 11) | (13 << 5) | 10) as u16);
        assert_eq!(dos_time(0).1, (1 << 5) | 1);
    }

    #[test]
    fn long_names_split() {
        let long = format!("{}/{}", "d".repeat(120), "f".repeat(50));
        let (n, p) = split_name(&long).unwrap();
        assert_eq!((n.len(), p.len()), (50, 120));
        assert!(split_name(&"x".repeat(300)).is_err());
    }
}

//! File reading with transparent gzip.
use std::fs::File;
use std::io::{BufReader, Read};
use std::path::Path;

pub fn read_all(path: &Path) -> std::io::Result<Vec<u8>> {
    let mut f = File::open(path)?;
    let mut head = [0u8; 2];
    let n = f.read(&mut head)?;
    let f = File::open(path)?;
    let size = f.metadata().map(|m| m.len() as usize).unwrap_or(0);
    if n == 2 && head == [0x1f, 0x8b] {
        let mut out = Vec::with_capacity(size * 8);
        flate2::read::MultiGzDecoder::new(BufReader::with_capacity(1 << 20, f)).read_to_end(&mut out)?;
        Ok(out)
    } else {
        let mut out = Vec::with_capacity(size);
        BufReader::new(f).read_to_end(&mut out)?;
        Ok(out)
    }
}

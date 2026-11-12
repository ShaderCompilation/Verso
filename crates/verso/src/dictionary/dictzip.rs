//! Seekable dictzip, using its RA extra field rather than inflating the file.
use anyhow::{Context, Result, ensure};
use flate2::{Decompress, FlushDecompress};
use std::{
    collections::VecDeque,
    fs::File,
    io::{Read, Seek, SeekFrom},
    path::Path,
};

pub struct Definitions {
    file: File,
    zip: Option<Zip>,
    pub len: u64,
}
struct Zip {
    chunk: usize,
    offsets: Vec<u64>,
    cache: VecDeque<(usize, Vec<u8>)>,
}
fn u16le(b: &[u8]) -> usize {
    u16::from_le_bytes([b[0], b[1]]) as usize
}
impl Definitions {
    pub fn open(path: &Path, compressed: bool) -> Result<Self> {
        let mut file = File::open(path)?;
        let file_len = file.metadata()?.len();
        if !compressed {
            return Ok(Self { file, zip: None, len: file_len });
        }
        let mut header = [0; 12];
        file.read_exact(&mut header)?;
        ensure!(
            header[..3] == [31, 139, 8] && header[3] & 4 != 0 && header[3] & 0xe0 == 0,
            "Definitions are not dictzip (missing random-access header)"
        );
        let mut extra = vec![0; u16le(&header[10..])];
        file.read_exact(&mut extra)?;
        let mut pos = 0;
        let mut table = None;
        while pos < extra.len() {
            ensure!(pos + 4 <= extra.len(), "Truncated gzip extra field");
            let size = u16le(&extra[pos + 2..]);
            let data = extra.get(pos + 4..pos + 4 + size).context("Truncated dictzip table")?;
            if &extra[pos..pos + 2] == b"RA" {
                ensure!(data.len() >= 6 && u16le(data) == 1, "Unsupported dictzip version");
                let chunk = u16le(&data[2..]);
                let count = u16le(&data[4..]);
                ensure!(chunk > 0 && count > 0 && data.len() == 6 + count * 2, "Invalid dictzip table");
                table = Some((chunk, data[6..].as_chunks::<2>().0.iter().map(|b| u16le(b)).collect::<Vec<_>>()));
            }
            pos += 4 + size;
        }
        // Optional gzip filename and comment are NUL terminated and bounded.
        for flag in [8, 16] {
            if header[3] & flag != 0 {
                let mut terminated = false;
                for _ in 0..65536 {
                    let mut byte = [0];
                    file.read_exact(&mut byte)?;
                    if byte[0] == 0 {
                        terminated = true;
                        break;
                    }
                }
                ensure!(terminated, "Oversized gzip header");
            }
        }
        if header[3] & 2 != 0 {
            let mut crc = [0; 2];
            file.read_exact(&mut crc)?;
        }
        let (chunk, sizes) = table.context("Missing dictzip RA table")?;
        let mut offsets = vec![file.stream_position()?];
        for size in sizes {
            ensure!(size > 0, "Empty compressed chunk");
            offsets.push(offsets.last().unwrap() + size as u64);
        }
        // dictzip has one gzip member, whose ISIZE fits in 32 bits.
        ensure!(file_len >= 8 && *offsets.last().unwrap() <= file_len - 8, "Truncated dictzip data");
        file.seek(SeekFrom::End(-4))?;
        let mut size = [0; 4];
        file.read_exact(&mut size)?;
        let len = u32::from_le_bytes(size) as u64;
        ensure!(
            len > (offsets.len() as u64 - 2) * chunk as u64 && len <= (offsets.len() as u64 - 1) * chunk as u64,
            "Invalid dictzip uncompressed size"
        );
        Ok(Self { file, zip: Some(Zip { chunk, offsets, cache: VecDeque::new() }), len })
    }
    pub fn read(&mut self, offset: u64, size: u32) -> Result<Vec<u8>> {
        ensure!(size <= 1024 * 1024, "Definition exceeds the 1 MiB limit");
        let end = offset.checked_add(size as u64).context("Definition offset overflow")?;
        ensure!(end <= self.len, "Definition points outside the data file");
        let mut output = Vec::with_capacity(size as usize);
        let Some(zip) = &mut self.zip else {
            self.file.seek(SeekFrom::Start(offset))?;
            output.resize(size as usize, 0);
            self.file.read_exact(&mut output)?;
            return Ok(output);
        };
        let mut at = offset;
        while at < end {
            let number = (at / zip.chunk as u64) as usize;
            let block = if let Some(i) = zip.cache.iter().position(|(n, _)| *n == number) {
                zip.cache.remove(i).unwrap().1
            } else {
                self.file.seek(SeekFrom::Start(zip.offsets[number]))?;
                let mut compressed = vec![0; (zip.offsets[number + 1] - zip.offsets[number]) as usize];
                self.file.read_exact(&mut compressed)?;
                let expected = (self.len - number as u64 * zip.chunk as u64).min(zip.chunk as u64) as usize;
                let mut block = vec![0; expected + 1];
                let mut decoder = Decompress::new(false);
                decoder.decompress(&compressed, &mut block, FlushDecompress::Sync)?;
                ensure!(
                    decoder.total_out() == expected as u64 && decoder.total_in() == compressed.len() as u64,
                    "Corrupt dictzip block"
                );
                block.truncate(expected);
                block
            };
            let start = (at % zip.chunk as u64) as usize;
            let count = (block.len() - start).min((end - at) as usize);
            output.extend_from_slice(&block[start..start + count]);
            at += count as u64;
            zip.cache.push_back((number, block));
            if zip.cache.len() > 8 {
                zip.cache.pop_front();
            }
        }
        Ok(output)
    }
}

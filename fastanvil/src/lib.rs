//! For handling Minecraft's region format, Anvil.
//!
//! `anvil::Region` can be given a `Read` and `Seek` type eg a file in order to extract chunk data.

use byteorder::{BigEndian, ReadBytesExt};
use fastnbt::de::from_bytes;
use flate2::read::ZlibDecoder;
use num_enum::TryFromPrimitive;
use serde::de::DeserializeOwned;
use std::io::{Read, Seek, SeekFrom};
use std::{cell::RefCell, convert::TryFrom};

/// the size in bytes of a 'sector' in a region file. Sectors are Minecraft's size unit
/// for chunks. For example, a chunk might be `3 * SECTOR_SIZE` bytes.
pub const SECTOR_SIZE: usize = 4096;

/// the size of the region file header.
pub const HEADER_SIZE: usize = 2 * SECTOR_SIZE;

pub mod biome;
pub mod tex;

mod bits;
mod dimension;
mod files;
mod java;
mod render;
mod rendered_palette;

pub use bits::*;
pub use dimension::*;
pub use files::*;
pub use java::*;
pub use render::*;
pub use rendered_palette::*;

#[cfg(test)]
mod test;

/// Various compression schemes that NBT data is typically compressed with.
#[derive(Debug, TryFromPrimitive)]
#[repr(u8)]
pub enum CompressionScheme {
    Gzip = 1,
    Zlib = 2,
    Uncompressed = 3,
}

/// A Minecraft Region. Allows access to chunk data, handling decompression.
pub struct RegionBuffer<S: Seek + Read> {
    data: RefCell<S>,
}

impl<S: Seek + Read, C: Chunk + DeserializeOwned> Region<C> for RegionBuffer<S> {
    fn chunk(&self, x: CCoord, z: CCoord) -> Option<C> {
        let loc = self.chunk_location(x.0 as usize, z.0 as usize).ok()?;

        let data = self.load_chunk(loc.x, loc.z).ok()?;

        from_bytes::<C>(&data).ok()
    }
}

/// The location of chunk data within a Region file.
#[derive(Debug, PartialEq)]
pub struct ChunkLocation {
    pub begin_sector: usize,
    pub sector_count: usize,
    pub x: usize,
    pub z: usize,
}

/// Encodes how the NBT-Data is compressed
#[derive(Debug)]
pub struct ChunkMeta {
    pub compressed_len: u32,
    pub compression_scheme: CompressionScheme,
}

impl ChunkMeta {
    pub fn new(data: &[u8]) -> Result<Self> {
        if data.len() < 5 {
            return Err(Error::InsufficientData);
        }

        let mut buf = &data[..5];
        let len = buf.read_u32::<BigEndian>()?;
        let scheme = buf.read_u8()?;
        let scheme = CompressionScheme::try_from(scheme).map_err(|_| Error::InvalidChunkMeta)?;

        Ok(Self {
            compressed_len: len - 1, // this len include the compression byte.
            compression_scheme: scheme,
        })
    }
}

impl<S: Seek + Read> RegionBuffer<S> {
    pub fn new(data: S) -> Self {
        Self {
            data: RefCell::new(data),
        }
    }

    /// Return the (region-relative) Chunk location (x, z)
    pub fn chunk_location(&self, x: usize, z: usize) -> Result<ChunkLocation> {
        if x >= 32 || z >= 32 {
            return Err(Error::InvalidOffset(x, z));
        }

        let pos = 4 * ((x % 32) + (z % 32) * 32);

        self.data.borrow_mut().seek(SeekFrom::Start(pos as u64))?;

        let mut buf = [0u8; 4];

        self.data.borrow_mut().read_exact(&mut buf[..])?;

        let mut off = 0usize;
        off |= (buf[0] as usize) << 16;
        off |= (buf[1] as usize) << 8;
        off |= buf[2] as usize;
        let count = buf[3] as usize;
        Ok(ChunkLocation {
            begin_sector: off,
            sector_count: count,
            x,
            z,
        })
    }

    /// Return the raw, uncompressed NBT data for a chunk at the
    /// (region-relative) Chunk location (x, z). Region's hold 32 by 32 chunks.
    ///
    /// Can be further processed with [`stream::Parser`] or even with
    /// `Blob::from_reader()` of hematite_nbt.
    ///
    /// [`stream::Parser`]: ../stream/struct.Parser.html
    pub fn load_chunk(&self, x: usize, z: usize) -> Result<Vec<u8>> {
        let data = self.load_raw_chunk_at(x, z)?;
        decompress_chunk(&data)
    }

    /// Call function with each uncompressed, non-empty chunk, calls f(x, z, data).
    pub fn for_each_chunk(&mut self, mut f: impl FnMut(usize, usize, &Vec<u8>)) -> Result<()> {
        let mut offsets = Vec::<ChunkLocation>::new();

        // Build list of existing chunks
        for x in 0..32 {
            for z in 0..32 {
                let loc = self.chunk_location(x, z)?;
                // 0,0 chunk location means the chunk isn't present.
                // cannot decide if this means we should return an error from chunk_location() or not.
                if loc.begin_sector != 0 && loc.sector_count != 0 {
                    offsets.push(loc);
                }
            }
        }

        // sort so we linearly seek through the file.
        // might make things easier on a HDD [citation needed]
        offsets.sort_by(|o1, o2| o2.begin_sector.cmp(&o1.begin_sector));

        for offset in offsets {
            let chunk = self.load_chunk(offset.x, offset.z)?;
            f(offset.x, offset.z, &chunk);
        }

        Ok(())
    }

    /// Return the raw, compressed data for a chunk at ChunkLocation
    fn load_raw_chunk(&self, offset: &ChunkLocation, dest: &mut Vec<u8>) -> Result<()> {
        self.data.borrow_mut().seek(SeekFrom::Start(
            offset.begin_sector as u64 * SECTOR_SIZE as u64,
        ))?;

        dest.resize(5, 0);
        self.data.borrow_mut().read_exact(&mut dest[0..5])?;
        let metadata = ChunkMeta::new(&dest[..5])?;

        dest.resize(5 + metadata.compressed_len as usize, 0u8);

        self.data.borrow_mut().read_exact(&mut dest[5..])?;
        Ok(())
    }

    /// Return the raw, compressed data for a chunk at the (region-relative) Chunk location (x, z)
    fn load_raw_chunk_at(&self, x: usize, z: usize) -> Result<Vec<u8>> {
        let location = self.chunk_location(x, z)?;

        // 0,0 chunk location means the chunk isn't present.
        if location.begin_sector != 0 && location.sector_count != 0 {
            let mut buf = Vec::new();
            self.load_raw_chunk(&location, &mut buf)?;
            Ok(buf)
        } else {
            Err(Error::ChunkNotFound)
        }
    }
}

// Read Information Bytes of Minecraft Chunk and decompress it
fn decompress_chunk(data: &[u8]) -> Result<Vec<u8>> {
    // Metadata encodes the length in bytes and the compression type
    let meta = ChunkMeta::new(data)?;

    // compressed data starts at byte 5
    let inbuf = &mut &data[5..];
    let mut decoder = match meta.compression_scheme {
        CompressionScheme::Zlib => ZlibDecoder::new(inbuf),
        _ => panic!("unknown compression scheme (gzip?)"),
    };
    let mut outbuf = Vec::new();
    // read the whole Chunk
    decoder.read_to_end(&mut outbuf)?;
    Ok(outbuf)
}

#[derive(Debug)]
pub enum Error {
    InsufficientData,
    IO(std::io::Error),
    InvalidOffset(usize, usize),
    InvalidChunkMeta,
    ChunkNotFound,
}

impl From<std::io::Error> for Error {
    fn from(err: std::io::Error) -> Error {
        Error::IO(err)
    }
}

pub type Result<T> = std::result::Result<T, Error>;

impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Error::InsufficientData => f.write_str("insufficient data to parse chunk metadata"),
            Error::IO(e) => f.write_fmt(format_args!("io error: {:?}", e)),
            Error::InvalidOffset(x, z) => {
                f.write_fmt(format_args!("invalid offset: x = {}, z = {}", x, z))
            }
            Error::InvalidChunkMeta => {
                f.write_str("compression scheme was not recognised for chunk")
            }
            Error::ChunkNotFound => f.write_str("chunk not found in region"),
        }
    }
}

impl std::error::Error for Error {}

#[cfg(test)]
use std::io::Cursor;
#[cfg(test)]
pub struct Builder {
    inner: Vec<u8>,
}

#[cfg(test)]
impl Builder {
    pub fn new() -> Self {
        Self { inner: Vec::new() }
    }

    pub fn location(mut self, offset: u32, sectors: u8) -> Self {
        self.inner.extend_from_slice(&offset.to_be_bytes()[1..4]);
        self.inner.push(sectors);
        self
    }

    pub fn build(mut self) -> Cursor<Vec<u8>> {
        let padded_sector_count = (self.inner.len() / SECTOR_SIZE) + 1;
        self.inner.resize(padded_sector_count * SECTOR_SIZE, 0);
        Cursor::new(self.inner)
    }

    pub fn build_unpadded(self) -> Cursor<Vec<u8>> {
        Cursor::new(self.inner)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn invalid_offset() {
        let r = Builder::new().location(2, 1).build();
        let r = RegionBuffer::new(r);
        match r.chunk_location(32, 32) {
            Err(Error::InvalidOffset(32, 32)) => {}
            _ => panic!("should error"),
        }
    }

    #[test]
    fn invalid_offset_only_in_x() {
        let r = Builder::new().location(2, 1).build();
        let r = RegionBuffer::new(r);
        match r.chunk_location(32, 0) {
            Err(Error::InvalidOffset(32, 0)) => {}
            _ => panic!("should error"),
        }
    }

    #[test]
    fn invalid_offset_only_in_z() {
        let r = Builder::new().location(2, 1).build();
        let r = RegionBuffer::new(r);
        match r.chunk_location(0, 32) {
            Err(Error::InvalidOffset(0, 32)) => {}
            _ => panic!("should error"),
        }
    }

    #[test]
    fn offset_beyond_data_given() {
        let r = Builder::new().location(2, 1).build_unpadded();
        let r = RegionBuffer::new(r);
        match r.chunk_location(1, 0) {
            Err(Error::IO(inner)) if inner.kind() == std::io::ErrorKind::UnexpectedEof => {}
            o => panic!("should error {:?}", o),
        }
    }
    #[test]
    fn first_location() -> Result<()> {
        let r = Builder::new().location(2, 1).build();
        let r = RegionBuffer::new(r);

        assert_eq!(
            ChunkLocation {
                begin_sector: 2,
                sector_count: 1,
                x: 0,
                z: 0
            },
            r.chunk_location(0, 0)?
        );
        Ok(())
    }
}

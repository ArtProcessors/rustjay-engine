//! `.dmxrec` file format — recorded DMX as a flat, gzipped event log.
//!
//! See `DMX_RECORDER.md`. The file is gzip (flate2) over an 8-byte header
//! (`b"DMXREC"` + `u16` version LE) followed by 9-byte little-endian event
//! records: `u32 t_ms | u16 universe | u16 channel | u8 value`.
//!
//! The log is append-only and written in time order during a recording pass
//! ([`RecWriter`] streams to disk, so a crash loses seconds, not the take).
//! Duration is not stored — gzip streams can't seek back to patch a header —
//! it is the max `t_ms`, computed on load.

use std::fs::File;
use std::io::{self, BufReader, BufWriter, Read, Write};
use std::path::Path;

use flate2::read::GzDecoder;
use flate2::write::GzEncoder;

const MAGIC: &[u8; 6] = b"DMXREC";
const VERSION: u16 = 1;
const EVENT_LEN: usize = 9;

/// One channel change at one moment, relative to recording start.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RecEvent {
    pub t_ms: u32,
    pub universe: u16,
    pub channel: u16,
    pub value: u8,
}

impl RecEvent {
    fn to_bytes(self) -> [u8; EVENT_LEN] {
        let mut b = [0u8; EVENT_LEN];
        b[0..4].copy_from_slice(&self.t_ms.to_le_bytes());
        b[4..6].copy_from_slice(&self.universe.to_le_bytes());
        b[6..8].copy_from_slice(&self.channel.to_le_bytes());
        b[8] = self.value;
        b
    }

    fn from_bytes(b: &[u8; EVENT_LEN]) -> Self {
        Self {
            t_ms: u32::from_le_bytes(b[0..4].try_into().unwrap()),
            universe: u16::from_le_bytes(b[4..6].try_into().unwrap()),
            channel: u16::from_le_bytes(b[6..8].try_into().unwrap()),
            value: b[8],
        }
    }
}

/// Streaming `.dmxrec` writer. Events must be appended in time order (the
/// recorder produces them that way); call [`RecWriter::finish`] to flush the
/// gzip trailer — dropping without it truncates the file.
pub struct RecWriter {
    enc: GzEncoder<BufWriter<File>>,
}

impl RecWriter {
    pub fn create(path: impl AsRef<Path>) -> io::Result<Self> {
        let file = BufWriter::new(File::create(path)?);
        let mut enc = GzEncoder::new(file, flate2::Compression::default());
        enc.write_all(MAGIC)?;
        enc.write_all(&VERSION.to_le_bytes())?;
        Ok(Self { enc })
    }

    pub fn write(&mut self, event: RecEvent) -> io::Result<()> {
        self.enc.write_all(&event.to_bytes())
    }

    /// Flush buffered data through to the OS so a crash after this point
    /// loses at most what came later. The gzip stream stays open.
    pub fn sync(&mut self) -> io::Result<()> {
        self.enc.flush()
    }

    pub fn finish(self) -> io::Result<()> {
        self.enc.finish()?.flush()
    }
}

/// Read a `.dmxrec` file into its event list (in file = time order).
///
/// A truncated trailing event (crash mid-write) is tolerated and dropped.
pub fn read_rec(path: impl AsRef<Path>) -> io::Result<Vec<RecEvent>> {
    let mut dec = GzDecoder::new(BufReader::new(File::open(path)?));
    let mut header = [0u8; 8];
    dec.read_exact(&mut header)?;
    if &header[0..6] != MAGIC {
        return Err(io::Error::new(io::ErrorKind::InvalidData, "not a .dmxrec file"));
    }
    let version = u16::from_le_bytes([header[6], header[7]]);
    if version != VERSION {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            format!("unsupported .dmxrec version {version}"),
        ));
    }

    let mut body = Vec::new();
    dec.read_to_end(&mut body)?;
    let mut events = Vec::with_capacity(body.len() / EVENT_LEN);
    for chunk in body.chunks_exact(EVENT_LEN) {
        events.push(RecEvent::from_bytes(chunk.try_into().unwrap()));
    }
    Ok(events)
}

/// Recording duration = timestamp of the last event (the log is time-ordered).
pub fn rec_duration_ms(events: &[RecEvent]) -> u32 {
    events.last().map_or(0, |e| e.t_ms)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_path(name: &str) -> std::path::PathBuf {
        std::env::temp_dir().join(format!("rustjay-rec-{}-{name}", std::process::id()))
    }

    #[test]
    fn roundtrip() {
        let path = temp_path("roundtrip.dmxrec");
        let events = vec![
            RecEvent { t_ms: 0, universe: 1, channel: 0, value: 255 },
            RecEvent { t_ms: 16, universe: 1, channel: 1, value: 128 },
            RecEvent { t_ms: 5000, universe: 400, channel: 511, value: 1 },
        ];

        let mut w = RecWriter::create(&path).unwrap();
        for e in &events {
            w.write(*e).unwrap();
        }
        w.finish().unwrap();

        let read = read_rec(&path).unwrap();
        assert_eq!(read, events);
        assert_eq!(rec_duration_ms(&read), 5000);
        std::fs::remove_file(&path).ok();
    }

    #[test]
    fn empty_recording() {
        let path = temp_path("empty.dmxrec");
        RecWriter::create(&path).unwrap().finish().unwrap();
        let read = read_rec(&path).unwrap();
        assert!(read.is_empty());
        assert_eq!(rec_duration_ms(&read), 0);
        std::fs::remove_file(&path).ok();
    }

    #[test]
    fn rejects_garbage_and_wrong_version() {
        let path = temp_path("garbage.dmxrec");
        std::fs::write(&path, b"not gzip at all").unwrap();
        assert!(read_rec(&path).is_err());

        // Valid gzip, wrong magic.
        let f = BufWriter::new(File::create(&path).unwrap());
        let mut enc = GzEncoder::new(f, flate2::Compression::default());
        enc.write_all(b"BOGUS\x00\x01\x00").unwrap();
        enc.finish().unwrap().flush().unwrap();
        assert!(read_rec(&path).is_err());
        std::fs::remove_file(&path).ok();
    }

    #[test]
    fn event_encoding_is_9_bytes_le() {
        let e = RecEvent { t_ms: 0x01020304, universe: 0x0506, channel: 0x0708, value: 0x09 };
        let b = e.to_bytes();
        assert_eq!(b, [0x04, 0x03, 0x02, 0x01, 0x06, 0x05, 0x08, 0x07, 0x09]);
        assert_eq!(RecEvent::from_bytes(&b), e);
    }
}

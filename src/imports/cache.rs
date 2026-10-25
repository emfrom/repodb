use crate::Result;
use std::{fs, io::BufReader, path::Path};

const SHA256_K: [u32; 64] = [
    0x428a2f98, 0x71374491, 0xb5c0fbcf, 0xe9b5dba5, 0x3956c25b, 0x59f111f1, 0x923f82a4, 0xab1c5ed5,
    0xd807aa98, 0x12835b01, 0x243185be, 0x550c7dc3, 0x72be5d74, 0x80deb1fe, 0x9bdc06a7, 0xc19bf174,
    0xe49b69c1, 0xefbe4786, 0x0fc19dc6, 0x240ca1cc, 0x2de92c6f, 0x4a7484aa, 0x5cb0a9dc, 0x76f988da,
    0x983e5152, 0xa831c66d, 0xb00327c8, 0xbf597fc7, 0xc6e00bf3, 0xd5a79147, 0x06ca6351, 0x14292967,
    0x27b70a85, 0x2e1b2138, 0x4d2c6dfc, 0x53380d13, 0x650a7354, 0x766a0abb, 0x81c2c92e, 0x92722c85,
    0xa2bfe8a1, 0xa81a664b, 0xc24b8b70, 0xc76c51a3, 0xd192e819, 0xd6990624, 0xf40e3585, 0x106aa070,
    0x19a4c116, 0x1e376c08, 0x2748774c, 0x34b0bcb5, 0x391c0cb3, 0x4ed8aa4a, 0x5b9cca4f, 0x682e6ff3,
    0x748f82ee, 0x78a5636f, 0x84c87814, 0x8cc70208, 0x90befffa, 0xa4506ceb, 0xbef9a3f7, 0xc67178f2,
];

pub(super) struct Sha256 {
    state: [u32; 8],
    buffer: [u8; 64],
    buffered: usize,
    bit_len: u64,
}

impl Sha256 {
    pub(super) fn new() -> Self {
        Self {
            state: [
                0x6a09e667, 0xbb67ae85, 0x3c6ef372, 0xa54ff53a, 0x510e527f, 0x9b05688c, 0x1f83d9ab,
                0x5be0cd19,
            ],
            buffer: [0; 64],
            buffered: 0,
            bit_len: 0,
        }
    }

    pub(super) fn update(&mut self, mut bytes: &[u8]) {
        self.bit_len = self
            .bit_len
            .wrapping_add((bytes.len() as u64).wrapping_mul(8));
        if self.buffered > 0 {
            let count = (64 - self.buffered).min(bytes.len());
            self.buffer[self.buffered..self.buffered + count].copy_from_slice(&bytes[..count]);
            self.buffered += count;
            bytes = &bytes[count..];
            if self.buffered == 64 {
                compress_block(&mut self.state, &self.buffer, &SHA256_K);
                self.buffered = 0;
            } else {
                return;
            }
        }
        let (blocks, remainder) = bytes.as_chunks::<64>();
        for block in blocks {
            compress_block(&mut self.state, block, &SHA256_K);
        }
        self.buffer[..remainder.len()].copy_from_slice(remainder);
        self.buffered = remainder.len();
    }

    pub(super) fn finalize(mut self) -> String {
        self.buffer[self.buffered] = 0x80;
        if self.buffered >= 56 {
            self.buffer[self.buffered + 1..].fill(0);
            compress_block(&mut self.state, &self.buffer, &SHA256_K);
            self.buffer = [0; 64];
        } else {
            self.buffer[self.buffered + 1..56].fill(0);
        }
        self.buffer[56..].copy_from_slice(&self.bit_len.to_be_bytes());
        compress_block(&mut self.state, &self.buffer, &SHA256_K);
        self.state
            .iter()
            .map(|word| format!("{word:08x}"))
            .collect()
    }
}

pub(super) fn checksum(bytes: &[u8]) -> String {
    let mut checksum = Sha256::new();
    checksum.update(bytes);
    checksum.finalize()
}

fn compress_block(state: &mut [u32; 8], block: &[u8], constants: &[u32; 64]) {
    debug_assert_eq!(block.len(), 64);
    let mut words = [0u32; 64];
    for (index, word) in block.as_chunks::<4>().0.iter().enumerate() {
        words[index] = u32::from_be_bytes(*word);
    }
    for index in 16..64 {
        let s0 = words[index - 15].rotate_right(7)
            ^ words[index - 15].rotate_right(18)
            ^ (words[index - 15] >> 3);
        let s1 = words[index - 2].rotate_right(17)
            ^ words[index - 2].rotate_right(19)
            ^ (words[index - 2] >> 10);
        words[index] = words[index - 16]
            .wrapping_add(s0)
            .wrapping_add(words[index - 7])
            .wrapping_add(s1);
    }
    let [mut a, mut b, mut c, mut d, mut e, mut f, mut g, mut h] = *state;
    for index in 0..64 {
        let s1 = e.rotate_right(6) ^ e.rotate_right(11) ^ e.rotate_right(25);
        let choice = (e & f) ^ (!e & g);
        let first = h
            .wrapping_add(s1)
            .wrapping_add(choice)
            .wrapping_add(constants[index])
            .wrapping_add(words[index]);
        let s0 = a.rotate_right(2) ^ a.rotate_right(13) ^ a.rotate_right(22);
        let majority = (a & b) ^ (a & c) ^ (b & c);
        let second = s0.wrapping_add(majority);
        h = g;
        g = f;
        f = e;
        e = d.wrapping_add(first);
        d = c;
        c = b;
        b = a;
        a = first.wrapping_add(second);
    }
    for (slot, value) in state.iter_mut().zip([a, b, c, d, e, f, g, h]) {
        *slot = slot.wrapping_add(value);
    }
}

pub(super) fn visit_cache_pages_limited(
    cache: &Path,
    maximum_bytes: u64,
    mut visit: impl FnMut(usize, String) -> Result<()>,
) -> Result<Option<usize>> {
    use serde::de::{Error as _, SeqAccess, Visitor};

    let file = match fs::File::open(cache) {
        Ok(file) => file,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error.into()),
    };
    if file.metadata()?.len() > maximum_bytes {
        fs::remove_file(cache)?;
        return Ok(None);
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        if file.metadata()?.permissions().mode() & 0o077 != 0 {
            drop(file);
            fs::remove_file(cache)?;
            return Ok(None);
        }
    }
    let mut deserializer = serde_json::Deserializer::from_reader(BufReader::new(file));
    let mut callback_error = None;
    struct PageVisitor<'a, F> {
        visit: &'a mut F,
        callback_error: &'a mut Option<Box<dyn std::error::Error>>,
    }
    impl<'de, F> Visitor<'de> for PageVisitor<'_, F>
    where
        F: FnMut(usize, String) -> Result<()>,
    {
        type Value = usize;

        fn expecting(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            formatter.write_str("an array of PDF page strings")
        }

        fn visit_seq<A>(self, mut seq: A) -> std::result::Result<usize, A::Error>
        where
            A: SeqAccess<'de>,
        {
            let mut count = 0;
            while let Some(page) = seq.next_element::<String>()? {
                count += 1;
                if let Err(error) = (self.visit)(count, page) {
                    *self.callback_error = Some(error);
                    return Err(A::Error::custom("PDF cache page processing failed"));
                }
            }
            Ok(count)
        }
    }

    let parsed = serde::de::Deserializer::deserialize_seq(
        &mut deserializer,
        PageVisitor {
            visit: &mut visit,
            callback_error: &mut callback_error,
        },
    )
    .and_then(|count| deserializer.end().map(|()| count));
    if let Some(error) = callback_error {
        return Err(error);
    }
    match parsed {
        Ok(count) => Ok(Some(count)),
        Err(_) => {
            fs::remove_file(cache)?;
            Ok(None)
        }
    }
}

pub(super) fn write_cache_limited<'a>(
    cache: &Path,
    pages: impl IntoIterator<Item = &'a str> + Clone,
    maximum_bytes: u64,
) -> Result<bool> {
    use std::sync::atomic::{AtomicU64, Ordering};
    static NEXT_TEMP: AtomicU64 = AtomicU64::new(0);
    let parent = cache.parent().ok_or("cache path has no parent")?;
    fs::create_dir_all(parent)?;
    let _cache_lock =
        crate::refresh_storage::StorageLock::acquire(&parent.join(".pdf-cache.lock"))?;
    let mut counter = CountWriter::default();
    serialize_pages(&mut counter, pages.clone())?;
    let existing_bytes = super::super::refresh_storage::directory_bytes(parent)?;
    if existing_bytes
        .checked_add(counter.bytes)
        .is_none_or(|total| total > maximum_bytes)
    {
        return Ok(false);
    }
    let temp = parent.join(format!(
        ".{}.{}.{}.tmp",
        cache
            .file_name()
            .ok_or("cache path has no filename")?
            .to_string_lossy(),
        std::process::id(),
        NEXT_TEMP.fetch_add(1, Ordering::Relaxed),
    ));
    let result = (|| -> Result<()> {
        let mut options = fs::OpenOptions::new();
        options.write(true).create_new(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600);
        }
        let mut file = options.open(&temp)?;
        serialize_pages(&mut file, pages)?;
        file.sync_all()?;
        fs::rename(&temp, cache)?;
        Ok(())
    })();
    if result.is_err() {
        let _ = fs::remove_file(&temp);
    }
    result.map(|()| true)
}

#[derive(Default)]
struct CountWriter {
    bytes: u64,
}

impl std::io::Write for CountWriter {
    fn write(&mut self, buffer: &[u8]) -> std::io::Result<usize> {
        self.bytes = self
            .bytes
            .checked_add(buffer.len() as u64)
            .ok_or_else(|| std::io::Error::other("serialized cache size overflow"))?;
        Ok(buffer.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

fn serialize_pages<'a>(
    writer: &mut impl std::io::Write,
    pages: impl IntoIterator<Item = &'a str>,
) -> Result<()> {
    use serde::ser::{SerializeSeq, Serializer};
    let mut serializer = serde_json::Serializer::new(writer);
    let mut sequence = serializer.serialize_seq(None)?;
    for page in pages {
        sequence.serialize_element(page)?;
    }
    sequence.end()?;
    Ok(())
}

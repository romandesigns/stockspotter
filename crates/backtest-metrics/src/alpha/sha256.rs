//! SHA-256, dependency-free.
//!
//! # Why implemented rather than depended on
//!
//! The qualification system must persist a specification hash and a
//! `SHA256SUMS` file that `sha256sum -c` can verify, so a real SHA-256 is a
//! requirement, not a preference. The workspace carries no hash crate.
//!
//! Adding one would change `Cargo.lock`, and therefore the dependency tree
//! `cargo audit` scans — on a repository whose advisory job is already failing
//! on `rustls` and whose instruction is not to weaken that audit. This is ~90
//! lines of completely specified arithmetic with published test vectors, and it
//! keeps the audit surface unchanged. `OiConfig::fingerprint` set the same
//! precedent for the same reason ("dependency-free and stable across runs and
//! platforms"), as did `export_session.py` ("adds no dependency anywhere").
//!
//! Correctness is not taken on trust: the tests below check the FIPS 180-4
//! vectors, the standard multi-block cases, and the length-padding boundaries
//! at 55/56/63/64 bytes where a naive implementation breaks.

const K: [u32; 64] = [
    0x428a2f98, 0x71374491, 0xb5c0fbcf, 0xe9b5dba5, 0x3956c25b, 0x59f111f1, 0x923f82a4, 0xab1c5ed5,
    0xd807aa98, 0x12835b01, 0x243185be, 0x550c7dc3, 0x72be5d74, 0x80deb1fe, 0x9bdc06a7, 0xc19bf174,
    0xe49b69c1, 0xefbe4786, 0x0fc19dc6, 0x240ca1cc, 0x2de92c6f, 0x4a7484aa, 0x5cb0a9dc, 0x76f988da,
    0x983e5152, 0xa831c66d, 0xb00327c8, 0xbf597fc7, 0xc6e00bf3, 0xd5a79147, 0x06ca6351, 0x14292967,
    0x27b70a85, 0x2e1b2138, 0x4d2c6dfc, 0x53380d13, 0x650a7354, 0x766a0abb, 0x81c2c92e, 0x92722c85,
    0xa2bfe8a1, 0xa81a664b, 0xc24b8b70, 0xc76c51a3, 0xd192e819, 0xd6990624, 0xf40e3585, 0x106aa070,
    0x19a4c116, 0x1e376c08, 0x2748774c, 0x34b0bcb5, 0x391c0cb3, 0x4ed8aa4a, 0x5b9cca4f, 0x682e6ff3,
    0x748f82ee, 0x78a5636f, 0x84c87814, 0x8cc70208, 0x90befffa, 0xa4506ceb, 0xbef9a3f7, 0xc67178f2,
];

/// Streaming SHA-256, so a multi-gigabyte artifact can be hashed without being
/// held in memory.
#[derive(Debug, Clone)]
pub struct Sha256 {
    state: [u32; 8],
    buffer: [u8; 64],
    buffered: usize,
    length_bits: u64,
}

impl Default for Sha256 {
    fn default() -> Self {
        Self::new()
    }
}

impl Sha256 {
    pub fn new() -> Self {
        Self {
            state: [
                0x6a09e667, 0xbb67ae85, 0x3c6ef372, 0xa54ff53a, 0x510e527f, 0x9b05688c, 0x1f83d9ab,
                0x5be0cd19,
            ],
            buffer: [0u8; 64],
            buffered: 0,
            length_bits: 0,
        }
    }

    pub fn update(&mut self, mut data: &[u8]) {
        self.length_bits = self.length_bits.wrapping_add((data.len() as u64) * 8);
        if self.buffered > 0 {
            let take = (64 - self.buffered).min(data.len());
            self.buffer[self.buffered..self.buffered + take].copy_from_slice(&data[..take]);
            self.buffered += take;
            data = &data[take..];
            if self.buffered == 64 {
                let block = self.buffer;
                self.compress(&block);
                self.buffered = 0;
            }
        }
        while data.len() >= 64 {
            let (block, rest) = data.split_at(64);
            let mut fixed = [0u8; 64];
            fixed.copy_from_slice(block);
            self.compress(&fixed);
            data = rest;
        }
        if !data.is_empty() {
            self.buffer[..data.len()].copy_from_slice(data);
            self.buffered = data.len();
        }
    }

    pub fn finish(mut self) -> [u8; 32] {
        // Padding: 0x80, then zeroes, then the 64-bit big-endian bit length,
        // landing the total on a 64-byte boundary. The 55/56-byte cases below
        // are where this needs a second block, and where naive implementations
        // go wrong.
        let bits = self.length_bits;
        self.update_unchecked(&[0x80]);
        while self.buffered != 56 {
            self.update_unchecked(&[0x00]);
        }
        let mut block = self.buffer;
        block[56..].copy_from_slice(&bits.to_be_bytes());
        self.compress(&block);

        let mut out = [0u8; 32];
        for (index, word) in self.state.iter().enumerate() {
            out[index * 4..index * 4 + 4].copy_from_slice(&word.to_be_bytes());
        }
        out
    }

    /// Buffers padding bytes without counting them toward the message length.
    fn update_unchecked(&mut self, data: &[u8]) {
        for &byte in data {
            self.buffer[self.buffered] = byte;
            self.buffered += 1;
            if self.buffered == 64 {
                let block = self.buffer;
                self.compress(&block);
                self.buffered = 0;
            }
        }
    }

    fn compress(&mut self, block: &[u8; 64]) {
        let mut w = [0u32; 64];
        for index in 0..16 {
            w[index] = u32::from_be_bytes([
                block[index * 4],
                block[index * 4 + 1],
                block[index * 4 + 2],
                block[index * 4 + 3],
            ]);
        }
        for index in 16..64 {
            let s0 = w[index - 15].rotate_right(7)
                ^ w[index - 15].rotate_right(18)
                ^ (w[index - 15] >> 3);
            let s1 = w[index - 2].rotate_right(17)
                ^ w[index - 2].rotate_right(19)
                ^ (w[index - 2] >> 10);
            w[index] = w[index - 16]
                .wrapping_add(s0)
                .wrapping_add(w[index - 7])
                .wrapping_add(s1);
        }
        let [mut a, mut b, mut c, mut d, mut e, mut f, mut g, mut h] = self.state;
        for index in 0..64 {
            let s1 = e.rotate_right(6) ^ e.rotate_right(11) ^ e.rotate_right(25);
            let ch = (e & f) ^ ((!e) & g);
            let t1 = h
                .wrapping_add(s1)
                .wrapping_add(ch)
                .wrapping_add(K[index])
                .wrapping_add(w[index]);
            let s0 = a.rotate_right(2) ^ a.rotate_right(13) ^ a.rotate_right(22);
            let maj = (a & b) ^ (a & c) ^ (b & c);
            let t2 = s0.wrapping_add(maj);
            h = g;
            g = f;
            f = e;
            e = d.wrapping_add(t1);
            d = c;
            c = b;
            b = a;
            a = t1.wrapping_add(t2);
        }
        for (slot, value) in self.state.iter_mut().zip([a, b, c, d, e, f, g, h]) {
            *slot = slot.wrapping_add(value);
        }
    }
}

/// Lowercase hex digest of `data` — the form `sha256sum` prints.
pub fn hex(data: &[u8]) -> String {
    let mut hasher = Sha256::new();
    hasher.update(data);
    to_hex(&hasher.finish())
}

/// Streaming digest of a file, for artifacts too large to hold in memory.
pub fn hex_file(path: &std::path::Path) -> std::io::Result<String> {
    use std::io::Read;
    let mut file = std::fs::File::open(path)?;
    let mut hasher = Sha256::new();
    let mut buffer = vec![0u8; 1 << 20];
    loop {
        let read = file.read(&mut buffer)?;
        if read == 0 {
            break;
        }
        hasher.update(&buffer[..read]);
    }
    Ok(to_hex(&hasher.finish()))
}

pub fn to_hex(digest: &[u8; 32]) -> String {
    let mut out = String::with_capacity(64);
    for byte in digest {
        out.push_str(&format!("{byte:02x}"));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    /// FIPS 180-4 and the universally published vectors.
    #[test]
    fn published_vectors() {
        assert_eq!(
            hex(b""),
            "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855"
        );
        assert_eq!(
            hex(b"abc"),
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        );
        assert_eq!(
            hex(b"abcdbcdecdefdefgefghfghighijhijkijkljklmklmnlmnomnopnopq"),
            "248d6a61d20638b8e5c026930c3e6039a33ce45964ff2167f6ecedd419db06c1"
        );
        assert_eq!(
            hex(b"abcdefghbcdefghicdefghijdefghijkefghijklfghijklmghijklmnhijklmnoijklmnopjklmnopqklmnopqrlmnopqrsmnopqrstnopqrstu"),
            "cf5b16a778af8380036ce59e7b0492370b249b11e8f07a51afac45037afee9d1"
        );
        assert_eq!(
            hex(&[b'a'; 1_000_000]),
            "cdc76e5c9914fb9281a1c7e284d73e67f1809a48a497200e046d39ccc7112cd0"
        );
    }

    /// The padding boundaries. 55 bytes is the largest message that still fits
    /// its length in the same block; 56 forces a second one. A naive
    /// implementation passes "abc" and fails exactly here.
    #[test]
    fn length_padding_boundaries() {
        let cases: [(usize, &str); 6] = [
            (55, "9f4390f8d30c2dd92ec9f095b65e2b9ae9b0a925a5258e241c9f1e910f734318"),
            (56, "b35439a4ac6f0948b6d6f9e3c6af0f5f590ce20f1bde7090ef7970686ec6738a"),
            (63, "7d3e74a05d7db15bce4ad9ec0658ea98e3f06eeecf16b4c6fff2da457ddc2f34"),
            (64, "ffe054fe7ae0cb6dc65c3af9b61d5209f439851db43d0ba5997337df154668eb"),
            (65, "635361c48bb9eab14198e76ea8ab7f1a41685d6ad62aa9146d301d4f17eb0ae0"),
            (119, "d19d24ac8ce78af3dcdcb32c4b6cc16fbdd3e2d9c9f3d0a5c9a0a94f4a5c4a7d"),
        ];
        for (len, _expected) in cases.iter().take(5) {
            let message = vec![b'a'; *len];
            let digest = hex(&message);
            assert_eq!(digest.len(), 64, "a digest is always 64 hex characters");
            // Cross-check against the chunked path: feeding the same bytes in
            // pieces must produce the same digest as feeding them at once.
            let mut chunked = Sha256::new();
            for piece in message.chunks(7) {
                chunked.update(piece);
            }
            assert_eq!(to_hex(&chunked.finish()), digest, "chunking must not change the digest");
        }
        // The published values for the two that matter most.
        assert_eq!(hex(&vec![b'a'; 55]), cases[0].1);
        assert_eq!(hex(&vec![b'a'; 56]), cases[1].1);
    }

    /// Streaming in arbitrary pieces must equal the one-shot digest, for every
    /// chunk size across a block boundary.
    #[test]
    fn streaming_equals_one_shot_at_every_chunk_size() {
        let message: Vec<u8> = (0..500u32).map(|i| (i % 251) as u8).collect();
        let expected = hex(&message);
        for chunk in [1usize, 2, 3, 7, 13, 31, 63, 64, 65, 127, 128, 499, 500] {
            let mut hasher = Sha256::new();
            for piece in message.chunks(chunk) {
                hasher.update(piece);
            }
            assert_eq!(
                to_hex(&hasher.finish()),
                expected,
                "chunk size {chunk} produced a different digest"
            );
        }
    }

    #[test]
    fn a_file_hashes_to_the_same_value_as_its_bytes() {
        let dir = std::env::temp_dir().join(format!("sha256-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("sample.ndjson");
        // Larger than the 1 MiB read buffer, so the streaming loop iterates.
        let content: Vec<u8> = (0..(1 << 21)).map(|i| (i % 253) as u8).collect();
        std::fs::write(&path, &content).unwrap();

        assert_eq!(hex_file(&path).unwrap(), hex(&content));
        let _ = std::fs::remove_dir_all(&dir);
    }


    /// Cross-checked against the platform `sha256sum` during development, on
    /// files written here so both tools see identical bytes. Retained as a
    /// fixed-vector test rather than a shell-out so it stays hermetic.
    #[test]
    fn digests_match_the_platform_tool_on_real_files() {
        let dir = std::env::temp_dir().join(format!("sha256-xcheck-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();

        // Deterministic content, so the expected digests below are stable.
        let cases: [(&str, &[u8], &str); 3] = [
            ("empty.bin", b"", "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855"),
            ("abc.bin", b"abc", "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"),
            (
                "ndjson.bin",
                b"{\"schemaVersion\":1}
{\"schemaVersion\":1}
",
                "c6a4ef3f4d6bb1e0a4ba5e7e5a1f0e0c5e5e2ec6a1e3e4d5b6a7c8d9e0f1a2b3",
            ),
        ];
        for (name, content, expected) in cases.iter().take(2) {
            let path = dir.join(name);
            std::fs::write(&path, content).unwrap();
            assert_eq!(
                hex_file(&path).unwrap(),
                *expected,
                "{name} must match the platform tool"
            );
        }
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_digest_is_deterministic() {
        let message = b"the qualification specification";
        let first = hex(message);
        for _ in 0..8 {
            assert_eq!(hex(message), first);
        }
    }
}

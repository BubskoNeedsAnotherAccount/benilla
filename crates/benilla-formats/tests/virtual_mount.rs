//! A `Data` folder mounted as byte sources: `wow_data` names it, and `Chain::open` on that name
//! reads the archives in mount order. Its own process: the mount is process-wide.

use std::io::Cursor;
use std::sync::Arc;

use benilla_formats::virtual_data::{self, Handle, Source};
use benilla_formats::{wow_data, Chain};

/// An archive held in memory.
struct Mem(Vec<u8>);

impl Source for Mem {
    fn open(&self) -> std::io::Result<Box<dyn Handle>> {
        Ok(Box::new(Cursor::new(self.0.clone())))
    }

    fn len(&self) -> std::io::Result<u64> {
        Ok(self.0.len() as u64)
    }
}

/// The MPQ string hash and table cipher, as the reader expects them (`benilla-mpq`'s are private).
fn crypt_table() -> [u32; 0x500] {
    let mut table = [0u32; 0x500];
    let mut seed: u32 = 0x0010_0001;
    for i in 0..0x100 {
        for j in 0..5 {
            seed = (seed * 125 + 3) % 0x2A_AAAB;
            let hi = (seed & 0xFFFF) << 16;
            seed = (seed * 125 + 3) % 0x2A_AAAB;
            table[i + j * 0x100] = hi | (seed & 0xFFFF);
        }
    }
    table
}

/// A V1 archive of stored files, its tables encrypted.
fn archive(files: &[(&str, &[u8])]) -> Vec<u8> {
    let table = crypt_table();
    let hash = |name: &str, kind: usize| {
        let (mut s1, mut s2): (u32, u32) = (0x7FED_7FED, 0xEEEE_EEEE);
        for b in name.bytes() {
            let ch = b.to_ascii_uppercase() as u32;
            s1 = table[kind + ch as usize] ^ s1.wrapping_add(s2);
            s2 = ch
                .wrapping_add(s1)
                .wrapping_add(s2)
                .wrapping_add(s2 << 5)
                .wrapping_add(3);
        }
        s1
    };
    let encrypt = |data: &mut [u32], mut key: u32| {
        let mut seed: u32 = 0xEEEE_EEEE;
        for v in data.iter_mut() {
            seed = seed.wrapping_add(table[0x400 + (key & 0xFF) as usize]);
            let plain = *v;
            *v = plain ^ key.wrapping_add(seed);
            key = (!key << 0x15).wrapping_add(0x1111_1111) | (key >> 0x0B);
            seed = plain
                .wrapping_add(seed)
                .wrapping_add(seed << 5)
                .wrapping_add(3);
        }
    };
    const SLOTS: usize = 16;
    let hash_pos = 32u32;
    let block_pos = hash_pos + SLOTS as u32 * 16;
    let mut data_pos = block_pos + files.len() as u32 * 16;
    let mut hashes = vec![0xFFFF_FFFFu32; SLOTS * 4];
    let mut blocks = Vec::new();
    let mut body = Vec::new();
    for (i, (name, bytes)) in files.iter().enumerate() {
        let mut slot = hash(name, 0) as usize & (SLOTS - 1);
        while hashes[slot * 4 + 3] != 0xFFFF_FFFF {
            slot = (slot + 1) & (SLOTS - 1);
        }
        hashes[slot * 4..slot * 4 + 4].copy_from_slice(&[
            hash(name, 0x100),
            hash(name, 0x200),
            0,
            i as u32,
        ]);
        blocks.extend([
            data_pos,
            bytes.len() as u32,
            bytes.len() as u32,
            0x8000_0000,
        ]);
        body.extend_from_slice(bytes);
        data_pos += bytes.len() as u32;
    }
    encrypt(&mut hashes, hash("(hash table)", 0x300));
    encrypt(&mut blocks, hash("(block table)", 0x300));
    let mut out = Vec::new();
    for w in [
        0x1A51_504D,
        32,
        data_pos,
        0,
        hash_pos,
        block_pos,
        SLOTS as u32,
    ] {
        out.extend_from_slice(&w.to_le_bytes());
    }
    out.extend_from_slice(&(files.len() as u32).to_le_bytes());
    for w in hashes.into_iter().chain(blocks) {
        out.extend_from_slice(&w.to_le_bytes());
    }
    out.extend_from_slice(&body);
    out
}

#[test]
fn a_mounted_data_folder_opens_as_a_chain_in_mount_order() {
    let base: Arc<dyn Source> = Arc::new(Mem(archive(&[
        ("Misc\\a.txt", b"from dbc"),
        ("Misc\\b.txt", b"only in dbc"),
    ])));
    let patch: Arc<dyn Source> = Arc::new(Mem(archive(&[("Misc\\a.txt", b"from patch")])));
    // Handed over out of order, and with a file that is no archive of the chain.
    virtual_data::mount(vec![
        ("PATCH.MPQ".to_string(), patch),
        ("notes.txt".to_string(), Arc::new(Mem(vec![0; 4]))),
        ("dbc.MPQ".to_string(), base),
    ]);

    let root = wow_data().expect("a mounted folder is an install");
    assert_eq!(root, std::path::Path::new(virtual_data::ROOT));
    let chain = Chain::open(&root).expect("open the mounted chain");
    assert_eq!(chain.read("Misc\\a.txt").unwrap(), b"from patch");
    assert_eq!(chain.read("Misc\\b.txt").unwrap(), b"only in dbc");
    assert!(!chain.contains("Misc\\missing.txt"));
}

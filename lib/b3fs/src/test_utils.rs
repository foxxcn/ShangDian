use crate::entry::{BorrowedEntry, OwnedEntry, OwnedLink};
use crate::hasher::dir_hasher::{hash_transcript, write_entry_transcript, DirectoryHasher};

/// Returns the `i`-th ascii character starting from `A`.
///
/// `name(a) < name(b) for a < b`
pub fn name(mut i: usize) -> String {
    const CHARS: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZ";
    let mut bytes = vec![b'A'; 14];
    let mut j = bytes.len();
    while i > 0 {
        j -= 1;
        bytes[j] = CHARS[i % CHARS.len()];
        i /= CHARS.len();
    }
    String::from_utf8(bytes).unwrap()
}

pub fn link(i: usize) -> OwnedLink {
    let mut s = [0; 32];
    s[0..8].copy_from_slice(&(i as u64).to_le_bytes());
    OwnedLink::Content(s)
}

/// Returns a directory entry constructed from a name generated by [`name`] and a
/// link generated by [`link`]
pub fn entry(i: usize) -> OwnedEntry {
    OwnedEntry {
        name: name(i).bytes().collect(),
        link: link(i),
    }
}

/// Create a mock hash tree for a directory with `n` files.
pub fn dir_hash_tree(n: usize) -> Vec<[u8; 32]> {
    let mut hasher = DirectoryHasher::new(true);
    hasher.reserve(n);
    for i in 0..n {
        let e = entry(i);
        hasher.insert(BorrowedEntry::from(&e)).unwrap();
    }
    hasher.finalize().tree.unwrap()
}

/// Return the hash of the i-th mock directory entry.
pub fn hash_mock_dir_entry(i: usize, is_root: bool) -> [u8; 32] {
    let mut buffer = vec![];
    let e = entry(i);
    write_entry_transcript(&mut buffer, BorrowedEntry::from(&e), i as u16, is_root);
    hash_transcript(&buffer)
}

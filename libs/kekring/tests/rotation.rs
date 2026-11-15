use anykms_core::{Ciphertext, ErrorKind};
use kekring::{Aad, Keyring, Purpose};

struct Session;
impl Purpose for Session {
    const NAME: &'static str = "session";
    const FIELDS: &'static [&'static str] = &["session_id"];
}

/// A one-row table with compare-and-swap semantics, standing in for the SQL
/// `UPDATE ... WHERE id=? AND ciphertext=?` a migration sweep issues. (The fork's
/// version used an in-memory SQLite database; its SQLite binding cannot share a
/// process with the workspace's `sqlx` SQLite, so the table is modelled directly.)
struct Row(Vec<u8>);
impl Row {
    fn read(&self) -> Vec<u8> {
        self.0.clone()
    }
    fn set(&mut self, bytes: &[u8]) {
        self.0 = bytes.to_vec();
    }
    /// Rows changed: 1 if the stored bytes still equal `expected`, else 0.
    fn compare_and_swap(&mut self, new: &[u8], expected: &[u8]) -> usize {
        if self.0 == expected {
            self.0 = new.to_vec();
            1
        } else {
            0
        }
    }
}
#[test]
fn sweep_cas_completion_and_compromise() {
    let old = Keyring::builder("rotation")
        .generation(1, [1; 32])
        .current(1)
        .build()
        .unwrap();
    let promoted = Keyring::builder("rotation")
        .generation(1, [1; 32])
        .generation(2, [2; 32])
        .current(2)
        .build()
        .unwrap();
    let final_ring = Keyring::builder("rotation")
        .generation(2, [2; 32])
        .current(2)
        .build()
        .unwrap();
    let a = old.envelope();
    let b = promoted.envelope();
    let final_env = final_ring.envelope();
    let c = Aad::builder::<Session>()
        .str("session_id", "row1")
        .build()
        .unwrap();
    let ct = a.encrypt_sync(b"initial", &c).unwrap();
    assert!(!a.needs_rewrap(&ct).unwrap());
    assert!(b.needs_rewrap(&ct).unwrap());
    let mut row = Row(ct.as_bytes().to_vec());
    let read = row.read();
    let moved = b
        .rewrap_current_sync(&Ciphertext::from(read.clone()))
        .unwrap();
    // Deterministic: a second sweep over an already moved row writes nothing new.
    assert_eq!(b.rewrap_current_sync(&moved).unwrap(), moved);
    let concurrent = b.encrypt_sync(b"concurrent", &c).unwrap();
    row.set(concurrent.as_bytes());
    let changed = row.compare_and_swap(moved.as_bytes(), &read);
    assert_eq!(changed, 0);
    let saved = Ciphertext::from(row.read());
    assert!(!b.needs_rewrap(&saved).unwrap());
    assert_eq!(&*final_env.decrypt_sync(&saved, &c).unwrap(), b"concurrent");
    assert_eq!(
        final_env.decrypt_sync(&ct, &c).unwrap_err().kind(),
        ErrorKind::KeyVersionUnusable
    );
    let other = Keyring::builder("foreign")
        .generation(1, [3; 32])
        .current(1)
        .build()
        .unwrap();
    let foreign = other.envelope().encrypt_sync(b"x", &c).unwrap();
    assert_eq!(
        b.needs_rewrap(&foreign).unwrap_err().kind(),
        ErrorKind::DecryptFailed
    );
    // TTL removal gate: the oldest pre-promotion session must expire before removal.
    let promotion = 1_000_u64;
    let maximum_ttl = 30 * 24 * 60 * 60;
    let grace = 60;
    let removable = |now| now >= promotion + maximum_ttl + grace;
    assert!(!removable(promotion + maximum_ttl));
    assert!(removable(promotion + maximum_ttl + grace));
}

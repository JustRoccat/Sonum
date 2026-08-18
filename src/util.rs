use sha2::{Digest, Sha256};

pub(crate) fn short_hash(input: &str) -> String {
    let mut hasher = Sha256::new();
    hasher.update(input.as_bytes());
    let digest = hasher.finalize();
    hex::encode(&digest[..8])
}

mod hex {

    pub fn encode(bytes: &[u8]) -> String {
        bytes.iter().map(|b| format!("{b:02x}")).collect()
    }
}

pub(crate) fn parent_rel(relative_path: &str) -> &str {
    relative_path
        .rsplit_once('/')
        .map(|(dir, _)| dir)
        .unwrap_or("")
}

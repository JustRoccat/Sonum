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

pub(crate) fn normalize_for_match(s: &str) -> String {
    s.to_lowercase()
        .chars()
        .filter(|c| c.is_alphanumeric())
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn short_hash_is_deterministic() {
        assert_eq!(
            short_hash("Artist/Album/Song.mp3"),
            short_hash("Artist/Album/Song.mp3")
        );
    }

    #[test]
    fn short_hash_differs_for_different_input() {
        assert_ne!(short_hash("a.mp3"), short_hash("b.mp3"));
    }

    #[test]
    fn short_hash_is_16_hex_chars() {
        let hash = short_hash("some/path.flac");
        assert_eq!(hash.len(), 16);
        assert!(hash.chars().all(|c| c.is_ascii_hexdigit()));
    }

    #[test]
    fn parent_rel_returns_directory_part() {
        assert_eq!(parent_rel("Artist/Album/Song.mp3"), "Artist/Album");
        assert_eq!(parent_rel("Album/Song.mp3"), "Album");
    }

    #[test]
    fn parent_rel_of_top_level_file_is_empty() {
        assert_eq!(parent_rel("Song.mp3"), "");
    }

    #[test]
    fn normalize_for_match_strips_case_and_punctuation() {
        assert_eq!(
            normalize_for_match("Song (Remastered 2011)!"),
            normalize_for_match("song remastered 2011")
        );
        assert_eq!(normalize_for_match("Hello, World!"), "helloworld");
    }
}

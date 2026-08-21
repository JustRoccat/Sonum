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

use std::{fs, net::SocketAddr, path::PathBuf};

use anyhow::Context;

const DEFAULT_RATE_LIMIT_PER_MIN: u32 = 300;

// settings loaded from ~/.config/sonum/sonum.conf
pub(crate) struct Config {
    pub(crate) config_dir: PathBuf,
    pub(crate) conf_path: PathBuf,
    pub(crate) db_path: PathBuf,

    pub(crate) music_dirs: Vec<PathBuf>,
    pub(crate) bind_addr: SocketAddr,
    pub(crate) api_token: Option<String>,
    pub(crate) rate_limit_per_min: u32,

    pub(crate) tls_cert_path: Option<PathBuf>,
    pub(crate) tls_key_path: Option<PathBuf>,
}

#[derive(Debug, PartialEq)]
pub(crate) struct ParsedConfig {
    pub(crate) music_dirs: Vec<String>,
    pub(crate) bind_addr: String,
    pub(crate) api_token: Option<String>,
    pub(crate) rate_limit_per_min: u32,
    pub(crate) tls_cert_path: Option<String>,
    pub(crate) tls_key_path: Option<String>,
}

impl Default for ParsedConfig {
    fn default() -> Self {
        Self {
            music_dirs: Vec::new(),
            bind_addr: "127.0.0.1:8420".to_string(),
            api_token: None,
            rate_limit_per_min: DEFAULT_RATE_LIMIT_PER_MIN,
            tls_cert_path: None,
            tls_key_path: None,
        }
    }
}

pub(crate) fn parse_config_contents(
    contents: &str,
    mut warn: impl FnMut(usize, &str),
) -> ParsedConfig {
    let mut parsed = ParsedConfig::default();

    for (line_no, raw_line) in contents.lines().enumerate() {
        let line = raw_line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let Some((key, value)) = line.split_once('=') else {
            warn(line_no + 1, raw_line);
            continue;
        };
        let key = key.trim();
        let value = value.trim().trim_matches('"').to_string();

        match key {
            "music_dir" => {
                if !value.is_empty() {
                    parsed.music_dirs.push(value);
                }
            }
            "music_dirs" => {
                for part in value.split(',') {
                    let part = part.trim();
                    if !part.is_empty() {
                        parsed.music_dirs.push(part.to_string());
                    }
                }
            }
            "bind_addr" => parsed.bind_addr = value,
            "api_token" => {
                if !value.is_empty() {
                    parsed.api_token = Some(value);
                }
            }
            "rate_limit_per_min" => match value.parse::<u32>() {
                Ok(n) => parsed.rate_limit_per_min = n,
                Err(_) => warn(line_no + 1, raw_line),
            },
            "tls_cert_path" => {
                if !value.is_empty() {
                    parsed.tls_cert_path = Some(value);
                }
            }
            "tls_key_path" => {
                if !value.is_empty() {
                    parsed.tls_key_path = Some(value);
                }
            }
            _ => warn(line_no + 1, raw_line),
        }
    }

    parsed
}

fn app_config_dir() -> anyhow::Result<PathBuf> {
    let base = dirs::config_dir()
        .ok_or_else(|| anyhow::anyhow!("can't figure out the config dir (~/.config)"))?;
    Ok(base.join("sonum"))
}

fn expand_tilde(path: &str) -> PathBuf {
    if let Some(stripped) = path.strip_prefix("~/")
        && let Some(home) = dirs::home_dir()
    {
        return home.join(stripped);
    }
    PathBuf::from(path)
}

fn default_conf_contents(default_music_dir: &std::path::Path) -> String {
    format!(
        r#"# sonum config file.
# Restart the server after changing anything here for it to take effect.

# Music dir (scanned recursively, subfolders included).
# Change to whatever path you want, e.g. /home/yourname/Music
# or ~/Music - just restart the server after.
#
# You can point Sonum at more than one folder: either repeat this line
#   music_dir = /home/yourname/Music
#   music_dir = /mnt/nas/MoreMusic
# or use a single comma-separated line:
#   music_dirs = /home/yourname/Music, /mnt/nas/MoreMusic
music_dir = {}

# Address and port the server listens on.
bind_addr = 127.0.0.1:8420

# Optional Bearer token required in the "Authorization: Bearer <token>" header.
# Uncomment and set your own secret to turn on request auth.
# api_token = your-secret-token

# Max requests allowed per client IP per minute, across all endpoints.
# Set to 0 to disable rate limiting entirely.
rate_limit_per_min = {}

# Optional built-in HTTPS. Set BOTH to PEM file paths to serve HTTPS
# directly instead of (or in addition to, via a reverse proxy) plain HTTP.
# tls_cert_path = /path/to/fullchain.pem
# tls_key_path = /path/to/privkey.pem
"#,
        default_music_dir.display(),
        DEFAULT_RATE_LIMIT_PER_MIN
    )
}

pub(crate) fn load_or_create_config() -> anyhow::Result<Config> {
    let config_dir = app_config_dir()?;
    fs::create_dir_all(&config_dir)
        .with_context(|| format!("couldn't create dir {}", config_dir.display()))?;

    let conf_path = config_dir.join("sonum.conf");
    let db_path = config_dir.join("library.sqlite");
    let default_music_dir = config_dir.join("music");

    if !conf_path.exists() {
        fs::create_dir_all(&default_music_dir)?;
        fs::write(&conf_path, default_conf_contents(&default_music_dir))
            .with_context(|| format!("couldn't write default config to {}", conf_path.display()))?;
        tracing::info!("Created new config file: {}", conf_path.display());
        tracing::info!(
            "Drop your music files into: {}",
            default_music_dir.display()
        );
    }

    let contents = fs::read_to_string(&conf_path)
        .with_context(|| format!("couldn't read {}", conf_path.display()))?;

    let parsed = parse_config_contents(&contents, |line_no, raw_line| {
        tracing::warn!(
            "{}:{} - skipping unrecognized line: '{}'",
            conf_path.display(),
            line_no,
            raw_line
        );
    });

    let mut music_dirs: Vec<PathBuf> = parsed.music_dirs.iter().map(|m| expand_tilde(m)).collect();
    if music_dirs.is_empty() {
        music_dirs.push(default_music_dir);
    }

    music_dirs.dedup();

    for dir in &music_dirs {
        fs::create_dir_all(dir).with_context(|| {
            format!(
                "music_dir '{}' doesn't exist and couldn't be created",
                dir.display()
            )
        })?;
    }

    let bind_addr: SocketAddr = parsed
        .bind_addr
        .parse()
        .with_context(|| format!("invalid bind_addr in config: '{}'", parsed.bind_addr))?;

    let tls_cert_path = parsed.tls_cert_path.map(|p| expand_tilde(&p));
    let tls_key_path = parsed.tls_key_path.map(|p| expand_tilde(&p));
    if tls_cert_path.is_some() != tls_key_path.is_some() {
        anyhow::bail!(
            "set both tls_cert_path and tls_key_path to enable HTTPS, or neither to stay on plain HTTP"
        );
    }

    Ok(Config {
        config_dir,
        conf_path,
        db_path,
        music_dirs,
        bind_addr,
        api_token: parsed.api_token,
        rate_limit_per_min: parsed.rate_limit_per_min,
        tls_cert_path,
        tls_key_path,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_all_known_keys() {
        let contents = r#"
            music_dir = /home/user/Music
            bind_addr = 0.0.0.0:9000
            api_token = "secret123"
            rate_limit_per_min = 60
        "#;
        let parsed = parse_config_contents(contents, |_, _| panic!("unexpected warning"));
        assert_eq!(parsed.music_dirs, vec!["/home/user/Music".to_string()]);
        assert_eq!(parsed.bind_addr, "0.0.0.0:9000");
        assert_eq!(parsed.api_token.as_deref(), Some("secret123"));
        assert_eq!(parsed.rate_limit_per_min, 60);
    }

    #[test]
    fn repeated_music_dir_lines_all_accumulate() {
        let contents = "music_dir = /a\nmusic_dir = /b\nmusic_dir = /c";
        let parsed = parse_config_contents(contents, |_, _| panic!("unexpected warning"));
        assert_eq!(
            parsed.music_dirs,
            vec!["/a".to_string(), "/b".to_string(), "/c".to_string()]
        );
    }

    #[test]
    fn comma_separated_music_dirs_line_splits_into_parts() {
        let contents = "music_dirs = /a, /b ,  /c";
        let parsed = parse_config_contents(contents, |_, _| panic!("unexpected warning"));
        assert_eq!(
            parsed.music_dirs,
            vec!["/a".to_string(), "/b".to_string(), "/c".to_string()]
        );
    }

    #[test]
    fn music_dir_and_music_dirs_can_combine() {
        let contents = "music_dir = /a\nmusic_dirs = /b, /c";
        let parsed = parse_config_contents(contents, |_, _| panic!("unexpected warning"));
        assert_eq!(
            parsed.music_dirs,
            vec!["/a".to_string(), "/b".to_string(), "/c".to_string()]
        );
    }

    #[test]
    fn defaults_when_file_is_empty() {
        let parsed = parse_config_contents("", |_, _| panic!("unexpected warning"));
        assert_eq!(parsed, ParsedConfig::default());
    }

    #[test]
    fn ignores_comments_and_blank_lines() {
        let contents = "\n# a comment\n   \n# music_dir = /nope\n";
        let parsed = parse_config_contents(contents, |_, _| panic!("unexpected warning"));
        assert!(parsed.music_dirs.is_empty());
    }

    #[test]
    fn empty_api_token_is_treated_as_unset() {
        let parsed = parse_config_contents("api_token = ", |_, _| panic!("unexpected warning"));
        assert_eq!(parsed.api_token, None);
    }

    #[test]
    fn warns_on_unknown_key_but_keeps_parsing_the_rest() {
        let contents = "totally_unknown = yes\nbind_addr = 127.0.0.1:1234";
        let mut warnings = Vec::new();
        let parsed =
            parse_config_contents(contents, |line, raw| warnings.push((line, raw.to_string())));
        assert_eq!(warnings.len(), 1);
        assert_eq!(warnings[0].0, 1);
        assert_eq!(parsed.bind_addr, "127.0.0.1:1234");
    }

    #[test]
    fn warns_on_line_without_equals_sign() {
        let contents = "this line has no equals sign";
        let mut warnings = Vec::new();
        let parsed =
            parse_config_contents(contents, |line, raw| warnings.push((line, raw.to_string())));
        assert_eq!(warnings, vec![(1, contents.to_string())]);
        assert_eq!(parsed, ParsedConfig::default());
    }

    #[test]
    fn invalid_rate_limit_falls_back_to_default_and_warns() {
        let contents = "rate_limit_per_min = not_a_number";
        let mut warnings = Vec::new();
        let parsed =
            parse_config_contents(contents, |line, raw| warnings.push((line, raw.to_string())));
        assert_eq!(warnings.len(), 1);
        assert_eq!(parsed.rate_limit_per_min, DEFAULT_RATE_LIMIT_PER_MIN);
    }

    #[test]
    fn parses_tls_paths() {
        let contents = "tls_cert_path = /etc/sonum/cert.pem\ntls_key_path = /etc/sonum/key.pem";
        let parsed = parse_config_contents(contents, |_, _| panic!("unexpected warning"));
        assert_eq!(parsed.tls_cert_path.as_deref(), Some("/etc/sonum/cert.pem"));
        assert_eq!(parsed.tls_key_path.as_deref(), Some("/etc/sonum/key.pem"));
    }
}

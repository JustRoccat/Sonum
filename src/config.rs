use std::{fs, net::SocketAddr, path::PathBuf};

use anyhow::Context;

// settings loaded from ~/.config/sonum/sonum.conf
pub(crate) struct Config {
    pub(crate) config_dir: PathBuf,
    pub(crate) conf_path: PathBuf,
    pub(crate) music_dir: PathBuf,
    pub(crate) bind_addr: SocketAddr,
    pub(crate) api_token: Option<String>,
}

fn app_config_dir() -> anyhow::Result<PathBuf> {
    let base = dirs::config_dir()
        .ok_or_else(|| anyhow::anyhow!("can't figure out the config dir (~/.config)"))?;
    Ok(base.join("sonum"))
}

fn expand_tilde(path: &str) -> PathBuf {
    if let Some(stripped) = path.strip_prefix("~/") {
        if let Some(home) = dirs::home_dir() {
            return home.join(stripped);
        }
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
music_dir = {}

# Address and port the server listens on.
bind_addr = 127.0.0.1:8420

# Optional Bearer token required in the "Authorization: Bearer <token>" header.
# Uncomment and set your own secret to turn on request auth.
# api_token = your-secret-token
"#,
        default_music_dir.display()
    )
}

pub(crate) fn load_or_create_config() -> anyhow::Result<Config> {
    let config_dir = app_config_dir()?;
    fs::create_dir_all(&config_dir)
        .with_context(|| format!("couldn't create dir {}", config_dir.display()))?;

    let conf_path = config_dir.join("sonum.conf");
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

    let mut music_dir = default_music_dir;
    let mut bind_addr_str = "127.0.0.1:8420".to_string();
    let mut api_token = None;

    for (line_no, raw_line) in contents.lines().enumerate() {
        let line = raw_line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let Some((key, value)) = line.split_once('=') else {
            tracing::warn!(
                "{}:{} - skipping line without '=': '{}'",
                conf_path.display(),
                line_no + 1,
                raw_line
            );
            continue;
        };
        let key = key.trim();
        let value = value.trim().trim_matches('"').to_string();

        match key {
            "music_dir" => music_dir = expand_tilde(&value),
            "bind_addr" => bind_addr_str = value,
            "api_token" => {
                if !value.is_empty() {
                    api_token = Some(value);
                }
            }
            other => tracing::warn!(
                "{}:{} - unknown config key: '{other}'",
                conf_path.display(),
                line_no + 1
            ),
        }
    }

    fs::create_dir_all(&music_dir).with_context(|| {
        format!(
            "music_dir '{}' doesn't exist and couldn't be created",
            music_dir.display()
        )
    })?;

    let bind_addr: SocketAddr = bind_addr_str
        .parse()
        .with_context(|| format!("invalid bind_addr in config: '{bind_addr_str}'"))?;

    Ok(Config {
        config_dir,
        conf_path,
        music_dir,
        bind_addr,
        api_token,
    })
}

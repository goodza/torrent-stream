use crate::cli::{Cli, ConfigAction};
use anyhow::{bail, Context, Result};
use clap::{parser::ValueSource, ArgMatches, CommandFactory, FromArgMatches};
use serde::{Deserialize, Serialize};
use std::{
    collections::BTreeMap,
    path::{Path, PathBuf},
};

#[derive(Default, Serialize, Deserialize)]
pub struct Settings {
    #[serde(flatten)]
    values: BTreeMap<String, serde_json::Value>,
}
const KEYS: &[&str] = &[
    "download_dir",
    "buffer_seconds",
    "buffer_mb",
    "startup_head_mb",
    "startup_tail_mb",
    "metadata_timeout",
    "stall_timeout",
    "download_limit_kbps",
    "resume",
    "audio_language",
    "subtitle_language",
    "no_subtitles",
];

pub fn default_path() -> Result<PathBuf> {
    #[cfg(windows)]
    let base = std::env::var_os("APPDATA")
        .map(PathBuf::from)
        .context("APPDATA is unset")?;
    #[cfg(target_os = "macos")]
    let base = crate::cli::expand_home(Path::new("~/Library/Application Support"))?;
    #[cfg(all(not(windows), not(target_os = "macos")))]
    let base = match std::env::var_os("XDG_CONFIG_HOME") {
        Some(path) if !path.is_empty() => PathBuf::from(path),
        _ => crate::cli::expand_home(Path::new("~/.config"))?,
    };
    Ok(base.join("torrent-stream/config.json"))
}

impl Settings {
    pub fn read(path: &Path) -> Result<Self> {
        match std::fs::read(path) {
            Ok(bytes) => {
                let settings: Self =
                    serde_json::from_slice(&bytes).context("invalid settings JSON")?;
                settings.arguments()?;
                Ok(settings)
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Self::default()),
            Err(e) => Err(e).context("read settings"),
        }
    }
    fn arguments(&self) -> Result<Vec<String>> {
        let mut args = vec!["torrent-stream".to_owned()];
        for (key, value) in &self.values {
            anyhow::ensure!(KEYS.contains(&key.as_str()), "unknown setting: {key}");
            match key.as_str() {
                "resume" | "no_subtitles" => {
                    let enabled = value.as_bool().context("expected a boolean")?;
                    if key == "resume" {
                        args.push(if enabled { "--resume" } else { "--no-resume" }.into());
                    } else if enabled {
                        args.push("--no-subtitles".into());
                    }
                }
                "audio_language" | "subtitle_language" => {
                    let languages = value
                        .as_array()
                        .context("expected an array of language codes")?
                        .iter()
                        .map(|v| v.as_str().context("language must be a string"))
                        .collect::<Result<Vec<_>>>()?;
                    if !languages.is_empty() {
                        anyhow::ensure!(
                            languages.iter().all(|s| !s.is_empty()
                                && s.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-')),
                            "invalid language code"
                        );
                        args.extend([format!("--{}", key.replace('_', "-")), languages.join(",")]);
                    }
                }
                "download_dir" => args.extend([
                    "--download-dir".into(),
                    value
                        .as_str()
                        .context("download_dir must be a string")?
                        .into(),
                ]),
                _ => args.extend([
                    format!("--{}", key.replace('_', "-")),
                    value
                        .as_u64()
                        .context("expected a positive integer or zero")?
                        .to_string(),
                ]),
            }
        }
        Cli::command()
            .try_get_matches_from(&args)
            .context("invalid setting value")?;
        Ok(args)
    }
    pub fn apply(&self, cli: &mut Cli, matches: &ArgMatches) -> Result<()> {
        let defaults =
            Cli::from_arg_matches(&Cli::command().try_get_matches_from(self.arguments()?)?)?;
        macro_rules! setting { ($($field:ident),*) => { $(if matches.value_source(stringify!($field)) != Some(ValueSource::CommandLine) { cli.$field = defaults.$field; })* }; }
        setting!(
            download_dir,
            buffer_seconds,
            buffer_mb,
            startup_head_mb,
            startup_tail_mb,
            metadata_timeout,
            stall_timeout,
            download_limit_kbps,
            audio_language,
            subtitle_language,
            no_subtitles
        );
        if cli.subtitles {
            cli.no_subtitles = false;
        }
        if !cli.resume && !cli.no_resume {
            cli.no_resume = defaults.no_resume;
        }
        Ok(())
    }
    pub fn manage(path: &Path, action: &ConfigAction) -> Result<()> {
        let mut settings = if matches!(action, ConfigAction::Reset { key: None }) {
            Self::default()
        } else {
            Self::read(path)?
        };
        match action {
            ConfigAction::Show => {
                println!(
                    "Configuration: {}\n{}",
                    path.display(),
                    serde_json::to_string_pretty(&settings)?
                );
                return Ok(());
            }
            ConfigAction::Set { key, value } => {
                let key = key.replace('-', "_");
                anyhow::ensure!(KEYS.contains(&key.as_str()), "unknown setting: {key}");
                let value = match key.as_str() {
                    "download_dir" => serde_json::Value::String(value.clone()),
                    "audio_language" | "subtitle_language" => serde_json::json!(value
                        .split(',')
                        .filter(|s| !s.is_empty())
                        .collect::<Vec<_>>()),
                    _ => serde_json::from_str(value).context("expected a number or boolean")?,
                };
                settings.values.insert(key, value);
            }
            ConfigAction::Reset { key: Some(key) } => {
                let key = key.replace('-', "_");
                if !KEYS.contains(&key.as_str()) {
                    bail!("unknown setting: {key}");
                }
                settings.values.remove(&key);
            }
            ConfigAction::Reset { key: None } => settings = Self::default(),
        }
        settings.arguments()?;
        crate::downloads::atomic_write(path, &serde_json::to_vec_pretty(&settings)?)?;
        println!("Saved configuration: {}", path.display());
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn explicit_defaults_override_settings_and_unknowns_fail() {
        let settings: Settings =
            serde_json::from_str(r#"{"buffer_mb":42,"resume":false}"#).unwrap();
        let matches = Cli::command()
            .try_get_matches_from(["app", "--buffer-mb", "500", "--resume"])
            .unwrap();
        let mut cli = Cli::from_arg_matches(&matches).unwrap();
        settings.apply(&mut cli, &matches).unwrap();
        assert_eq!(cli.buffer_mb, 500);
        assert!(!cli.no_resume);
        let unknown: Settings = serde_json::from_str(r#"{"unknown":42}"#).unwrap();
        assert!(unknown.arguments().is_err());
        let invalid: Settings = serde_json::from_str(r#"{"buffer_mb":0}"#).unwrap();
        assert!(invalid.arguments().is_err());
    }
}

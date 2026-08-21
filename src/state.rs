use std::{
    collections::BTreeMap,
    env,
    fs::{self, File, OpenOptions},
    io::{BufWriter, Write},
    path::{Path, PathBuf},
    time::{SystemTime, UNIX_EPOCH},
};

use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};

const STATE_VERSION: u8 = 1;

#[derive(Clone, Debug, Deserialize, PartialEq, Eq, Serialize)]
pub struct SavedDisplay {
    pub display_id: u32,
    pub name: String,
    pub vendor: u32,
    pub model: u32,
    pub serial: u32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub disabled_session: Option<String>,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Eq, Serialize)]
pub struct StoredState {
    version: u8,
    pub displays: BTreeMap<String, SavedDisplay>,
}

impl Default for StoredState {
    fn default() -> Self {
        Self {
            version: STATE_VERSION,
            displays: BTreeMap::new(),
        }
    }
}

#[derive(Clone, Debug)]
pub struct StateStore {
    path: PathBuf,
}

impl StateStore {
    pub fn for_current_user() -> Result<Self> {
        if let Some(path) = env::var_os("MEXTDISPLAY_STATE") {
            return Ok(Self::new(path));
        }

        let home = env::var_os("HOME").context("HOME is not set")?;
        Ok(Self::new(PathBuf::from(home).join(
            "Library/Application Support/mextdisplay/state.json",
        )))
    }

    pub fn new(path: impl Into<PathBuf>) -> Self {
        Self { path: path.into() }
    }

    pub fn load(&self) -> Result<StoredState> {
        let contents = match fs::read(&self.path) {
            Ok(contents) => contents,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                return Ok(StoredState::default());
            }
            Err(error) => {
                return Err(error)
                    .with_context(|| format!("could not read {}", self.path.display()));
            }
        };

        let state: StoredState = serde_json::from_slice(&contents)
            .with_context(|| format!("invalid state file {}", self.path.display()))?;
        if state.version != STATE_VERSION {
            bail!(
                "unsupported state version {} in {}",
                state.version,
                self.path.display()
            );
        }
        Ok(state)
    }

    pub fn save(&self, state: &StoredState) -> Result<()> {
        let parent = self
            .path
            .parent()
            .context("display state path has no parent directory")?;
        fs::create_dir_all(parent)
            .with_context(|| format!("could not create {}", parent.display()))?;

        let temporary = temporary_path(&self.path);
        let result = self.write_and_replace(state, &temporary);
        if result.is_err() {
            let _ = fs::remove_file(&temporary);
        }
        result
    }

    fn write_and_replace(&self, state: &StoredState, temporary: &Path) -> Result<()> {
        let file = OpenOptions::new()
            .create_new(true)
            .write(true)
            .open(temporary)
            .with_context(|| format!("could not create {}", temporary.display()))?;
        let mut writer = BufWriter::new(file);
        serde_json::to_writer_pretty(&mut writer, state)
            .context("could not encode display state")?;
        writer
            .write_all(b"\n")
            .context("could not write display state")?;
        writer.flush().context("could not flush display state")?;
        let file: File = writer
            .into_inner()
            .map_err(|error| error.into_error())
            .context("could not finish display state")?;
        file.sync_all().context("could not sync display state")?;
        fs::rename(temporary, &self.path).with_context(|| {
            format!(
                "could not replace {} with saved display state",
                self.path.display()
            )
        })?;
        Ok(())
    }
}

fn temporary_path(path: &Path) -> PathBuf {
    let file_name = path
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("state.json");
    let nonce = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |duration| duration.as_nanos());
    path.with_file_name(format!(".{file_name}.tmp.{}.{nonce}", std::process::id()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn state_round_trips() {
        let unique = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("clock should be after epoch")
            .as_nanos();
        let directory = env::temp_dir().join(format!("mextdisplay-test-{unique}"));
        let store = StateStore::new(directory.join("state.json"));
        let mut state = StoredState::default();
        state.displays.insert(
            "A-DISPLAY-UUID".to_owned(),
            SavedDisplay {
                display_id: 2,
                name: "Studio Display".to_owned(),
                vendor: 1,
                model: 2,
                serial: 3,
                disabled_session: Some("session".to_owned()),
            },
        );

        store.save(&state).expect("state should save");
        assert_eq!(store.load().expect("state should load"), state);

        fs::remove_dir_all(directory).expect("test state should be removable");
    }
}

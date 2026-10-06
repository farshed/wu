//! Credential bytes are hashed locally and never included in diagnostics or disk catalogs.
use crate::HarnessId;
use crate::{HarnessError, ModelContext};
use sha2::{Digest, Sha256};
use std::path::{Path, PathBuf};

pub(crate) fn root(variable: &str, fallback: PathBuf) -> PathBuf {
    std::env::var_os(variable)
        .filter(|v| !v.is_empty())
        .map(PathBuf::from)
        .unwrap_or(fallback)
}

pub(crate) fn context(
    id: HarnessId,
    binary: &Path,
    extra: &[PathBuf],
) -> Result<ModelContext, HarnessError> {
    let home = crate::executable::home_or_current_dir();
    let files = match id {
        HarnessId::Codex => vec![root("CODEX_HOME", home.join(".codex")).join("auth.json")],
        HarnessId::ClaudeCode => {
            let root = root("CLAUDE_CONFIG_DIR", home.join(".claude"));
            vec![root.join("settings.json"), root.join(".credentials.json")]
        }
        HarnessId::Opencode => opencode_files(&home),
    };
    let binary = binary
        .canonicalize()
        .unwrap_or_else(|_| binary.to_path_buf());
    let version = crate::executable::binary_version(&binary).map(|v| v.to_string());
    let mut hash = Sha256::new();
    field(&mut hash, binary.as_os_str().as_encoded_bytes());
    field(
        &mut hash,
        version.as_deref().unwrap_or("unknown").as_bytes(),
    );
    // Unknown-version executables must still invalidate on replacement.
    if let Ok(metadata) = binary.metadata() {
        field(
            &mut hash,
            format!("{:?}:{}", metadata.modified().ok(), metadata.len()).as_bytes(),
        );
    }
    if id == HarnessId::Opencode {
        hash_opencode_files(&mut hash, files.iter().chain(extra));
    } else {
        hash_files(&mut hash, files.iter().chain(extra))?;
    }
    let prefixes: &[&str] = match id {
        HarnessId::Codex => &["CODEX_", "OPENAI_"],
        HarnessId::ClaudeCode => &["CLAUDE_", "ANTHROPIC_", "AWS_"],
        HarnessId::Opencode => &["OPENCODE_", "OPENAI_", "ANTHROPIC_", "GOOGLE_"],
    };
    let mut env: Vec<_> = std::env::vars_os()
        .filter(|(key, _)| {
            prefixes
                .iter()
                .any(|prefix| key.to_string_lossy().starts_with(prefix))
        })
        .collect();
    env.sort();
    for (key, value) in env {
        field(&mut hash, key.as_encoded_bytes());
        field(&mut hash, value.as_encoded_bytes());
    }
    Ok(ModelContext {
        hash: format!("{:x}", hash.finalize()),
        binary_path: binary,
        binary_version: version,
    })
}

/// The files that shape opencode's discovery run, which boots in Wu's scratch folder.
fn opencode_files(home: &Path) -> Vec<PathBuf> {
    let data = root("XDG_DATA_HOME", home.join(".local").join("share"));
    let config = root("XDG_CONFIG_HOME", home.join(".config")).join("opencode");
    let mut files = vec![
        data.join("opencode").join("auth.json"),
        config.join("config.json"),
        config.join("opencode.json"),
        config.join("opencode.jsonc"),
    ];
    for directory in crate::executable::scratch_dir().ancestors() {
        for name in [
            "opencode.json",
            "opencode.jsonc",
            ".opencode/opencode.json",
            ".opencode/opencode.jsonc",
        ] {
            files.push(directory.join(name));
        }
    }
    if let Some(directory) =
        std::env::var_os("OPENCODE_CONFIG_DIR").filter(|value| !value.is_empty())
    {
        let directory = PathBuf::from(directory);
        files.push(directory.join("opencode.json"));
        files.push(directory.join("opencode.jsonc"));
    }
    if let Some(path) = std::env::var_os("OPENCODE_CONFIG").filter(|path| !path.is_empty()) {
        files.push(path.into());
    }
    files
}

/// opencode writes a schema-only global config on first boot, so that file must hash like a missing one.
fn is_generated_opencode_config(bytes: &[u8]) -> bool {
    serde_json_lenient::from_slice::<serde_json::Value>(bytes)
        .ok()
        .and_then(|config| {
            config
                .as_object()
                .map(|config| config.keys().all(|key| key == "$schema"))
        })
        .unwrap_or(false)
}

fn hash_opencode_files<'a>(hash: &mut Sha256, files: impl Iterator<Item = &'a PathBuf>) {
    for path in files {
        field(hash, path.as_os_str().as_encoded_bytes());
        match std::fs::read(path) {
            Ok(bytes) if is_generated_opencode_config(&bytes) => hash.update([0]),
            Ok(bytes) => {
                hash.update([1]);
                field(hash, &bytes);
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => hash.update([0]),
            Err(error) => {
                hash.update([2]);
                field(hash, format!("{:?}", error.kind()).as_bytes());
            }
        }
    }
}

fn field(hash: &mut Sha256, bytes: &[u8]) {
    hash.update((bytes.len() as u64).to_le_bytes());
    hash.update(bytes);
}
fn hash_files<'a>(
    hash: &mut Sha256,
    files: impl Iterator<Item = &'a PathBuf>,
) -> Result<(), HarnessError> {
    for path in files {
        field(hash, path.as_os_str().as_encoded_bytes());
        match std::fs::read(path) {
            Ok(bytes) => {
                hash.update([1]);
                field(hash, &bytes);
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => hash.update([0]),
            Err(e) => return Err(e.into()),
        }
    }
    Ok(())
}

impl ModelContext {
    pub(crate) fn key(&self) -> [u8; 32] {
        Sha256::digest(self.hash.as_bytes()).into()
    }
    pub(crate) fn log(&self) {
        tracing::info!(binary_path = %self.binary_path.display(), binary_version = self.binary_version.as_deref().unwrap_or("unknown"), "Model discovery binary");
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn content_changes_and_missing_files_invalidate_without_exposing_secrets() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("auth.json");
        let key = || {
            let mut hash = Sha256::new();
            hash_files(&mut hash, std::iter::once(&file)).unwrap();
            format!("{:x}", hash.finalize())
        };
        let missing = key();
        std::fs::write(&file, "account-one").unwrap();
        let first = key();
        std::fs::write(&file, "account-two").unwrap();
        assert_ne!(first, key());
        assert_ne!(missing, first);
        assert!(!first.contains("account"));
        std::fs::remove_file(&file).unwrap();
        assert_eq!(key(), missing);
    }

    #[test]
    fn opencode_first_boot_config_and_unreadable_files_keep_the_key() {
        let dir = tempfile::tempdir().unwrap();
        let config = dir.path().join("opencode.jsonc");
        let ancestor_file = dir.path().join(".opencode");
        std::fs::write(&ancestor_file, "a file where a folder is expected").unwrap();
        let unreadable = ancestor_file.join("opencode.json");
        let files = [config.clone(), unreadable];
        let key = || {
            let mut hash = Sha256::new();
            hash_opencode_files(&mut hash, files.iter());
            format!("{:x}", hash.finalize())
        };
        let missing = key();
        std::fs::write(&config, r#"{"$schema":"https://opencode.ai/config.json"}"#).unwrap();
        assert_eq!(key(), missing, "the config opencode writes on first boot");
        std::fs::write(
            &config,
            r#"{"$schema":"https://opencode.ai/config.json","model":"a/b"}"#,
        )
        .unwrap();
        assert_ne!(key(), missing);
    }
}

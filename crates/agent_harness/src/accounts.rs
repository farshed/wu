//! Never refresh tokens, and never write a CLI's credential store outside `activate`.

use std::path::{Path, PathBuf};

use async_trait::async_trait;
use base64::Engine as _;
use base64::engine::general_purpose::{STANDARD as BASE64, URL_SAFE_NO_PAD as BASE64_URL};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};

use crate::HarnessId;
use crate::usage::{
    UsageRequest, claude_plan, claude_usage_request, codex_plan, codex_usage_request,
};

/// Machine-shared MCP and plugin tokens stored beside `claudeAiOauth`; the live copies win on activate.
const CLAUDE_SHARED_CREDENTIAL_KEYS: &[&str] = &[
    "mcpOAuth",
    "mcpOAuthClientConfig",
    "mcpXaaIdp",
    "mcpXaaIdpConfig",
    "pluginSecrets",
];

static OPERATIONS: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Account {
    pub id: String,
    pub harness: HarnessId,
    pub label: String,
    pub plan: Option<String>,
    pub active: bool,
}

pub async fn list(harness: HarnessId, data_dir: &Path) -> Result<Vec<Account>, String> {
    Stores::detect(data_dir)?.list(harness).await
}

pub async fn activate(harness: HarnessId, account_id: &str, data_dir: &Path) -> Result<(), String> {
    Stores::detect(data_dir)?
        .activate(harness, account_id)
        .await
}

pub async fn usage_request(
    harness: HarnessId,
    account_id: &str,
    data_dir: &Path,
) -> Result<UsageRequest, String> {
    Stores::detect(data_dir)?
        .usage_request(harness, account_id)
        .await
}

pub async fn remove(harness: HarnessId, account_id: &str, data_dir: &Path) -> Result<(), String> {
    Stores::detect(data_dir)?.remove(harness, account_id).await
}

#[async_trait]
trait Keychain: Send + Sync {
    async fn has_item(&self) -> bool;
    async fn read(&self) -> Result<Option<Value>, String>;
    async fn write(&self, json: &str) -> Result<(), String>;
}

#[cfg(target_os = "macos")]
struct SystemKeychain {
    service: String,
}

#[cfg(target_os = "macos")]
#[async_trait]
impl Keychain for SystemKeychain {
    async fn has_item(&self) -> bool {
        crate::usage::keychain::item_exists(&self.service).await
    }

    async fn read(&self) -> Result<Option<Value>, String> {
        crate::usage::keychain::read_credentials(&self.service).await
    }

    async fn write(&self, json: &str) -> Result<(), String> {
        const UNANSWERED_PROMPT_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(60);
        let service = self.service.clone();
        let json = json.to_owned();
        // In-process so the secret never appears in another process's arguments.
        let write = tokio::task::spawn_blocking(move || {
            security_framework::passwords::set_generic_password(
                &service,
                &crate::usage::keychain::account(),
                json.as_bytes(),
            )
        });
        match tokio::time::timeout(UNANSWERED_PROMPT_TIMEOUT, write).await {
            Ok(Ok(Ok(()))) => Ok(()),
            Ok(Ok(Err(error))) => Err(format!("Keychain write failed: {error}")),
            Ok(Err(error)) => Err(format!("Keychain write failed: {error}")),
            Err(_) => Err("The macOS Keychain didn't answer in time".into()),
        }
    }
}

#[cfg(target_os = "macos")]
fn system_keychain(claude_config_dir: Option<&Path>) -> Option<Box<dyn Keychain>> {
    Some(Box::new(SystemKeychain {
        service: crate::usage::claude_keychain_service(claude_config_dir),
    }))
}

#[cfg(not(target_os = "macos"))]
fn system_keychain(_claude_config_dir: Option<&Path>) -> Option<Box<dyn Keychain>> {
    None
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct Profile {
    email: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    display_name: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    organization: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    plan: Option<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct Slot {
    id: String,
    harness: HarnessId,
    account_key: String,
    profile: Profile,
    credentials: Value,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    claude_config: Option<Value>,
    saved_at: i64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    created_at: Option<i64>,
}

struct Detected {
    account_key: String,
    profile: Profile,
    credentials: Result<Option<Value>, String>,
    claude_config: Option<Value>,
}

struct Stores {
    data_dir: PathBuf,
    claude_config_dir: PathBuf,
    claude_config_file: PathBuf,
    codex_home: PathBuf,
    keychain: Option<Box<dyn Keychain>>,
}

impl Stores {
    fn detect(data_dir: &Path) -> Result<Self, String> {
        let home = crate::executable::home_dir()
            .ok_or_else(|| "Couldn't find your home directory".to_string())?;
        let env_dir = |name: &str| {
            std::env::var_os(name)
                .filter(|value| !value.is_empty())
                .map(PathBuf::from)
        };
        let claude_dir = env_dir("CLAUDE_CONFIG_DIR");
        Ok(Self {
            data_dir: data_dir.to_path_buf(),
            claude_config_file: match &claude_dir {
                Some(dir) => dir.join(".claude.json"),
                None => home.join(".claude.json"),
            },
            keychain: system_keychain(claude_dir.as_deref()),
            claude_config_dir: claude_dir.unwrap_or_else(|| home.join(".claude")),
            codex_home: env_dir("CODEX_HOME").unwrap_or_else(|| home.join(".codex")),
        })
    }

    fn claude_credentials_file(&self) -> PathBuf {
        self.claude_config_dir.join(".credentials.json")
    }

    fn codex_auth_file(&self) -> PathBuf {
        self.codex_home.join("auth.json")
    }

    fn slots_dir(&self, harness: HarnessId) -> PathBuf {
        self.data_dir
            .join("agent-accounts")
            .join(harness_slug(harness))
    }

    async fn list(&self, harness: HarnessId) -> Result<Vec<Account>, String> {
        let _operation = OPERATIONS.lock().await;
        let live = self.refresh_live(harness).await?;
        let slots = self.read_slots(harness)?;
        let live_key = live.as_ref().map(|live| live.account_key.as_str());
        let mut accounts: Vec<Account> = slots
            .iter()
            .map(|slot| Account {
                id: slot.id.clone(),
                harness,
                label: slot.profile.email.clone(),
                plan: slot.profile.plan.clone(),
                active: live_key == Some(slot.account_key.as_str()),
            })
            .collect();
        if let Some(live) = &live
            && !slots
                .iter()
                .any(|slot| slot.account_key == live.account_key)
        {
            accounts.push(Account {
                id: slot_id_for(harness, &live.account_key),
                harness,
                label: live.profile.email.clone(),
                plan: live.profile.plan.clone(),
                active: true,
            });
        }
        Ok(accounts)
    }

    async fn activate(&self, harness: HarnessId, account_id: &str) -> Result<(), String> {
        let slot_file = self.slot_file(harness, account_id)?;
        let _operation = OPERATIONS.lock().await;
        let live = self.refresh_live(harness).await?;
        match &live {
            Some(Detected {
                credentials: Err(reason),
                ..
            }) => {
                return Err(format!(
                    "Couldn't back up the current login before switching: {reason}"
                ));
            }
            None if harness == HarnessId::Codex && self.codex_auth_file().exists() => {
                return Err(
                    "Codex's auth.json holds a login that couldn't be read, so it can't be backed \
                     up. Not switching."
                        .into(),
                );
            }
            None if harness == HarnessId::ClaudeCode && self.claude_login_present().await => {
                return Err(
                    "Claude Code is signed in, but ~/.claude.json doesn't say which account it is, \
                     so the login can't be backed up. Not switching."
                        .into(),
                );
            }
            _ => {}
        }
        let slot = read_slot(&slot_file).ok_or_else(|| {
            "That saved login no longer exists. Refresh and try again.".to_string()
        })?;
        match harness {
            HarnessId::ClaudeCode => self.activate_claude(&slot).await,
            HarnessId::Codex => self.activate_codex(&slot),
        }
    }

    async fn activate_claude(&self, slot: &Slot) -> Result<(), String> {
        let config_file = &self.claude_config_file;
        let config = read_json(config_file);
        if config.is_none() && config_file.exists() {
            return Err(
                "~/.claude.json exists but couldn't be parsed, so it was left alone. Fix or \
                 remove the file and try again."
                    .into(),
            );
        }
        let mut merged = config.unwrap_or_else(|| json!({}));
        let map = merged
            .as_object_mut()
            .ok_or_else(|| "~/.claude.json is not a JSON object. Not switching.".to_string())?;
        let (oauth_account, user_id) = match &slot.claude_config {
            Some(config) => (
                config.get("oauthAccount").cloned(),
                config.get("userID").cloned(),
            ),
            None => (None, None),
        };
        map.insert(
            "oauthAccount".into(),
            oauth_account.unwrap_or_else(|| {
                json!({
                    "accountUuid": slot.account_key,
                    "emailAddress": slot.profile.email,
                    "organizationName": slot.profile.organization,
                    "displayName": slot.profile.display_name,
                })
            }),
        );
        match user_id.filter(Value::is_string) {
            Some(user_id) => {
                map.insert("userID".into(), user_id);
            }
            None => {
                map.remove("userID");
            }
        }

        let live = self.read_claude_credentials().await?;
        let credentials = compose_claude_credentials(&slot.credentials, live.as_ref());
        self.write_claude_credentials(&credentials).await?;
        if let Err(error) = write_file_atomic(config_file, merged.to_string().as_bytes(), false) {
            // Credentials without the matching identity would be snapshotted into the wrong slot.
            if let Some(live) = &live
                && let Err(restore_error) = self.write_claude_credentials(live).await
            {
                log::error!("restoring Claude credentials after a failed switch: {restore_error}");
            }
            return Err(error);
        }
        Ok(())
    }

    fn activate_codex(&self, slot: &Slot) -> Result<(), String> {
        create_dir_all(&self.codex_home)?;
        let json = serde_json::to_string_pretty(&slot.credentials)
            .map_err(|error| format!("Couldn't serialize the Codex login: {error}"))?;
        write_file_atomic(&self.codex_auth_file(), json.as_bytes(), true)
    }

    async fn usage_request(
        &self,
        harness: HarnessId,
        account_id: &str,
    ) -> Result<UsageRequest, String> {
        let slot_file = self.slot_file(harness, account_id)?;
        let _operation = OPERATIONS.lock().await;
        let live_key = match harness {
            HarnessId::ClaudeCode => self.claude_identity().map(|identity| identity.0),
            HarnessId::Codex => self.detect_codex().map(|live| live.account_key),
        };
        if live_key.is_some_and(|key| slot_id_for(harness, &key) == account_id)
            && let Some(Detected {
                credentials: Err(reason),
                ..
            }) = self.refresh_live(harness).await?
        {
            return Err(reason);
        }
        let slot =
            read_slot(&slot_file).ok_or_else(|| "That saved login no longer exists".to_string())?;
        match harness {
            HarnessId::ClaudeCode => claude_usage_request(&slot.credentials),
            HarnessId::Codex => codex_usage_request(&slot.credentials),
        }
    }

    async fn remove(&self, harness: HarnessId, account_id: &str) -> Result<(), String> {
        let slot_file = self.slot_file(harness, account_id)?;
        let _operation = OPERATIONS.lock().await;
        let live = self.refresh_live(harness).await?;
        if live.is_some_and(|live| slot_id_for(harness, &live.account_key) == account_id) {
            return Err(
                "That account is signed in right now. Switch to another account before \
                 removing it."
                    .into(),
            );
        }
        remove_if_exists(&slot_file)
    }

    async fn refresh_live(&self, harness: HarnessId) -> Result<Option<Detected>, String> {
        let live = match harness {
            HarnessId::ClaudeCode => self.detect_claude().await,
            HarnessId::Codex => self.detect_codex(),
        };
        if let Some(live) = &live
            && let Ok(Some(credentials)) = &live.credentials
        {
            self.write_slot(&Slot {
                id: slot_id_for(harness, &live.account_key),
                harness,
                account_key: live.account_key.clone(),
                profile: live.profile.clone(),
                credentials: credentials.clone(),
                claude_config: live.claude_config.clone(),
                saved_at: now_ms(),
                created_at: None,
            })?;
        }
        Ok(live)
    }

    fn claude_identity(&self) -> Option<(String, Profile, Value)> {
        let config = read_json(&self.claude_config_file)?;
        let oauth = config.get("oauthAccount")?.clone();
        let email = str_field(&oauth, "emailAddress")?;
        let account_key = str_field(&oauth, "accountUuid").unwrap_or_else(|| email.clone());
        let profile = Profile {
            email,
            display_name: str_field(&oauth, "displayName"),
            organization: str_field(&oauth, "organizationName"),
            plan: claude_plan(
                str_field(&oauth, "organizationType").as_deref(),
                str_field(&oauth, "organizationRateLimitTier").as_deref(),
            ),
        };
        let mut claude_config = json!({ "oauthAccount": oauth });
        if let Some(user_id) = config.get("userID").filter(|value| value.is_string())
            && let Some(map) = claude_config.as_object_mut()
        {
            map.insert("userID".into(), user_id.clone());
        }
        Some((account_key, profile, claude_config))
    }

    async fn detect_claude(&self) -> Option<Detected> {
        let (account_key, profile, claude_config) = self.claude_identity()?;
        Some(Detected {
            account_key,
            profile,
            credentials: self.read_claude_credentials().await,
            claude_config: Some(claude_config),
        })
    }

    fn detect_codex(&self) -> Option<Detected> {
        read_json(&self.codex_auth_file()).and_then(parse_codex_auth)
    }

    /// Keychain first, matching Claude Code: the file is a fallback it often leaves stale beside a Keychain login.
    async fn read_claude_credentials(&self) -> Result<Option<Value>, String> {
        if let Some(keychain) = &self.keychain
            && let Some(credentials) = keychain.read().await?
        {
            return Ok(Some(credentials));
        }
        Ok(read_json(&self.claude_credentials_file()))
    }

    async fn claude_login_present(&self) -> bool {
        if let Some(keychain) = &self.keychain
            && keychain.has_item().await
        {
            return true;
        }
        self.claude_credentials_file().exists()
    }

    async fn write_claude_credentials(&self, credentials: &Value) -> Result<(), String> {
        let json = credentials.to_string();
        let file = self.claude_credentials_file();
        if let Some(keychain) = &self.keychain
            && (keychain.has_item().await || !file.exists())
        {
            return keychain.write(&json).await;
        }
        create_dir_all(&self.claude_config_dir)?;
        write_file_atomic(&file, json.as_bytes(), true)
    }

    fn slot_file(&self, harness: HarnessId, account_id: &str) -> Result<PathBuf, String> {
        let is_slot_id = account_id.len() == 16
            && account_id
                .bytes()
                .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte));
        if !is_slot_id {
            return Err("Unknown account".into());
        }
        Ok(self.slots_dir(harness).join(format!("{account_id}.json")))
    }

    fn read_slots(&self, harness: HarnessId) -> Result<Vec<Slot>, String> {
        let dir = self.slots_dir(harness);
        let entries = match std::fs::read_dir(&dir) {
            Ok(entries) => entries,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
            Err(error) => return Err(format!("Couldn't read {}: {error}", dir.display())),
        };
        let mut slots: Vec<Slot> = entries
            .flatten()
            .map(|entry| entry.path())
            .filter(|path| {
                path.extension()
                    .is_some_and(|extension| extension == "json")
            })
            .filter_map(|path| {
                let slot = read_slot(&path);
                if slot.is_none() {
                    log::warn!("skipping unreadable saved login {}", path.display());
                }
                slot
            })
            .collect();
        slots.sort_by(|a, b| {
            (a.created_at.unwrap_or(a.saved_at), &a.id)
                .cmp(&(b.created_at.unwrap_or(b.saved_at), &b.id))
        });
        Ok(slots)
    }

    fn write_slot(&self, slot: &Slot) -> Result<(), String> {
        let dir = self.slots_dir(slot.harness);
        if let Some(root) = dir.parent() {
            private_dir(root)?;
        }
        private_dir(&dir)?;
        let file = dir.join(format!("{}.json", slot.id));
        let created_at = match read_slot(&file) {
            Some(existing) => existing.created_at.unwrap_or(existing.saved_at),
            None => slot.created_at.unwrap_or_else(|| {
                self.read_slots(slot.harness)
                    .unwrap_or_default()
                    .iter()
                    .map(|sibling| sibling.created_at.unwrap_or(sibling.saved_at) + 1)
                    .max()
                    .unwrap_or(slot.saved_at)
                    .max(slot.saved_at)
            }),
        };
        let full = Slot {
            created_at: Some(created_at),
            ..slot.clone()
        };
        let json = serde_json::to_string_pretty(&full)
            .map_err(|error| format!("Couldn't serialize the saved login: {error}"))?;
        write_file_atomic(&file, json.as_bytes(), true)
    }
}

fn harness_slug(harness: HarnessId) -> &'static str {
    match harness {
        HarnessId::ClaudeCode => "claude-code",
        HarnessId::Codex => "codex",
    }
}

fn slot_id_for(harness: HarnessId, account_key: &str) -> String {
    let digest = Sha256::digest(format!("{}:{account_key}", harness_slug(harness)).as_bytes());
    hex(&digest[..8])
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|byte| format!("{byte:02x}")).collect()
}

fn now_ms() -> i64 {
    chrono::Utc::now().timestamp_millis()
}

fn read_slot(file: &Path) -> Option<Slot> {
    serde_json::from_slice(&std::fs::read(file).ok()?).ok()
}

fn read_json(file: &Path) -> Option<Value> {
    serde_json::from_slice::<Value>(&std::fs::read(file).ok()?)
        .ok()
        .filter(Value::is_object)
}

fn str_field(value: &Value, key: &str) -> Option<String> {
    value
        .get(key)
        .and_then(Value::as_str)
        .filter(|text| !text.is_empty())
        .map(str::to_owned)
}

fn jwt_claims(jwt: &str) -> Option<Value> {
    let payload = jwt.split('.').nth(1)?;
    let bytes = BASE64_URL
        .decode(payload)
        .or_else(|_| BASE64.decode(payload))
        .ok()?;
    serde_json::from_slice(&bytes).ok()
}

fn parse_codex_auth(auth: Value) -> Option<Detected> {
    if let Some(id_token) = auth
        .get("tokens")
        .and_then(|tokens| tokens.get("id_token"))
        .and_then(Value::as_str)
    {
        let claims = jwt_claims(id_token).unwrap_or_else(|| json!({}));
        let openai = claims
            .get("https://api.openai.com/auth")
            .cloned()
            .unwrap_or_default();
        let email = str_field(&claims, "email")?;
        // Team seats share a workspace id, so the user id keeps teammates apart.
        let workspace = str_field(&openai, "chatgpt_account_id");
        let user = str_field(&openai, "chatgpt_user_id").or_else(|| str_field(&openai, "user_id"));
        let account_key = match (user, workspace) {
            (Some(user), Some(workspace)) => format!("{user}::{workspace}"),
            (None, Some(workspace)) => workspace,
            _ => email.clone(),
        };
        return Some(Detected {
            account_key,
            profile: Profile {
                email,
                display_name: str_field(&claims, "name"),
                organization: None,
                plan: codex_plan(str_field(&openai, "chatgpt_plan_type").as_deref()),
            },
            credentials: Ok(Some(auth)),
            claude_config: None,
        });
    }
    let api_key = str_field(&auth, "OPENAI_API_KEY")?;
    let digest = Sha256::digest(api_key.as_bytes());
    let tail_start = api_key
        .char_indices()
        .rev()
        .nth(3)
        .map_or(0, |(index, _)| index);
    let tail = &api_key[tail_start..];
    Some(Detected {
        account_key: format!("api-key:{}", hex(&digest[..6])),
        profile: Profile {
            email: format!("API key ·…{tail}"),
            display_name: None,
            organization: None,
            plan: Some("API key".into()),
        },
        credentials: Ok(Some(auth)),
        claude_config: None,
    })
}

fn compose_claude_credentials(target: &Value, live: Option<&Value>) -> Value {
    let Some(live) = live.and_then(Value::as_object) else {
        return target.clone();
    };
    let Some(target_map) = target.as_object() else {
        return target.clone();
    };
    if !target_map.contains_key("claudeAiOauth") {
        return target.clone();
    }
    let mut composed: serde_json::Map<String, Value> = target_map
        .iter()
        .filter(|(key, _)| !CLAUDE_SHARED_CREDENTIAL_KEYS.contains(&key.as_str()))
        .map(|(key, value)| (key.clone(), value.clone()))
        .collect();
    for key in CLAUDE_SHARED_CREDENTIAL_KEYS {
        if let Some(value) = live.get(*key) {
            composed.insert((*key).to_string(), value.clone());
        }
    }
    Value::Object(composed)
}

fn create_dir_all(dir: &Path) -> Result<(), String> {
    std::fs::create_dir_all(dir)
        .map_err(|error| format!("Couldn't create {}: {error}", dir.display()))
}

fn private_dir(dir: &Path) -> Result<(), String> {
    create_private_dir(dir).map_err(|error| format!("Couldn't create {}: {error}", dir.display()))
}

#[cfg(unix)]
fn create_private_dir(dir: &Path) -> std::io::Result<()> {
    use std::os::unix::fs::{DirBuilderExt, PermissionsExt};
    if let Some(parent) = dir.parent() {
        std::fs::create_dir_all(parent)?;
    }
    match std::fs::DirBuilder::new().mode(0o700).create(dir) {
        Ok(()) => {}
        Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists && dir.is_dir() => {}
        Err(error) => return Err(error),
    }
    std::fs::set_permissions(dir, std::fs::Permissions::from_mode(0o700))
}

#[cfg(not(unix))]
fn create_private_dir(dir: &Path) -> std::io::Result<()> {
    std::fs::create_dir_all(dir)
}

fn remove_if_exists(file: &Path) -> Result<(), String> {
    match std::fs::remove_file(file) {
        Err(error) if error.kind() != std::io::ErrorKind::NotFound => {
            Err(format!("Couldn't remove {}: {error}", file.display()))
        }
        _ => Ok(()),
    }
}

fn write_file_atomic(file: &Path, bytes: &[u8], secret: bool) -> Result<(), String> {
    let dir = file
        .parent()
        .filter(|dir| !dir.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("."));
    let name = file
        .file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_default();
    let temp = dir.join(format!(".{name}.{}.tmp", uuid::Uuid::new_v4().simple()));
    let result = write_new_file(&temp, bytes, file_mode(file, secret))
        .and_then(|()| std::fs::rename(&temp, file));
    if result.is_err()
        && let Err(error) = std::fs::remove_file(&temp)
        && error.kind() != std::io::ErrorKind::NotFound
    {
        log::warn!("couldn't remove {}: {error}", temp.display());
    }
    result.map_err(|error| format!("Couldn't write {}: {error}", file.display()))
}

#[cfg_attr(not(unix), allow(unused_variables))]
fn file_mode(file: &Path, secret: bool) -> u32 {
    if secret {
        return 0o600;
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        if let Ok(metadata) = std::fs::metadata(file) {
            return metadata.permissions().mode() & 0o777;
        }
    }
    0o644
}

#[cfg_attr(not(unix), allow(unused_variables))]
fn write_new_file(path: &Path, bytes: &[u8], mode: u32) -> std::io::Result<()> {
    use std::io::Write as _;
    let mut options = std::fs::OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    std::os::unix::fs::OpenOptionsExt::mode(&mut options, mode);
    let mut handle = options.open(path)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        // The umask may have narrowed the mode given at creation.
        handle.set_permissions(std::fs::Permissions::from_mode(mode))?;
    }
    handle.write_all(bytes)?;
    handle.sync_all()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{Arc, Mutex};

    #[derive(Default)]
    struct FakeKeychainState {
        item: Option<String>,
        denied: bool,
    }

    #[derive(Clone, Default)]
    struct FakeKeychain(Arc<Mutex<FakeKeychainState>>);

    impl FakeKeychain {
        fn with_item(item: &Value) -> Self {
            let keychain = Self::default();
            keychain.0.lock().unwrap().item = Some(item.to_string());
            keychain
        }

        fn item(&self) -> Option<Value> {
            let state = self.0.lock().unwrap();
            state
                .item
                .as_deref()
                .map(|item| serde_json::from_str(item).unwrap())
        }
    }

    #[async_trait]
    impl Keychain for FakeKeychain {
        async fn has_item(&self) -> bool {
            self.0.lock().unwrap().item.is_some()
        }

        async fn read(&self) -> Result<Option<Value>, String> {
            let state = self.0.lock().unwrap();
            match (&state.item, state.denied) {
                (None, _) => Ok(None),
                (Some(_), true) => Err("denied".into()),
                (Some(item), false) => Ok(Some(serde_json::from_str(item).unwrap())),
            }
        }

        async fn write(&self, json: &str) -> Result<(), String> {
            self.0.lock().unwrap().item = Some(json.to_string());
            Ok(())
        }
    }

    fn stores(root: &Path, keychain: Option<FakeKeychain>) -> Stores {
        Stores {
            data_dir: root.join("data"),
            claude_config_dir: root.join("claude"),
            claude_config_file: root.join("claude.json"),
            codex_home: root.join("codex"),
            keychain: keychain.map(|keychain| Box::new(keychain) as Box<dyn Keychain>),
        }
    }

    fn read_json_file(path: &Path) -> Value {
        serde_json::from_slice(&std::fs::read(path).unwrap()).unwrap()
    }

    fn claude_oauth(token: &str) -> Value {
        json!({
            "claudeAiOauth": {
                "accessToken": token,
                "refreshToken": format!("refresh-{token}"),
                "expiresAt": 4_102_444_800_000i64,
            }
        })
    }

    fn write_claude_identity(stores: &Stores, email: &str, uuid: &str) {
        std::fs::write(
            &stores.claude_config_file,
            json!({
                "oauthAccount": {
                    "accountUuid": uuid,
                    "emailAddress": email,
                    "displayName": "Test User",
                    "organizationName": "Test Org",
                    "organizationType": "claude_max",
                    "organizationRateLimitTier": "default_claude_max_20x",
                },
                "userID": format!("user-{uuid}"),
                "projects": { "/keep/me": { "history": [] } },
            })
            .to_string(),
        )
        .unwrap();
    }

    fn write_claude_login(stores: &Stores, email: &str, uuid: &str, token: &str) {
        std::fs::create_dir_all(&stores.claude_config_dir).unwrap();
        write_claude_identity(stores, email, uuid);
        std::fs::write(
            stores.claude_credentials_file(),
            claude_oauth(token).to_string(),
        )
        .unwrap();
    }

    fn id_token(email: &str, openai: Value) -> String {
        let header = BASE64_URL.encode(br#"{"alg":"none"}"#);
        let payload = BASE64_URL.encode(
            json!({
                "email": email,
                "name": "Codex User",
                "https://api.openai.com/auth": openai,
            })
            .to_string(),
        );
        format!("{header}.{payload}.x")
    }

    fn write_codex_auth(stores: &Stores, auth: Value) {
        std::fs::create_dir_all(&stores.codex_home).unwrap();
        std::fs::write(stores.codex_auth_file(), auth.to_string()).unwrap();
    }

    fn write_codex_login(stores: &Stores, email: &str, account_id: &str) {
        write_codex_auth(
            stores,
            json!({
                "tokens": {
                    "id_token": id_token(email, json!({
                        "chatgpt_account_id": account_id,
                        "chatgpt_plan_type": "plus",
                    })),
                    "access_token": format!("at-{account_id}"),
                    "account_id": account_id,
                }
            }),
        );
    }

    fn write_codex_team_login(stores: &Stores, email: &str, user_id: &str, workspace_id: &str) {
        write_codex_auth(
            stores,
            json!({
                "tokens": {
                    "id_token": id_token(email, json!({
                        "chatgpt_account_id": workspace_id,
                        "chatgpt_user_id": user_id,
                        "chatgpt_plan_type": "team",
                    })),
                    "access_token": format!("at-{user_id}"),
                    "account_id": workspace_id,
                }
            }),
        );
    }

    fn labels(accounts: &[Account]) -> Vec<(String, bool)> {
        let mut labels: Vec<(String, bool)> = accounts
            .iter()
            .map(|account| (account.label.clone(), account.active))
            .collect();
        labels.sort();
        labels
    }

    fn id_of(accounts: &[Account], label: &str) -> String {
        accounts
            .iter()
            .find(|account| account.label == label)
            .unwrap()
            .id
            .clone()
    }

    fn slot_count(stores: &Stores, harness: HarnessId) -> usize {
        std::fs::read_dir(stores.slots_dir(harness))
            .unwrap()
            .flatten()
            .filter(|entry| entry.path().extension().is_some_and(|ext| ext == "json"))
            .count()
    }

    #[cfg(unix)]
    fn mode(path: &Path) -> u32 {
        use std::os::unix::fs::PermissionsExt;
        std::fs::metadata(path).unwrap().permissions().mode() & 0o777
    }

    #[tokio::test]
    async fn an_unidentified_claude_login_is_never_overwritten() {
        let tmp = tempfile::tempdir().unwrap();
        let stores = stores(tmp.path(), None);
        write_claude_login(&stores, "alice@example.com", "uuid-alice", "token-alice");
        let alice_id = stores.list(HarnessId::ClaudeCode).await.unwrap()[0].id.clone();

        std::fs::write(&stores.claude_config_file, json!({ "projects": {} }).to_string()).unwrap();
        std::fs::write(
            stores.claude_credentials_file(),
            claude_oauth("token-unknown").to_string(),
        )
        .unwrap();
        let error = stores
            .activate(HarnessId::ClaudeCode, &alice_id)
            .await
            .unwrap_err();
        assert!(error.contains("can't be backed up"), "{error}");
        let credentials = read_json_file(&stores.claude_credentials_file());
        assert_eq!(credentials["claudeAiOauth"]["accessToken"], "token-unknown");
    }

    #[tokio::test]
    async fn claude_slot_swap_round_trip() {
        let tmp = tempfile::tempdir().unwrap();
        let stores = stores(tmp.path(), None);

        write_claude_login(&stores, "alice@example.com", "uuid-alice", "token-alice");
        let accounts = stores.list(HarnessId::ClaudeCode).await.unwrap();
        assert_eq!(
            labels(&accounts),
            vec![("alice@example.com".to_string(), true)]
        );
        assert_eq!(accounts[0].plan.as_deref(), Some("Max 20×"));
        let alice_id = accounts[0].id.clone();
        assert_eq!(alice_id.len(), 16);

        write_claude_login(&stores, "bob@example.com", "uuid-bob", "token-bob");
        let accounts = stores.list(HarnessId::ClaudeCode).await.unwrap();
        assert_eq!(
            labels(&accounts),
            vec![
                ("alice@example.com".to_string(), false),
                ("bob@example.com".to_string(), true),
            ]
        );
        assert_eq!(accounts[0].label, "alice@example.com", "creation order");

        stores
            .activate(HarnessId::ClaudeCode, &alice_id)
            .await
            .unwrap();
        let accounts = stores.list(HarnessId::ClaudeCode).await.unwrap();
        assert_eq!(
            labels(&accounts),
            vec![
                ("alice@example.com".to_string(), true),
                ("bob@example.com".to_string(), false),
            ]
        );
        let credentials = read_json_file(&stores.claude_credentials_file());
        assert_eq!(credentials["claudeAiOauth"]["accessToken"], "token-alice");
        let config = read_json_file(&stores.claude_config_file);
        assert_eq!(config["oauthAccount"]["emailAddress"], "alice@example.com");
        assert_eq!(config["userID"], "user-uuid-alice");
        assert!(config["projects"]["/keep/me"].is_object());
        assert_eq!(slot_count(&stores, HarnessId::ClaudeCode), 2);
        #[cfg(unix)]
        {
            assert_eq!(mode(&stores.claude_credentials_file()), 0o600);
            assert_eq!(mode(&stores.slots_dir(HarnessId::ClaudeCode)), 0o700);
        }

        std::fs::write(&stores.claude_config_file, "{ definitely not json").unwrap();
        let bob_id = id_of(&accounts, "bob@example.com");
        assert!(
            stores
                .activate(HarnessId::ClaudeCode, &bob_id)
                .await
                .is_err()
        );
        assert_eq!(
            std::fs::read_to_string(&stores.claude_config_file).unwrap(),
            "{ definitely not json"
        );
        let credentials = read_json_file(&stores.claude_credentials_file());
        assert_eq!(
            credentials["claudeAiOauth"]["accessToken"], "token-alice",
            "a refused switch writes nothing"
        );
    }

    #[tokio::test]
    async fn claude_account_switch_keeps_live_mcp_oauth() {
        let tmp = tempfile::tempdir().unwrap();
        let stores = stores(tmp.path(), None);
        let credentials_file = stores.claude_credentials_file();

        write_claude_login(&stores, "alice@example.com", "uuid-alice", "token-alice");
        std::fs::write(
            &credentials_file,
            json!({
                "claudeAiOauth": { "accessToken": "token-alice" },
                "mcpOAuth": { "github": { "accessToken": "stale-github" } },
                "pluginSecrets": { "old": true },
                "trustedDeviceToken": "alice-device",
            })
            .to_string(),
        )
        .unwrap();
        let accounts = stores.list(HarnessId::ClaudeCode).await.unwrap();
        let alice_id = id_of(&accounts, "alice@example.com");

        write_claude_login(&stores, "bob@example.com", "uuid-bob", "token-bob");
        std::fs::write(
            &credentials_file,
            json!({
                "claudeAiOauth": { "accessToken": "token-bob" },
                "mcpOAuth": { "github": { "accessToken": "live-github" } },
                "pluginSecrets": { "live": true },
                "trustedDeviceToken": "bob-device",
            })
            .to_string(),
        )
        .unwrap();

        stores
            .activate(HarnessId::ClaudeCode, &alice_id)
            .await
            .unwrap();

        let credentials = read_json_file(&credentials_file);
        assert_eq!(credentials["claudeAiOauth"]["accessToken"], "token-alice");
        assert_eq!(credentials["trustedDeviceToken"], "alice-device");
        assert_eq!(
            credentials["mcpOAuth"]["github"]["accessToken"],
            "live-github"
        );
        assert_eq!(credentials["pluginSecrets"]["live"], true);
        assert!(credentials["pluginSecrets"].get("old").is_none());

        let bob_slot = read_slot(
            &stores
                .slot_file(
                    HarnessId::ClaudeCode,
                    &slot_id_for(HarnessId::ClaudeCode, "uuid-bob"),
                )
                .unwrap(),
        )
        .unwrap();
        assert_eq!(
            bob_slot.credentials["claudeAiOauth"]["accessToken"], "token-bob",
            "the replaced login was backed up before the switch"
        );
    }

    #[tokio::test]
    async fn claude_account_switch_keeps_mcp_when_target_slot_has_none() {
        let tmp = tempfile::tempdir().unwrap();
        let stores = stores(tmp.path(), None);
        let credentials_file = stores.claude_credentials_file();

        write_claude_login(&stores, "alice@example.com", "uuid-alice", "token-alice");
        let accounts = stores.list(HarnessId::ClaudeCode).await.unwrap();
        let alice_id = accounts[0].id.clone();

        write_claude_login(&stores, "bob@example.com", "uuid-bob", "token-bob");
        std::fs::write(
            &credentials_file,
            json!({
                "claudeAiOauth": { "accessToken": "token-bob" },
                "mcpOAuth": { "linear": { "accessToken": "live-linear" } },
            })
            .to_string(),
        )
        .unwrap();

        stores
            .activate(HarnessId::ClaudeCode, &alice_id)
            .await
            .unwrap();

        let credentials = read_json_file(&credentials_file);
        assert_eq!(credentials["claudeAiOauth"]["accessToken"], "token-alice");
        assert_eq!(
            credentials["mcpOAuth"]["linear"]["accessToken"],
            "live-linear"
        );
    }

    #[tokio::test]
    async fn claude_keychain_wins_over_a_stale_file_and_receives_the_switch() {
        let tmp = tempfile::tempdir().unwrap();
        let keychain = FakeKeychain::with_item(&claude_oauth("token-alice"));
        let stores = stores(tmp.path(), Some(keychain.clone()));
        std::fs::create_dir_all(&stores.claude_config_dir).unwrap();
        std::fs::write(
            stores.claude_credentials_file(),
            claude_oauth("stale").to_string(),
        )
        .unwrap();
        write_claude_identity(&stores, "alice@example.com", "uuid-alice");
        let accounts = stores.list(HarnessId::ClaudeCode).await.unwrap();
        let alice_id = accounts[0].id.clone();
        let request = stores
            .usage_request(HarnessId::ClaudeCode, &alice_id)
            .await
            .unwrap();
        assert!(
            request
                .headers
                .contains(&("Authorization", "Bearer token-alice".to_string()))
        );

        keychain.0.lock().unwrap().item = Some(claude_oauth("token-bob").to_string());
        write_claude_identity(&stores, "bob@example.com", "uuid-bob");
        stores
            .activate(HarnessId::ClaudeCode, &alice_id)
            .await
            .unwrap();
        assert_eq!(
            keychain.item().unwrap()["claudeAiOauth"]["accessToken"],
            "token-alice"
        );
        assert_eq!(
            read_json_file(&stores.claude_credentials_file())["claudeAiOauth"]["accessToken"],
            "stale",
            "the fallback file is left alone while the Keychain holds the login"
        );
    }

    #[tokio::test]
    async fn a_denied_keychain_lists_the_login_but_never_snapshots_or_switches() {
        let tmp = tempfile::tempdir().unwrap();
        let keychain = FakeKeychain::with_item(&claude_oauth("token-alice"));
        let stores = stores(tmp.path(), Some(keychain.clone()));
        write_claude_login(&stores, "alice@example.com", "uuid-alice", "token-alice");
        let alice_id = stores.list(HarnessId::ClaudeCode).await.unwrap()[0]
            .id
            .clone();

        keychain.0.lock().unwrap().item = Some(claude_oauth("token-bob").to_string());
        keychain.0.lock().unwrap().denied = true;
        std::fs::write(
            stores.claude_credentials_file(),
            claude_oauth("stale").to_string(),
        )
        .unwrap();
        write_claude_identity(&stores, "bob@example.com", "uuid-bob");
        let accounts = stores.list(HarnessId::ClaudeCode).await.unwrap();
        assert_eq!(
            labels(&accounts),
            vec![
                ("alice@example.com".to_string(), false),
                ("bob@example.com".to_string(), true),
            ]
        );
        assert_eq!(slot_count(&stores, HarnessId::ClaudeCode), 1);
        let bob_id = id_of(&accounts, "bob@example.com");
        assert_eq!(
            stores
                .usage_request(HarnessId::ClaudeCode, &bob_id)
                .await
                .err()
                .as_deref(),
            Some("denied")
        );

        assert!(
            stores
                .activate(HarnessId::ClaudeCode, &alice_id)
                .await
                .is_err()
        );
        assert_eq!(
            keychain.item().unwrap()["claudeAiOauth"]["accessToken"],
            "token-bob"
        );
    }

    #[tokio::test]
    async fn codex_slot_swap_and_api_key_detection() {
        let tmp = tempfile::tempdir().unwrap();
        let stores = stores(tmp.path(), None);

        write_codex_login(&stores, "carol@example.com", "acct-carol");
        let accounts = stores.list(HarnessId::Codex).await.unwrap();
        assert_eq!(accounts.len(), 1);
        assert_eq!(accounts[0].label, "carol@example.com");
        assert_eq!(accounts[0].plan.as_deref(), Some("ChatGPT Plus"));
        assert!(accounts[0].active);
        let carol_id = accounts[0].id.clone();

        write_codex_login(&stores, "dave@example.com", "acct-dave");
        stores.activate(HarnessId::Codex, &carol_id).await.unwrap();
        let accounts = stores.list(HarnessId::Codex).await.unwrap();
        assert_eq!(
            labels(&accounts),
            vec![
                ("carol@example.com".to_string(), true),
                ("dave@example.com".to_string(), false),
            ]
        );
        let auth = read_json_file(&stores.codex_auth_file());
        assert_eq!(auth["tokens"]["account_id"], "acct-carol");
        #[cfg(unix)]
        assert_eq!(mode(&stores.codex_auth_file()), 0o600);

        let dave_id = id_of(&accounts, "dave@example.com");
        let request = stores
            .usage_request(HarnessId::Codex, &dave_id)
            .await
            .unwrap();
        assert!(
            request
                .headers
                .contains(&("Authorization", "Bearer at-acct-dave".to_string()))
        );
        assert!(
            request
                .headers
                .contains(&("chatgpt-account-id", "acct-dave".to_string()))
        );

        write_codex_auth(&stores, json!({ "OPENAI_API_KEY": "sk-test-12345678abcd" }));
        let accounts = stores.list(HarnessId::Codex).await.unwrap();
        let key_account = accounts.iter().find(|account| account.active).unwrap();
        assert_eq!(key_account.plan.as_deref(), Some("API key"));
        assert_eq!(key_account.label, "API key ·…abcd");
        assert_eq!(
            stores
                .usage_request(HarnessId::Codex, &key_account.id)
                .await
                .err()
                .as_deref(),
            Some("Usage isn't reported for API keys")
        );
    }

    #[tokio::test]
    async fn codex_team_seats_are_distinct() {
        let tmp = tempfile::tempdir().unwrap();
        let stores = stores(tmp.path(), None);
        write_codex_team_login(&stores, "erin@team.com", "user-erin", "ws-team");
        stores.list(HarnessId::Codex).await.unwrap();
        write_codex_team_login(&stores, "finn@team.com", "user-finn", "ws-team");
        let accounts = stores.list(HarnessId::Codex).await.unwrap();
        assert_eq!(
            labels(&accounts),
            vec![
                ("erin@team.com".to_string(), false),
                ("finn@team.com".to_string(), true),
            ]
        );
    }

    #[tokio::test]
    async fn an_unrecognized_codex_login_is_never_overwritten() {
        let tmp = tempfile::tempdir().unwrap();
        let stores = stores(tmp.path(), None);
        write_codex_login(&stores, "carol@example.com", "acct-carol");
        let carol_id = stores.list(HarnessId::Codex).await.unwrap()[0].id.clone();
        write_codex_auth(&stores, json!({ "something": "else" }));
        assert!(stores.activate(HarnessId::Codex, &carol_id).await.is_err());
        assert_eq!(
            read_json_file(&stores.codex_auth_file()),
            json!({ "something": "else" })
        );
    }

    #[tokio::test]
    async fn remove_guards_and_removes_saved_slots_only() {
        let tmp = tempfile::tempdir().unwrap();
        let stores = stores(tmp.path(), None);
        write_claude_login(&stores, "alice@example.com", "uuid-alice", "token-alice");
        let alice_id = stores.list(HarnessId::ClaudeCode).await.unwrap()[0]
            .id
            .clone();

        assert!(
            stores
                .remove(HarnessId::ClaudeCode, "../../evil")
                .await
                .is_err()
        );
        assert!(
            stores
                .remove(HarnessId::ClaudeCode, "ABCDEF0123456789")
                .await
                .is_err()
        );
        assert!(
            stores
                .remove(HarnessId::ClaudeCode, &alice_id)
                .await
                .is_err(),
            "the live login is never removed"
        );

        write_claude_login(&stores, "bob@example.com", "uuid-bob", "token-bob");
        stores
            .remove(HarnessId::ClaudeCode, &alice_id)
            .await
            .unwrap();
        let accounts = stores.list(HarnessId::ClaudeCode).await.unwrap();
        assert_eq!(
            labels(&accounts),
            vec![("bob@example.com".to_string(), true)]
        );
        assert!(
            read_json_file(&stores.claude_config_file)
                .get("oauthAccount")
                .is_some()
        );
        assert!(
            read_json_file(&stores.claude_credentials_file())
                .get("claudeAiOauth")
                .is_some()
        );
    }

    #[test]
    fn compose_claude_credentials_live_mcp_wins_over_stale_slot() {
        let target = json!({
            "claudeAiOauth": { "accessToken": "alice" },
            "trustedDeviceToken": "alice-device",
            "mcpOAuth": { "github": { "accessToken": "stale" } },
            "pluginSecrets": { "old": true },
        });
        let live = json!({
            "claudeAiOauth": { "accessToken": "bob" },
            "trustedDeviceToken": "bob-device",
            "mcpOAuth": { "github": { "accessToken": "live" } },
            "pluginSecrets": { "live": true },
        });
        let composed = compose_claude_credentials(&target, Some(&live));
        assert_eq!(composed["claudeAiOauth"]["accessToken"], "alice");
        assert_eq!(composed["trustedDeviceToken"], "alice-device");
        assert_eq!(composed["mcpOAuth"]["github"]["accessToken"], "live");
        assert_eq!(composed["pluginSecrets"]["live"], true);
        assert!(composed["pluginSecrets"].get("old").is_none());
    }

    #[test]
    fn compose_claude_credentials_does_not_resurrect_absent_live_mcp() {
        let target = json!({
            "claudeAiOauth": { "accessToken": "alice" },
            "mcpOAuth": { "github": { "accessToken": "stale" } },
        });
        let live = json!({ "claudeAiOauth": { "accessToken": "bob" } });
        let composed = compose_claude_credentials(&target, Some(&live));
        assert_eq!(composed["claudeAiOauth"]["accessToken"], "alice");
        assert!(composed.get("mcpOAuth").is_none());
    }

    #[test]
    fn compose_claude_credentials_passthrough_without_live_or_oauth() {
        let target = json!({
            "claudeAiOauth": { "accessToken": "alice" },
            "mcpOAuth": { "github": { "accessToken": "slot" } },
        });
        assert_eq!(compose_claude_credentials(&target, None), target);

        let api_key = json!({ "apiKey": "sk-x" });
        let live = json!({
            "claudeAiOauth": { "accessToken": "bob" },
            "mcpOAuth": { "github": { "accessToken": "live" } },
        });
        assert_eq!(compose_claude_credentials(&api_key, Some(&live)), api_key);
    }

    #[cfg(unix)]
    #[test]
    fn secret_writes_are_exclusive_owner_only_and_never_follow_symlinks() {
        use std::os::unix::fs::PermissionsExt;
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path();
        let target = dir.join("auth.json");
        std::fs::write(&target, "old").unwrap();
        std::fs::set_permissions(&target, std::fs::Permissions::from_mode(0o644)).unwrap();
        write_file_atomic(&target, b"secret", true).unwrap();
        assert_eq!(std::fs::read_to_string(&target).unwrap(), "secret");
        assert_eq!(mode(&target), 0o600);

        let victim = dir.join("victim");
        std::fs::write(&victim, "keep").unwrap();
        let link = dir.join("link.json");
        std::os::unix::fs::symlink(&victim, &link).unwrap();
        write_file_atomic(&link, b"secret", true).unwrap();
        assert_eq!(std::fs::read_to_string(&victim).unwrap(), "keep");
        assert!(
            !std::fs::symlink_metadata(&link)
                .unwrap()
                .file_type()
                .is_symlink()
        );
        assert_eq!(mode(&link), 0o600);

        let names: Vec<String> = std::fs::read_dir(dir)
            .unwrap()
            .flatten()
            .map(|entry| entry.file_name().to_string_lossy().into_owned())
            .collect();
        assert!(
            names.iter().all(|name| !name.ends_with(".tmp")),
            "{names:?}"
        );

        let config = dir.join("config.json");
        std::fs::write(&config, "{}").unwrap();
        std::fs::set_permissions(&config, std::fs::Permissions::from_mode(0o640)).unwrap();
        write_file_atomic(&config, b"{\"a\":1}", false).unwrap();
        assert_eq!(mode(&config), 0o640);
    }
}

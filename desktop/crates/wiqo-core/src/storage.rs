//! What the desktop keeps on this machine: who it is, who it is paired with, its tokens and its switches. Plain JSON files in one folder, written
//! atomically and readable by the owner only; the tokens and the phones' keys go to the operating system keychain when there is one (vault.rs).

use std::collections::BTreeSet;
use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, RwLock};

use serde::{de::DeserializeOwned, Deserialize, Serialize};

use crate::vault::Vault;

#[derive(Debug, Clone)]
pub struct DataDir(PathBuf);

impl DataDir {
    pub fn new(path: impl Into<PathBuf>) -> std::io::Result<DataDir> {
        let path = path.into();
        fs::create_dir_all(&path)?;
        Ok(DataDir(path))
    }

    /// `WIQO_DATA_DIR`, or the platform's application data folder.
    pub fn default_location() -> std::io::Result<DataDir> {
        let path = std::env::var_os("WIQO_DATA_DIR")
            .map(PathBuf::from)
            .or_else(|| dirs::data_dir().map(|dir| dir.join("Wiqo Desktop")))
            .unwrap_or_else(|| PathBuf::from(".wiqo"));
        DataDir::new(path)
    }

    pub fn file(&self, name: &str) -> PathBuf {
        self.0.join(name)
    }
}

fn read_json<T: DeserializeOwned>(path: &Path) -> Option<T> {
    serde_json::from_slice(&fs::read(path).ok()?).ok()
}

pub(crate) fn write_json<T: Serialize>(path: &Path, value: &T) -> std::io::Result<()> {
    let bytes = serde_json::to_vec_pretty(value).map_err(std::io::Error::other)?;
    let temporary = path.with_extension("tmp");
    let mut file = fs::File::create(&temporary)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        file.set_permissions(fs::Permissions::from_mode(0o600))?;
    }
    file.write_all(&bytes)?;
    file.sync_all()?;
    fs::rename(&temporary, path)
}

/// Who this desktop is to the server. Made once, kept for good.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Identity {
    pub device_id: String,
    pub display_name: String,
}

impl Identity {
    pub fn load_or_create(dir: &DataDir, display_name: &str) -> std::io::Result<Identity> {
        let path = dir.file("device.json");
        if let Some(identity) = read_json::<Identity>(&path) {
            return Ok(identity);
        }
        // Upper case, the way every other Wiqo app writes a UUID (Swift's `uuidString`): the phone sends commands to `/devices/<ID>/command` with
        // that spelling, and the server matches device ids exactly.
        let identity = Identity {
            device_id: uuid::Uuid::new_v4().to_string().to_uppercase(),
            display_name: display_name.to_string(),
        };
        write_json(&path, &identity)?;
        Ok(identity)
    }
}

/// The tokens the server gave this desktop when a phone paired it.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Credentials {
    pub api_base: String,
    pub access_token: String,
    pub refresh_token: String,
    /// When the access token stops working (seconds since the epoch).
    pub access_expires_at: i64,
}

pub struct CredentialStore {
    path: PathBuf,
    value: Mutex<Option<Credentials>>,
    vault: Option<Arc<Vault>>,
}

impl CredentialStore {
    pub fn open(dir: &DataDir) -> CredentialStore {
        let path = dir.file("credentials.json");
        CredentialStore {
            value: Mutex::new(read_json(&path)),
            path,
            vault: None,
        }
    }

    /// Tokens in the keychain. A token file left by an earlier version moves there and is removed.
    pub fn open_with(dir: &DataDir, vault: Arc<Vault>) -> CredentialStore {
        if !vault.enabled() {
            return CredentialStore::open(dir);
        }
        let path = dir.file("credentials.json");
        let mut value = vault.load().credentials;
        if let Some(old) = read_json::<Credentials>(&path) {
            if value.is_none()
                && vault
                    .update(|secrets| secrets.credentials = Some(old.clone()))
                    .is_ok()
            {
                value = Some(old);
            }
            if value.is_some() {
                let _ = fs::remove_file(&path);
            }
        }
        CredentialStore {
            path,
            value: Mutex::new(value),
            vault: Some(vault),
        }
    }

    pub fn get(&self) -> Option<Credentials> {
        self.value.lock().expect("credentials lock").clone()
    }

    /// Forgets the device session (signing out).
    pub fn clear(&self) -> std::io::Result<()> {
        match &self.vault {
            Some(vault) => vault.update(|secrets| secrets.credentials = None)?,
            None => {
                let _ = fs::remove_file(&self.path);
            }
        }
        *self.value.lock().expect("credentials lock") = None;
        Ok(())
    }

    pub fn set(&self, credentials: Credentials) -> std::io::Result<()> {
        match &self.vault {
            Some(vault) => {
                vault.update(|secrets| secrets.credentials = Some(credentials.clone()))?
            }
            None => write_json(&self.path, &credentials)?,
        }
        *self.value.lock().expect("credentials lock") = Some(credentials);
        Ok(())
    }
}

/// A phone this desktop is paired with and the key that phone signs its commands with.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PairedPhone {
    pub pairing_id: String,
    pub name: String,
    pub platform: String,
    pub token: String,
}

pub struct PairedPhones {
    path: PathBuf,
    phones: RwLock<Vec<PairedPhone>>,
    vault: Option<Arc<Vault>>,
}

impl PairedPhones {
    pub fn open(dir: &DataDir) -> PairedPhones {
        let path = dir.file("phones.json");
        PairedPhones {
            phones: RwLock::new(read_json(&path).unwrap_or_default()),
            path,
            vault: None,
        }
    }

    /// The phones' keys in the keychain; the file keeps names only. Keys left in the file by an earlier version move to the keychain.
    pub fn open_with(dir: &DataDir, vault: Arc<Vault>) -> PairedPhones {
        if !vault.enabled() {
            return PairedPhones::open(dir);
        }
        let path = dir.file("phones.json");
        let mut phones: Vec<PairedPhone> = read_json(&path).unwrap_or_default();
        let found: Vec<(String, String)> = phones
            .iter()
            .filter(|phone| !phone.token.is_empty())
            .map(|phone| (phone.pairing_id.clone(), phone.token.clone()))
            .collect();
        let moved = found.is_empty()
            || vault
                .update(|secrets| secrets.phone_tokens.extend(found.clone()))
                .is_ok();
        let tokens = vault.load().phone_tokens;
        for phone in &mut phones {
            if let Some(token) = tokens.get(&phone.pairing_id) {
                phone.token = token.clone();
            }
        }
        let store = PairedPhones {
            path,
            phones: RwLock::new(phones),
            vault: Some(vault),
        };
        if moved {
            let _ = store.save(&store.list());
        }
        store
    }

    fn save(&self, phones: &[PairedPhone]) -> std::io::Result<()> {
        match &self.vault {
            Some(vault) => {
                let tokens: std::collections::BTreeMap<String, String> = phones
                    .iter()
                    .map(|phone| (phone.pairing_id.clone(), phone.token.clone()))
                    .collect();
                vault.update(|secrets| secrets.phone_tokens = tokens)?;
                let names: Vec<PairedPhone> = phones
                    .iter()
                    .map(|phone| PairedPhone {
                        token: String::new(),
                        ..phone.clone()
                    })
                    .collect();
                write_json(&self.path, &names)
            }
            None => write_json(&self.path, &phones),
        }
    }

    pub fn list(&self) -> Vec<PairedPhone> {
        self.phones.read().expect("phones lock").clone()
    }

    /// Adds a phone, or replaces the key of one already paired under the same id.
    pub fn add(&self, phone: PairedPhone) -> std::io::Result<()> {
        let mut phones = self.phones.write().expect("phones lock");
        phones.retain(|existing| existing.pairing_id != phone.pairing_id);
        phones.push(phone);
        self.save(&phones)
    }

    pub fn remove(&self, pairing_id: &str) -> std::io::Result<bool> {
        let mut phones = self.phones.write().expect("phones lock");
        let before = phones.len();
        phones.retain(|phone| phone.pairing_id != pairing_id);
        self.save(&phones)?;
        Ok(phones.len() != before)
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SettingsData {
    /// The owner allows the server's PC agent to run commands here without a phone confirmation each time. Off until the owner turns it on.
    #[serde(default)]
    pub full_access: bool,
    /// Capabilities (contracts/companion/operations.json) the owner switched off.
    #[serde(default)]
    pub disabled_capabilities: BTreeSet<String>,
    /// The server's agent waits for a click on this computer before anything that changes or deletes (off: the phone's approval is enough).
    #[serde(default)]
    pub approve_on_pc: bool,
    /// The server's agent may read in the own folders without the phone in the loop (grants.rs `read_without_phone`). On unless turned off.
    #[serde(default = "on")]
    pub reads_without_phone: bool,
}

fn on() -> bool {
    true
}

impl Default for SettingsData {
    fn default() -> Self {
        SettingsData {
            full_access: false,
            disabled_capabilities: BTreeSet::new(),
            approve_on_pc: false,
            reads_without_phone: true,
        }
    }
}

pub struct Settings {
    path: PathBuf,
    data: RwLock<SettingsData>,
}

impl Settings {
    pub fn open(dir: &DataDir) -> Settings {
        let path = dir.file("settings.json");
        Settings {
            data: RwLock::new(read_json(&path).unwrap_or_default()),
            path,
        }
    }

    pub fn snapshot(&self) -> SettingsData {
        self.data.read().expect("settings lock").clone()
    }

    pub fn full_access(&self) -> bool {
        self.data.read().expect("settings lock").full_access
    }

    pub fn reads_without_phone(&self) -> bool {
        self.data.read().expect("settings lock").reads_without_phone
    }

    pub fn approve_on_pc(&self) -> bool {
        self.data.read().expect("settings lock").approve_on_pc
    }

    pub fn capability_enabled(&self, capability: &str) -> bool {
        !self
            .data
            .read()
            .expect("settings lock")
            .disabled_capabilities
            .contains(capability)
    }

    pub fn update(&self, change: impl FnOnce(&mut SettingsData)) -> std::io::Result<()> {
        let mut data = self.data.write().expect("settings lock");
        change(&mut data);
        write_json(&self.path, &*data)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_dir(name: &str) -> DataDir {
        DataDir::new(
            std::env::temp_dir().join(format!("wiqo-test-{name}-{}", uuid::Uuid::new_v4())),
        )
        .unwrap()
    }

    #[test]
    fn identity_is_made_once_and_kept() {
        let dir = temp_dir("identity");
        let first = Identity::load_or_create(&dir, "Ofis PC").unwrap();
        let second = Identity::load_or_create(&dir, "başka ad").unwrap();
        assert_eq!(first.device_id, second.device_id);
        assert_eq!(second.display_name, "Ofis PC");
        assert_eq!(
            first.device_id,
            first.device_id.to_uppercase(),
            "phones address devices by the upper-case UUID"
        );
    }

    #[test]
    fn phones_and_settings_survive_a_restart() {
        let dir = temp_dir("state");
        let phones = PairedPhones::open(&dir);
        phones
            .add(PairedPhone {
                pairing_id: "a".into(),
                name: "iPhone".into(),
                platform: "ios".into(),
                token: "t1".into(),
            })
            .unwrap();
        phones
            .add(PairedPhone {
                pairing_id: "a".into(),
                name: "iPhone".into(),
                platform: "ios".into(),
                token: "t2".into(),
            })
            .unwrap();
        let settings = Settings::open(&dir);
        settings
            .update(|data| {
                data.full_access = true;
                data.disabled_capabilities
                    .insert("device.files.delete".into());
            })
            .unwrap();

        let reopened = PairedPhones::open(&dir);
        assert_eq!(reopened.list().len(), 1);
        assert_eq!(reopened.list()[0].token, "t2");
        assert!(reopened.remove("a").unwrap());
        assert!(PairedPhones::open(&dir).list().is_empty());
        let settings = Settings::open(&dir);
        assert!(settings.full_access());
        assert!(!settings.capability_enabled("device.files.delete"));
        assert!(settings.capability_enabled("device.files.read"));
    }
}

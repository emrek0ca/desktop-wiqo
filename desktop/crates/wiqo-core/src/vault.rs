//! Where the secrets live: the access and refresh tokens and every paired phone's signing key. In the operating system's keychain (macOS Keychain,
//! Windows Credential Manager, the Secret Service on Linux) as one item (on Windows split into parts, see `parts`), so the app asks at most once; in the data folder's files only when the
//! keychain cannot be used (a test's own data folder, a Linux without a keyring).

use std::collections::BTreeMap;
use std::sync::Mutex;

use serde::{Deserialize, Serialize};

use crate::storage::Credentials;

const SERVICE: &str = "com.wiqo.desktop";
const ACCOUNT: &str = "secrets";

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Secrets {
    pub credentials: Option<Credentials>,
    /// Pairing id → the phone's signing key.
    pub phone_tokens: BTreeMap<String, String>,
    /// The person's own Wiqo account session for the chat in the window.
    #[serde(default)]
    pub account: Option<Account>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Account {
    pub email: String,
    pub token: String,
}

/// Reads and writes the one keychain item. Writes are serialized, so two stores updating at once never drop each other's change.
pub struct Vault {
    enabled: bool,
    cache: Mutex<Option<Secrets>>,
}

impl Vault {
    /// The keychain, or nothing (`enabled = false`: the stores keep their secrets in their own files).
    pub fn new(enabled: bool) -> Vault {
        let vault = Vault {
            enabled,
            cache: Mutex::new(None),
        };
        if enabled && vault.read().is_none() && vault.write(&Secrets::default()).is_err() {
            tracing::warn!("keychain unavailable; secrets stay in the data folder");
            return Vault {
                enabled: false,
                cache: Mutex::new(None),
            };
        }
        vault
    }

    pub fn enabled(&self) -> bool {
        self.enabled
    }

    #[cfg(any(target_os = "macos", target_os = "linux"))]
    fn entry() -> Option<keyring::Entry> {
        keyring::Entry::new(SERVICE, ACCOUNT).ok()
    }

    fn read(&self) -> Option<Secrets> {
        if !self.enabled {
            return None;
        }
        #[cfg(windows)]
        {
            serde_json::from_slice(&parts::read()?).ok()
        }
        #[cfg(any(target_os = "macos", target_os = "linux"))]
        {
            let text = Self::entry()?.get_password().ok()?;
            serde_json::from_str(&text).ok()
        }
        #[cfg(not(any(target_os = "macos", windows, target_os = "linux")))]
        None
    }

    fn write(&self, secrets: &Secrets) -> Result<(), String> {
        #[cfg(windows)]
        {
            let text = serde_json::to_vec(secrets).map_err(|error| error.to_string())?;
            parts::write(&text)
        }
        #[cfg(any(target_os = "macos", target_os = "linux"))]
        {
            let text = serde_json::to_string(secrets).map_err(|error| error.to_string())?;
            Self::entry()
                .ok_or("no keychain")?
                .set_password(&text)
                .map_err(|error| error.to_string())
        }
        #[cfg(not(any(target_os = "macos", windows, target_os = "linux")))]
        {
            let _ = secrets;
            Err("no keychain".into())
        }
    }

    pub fn load(&self) -> Secrets {
        let mut cache = self.cache.lock().expect("vault lock");
        if cache.is_none() {
            *cache = Some(self.read().unwrap_or_default());
        }
        cache.clone().unwrap_or_default()
    }

    pub fn update(&self, change: impl FnOnce(&mut Secrets)) -> std::io::Result<()> {
        let mut cache = self.cache.lock().expect("vault lock");
        let mut secrets = cache
            .clone()
            .unwrap_or_else(|| self.read().unwrap_or_default());
        change(&mut secrets);
        self.write(&secrets).map_err(std::io::Error::other)?;
        *cache = Some(secrets);
        Ok(())
    }
}

/// Windows Credential Manager holds at most 2560 bytes per item, less than the tokens and phone keys together: the secrets go in numbered
/// items ("secrets.1", "secrets.2", …) and the "secrets" item names how many there are ("parts:3").
#[cfg(windows)]
mod parts {
    use super::{ACCOUNT, SERVICE};

    const PART: usize = 2048;
    const HEADER: &str = "parts:";

    fn entry(name: &str) -> Option<keyring::Entry> {
        keyring::Entry::new(SERVICE, name).ok()
    }

    fn count(entry: &keyring::Entry) -> Option<usize> {
        let header = entry.get_secret().ok()?;
        std::str::from_utf8(&header)
            .ok()?
            .strip_prefix(HEADER)?
            .parse()
            .ok()
    }

    pub fn read() -> Option<Vec<u8>> {
        let count = count(&entry(ACCOUNT)?)?;
        let mut bytes = Vec::new();
        for index in 1..=count {
            bytes.extend(entry(&format!("{ACCOUNT}.{index}"))?.get_secret().ok()?);
        }
        Some(bytes)
    }

    pub fn write(bytes: &[u8]) -> Result<(), String> {
        let head = entry(ACCOUNT).ok_or("no keychain")?;
        let before = count(&head).unwrap_or(0);
        let chunks: Vec<&[u8]> = bytes.chunks(PART).collect();
        for (index, chunk) in chunks.iter().enumerate() {
            entry(&format!("{ACCOUNT}.{}", index + 1))
                .ok_or("no keychain")?
                .set_secret(chunk)
                .map_err(|error| error.to_string())?;
        }
        head.set_secret(format!("{HEADER}{}", chunks.len()).as_bytes())
            .map_err(|error| error.to_string())?;
        for index in chunks.len() + 1..=before {
            if let Some(stale) = entry(&format!("{ACCOUNT}.{index}")) {
                let _ = stale.delete_credential();
            }
        }
        Ok(())
    }
}

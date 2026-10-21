use std::collections::HashMap;
use std::sync::OnceLock;

use serde::Deserialize;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Deserialize)]
pub enum Platform {
    #[serde(rename = "macOS")]
    MacOs,
    #[serde(rename = "windows")]
    Windows,
    #[serde(rename = "linux")]
    Linux,
}

impl Platform {
    /// The platform this build runs on.
    pub fn current() -> Platform {
        if cfg!(target_os = "macos") {
            Platform::MacOs
        } else if cfg!(target_os = "windows") {
            Platform::Windows
        } else {
            Platform::Linux
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum TimeoutClass {
    Short,
    Medium,
    Long,
}

impl TimeoutClass {
    /// How long a desktop may work on one command. The server waits 20 s for ordinary operations and 65 s for long ones, so the desktop always answers
    /// (with `COMMAND_TIMEOUT`) before the server gives up and reports a lost result.
    pub fn budget(self) -> std::time::Duration {
        std::time::Duration::from_secs(match self {
            TimeoutClass::Short => 10,
            TimeoutClass::Medium => 15,
            TimeoutClass::Long => 55,
        })
    }
}

/// One thing a phone or the server can ask a desktop to do.
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Operation {
    pub operation: String,
    pub payload_kind: String,
    pub capability: String,
    pub authority: String,
    pub is_destructive: bool,
    pub requires_user_presence: bool,
    pub timeout_class: TimeoutClass,
    pub verification_rule: String,
    pub supported_platforms: Vec<Platform>,
}

#[derive(Debug, Deserialize)]
struct File {
    version: u32,
    operations: Vec<Operation>,
}

/// `contracts/companion/operations.json`, parsed once.
#[derive(Debug)]
pub struct Manifest {
    pub version: u32,
    operations: Vec<Operation>,
    by_name: HashMap<String, usize>,
}

impl Manifest {
    pub fn get() -> &'static Manifest {
        static MANIFEST: OnceLock<Manifest> = OnceLock::new();
        MANIFEST.get_or_init(|| {
            Manifest::parse(crate::OPERATIONS_JSON).expect("operations.json is valid")
        })
    }

    pub fn parse(text: &str) -> Result<Manifest, serde_json::Error> {
        let file: File = serde_json::from_str(text)?;
        let by_name = file
            .operations
            .iter()
            .enumerate()
            .map(|(index, operation)| (operation.operation.clone(), index))
            .collect();
        Ok(Manifest {
            version: file.version,
            operations: file.operations,
            by_name,
        })
    }

    pub fn operations(&self) -> &[Operation] {
        &self.operations
    }

    pub fn operation(&self, name: &str) -> Option<&Operation> {
        self.by_name.get(name).map(|&index| &self.operations[index])
    }

    /// What this platform may be asked to do: the only list a desktop reports as its abilities.
    pub fn for_platform(&self, platform: Platform) -> impl Iterator<Item = &Operation> {
        self.operations
            .iter()
            .filter(move |operation| operation.supported_platforms.contains(&platform))
    }

    pub fn supports(&self, name: &str, platform: Platform) -> bool {
        self.operation(name)
            .is_some_and(|operation| operation.supported_platforms.contains(&platform))
    }
}

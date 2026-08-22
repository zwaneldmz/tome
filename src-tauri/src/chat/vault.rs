//! The chat-key vault (plan §4.3): every pasted provider API key in ONE
//! OS-keychain item (`tech.abantu.tome` / `chat-keys` — the same service
//! `authlock.rs` already uses for its TOTP secret) as a single JSON map
//! `{provider-id → key}`, with a 0600 `dir/chat-secrets.json` file as the
//! fallback when no keychain is usable. Keychain first, file second —
//! the exact ladder Cursor's companion CLI and Zed ship (plan Part 2).
//! Slice 2 loads this once at unlock into `AppState.chat_keys` and
//! re-loads after every `chat_key_set`; there is deliberately no
//! read-back command (write-only keys, Cursor's contract).
//!
//! ## Why one blob instead of one keychain item per provider
//!
//! `tauri.conf.json` sets macOS `signingIdentity: null` — builds are
//! ad-hoc signed, so the cdhash changes on every rebuild, and macOS binds
//! a keychain item's ACL to the creating process's code signature. N
//! items would mean N "allow access?" authorization prompts per rebuild
//! (one per provider, on every dev build); one item means at most one.
//! One blob is also one atomic unit for save/load — no window where half
//! the keys moved and half didn't.
//!
//! ## The `SecretIo` seam
//!
//! Mirrors `authlock.rs`'s `SecretProtector`: the production impl
//! collapses every `keyring` error into "unavailable" (a locked keychain,
//! a missing Secret Service, or any other failure is indistinguishable
//! from "no keychain" — fall back to the file, never crash), and tests
//! inject a fake via the private `Vault::with_io` so the real keychain
//! never runs under `cargo test` (it would prompt and write to the
//! developer's own login keychain).

// Landed ahead of its consumer: `state.rs`/`ipc::chat` (the slice-2
// rewrite) are this module's production callers; until then only its own
// tests exercise it. Same transitional allow as confine.rs/pty.rs carried.
#![allow(dead_code)]

use std::collections::HashMap;
use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};

/// Same keychain service `authlock.rs` uses for its TOTP item — one
/// service, two accounts (`totp`, `chat-keys`), so both credentials show
/// up under the same app name in Keychain Access.
const KEYRING_SERVICE: &str = "tech.abantu.tome";
const KEYRING_ACCOUNT: &str = "chat-keys";

/// The no-keychain fallback file (0600, `dir`-relative — the same write
/// shape `store.rs` uses for every other store file).
const FILE_NAME: &str = "chat-secrets.json";

/// The one side effect this module knows how to perform: read or write
/// the single keychain blob. Kept a trait (not inline calls) purely as
/// the test seam — see the module doc comment. `pub(crate)` so
/// `chat::migrate`'s tests can inject the same fake (both run under
/// `cargo test`, where the real keychain must never be touched).
pub(crate) trait SecretIo: Send + Sync {
    fn get(&self) -> Option<String>;
    fn set(&self, secret: &str) -> bool;
}

/// Production [`SecretIo`]: the OS credential store via the `keyring`
/// crate, collapsing every error into "unavailable" — the same posture
/// `authlock.rs` ships for TOTP (`KeyringProtector`).
struct KeyringIo;

impl SecretIo for KeyringIo {
    fn get(&self) -> Option<String> {
        keyring::Entry::new(KEYRING_SERVICE, KEYRING_ACCOUNT)
            .ok()?
            .get_password()
            .ok()
    }

    fn set(&self, secret: &str) -> bool {
        keyring::Entry::new(KEYRING_SERVICE, KEYRING_ACCOUNT)
            .and_then(|e| e.set_password(secret))
            .is_ok()
    }
}

/// Where the keys returned by [`Vault::load`] (or written by
/// [`Vault::save`]) physically live. Reported to the UI so a provider
/// card can say "key stored in the keychain" vs "key stored in a file"
/// without ever reading the key back.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    Keychain,
    File,
}

/// The one-blob key store: `dir` locates the fallback file, `io` is the
/// keychain. Not `Clone` (holds a `Box<dyn SecretIo>`); wrap in whatever
/// synchronization the integrator needs (slice 2: a `RwLock` in
/// `AppState`).
pub struct Vault {
    dir: PathBuf,
    io: Box<dyn SecretIo>,
}

impl Vault {
    /// Production constructor: the real OS keychain behind [`KeyringIo`].
    pub fn new(dir: impl AsRef<Path>) -> Self {
        Self::with_io(dir, Box::new(KeyringIo))
    }

    /// Test-only seam: like [`Self::new`], but with an injected
    /// [`SecretIo`] instead of the real OS keychain. Not `pub` — nothing
    /// outside this module's own tests and `chat::migrate`'s tests should
    /// construct a `Vault` with a fake (same discipline as `authlock.rs`'s
    /// `load_with_protector`).
    pub(crate) fn with_io(dir: impl AsRef<Path>, io: Box<dyn SecretIo>) -> Self {
        Vault {
            dir: dir.as_ref().to_path_buf(),
            io,
        }
    }

    /// The whole key map plus where it came from. Keychain first: a
    /// keyring string that parses as a JSON object map wins. A keyring
    /// that's unavailable, or holding a string that doesn't parse (say a
    /// hand-edited or foreign item), falls to `dir/chat-secrets.json`;
    /// a missing or corrupt file yields an empty map. Either file-path
    /// outcome reports `Kind::File`.
    pub fn load(&self) -> (HashMap<String, String>, Kind) {
        if let Some(blob) = self.io.get() {
            if let Ok(map) = serde_json::from_str::<HashMap<String, String>>(&blob) {
                return (map, Kind::Keychain);
            }
        }
        (self.read_file(), Kind::File)
    }

    /// Serialize and persist the whole map. Keychain first; a failed
    /// keyring write falls to `dir/chat-secrets.json` (parent dirs
    /// created, 0600 — `store.rs`'s write shape). Only a file-write
    /// failure returns `Err`.
    pub fn save(&self, map: &HashMap<String, String>) -> Result<Kind, String> {
        let blob = serde_json::to_string_pretty(map).map_err(|e| e.to_string())?;
        if self.io.set(&blob) {
            return Ok(Kind::Keychain);
        }
        fs::create_dir_all(&self.dir).map_err(|e| e.to_string())?;
        let path = self.dir.join(FILE_NAME);
        fs::write(&path, blob).map_err(|e| e.to_string())?;
        fs::set_permissions(&path, fs::Permissions::from_mode(0o600)).map_err(|e| e.to_string())?;
        Ok(Kind::File)
    }

    /// Missing or corrupt file → empty map, never an error: an unreadable
    /// fallback behaves like "no keys", which `registry::resolve` turns
    /// into `NoKey` — the honest answer, not a crash.
    fn read_file(&self) -> HashMap<String, String> {
        fs::read_to_string(self.dir.join(FILE_NAME))
            .ok()
            .and_then(|text| serde_json::from_str(&text).ok())
            .unwrap_or_default()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;
    use tempfile::tempdir;

    /// Keychain-shaped fake: an in-memory slot behind a `Mutex` (the real
    /// keyring is process-global state, and tests run in parallel).
    struct MemoryIo(Mutex<Option<String>>);

    impl MemoryIo {
        fn empty() -> Self {
            MemoryIo(Mutex::new(None))
        }

        fn with(s: &str) -> Self {
            MemoryIo(Mutex::new(Some(s.to_string())))
        }
    }

    impl SecretIo for MemoryIo {
        fn get(&self) -> Option<String> {
            self.0.lock().unwrap().clone()
        }

        fn set(&self, secret: &str) -> bool {
            *self.0.lock().unwrap() = Some(secret.to_string());
            true
        }
    }

    /// No-keychain fake (`get` → `None`, `set` → `false`): a headless
    /// Linux box with no Secret Service, or any keyring error at all.
    struct UnavailableIo;

    impl SecretIo for UnavailableIo {
        fn get(&self) -> Option<String> {
            None
        }

        fn set(&self, _secret: &str) -> bool {
            false
        }
    }

    fn keys(pairs: &[(&str, &str)]) -> HashMap<String, String> {
        pairs
            .iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect()
    }

    #[test]
    fn save_via_file_round_trips_and_writes_0600() {
        let dir = tempdir().unwrap();
        let vault = Vault::with_io(dir.path(), Box::new(UnavailableIo));
        let map = keys(&[("glm", "z-key"), ("claude", "a-key")]);

        assert_eq!(vault.save(&map).unwrap(), Kind::File);
        assert_eq!(vault.load(), (map.clone(), Kind::File));

        let mode = fs::metadata(dir.path().join(FILE_NAME))
            .unwrap()
            .permissions()
            .mode();
        assert_eq!(mode & 0o777, 0o600);
    }

    #[test]
    fn keychain_round_trips_through_a_working_keyring_without_touching_the_file() {
        let dir = tempdir().unwrap();
        let vault = Vault::with_io(dir.path(), Box::new(MemoryIo::empty()));
        let map = keys(&[("kimi", "m-key")]);

        assert_eq!(vault.save(&map).unwrap(), Kind::Keychain);
        assert!(
            !dir.path().join(FILE_NAME).exists(),
            "keyring took it; no file"
        );
        assert_eq!(vault.load(), (map, Kind::Keychain));
    }

    #[test]
    fn a_corrupt_keychain_blob_falls_back_to_the_file() {
        let dir = tempdir().unwrap();
        // good data already on disk via the file path
        let map = keys(&[("glm", "z-key")]);
        Vault::with_io(dir.path(), Box::new(UnavailableIo))
            .save(&map)
            .unwrap();

        // keyring present but holding a non-map string
        let vault = Vault::with_io(dir.path(), Box::new(MemoryIo::with("not json at all")));
        assert_eq!(vault.load(), (map, Kind::File));
    }

    #[test]
    fn a_corrupt_file_with_no_keychain_yields_an_empty_map() {
        let dir = tempdir().unwrap();
        fs::create_dir_all(dir.path()).unwrap();
        fs::write(dir.path().join(FILE_NAME), "{corrupt").unwrap();

        let vault = Vault::with_io(dir.path(), Box::new(UnavailableIo));
        assert_eq!(vault.load(), (HashMap::new(), Kind::File));
    }

    #[test]
    fn a_missing_file_with_no_keychain_yields_an_empty_map() {
        let dir = tempdir().unwrap();
        let vault = Vault::with_io(dir.path(), Box::new(UnavailableIo));
        assert_eq!(vault.load(), (HashMap::new(), Kind::File));
    }

    #[test]
    fn save_via_file_creates_missing_parent_directories() {
        let dir = tempdir().unwrap();
        let nested = dir.path().join("a/b");
        let vault = Vault::with_io(&nested, Box::new(UnavailableIo));
        let map = keys(&[("glm", "k")]);

        assert_eq!(vault.save(&map).unwrap(), Kind::File);
        assert!(nested.join(FILE_NAME).exists());
        assert_eq!(vault.load(), (map, Kind::File));
    }
}

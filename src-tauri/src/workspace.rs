use chacha20poly1305::{
    aead::{Aead, KeyInit},
    XChaCha20Poly1305, XNonce,
};
use keyring::Entry;
use rand::{rngs::OsRng, RngCore};
use std::{fs, path::PathBuf, sync::Mutex};
use tauri::{AppHandle, Manager};

const KEYRING_SERVICE: &str = "com.friday.assistant";
const KEYRING_ACCOUNT: &str = "local-workspace-encryption-key";
const NOTE_FILE: &str = "private-note.enc";

// Serialize access to the same credential while creating or retrieving the key.
static KEYRING_LOCK: Mutex<()> = Mutex::new(());

fn workspace_dir(app: &AppHandle) -> Result<PathBuf, String> {
    let base = app
        .path()
        .app_data_dir()
        .map_err(|e| format!("Cannot locate this user's application data folder: {e}"))?;
    Ok(base.join("workspace"))
}

pub(crate) fn encryption_key() -> Result<[u8; 32], String> {
    let _guard = KEYRING_LOCK
        .lock()
        .map_err(|_| "Workspace key lock is unavailable.".to_string())?;

    let entry = Entry::new(KEYRING_SERVICE, KEYRING_ACCOUNT)
        .map_err(|e| format!("Windows credential storage is unavailable: {e}"))?;

    match entry.get_secret() {
        Ok(bytes) => bytes
            .try_into()
            .map_err(|_| "Stored workspace key has an invalid length.".to_string()),
        Err(keyring::Error::NoEntry) => {
            let mut key = [0_u8; 32];
            OsRng.fill_bytes(&mut key);
            entry
                .set_secret(&key)
                .map_err(|e| format!("Could not protect the workspace key in Windows Credential Manager: {e}"))?;
            Ok(key)
        }
        Err(e) => Err(format!(
            "Could not access the workspace key in Windows Credential Manager: {e}"
        )),
    }
}

pub(crate) fn encrypt_bytes(key: &[u8; 32], plaintext: &[u8]) -> Result<Vec<u8>, String> {
    let cipher = XChaCha20Poly1305::new_from_slice(key)
        .map_err(|_| "Invalid workspace encryption key.".to_string())?;
    let mut nonce = [0_u8; 24];
    OsRng.fill_bytes(&mut nonce);

    let encrypted = cipher
        .encrypt(XNonce::from_slice(&nonce), plaintext)
        .map_err(|_| "Could not encrypt workspace content.".to_string())?;

    let mut output = nonce.to_vec();
    output.extend_from_slice(&encrypted);
    Ok(output)
}

pub(crate) fn decrypt_bytes(key: &[u8; 32], payload: &[u8]) -> Result<Vec<u8>, String> {
    if payload.len() < 24 {
        return Err("Encrypted workspace file is incomplete.".to_string());
    }

    let (nonce, ciphertext) = payload.split_at(24);
    let cipher = XChaCha20Poly1305::new_from_slice(key)
        .map_err(|_| "Invalid workspace encryption key.".to_string())?;

    cipher
        .decrypt(XNonce::from_slice(nonce), ciphertext)
        .map_err(|_| {
            "Could not decrypt workspace content. The key may be unavailable or the file may have changed."
                .to_string()
        })
}

#[tauri::command]
pub fn initialize_workspace(app: AppHandle) -> Result<String, String> {
    let directory = workspace_dir(&app)?;
    fs::create_dir_all(&directory)
        .map_err(|e| format!("Could not create the local workspace: {e}"))?;
    let _key = encryption_key()?;

    Ok("Your local workspace is ready. Its encryption key is protected by Windows Credential Manager."
        .to_string())
}

#[tauri::command]
pub fn save_private_note(app: AppHandle, note: String) -> Result<String, String> {
    if note.len() > 20_000 {
        return Err("This preview note is limited to 20,000 bytes.".to_string());
    }

    let directory = workspace_dir(&app)?;
    fs::create_dir_all(&directory)
        .map_err(|e| format!("Could not create the local workspace: {e}"))?;

    let key = encryption_key()?;
    let encrypted = encrypt_bytes(&key, note.as_bytes())?;
    let destination = directory.join(NOTE_FILE);
    let temporary = directory.join("private-note.enc.tmp");

    fs::write(&temporary, encrypted)
        .map_err(|e| format!("Could not write encrypted note: {e}"))?;

    if destination.exists() {
        fs::remove_file(&destination)
            .map_err(|e| format!("Could not replace previous encrypted note: {e}"))?;
    }

    fs::rename(&temporary, &destination)
        .map_err(|e| format!("Could not finish saving encrypted note: {e}"))?;

    Ok("Private note saved in encrypted form on this device.".to_string())
}

#[tauri::command]
pub fn read_private_note(app: AppHandle) -> Result<String, String> {
    let path = workspace_dir(&app)?.join(NOTE_FILE);
    if !path.exists() {
        return Ok(String::new());
    }

    let key = encryption_key()?;
    let payload =
        fs::read(path).map_err(|e| format!("Could not read encrypted note: {e}"))?;
    let plaintext = decrypt_bytes(&key, &payload)?;

    String::from_utf8(plaintext)
        .map_err(|e| format!("Saved note is not valid UTF-8: {e}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn encrypted_payload_round_trips() {
        let key = [7_u8; 32];
        let original = b"FRIDAY local workspace test";
        let encrypted = encrypt_bytes(&key, original).expect("encryption should succeed");

        assert_ne!(encrypted, original);

        let restored =
            decrypt_bytes(&key, &encrypted).expect("decryption should succeed");
        assert_eq!(restored, original);
    }

    #[test]
    fn tampered_payload_is_rejected() {
        let key = [9_u8; 32];
        let mut encrypted =
            encrypt_bytes(&key, b"private").expect("encryption should succeed");
        let last = encrypted.len() - 1;
        encrypted[last] ^= 0x01;

        assert!(decrypt_bytes(&key, &encrypted).is_err());
    }
}

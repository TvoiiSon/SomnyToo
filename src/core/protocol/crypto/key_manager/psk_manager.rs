use anyhow::{anyhow, Result};
use std::env;
use hex;
use hkdf::Hkdf;
use sha2::Sha256;

pub fn get_psk() -> Result<Vec<u8>> {
    let psk_hex = env::var("PSK_SECRET")
        .map_err(|_| anyhow!("PSK_SECRET environment variable not set"))?;

    if psk_hex.len() < 64 {
        return Err(anyhow!("PSK_SECRET must be at least 64 hex characters long"));
    }

    let psk_bytes = hex::decode(&psk_hex)
        .map_err(|_| anyhow!("PSK_SECRET must be a valid hex string"))?;

    if psk_bytes.len() < 32 {
        return Err(anyhow!("PSK_SECRET must be at least 32 bytes long"));
    }

    Ok(psk_bytes)
}

pub fn derive_psk_keys(psk: &[u8]) -> Result<(Vec<u8>, Vec<u8>)> {
    let hk = Hkdf::<Sha256>::new(None, psk);

    let mut client_key = vec![0u8; 32];
    let mut server_key = vec![0u8; 32];

    hk.expand(b"client-auth-key", &mut client_key)
        .map_err(|_| anyhow!("HKDF expansion failed"))?;
    hk.expand(b"server-auth-key", &mut server_key)
        .map_err(|_| anyhow!("HKDF expansion failed"))?;

    Ok((client_key, server_key))
}

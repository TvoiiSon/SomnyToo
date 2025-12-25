use hkdf::Hkdf;
use sha2::Sha256;

pub struct KeyDeriver;

impl KeyDeriver {
    pub fn derive_keys(shared_secret: &[u8; 32], salt: &[u8], info: &[u8]) -> [u8; 32] {
        let hk = Hkdf::<Sha256>::new(Some(salt), shared_secret);
        let mut key = [0u8; 32];
        hk.expand(info, &mut key).expect("HKDF expansion failed");
        key
    }
}

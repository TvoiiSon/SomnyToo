use aes_gcm::{Aes256Gcm, KeyInit};
use generic_array::GenericArray;
use aes_gcm::aead::Aead;

pub struct AesGcmCipher;

impl AesGcmCipher {
    pub fn new(key: &[u8; 32]) -> Aes256Gcm {
        Aes256Gcm::new_from_slice(key).expect("Invalid key length")
    }

    pub fn encrypt(&self, cipher: &Aes256Gcm, nonce: &[u8], plaintext: &[u8], _aad: &[u8]) -> Result<Vec<u8>, aes_gcm::Error> {
        let nonce = GenericArray::from_slice(nonce);
        cipher.encrypt(nonce, plaintext)
    }

    pub fn decrypt(&self, cipher: &Aes256Gcm, nonce: &[u8], ciphertext: &[u8], _aad: &[u8]) -> Result<Vec<u8>, aes_gcm::Error> {
        let nonce = GenericArray::from_slice(nonce);
        cipher.decrypt(nonce, ciphertext)
    }
}

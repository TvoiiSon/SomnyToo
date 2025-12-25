use hmac::{Hmac, Mac};
use sha2::Sha256;

type HmacSha256 = Hmac<Sha256>;

pub struct HmacSigner;

impl HmacSigner {
    pub fn sign(&self, key: &[u8], data: &[u8]) -> [u8; 32] {
        let mut mac = HmacSha256::new_from_slice(key).expect("Invalid key length");
        mac.update(data);
        mac.finalize().into_bytes().into()
    }

    pub fn verify(&self, key: &[u8], data: &[u8], signature: &[u8]) -> bool {
        let expected = self.sign(key, data);
        constant_time_eq::constant_time_eq(&expected, signature)
    }
}
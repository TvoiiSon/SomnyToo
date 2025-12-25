use constant_time_eq::constant_time_eq;

pub struct SignatureVerifier;

impl SignatureVerifier {
    pub fn verify_constant_time(expected: &[u8], actual: &[u8]) -> bool {
        constant_time_eq(expected, actual)
    }
}
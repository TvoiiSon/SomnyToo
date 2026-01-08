use blake3::Hasher;

/// SIMD-ускоренный Blake3 процессор
pub struct Blake3Accelerator {
    use_avx2: bool,
    use_sse41: bool,
}

impl Blake3Accelerator {
    pub fn new() -> Self {
        #[cfg(target_arch = "x86_64")]
        {
            let use_avx2 = is_x86_feature_detected!("avx2");
            let use_sse41 = is_x86_feature_detected!("sse4.1");

            Self {
                use_avx2,
                use_sse41,
            }
        }

        #[cfg(not(target_arch = "x86_64"))]
        {
            Self {
                use_avx2: false,
                use_sse41: false,
            }
        }
    }

    #[inline]
    pub fn hash_keyed(&self, key: &[u8; 32], input: &[u8]) -> [u8; 32] {
        let mut hasher = Hasher::new_keyed(key);
        hasher.update(input);

        let mut output = [0u8; 32];
        hasher.finalize_xof().fill(&mut output);
        output
    }

    #[inline]
    pub fn hash_keyed_batch(
        &self,
        key: &[u8; 32],
        inputs: &[&[u8]],
        outputs: &mut [[u8; 32]]
    ) {
        assert_eq!(inputs.len(), outputs.len());

        // Параллельная обработка batch
        if inputs.len() >= 4 {
            use rayon::prelude::*;

            inputs.par_iter()
                .zip(outputs.par_iter_mut())
                .for_each(|(input, output)| {
                    *output = self.hash_keyed(key, input);
                });
        } else {
            // Последовательная обработка для маленьких batch
            for (i, input) in inputs.iter().enumerate() {
                outputs[i] = self.hash_keyed(key, input);
            }
        }
    }

    #[inline]
    pub fn derive_key(&self, context: &str, key_material: &[u8]) -> [u8; 32] {
        let mut hasher = Hasher::new_derive_key(context);
        hasher.update(key_material);

        let mut output = [0u8; 32];
        hasher.finalize_xof().fill(&mut output);
        output
    }

    #[inline]
    pub fn derive_key_batch(
        &self,
        context: &str,
        key_materials: &[&[u8]],
        outputs: &mut [[u8; 32]]
    ) {
        assert_eq!(key_materials.len(), outputs.len());

        if key_materials.len() >= 4 {
            use rayon::prelude::*;

            key_materials.par_iter()
                .zip(outputs.par_iter_mut())
                .for_each(|(key_material, output)| {
                    *output = self.derive_key(context, key_material);
                });
        } else {
            for (i, key_material) in key_materials.iter().enumerate() {
                outputs[i] = self.derive_key(context, key_material);
            }
        }
    }
}

impl Clone for Blake3Accelerator {
    fn clone(&self) -> Self {
        Self {
            use_avx2: self.use_avx2,
            use_sse41: self.use_sse41,
        }
    }
}

impl Default for Blake3Accelerator {
    fn default() -> Self {
        Self::new()
    }
}
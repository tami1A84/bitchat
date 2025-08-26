//! LZ4 compression and decompression utilities.

/// Compresses a byte slice using LZ4.
pub fn compress(data: &[u8]) -> Vec<u8> {
    lz4_flex::compress_prepend_size(data)
}

/// Decompresses a byte slice using LZ4.
/// Expects the data to have the original size prepended.
pub fn decompress(data: &[u8]) -> Result<Vec<u8>, lz4_flex::block::DecompressError> {
    lz4_flex::decompress_size_prepended(data)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_compression_decompression_roundtrip() {
        let original_data = b"hello world, this is a test of the lz4 compression system. it should be able to handle this just fine.";
        let compressed_data = compress(original_data);
        let decompressed_data = decompress(&compressed_data).unwrap();

        assert_ne!(original_data.to_vec(), compressed_data);
        assert_eq!(original_data.to_vec(), decompressed_data);
    }

    #[test]
    fn test_decompress_empty() {
        let compressed = compress(b"");
        let decompressed = decompress(&compressed).unwrap();
        assert_eq!(decompressed, b"");
    }

    #[test]
    fn test_decompress_invalid_data() {
        let invalid_data = vec![1, 2, 3, 4];
        let result = decompress(&invalid_data);
        assert!(result.is_err());
    }
}

use crate::common::*;
use crate::decode::DecodeError;

mod common;
mod decode;
mod encode;

// NOTE: The original rbase64 crate set MiMalloc as the global allocator here,
// which conflicts with binaries that define their own global allocator (e.g.
// jemalloc in reth).  This patched version removes that declaration so that
// the crate can be used as a library without hijacking the allocator.

pub fn encode(input: &[u8]) -> String {
    let mut buffer = vec![0; ((input.len() / 3) + 1) * 4];
    let total_chunks = input.len() / (ENC_CHUNK_SIZE * 3);

    encode::encode_u128_chunks(input, &mut buffer);

    let bytes_rem = encode::encode_u128_remainder(
        &input[ENC_CHUNK_SIZE * total_chunks * 3..],
        &mut buffer[ENC_CHUNK_SIZE * total_chunks * 4..],
    );

    buffer.truncate(ENC_CHUNK_SIZE * total_chunks * 4 + bytes_rem);

    // SAFETY: The buffer only contains bytes from the base64 alphabet (A-Z, a-z, 0-9, +, /, =),
    // which are all valid single-byte UTF-8 characters.
    unsafe { String::from_utf8_unchecked(buffer) }
}

pub fn decode(encoded: &str) -> Result<Vec<u8>, DecodeError> {
    let input = encoded.as_bytes();
    let mut buffer = vec![0; ((input.len() + 3) / 4) * 3];

    let total_chunks = input.len().saturating_sub(2) / (DEC_CHUNK_SIZE * 4);
    let in_limit = total_chunks * DEC_CHUNK_SIZE * 4;
    let out_limit = total_chunks * DEC_CHUNK_SIZE * 3;

    decode::decode_u64_chunks(&input[..in_limit], &mut buffer)?;

    let bytes_rem = decode::decode_u64_remainder(&input[in_limit..], &mut buffer[out_limit..])?;

    buffer.truncate(out_limit + bytes_rem);
    Ok(buffer)
}

//! Cryptographically secure random identifiers without UUID dependency.

pub fn secure_random_hex(bytes: usize) -> Result<String, getrandom::Error> {
    let mut buffer = vec![0_u8; bytes];
    getrandom::getrandom(&mut buffer)?;
    Ok(buffer.iter().map(|byte| format!("{byte:02x}")).collect())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn produces_requested_hex_length_and_distinct_values() {
        let first = secure_random_hex(16).unwrap();
        let second = secure_random_hex(16).unwrap();
        assert_eq!(first.len(), 32);
        assert_eq!(second.len(), 32);
        assert!(first.bytes().all(|byte| byte.is_ascii_hexdigit()));
        assert_ne!(first, second);
    }
}

//! Version 4 UUIDs for the ids the app makes itself: a new Linear issue's and a Linear
//! comment's client id, so a resend names the same issue or comment (spec section 9, T6).
//! The bytes come from `/dev/urandom`, which both supported platforms have (plan
//! Decision 6), so no dependency is needed.

use std::fmt::Write as _;
use std::io::Read;

/// A random (version 4) UUID, lowercase `8-4-4-4-12`.
pub fn new_v4() -> std::io::Result<String> {
    let mut bytes = [0u8; 16];
    std::fs::File::open("/dev/urandom")?.read_exact(&mut bytes)?;
    Ok(format_v4(bytes))
}

/// `bytes` as a UUID with the version (4) and RFC 4122 variant bits set.
fn format_v4(mut bytes: [u8; 16]) -> String {
    bytes[6] = (bytes[6] & 0x0f) | 0x40;
    bytes[8] = (bytes[8] & 0x3f) | 0x80;
    let mut out = String::with_capacity(36);
    for (i, b) in bytes.iter().enumerate() {
        if matches!(i, 4 | 6 | 8 | 10) {
            out.push('-');
        }
        let _ = write!(out, "{b:02x}");
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn v4_has_the_version_and_variant_bits_and_differs_each_time() {
        let a = new_v4().unwrap();
        let b = new_v4().unwrap();
        for id in [&a, &b] {
            assert_eq!(id.len(), 36, "{id}");
            for (i, c) in id.char_indices() {
                match i {
                    8 | 13 | 18 | 23 => assert_eq!(c, '-', "{id}"),
                    _ => assert!(matches!(c, '0'..='9' | 'a'..='f'), "{id}"),
                }
            }
            assert_eq!(&id[14..15], "4", "version 4: {id}");
            assert!("89ab".contains(&id[19..20]), "RFC 4122 variant: {id}");
        }
        assert_ne!(a, b);
        // The bits are set whatever the random bytes are.
        assert_eq!(format_v4([0; 16]), "00000000-0000-4000-8000-000000000000");
        assert_eq!(
            format_v4([0xff; 16]),
            "ffffffff-ffff-4fff-bfff-ffffffffffff"
        );
        let counting: [u8; 16] = std::array::from_fn(|i| i as u8);
        assert_eq!(format_v4(counting), "00010203-0405-4607-8809-0a0b0c0d0e0f");
    }
}

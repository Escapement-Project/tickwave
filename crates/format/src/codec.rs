//! Unsigned LEB128, the coding of a digital channel's intervals.

pub fn put_uleb(out: &mut Vec<u8>, mut v: u64) {
    loop {
        let byte = (v & 0x7f) as u8;
        v >>= 7;
        if v == 0 {
            out.push(byte);
            return;
        }
        out.push(byte | 0x80);
    }
}

/// None at the end of `bytes`, or for a value that does not fit in 64 bits.
pub fn get_uleb(bytes: &[u8], pos: &mut usize) -> Option<u64> {
    let mut v = 0u64;
    let mut shift = 0;
    loop {
        let byte = *bytes.get(*pos)?;
        *pos += 1;
        // At shift 63 only the value's top bit is left: anything more overflows.
        if shift == 63 && byte > 1 {
            return None;
        }
        v |= u64::from(byte & 0x7f) << shift;
        if byte & 0x80 == 0 {
            return Some(v);
        }
        shift += 7;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn known_encodings() {
        // From the LEB128 examples in the DWARF specification, section 7.6.
        for (value, bytes) in [
            (2u64, &[2u8][..]),
            (127, &[127]),
            (128, &[0x80, 1]),
            (129, &[0x81, 1]),
            (130, &[0x82, 1]),
            (12857, &[0xb9, 100]),
        ] {
            let mut out = Vec::new();
            put_uleb(&mut out, value);
            assert_eq!(out, bytes, "{value}");
            let mut pos = 0;
            assert_eq!(get_uleb(bytes, &mut pos), Some(value));
            assert_eq!(pos, bytes.len());
        }
    }

    #[test]
    fn round_trips_the_extremes() {
        for value in [0, 1, u64::from(u32::MAX), u64::MAX - 1, u64::MAX] {
            let mut out = Vec::new();
            put_uleb(&mut out, value);
            let mut pos = 0;
            assert_eq!(get_uleb(&out, &mut pos), Some(value));
        }
    }

    #[test]
    fn refuses_overflow_and_truncation() {
        let mut too_big = vec![0xff; 9];
        too_big.push(0x02);
        assert_eq!(get_uleb(&too_big, &mut 0), None);
        assert_eq!(get_uleb(&[0x80, 0x80], &mut 0), None);
    }
}

/// NEON SIMD byte classifier for fast ASCII scanning.
///
/// Classifies 16 bytes at a time: printable ASCII (0x20..0x7E) are "fast",
/// everything else (control, DEL, high bytes) needs attention.
///
/// Adapted from tty.app's parser/simd.rs.
pub struct SimdScanner;

impl SimdScanner {
    /// Scan the buffer for a contiguous run of printable ASCII.
    /// Returns the length of the printable ASCII prefix.
    #[cfg(target_arch = "aarch64")]
    pub fn scan(buf: &[u8]) -> usize {
        use core::arch::aarch64::*;

        let len = buf.len();
        let mut pos = 0;

        // SAFETY: All NEON intrinsics operate on data within `buf`.
        // Loads are guarded by `pos + N <= len` checks.
        unsafe {
            let lo = vdupq_n_u8(0x20);
            let hi = vdupq_n_u8(0x7F);

            // Process 64 bytes at a time (4 × 16 unrolled)
            while pos + 64 <= len {
                let ptr = buf.as_ptr().add(pos);
                let c0 = vld1q_u8(ptr);
                let c1 = vld1q_u8(ptr.add(16));
                let c2 = vld1q_u8(ptr.add(32));
                let c3 = vld1q_u8(ptr.add(48));

                let ok0 = vandq_u8(vcgeq_u8(c0, lo), vcltq_u8(c0, hi));
                let ok1 = vandq_u8(vcgeq_u8(c1, lo), vcltq_u8(c1, hi));
                let ok2 = vandq_u8(vcgeq_u8(c2, lo), vcltq_u8(c2, hi));
                let ok3 = vandq_u8(vcgeq_u8(c3, lo), vcltq_u8(c3, hi));

                let all = vandq_u8(vandq_u8(ok0, ok1), vandq_u8(ok2, ok3));

                if vminvq_u8(all) == 0xFF {
                    pos += 64;
                    continue;
                }

                if vminvq_u8(ok0) != 0xFF {
                    return pos + Self::find_first_zero(ok0);
                }
                pos += 16;
                if vminvq_u8(ok1) != 0xFF {
                    return pos + Self::find_first_zero(ok1);
                }
                pos += 16;
                if vminvq_u8(ok2) != 0xFF {
                    return pos + Self::find_first_zero(ok2);
                }
                pos += 16;
                return pos + Self::find_first_zero(ok3);
            }

            // Process remaining 16-byte chunks
            while pos + 16 <= len {
                let c = vld1q_u8(buf.as_ptr().add(pos));
                let ok = vandq_u8(vcgeq_u8(c, lo), vcltq_u8(c, hi));
                if vminvq_u8(ok) == 0xFF {
                    pos += 16;
                } else {
                    return pos + Self::find_first_zero(ok);
                }
            }
        }

        // Scalar tail
        while pos < len {
            let b = buf[pos];
            if b < 0x20 || b >= 0x7F {
                return pos;
            }
            pos += 1;
        }
        pos
    }

    #[cfg(target_arch = "aarch64")]
    #[inline]
    unsafe fn find_first_zero(v: core::arch::aarch64::uint8x16_t) -> usize {
        use core::arch::aarch64::*;
        // Narrow 16×u8 to 8×u8 nibbles, find first zero nibble
        let narrowed = vshrn_n_u16::<4>(vreinterpretq_u16_u8(v));
        let bits = vget_lane_u64::<0>(vreinterpret_u64_u8(narrowed));
        let zero_mask =
            bits.wrapping_sub(0x1111_1111_1111_1111) & !bits & 0x8888_8888_8888_8888;
        (zero_mask.trailing_zeros() / 4) as usize
    }

    /// Scalar fallback for non-aarch64.
    #[cfg(not(target_arch = "aarch64"))]
    pub fn scan(buf: &[u8]) -> usize {
        let mut pos = 0;
        while pos < buf.len() {
            let b = buf[pos];
            if b < 0x20 || b >= 0x7F {
                return pos;
            }
            pos += 1;
        }
        pos
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn scan_all_ascii() {
        let data = b"Hello, World! This is plain ASCII text.";
        assert_eq!(SimdScanner::scan(data), data.len());
    }

    #[test]
    fn scan_empty() {
        assert_eq!(SimdScanner::scan(b""), 0);
    }

    #[test]
    fn scan_starts_with_control() {
        assert_eq!(SimdScanner::scan(b"\x1bHello"), 0);
    }

    #[test]
    fn scan_control_in_middle() {
        assert_eq!(SimdScanner::scan(b"Hello\nWorld"), 5);
    }

    #[test]
    fn scan_long_ascii() {
        // Test the 64-byte SIMD path
        let data = vec![b'A'; 256];
        assert_eq!(SimdScanner::scan(&data), 256);
    }

    #[test]
    fn scan_long_with_break() {
        let mut data = vec![b'X'; 200];
        data[150] = 0x1B; // ESC in the middle
        assert_eq!(SimdScanner::scan(&data), 150);
    }

    #[test]
    fn scan_exactly_64_bytes() {
        let data = vec![b'Z'; 64];
        assert_eq!(SimdScanner::scan(&data), 64);
    }

    #[test]
    fn scan_del_stops() {
        assert_eq!(SimdScanner::scan(b"abc\x7Fdef"), 3);
    }
}

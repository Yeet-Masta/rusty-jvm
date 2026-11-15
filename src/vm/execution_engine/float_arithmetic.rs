//! The floating-point instructions whose result HotSpot's interpreter decides beyond what the
//! language specifies: which NaN comes out of an addition, a multiplication or a remainder with
//! NaN operands.
//!
//! The Java language only says that the result is a NaN. Its bits are visible all the same
//! (`Double.doubleToRawLongBits`), so they are HotSpot's here, as its interpreter (`-Xint`)
//! computes them; its optimizing compiler may give other NaNs for the same expression.
//!
//! On x86-64, HotSpot adds and multiplies with the top of the operand stack as the destination
//! of the machine instruction (`addss xmm0, [rsp]` in `TemplateTable::fop2`), and the machine
//! returns the destination when both operands are NaNs. Subtraction and division have the first
//! operand as their destination, which is what the compiled Rust does too. The remainder is
//! computed by HotSpot's `fmod` stub (`StubGenerator::generate_libmFmod`), of which there are
//! three, chosen by the processor: two for processors with fused multiply-add, which treat NaNs
//! alike, and one that runs the x87 `fprem` instruction.
//!
//! On other architectures the operations are the machine's, as in HotSpot.

/// What the machine makes of a NaN operand it returns: the same NaN, quiet.
macro_rules! quiet {
    ($float:ty, $value:expr, $quiet_bit:expr) => {
        <$float>::from_bits($value.to_bits() | $quiet_bit)
    };
}

/// The remainder of two values of which one at least is a NaN, as the `fmod` stub for processors
/// with fused multiply-add returns it.
///
/// The stub works on the absolute values `a` and `b`. With a NaN among them, the quotient `q`
/// it starts with is a NaN, and it ends in one of two places: `y + y` when `y` is the NaN and
/// `x` is finite, and `-(q * b) + a` in a fused multiply-add otherwise, which returns `b`, `q`
/// or `a`, the first of them that is a NaN.
macro_rules! fma_nan_remainder {
    ($float:ty, $x:expr, $y:expr, $quiet_bit:expr) => {{
        let (x, y) = ($x, $y);
        if y.is_nan() {
            if x.is_finite() {
                quiet!($float, y, $quiet_bit)
            } else {
                quiet!($float, y.abs(), $quiet_bit)
            }
        } else {
            quiet!($float, x.abs(), $quiet_bit)
        }
    }};
}

/// The remainder of two values of which one at least is a NaN, as the x87 `fprem` instruction
/// returns it: the NaN operand; of two, the one with the larger significand once both are quiet;
/// of two with the same significand, the positive one.
macro_rules! x87_nan_remainder {
    ($float:ty, $x:expr, $y:expr, $quiet_bit:expr, $sign_bit:expr) => {{
        let x = $x.to_bits() | $quiet_bit;
        let y = $y.to_bits() | $quiet_bit;
        let bits = if !$y.is_nan() {
            x
        } else if !$x.is_nan() {
            y
        } else if (x & !$sign_bit) != (y & !$sign_bit) {
            // Both have every exponent bit set, so the larger significand is the larger value.
            if (x & !$sign_bit) > (y & !$sign_bit) {
                x
            } else {
                y
            }
        } else {
            x & y
        };
        <$float>::from_bits(bits)
    }};
}

/// Whether HotSpot runs one of its `fmod` stubs built on fused multiply-add on this processor,
/// rather than the x87 one.
#[cfg(target_arch = "x86_64")]
fn fmod_uses_fma() -> bool {
    std::arch::is_x86_feature_detected!("fma")
}

macro_rules! arithmetic {
    ($float:ty, $quiet_bit:expr, $sign_bit:expr, $invalid:expr, $add:ident, $mul:ident, $rem:ident) => {
        /// `a + b`, with HotSpot's NaN when both are NaNs.
        #[inline]
        pub(crate) fn $add(a: $float, b: $float) -> $float {
            #[cfg(target_arch = "x86_64")]
            if b.is_nan() {
                return quiet!($float, b, $quiet_bit);
            } else if a.is_nan() {
                return quiet!($float, a, $quiet_bit);
            }
            a + b
        }

        /// `a * b`, with HotSpot's NaN when both are NaNs.
        #[inline]
        pub(crate) fn $mul(a: $float, b: $float) -> $float {
            #[cfg(target_arch = "x86_64")]
            if b.is_nan() {
                return quiet!($float, b, $quiet_bit);
            } else if a.is_nan() {
                return quiet!($float, a, $quiet_bit);
            }
            a * b
        }

        /// `x % y`, with HotSpot's NaN where the result is one.
        #[inline]
        pub(crate) fn $rem(x: $float, y: $float) -> $float {
            #[cfg(target_arch = "x86_64")]
            {
                if x.is_nan() || y.is_nan() {
                    return if fmod_uses_fma() {
                        fma_nan_remainder!($float, x, y, $quiet_bit)
                    } else {
                        x87_nan_remainder!($float, x, y, $quiet_bit, $sign_bit)
                    };
                }
                if x.is_infinite() || y == 0.0 {
                    // An invalid operation: the machine's default NaN, which is negative.
                    return <$float>::from_bits($invalid);
                }
            }
            x % y
        }
    };
}

arithmetic!(
    f32,
    0x0040_0000u32,
    0x8000_0000u32,
    0xffc0_0000u32,
    fadd,
    fmul,
    frem
);
arithmetic!(
    f64,
    0x0008_0000_0000_0000u64,
    0x8000_0000_0000_0000u64,
    0xfff8_0000_0000_0000u64,
    dadd,
    dmul,
    drem
);

#[cfg(all(test, target_arch = "x86_64"))]
mod tests {
    use super::*;

    const QUIET: u64 = 0x7ff8_0000_0000_0000;
    const QUIET_PAYLOAD: u64 = 0x7ff8_0000_0000_0123;
    const NEGATIVE: u64 = 0xfff8_0000_0000_0000;
    const SIGNALING: u64 = 0x7ff0_0000_0000_0123;
    const NEGATIVE_SIGNALING: u64 = 0xfff0_0000_0000_0123;

    fn d(bits: u64) -> f64 {
        f64::from_bits(bits)
    }

    #[test]
    fn should_return_the_second_nan_of_a_sum_or_product() {
        assert_eq!(dadd(d(QUIET), d(QUIET_PAYLOAD)).to_bits(), QUIET_PAYLOAD);
        assert_eq!(dadd(d(QUIET_PAYLOAD), d(NEGATIVE)).to_bits(), NEGATIVE);
        assert_eq!(dmul(d(QUIET), d(SIGNALING)).to_bits(), QUIET_PAYLOAD);
        assert_eq!(dmul(d(SIGNALING), 1.0).to_bits(), QUIET_PAYLOAD);
        assert_eq!(dadd(1.0, d(NEGATIVE_SIGNALING)).to_bits(), NEGATIVE | 0x123);
        let sum = fadd(f32::from_bits(0x7fc0_0123), f32::from_bits(0xffc0_0000));
        assert_eq!(sum.to_bits(), 0xffc0_0000);
        assert_eq!(
            fmul(f32::from_bits(0x7f80_0123), 2.0).to_bits(),
            0x7fc0_0123
        );
        let infinity = std::hint::black_box(f64::INFINITY);
        assert_eq!(dadd(infinity, -infinity).to_bits(), NEGATIVE);
        assert_eq!(dadd(1.5, 2.25), 3.75);
        assert_eq!(dmul(1.5, 2.0), 3.0);
    }

    // The expected values are HotSpot's (`java -Xint`), on a processor with fused multiply-add.
    #[test]
    fn should_return_the_nan_of_the_fused_multiply_add_fmod_stub() {
        let rem = |x: u64, y: u64| {
            fma_nan_remainder!(f64, d(x), d(y), 0x0008_0000_0000_0000u64).to_bits()
        };
        assert_eq!(rem(QUIET_PAYLOAD, QUIET), QUIET);
        assert_eq!(rem(QUIET, QUIET_PAYLOAD), QUIET_PAYLOAD);
        assert_eq!(rem(NEGATIVE, NEGATIVE), QUIET);
        assert_eq!(rem(NEGATIVE, NEGATIVE_SIGNALING), QUIET_PAYLOAD);
        assert_eq!(rem(NEGATIVE_SIGNALING, 1.0f64.to_bits()), QUIET_PAYLOAD);
        assert_eq!(rem(1.0f64.to_bits(), NEGATIVE), NEGATIVE);
        assert_eq!(rem(1.0f64.to_bits(), NEGATIVE_SIGNALING), NEGATIVE | 0x123);
        assert_eq!(rem(f64::INFINITY.to_bits(), NEGATIVE), QUIET);
        assert_eq!(
            rem(f64::NEG_INFINITY.to_bits(), NEGATIVE_SIGNALING),
            QUIET_PAYLOAD
        );
    }

    // The expected values are HotSpot's (`java -Xint -XX:UseAVX=0`).
    #[test]
    fn should_return_the_nan_of_the_x87_fmod_stub() {
        let rem = |x: u64, y: u64| {
            x87_nan_remainder!(
                f64,
                d(x),
                d(y),
                0x0008_0000_0000_0000u64,
                0x8000_0000_0000_0000u64
            )
            .to_bits()
        };
        assert_eq!(rem(QUIET_PAYLOAD, QUIET), QUIET_PAYLOAD);
        assert_eq!(rem(QUIET, QUIET_PAYLOAD), QUIET_PAYLOAD);
        assert_eq!(rem(QUIET, NEGATIVE), QUIET);
        assert_eq!(rem(NEGATIVE, QUIET), QUIET);
        assert_eq!(rem(NEGATIVE, NEGATIVE), NEGATIVE);
        assert_eq!(rem(QUIET, NEGATIVE_SIGNALING), NEGATIVE | 0x123);
        assert_eq!(rem(NEGATIVE_SIGNALING, QUIET), NEGATIVE | 0x123);
        assert_eq!(rem(NEGATIVE_SIGNALING, SIGNALING), QUIET_PAYLOAD);
        assert_eq!(
            rem(NEGATIVE_SIGNALING, NEGATIVE_SIGNALING),
            NEGATIVE | 0x123
        );
        assert_eq!(rem(1.0f64.to_bits(), NEGATIVE_SIGNALING), NEGATIVE | 0x123);
        assert_eq!(rem(NEGATIVE, 1.0f64.to_bits()), NEGATIVE);
    }

    #[test]
    fn should_compute_remainders() {
        assert_eq!(drem(5.5, 2.0), 1.5);
        assert_eq!(drem(-5.5, 2.0), -1.5);
        assert_eq!(drem(-0.0, 2.0).to_bits(), (-0.0f64).to_bits());
        assert_eq!(drem(3.0, f64::INFINITY), 3.0);
        assert_eq!(drem(f64::INFINITY, 3.0).to_bits(), NEGATIVE);
        assert_eq!(drem(3.0, 0.0).to_bits(), NEGATIVE);
        assert_eq!(frem(5.5, -2.0), 1.5);
        assert_eq!(frem(1.0, 0.0).to_bits(), 0xffc0_0000);
    }
}

use crate::vm::error::Result;

/// `java.lang.Float.floatToRawIntBits(F)I`
pub(crate) fn float_to_raw_int_bits(value: f32) -> Result<i32> {
    Ok(value.to_bits() as i32)
}

/// `java.lang.Float.intBitsToFloat(I)F`
pub(crate) fn int_bits_to_float(bits: i32) -> Result<f32> {
    Ok(f32::from_bits(bits as u32))
}

use crate::nibble::U4;

pub fn convert_packed_u8(bcd: u8) -> Result<u8, ()> {
    let digit_0 = U4::from_hi(bcd).to_lo();
    let digit_1 = U4::from_lo(bcd).to_lo();

    if digit_0 > 9 || digit_1 > 9 {
        Err(())
    } else {
        Ok(digit_0 * 10 + digit_1)
    }
}

pub fn convert_packed_u16(bcd: u16) -> Result<u16, ()> {
    let digits_0 = convert_packed_u8((bcd >> 8) as u8)? as u16;
    let digits_1 = convert_packed_u8((bcd & 0xff) as u8)? as u16;

    Ok(digits_0 * 100 + digits_1)
}

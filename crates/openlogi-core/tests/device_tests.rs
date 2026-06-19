#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_adjust_dpi() {
        let device = Device::new();
        device.adjust_dpi(800);
        assert_eq!(device.get_dpi(), 800);
    }
}

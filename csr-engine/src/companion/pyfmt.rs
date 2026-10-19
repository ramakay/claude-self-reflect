/// Python round() uses ties to even; format! follows the same rule for a
/// binary float at a fixed precision (including values such as 2.675).
pub fn round(value: f64) -> f64 {
    value.round_ties_even()
}
pub fn fixed(value: f64, digits: usize) -> String {
    format!("{value:.digits$}")
}
pub fn general(value: f64) -> String {
    if value == 0. {
        return if value.is_sign_negative() {
            "-0".into()
        } else {
            "0".into()
        };
    }
    if !value.is_finite() {
        return if value.is_nan() {
            "nan".into()
        } else if value.is_sign_negative() {
            "-inf".into()
        } else {
            "inf".into()
        };
    }
    let scientific = format!("{value:.5e}");
    let (mant, exponent) = scientific.split_once('e').unwrap();
    let exp = exponent.parse::<i32>().unwrap();
    let digits = 6;
    let rounded = if exp < -4 || exp >= digits {
        format!(
            "{}e{:+03}",
            mant.trim_end_matches('0').trim_end_matches('.'),
            exp
        )
    } else {
        format!("{:.*}", (digits - 1 - exp).max(0) as usize, value)
    };
    if rounded.contains('e') {
        rounded
    } else if rounded.contains('.') {
        rounded.trim_end_matches('0').trim_end_matches('.').into()
    } else {
        rounded
    }
}
pub fn short(n: f64) -> String {
    for (size, unit) in [(1e9, "B"), (1e6, "M"), (1e3, "k")] {
        if n >= size {
            let v = n / size;
            let s = fixed(v, usize::from(v < 10.));
            return format!(
                "{}{unit}",
                if v < 10. {
                    s.trim_end_matches('0').trim_end_matches('.')
                } else {
                    &s
                }
            );
        }
    }
    format!("{}", n as i64)
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn test_python_round_is_half_to_even() {
        assert_eq!(
            [round(0.5), round(1.5), round(2.5), round(-2.5)],
            [0., 2., 2., -2.]
        );
    }
    #[test]
    fn test_fixed_uses_the_binary_value() {
        assert_eq!(fixed(2.675, 2), "2.67");
        assert_eq!(fixed(1.25, 1), "1.2");
    }
    #[test]
    fn test_short() {
        assert_eq!(short(12500.), "12k");
        assert_eq!(short(2500.), "2.5k");
        assert_eq!(short(1e6), "1M");
    }
    #[test]
    fn test_general() {
        assert_eq!(general(1e6), "1e+06");
        assert_eq!(general(0.00001), "1e-05");
        assert_eq!(general(12.3), "12.3");
        assert_eq!(general(999999.9), "1e+06");
        assert_eq!(general(0.00009999999), "0.0001");
    }
}

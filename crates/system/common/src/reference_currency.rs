//! The reference currencies and their series letters.

/// Each reference currency: its ISO 4217 numeric code and its series letter.
/// CNY, HKD, JPY, GBP, USD, EUR.
///
/// A new reference currency is a code change that names its letter: the first
/// letter of its alpha-3 code, or a free letter when that one is in use. A
/// letter never changes and is never given to a second currency.
pub const REFERENCE_CURRENCY_LETTERS: [(u16, u8); 6] = [
    (156, b'C'),
    (344, b'H'),
    (392, b'J'),
    (826, b'G'),
    (840, b'U'),
    (978, b'E'),
];

// The codes are sorted and unique. The letters are `A-Z` and distinct.
const _: () = {
    let list = REFERENCE_CURRENCY_LETTERS;
    let mut i = 0;
    while i < list.len() {
        assert!(i == 0 || list[i - 1].0 < list[i].0);
        assert!(list[i].1.is_ascii_uppercase());
        let mut j = 0;
        while j < i {
            assert!(list[j].1 != list[i].1);
            j += 1;
        }
        i += 1;
    }
};

/// The series letter of a reference currency, or `None` for another currency.
pub const fn series_letter(iso: u16) -> Option<u8> {
    let mut i = 0;
    while i < REFERENCE_CURRENCY_LETTERS.len() {
        if REFERENCE_CURRENCY_LETTERS[i].0 == iso {
            return Some(REFERENCE_CURRENCY_LETTERS[i].1);
        }
        i += 1;
    }
    None
}

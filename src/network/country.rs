//! ISO 3166-1 alpha-2 country codes for the wifi regulatory-domain prompt.
//!
//! The CYW43455 (Raspberry Pi onboard wifi) and other brcmfmac chips boot in the
//! restrictive world regulatory domain ("00"). In that domain the firmware
//! refuses the access point's channel — `brcmf_set_channel ... fail, reason -52`
//! in the kernel log — so association never completes and DHCP silently fails.
//! Setting a country with `iw reg set <CC>` before associating lifts the
//! restriction. This module is the list the installer presents for that choice.

/// A selectable regulatory country: ISO 3166-1 alpha-2 code plus display name.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Country {
    pub code: &'static str,
    pub name: &'static str,
}

/// (code, name) source table. **US is first on purpose** — it is the most common
/// target and the requirement is that it sit at the top of the picker. Every
/// other entry follows in alphabetical order by name. `country_list` preserves
/// this order; the filter preserves it too, so US stays at the top of an empty
/// or matching search.
const COUNTRIES: &[(&str, &str)] = &[
    ("US", "United States"),
    ("AU", "Australia"),
    ("AT", "Austria"),
    ("BE", "Belgium"),
    ("BR", "Brazil"),
    ("BG", "Bulgaria"),
    ("CA", "Canada"),
    ("CL", "Chile"),
    ("CN", "China"),
    ("CO", "Colombia"),
    ("HR", "Croatia"),
    ("CZ", "Czechia"),
    ("DK", "Denmark"),
    ("EE", "Estonia"),
    ("FI", "Finland"),
    ("FR", "France"),
    ("DE", "Germany"),
    ("GR", "Greece"),
    ("HK", "Hong Kong"),
    ("HU", "Hungary"),
    ("IS", "Iceland"),
    ("IN", "India"),
    ("ID", "Indonesia"),
    ("IE", "Ireland"),
    ("IL", "Israel"),
    ("IT", "Italy"),
    ("JP", "Japan"),
    ("KR", "Korea, Republic of"),
    ("LV", "Latvia"),
    ("LT", "Lithuania"),
    ("LU", "Luxembourg"),
    ("MY", "Malaysia"),
    ("MX", "Mexico"),
    ("NL", "Netherlands"),
    ("NZ", "New Zealand"),
    ("NO", "Norway"),
    ("PH", "Philippines"),
    ("PL", "Poland"),
    ("PT", "Portugal"),
    ("RO", "Romania"),
    ("RU", "Russian Federation"),
    ("SG", "Singapore"),
    ("SK", "Slovakia"),
    ("SI", "Slovenia"),
    ("ZA", "South Africa"),
    ("ES", "Spain"),
    ("SE", "Sweden"),
    ("CH", "Switzerland"),
    ("TW", "Taiwan"),
    ("TH", "Thailand"),
    ("TR", "Türkiye"),
    ("UA", "Ukraine"),
    ("GB", "United Kingdom"),
    ("VN", "Vietnam"),
];

/// The full country list in display order (US first, then alphabetical).
pub fn country_list() -> Vec<Country> {
    COUNTRIES
        .iter()
        .map(|&(code, name)| Country { code, name })
        .collect()
}

/// Case-insensitive substring filter over both code and name, preserving the
/// source order (so US stays on top). An empty/whitespace query returns the full
/// list. A two-letter query that is an exact code match still also matches by
/// substring, so typing "us" surfaces United States without special-casing.
pub fn filter_countries(query: &str) -> Vec<Country> {
    let q = query.trim().to_ascii_lowercase();
    country_list()
        .into_iter()
        .filter(|c| {
            q.is_empty()
                || c.code.to_ascii_lowercase().contains(&q)
                || c.name.to_ascii_lowercase().contains(&q)
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn us_is_first() {
        assert_eq!(country_list()[0].code, "US");
        // ...and stays first with an empty filter.
        assert_eq!(filter_countries("")[0].code, "US");
    }

    #[test]
    fn codes_are_two_upper_letters_and_unique() {
        let list = country_list();
        let mut seen = std::collections::HashSet::new();
        for c in &list {
            assert!(
                c.code.len() == 2 && c.code.chars().all(|ch| ch.is_ascii_uppercase()),
                "bad code: {:?}",
                c.code
            );
            assert!(seen.insert(c.code), "duplicate code: {}", c.code);
        }
    }

    #[test]
    fn filter_matches_code_and_name_case_insensitively() {
        assert!(filter_countries("united").iter().any(|c| c.code == "US"));
        assert!(filter_countries("GB").iter().any(|c| c.code == "GB"));
        assert!(filter_countries("germ").iter().any(|c| c.code == "DE"));
        assert!(filter_countries("zz").is_empty());
    }

    #[test]
    fn non_us_tail_is_alphabetical_by_name() {
        let list = country_list();
        // Skip US (index 0); the remainder must be sorted by name.
        for pair in list[1..].windows(2) {
            assert!(
                pair[0].name <= pair[1].name,
                "out of order: {:?} before {:?}",
                pair[0].name,
                pair[1].name
            );
        }
    }
}
